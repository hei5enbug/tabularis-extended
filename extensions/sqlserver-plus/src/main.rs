//! Entry point: read JSON-RPC lines from stdin, dispatch, write responses.
//!
//! Requests are fanned out to a small worker pool so a slow query on one
//! connection does not block a `ping` or metadata call on another. Responses
//! are funneled through a single writer task so concurrent handlers never
//! interleave bytes on stdout.

use std::{sync::Arc, time::Duration};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{mpsc, watch, Semaphore},
    task::JoinSet,
    time::sleep,
};

mod common;
mod connection;
mod driver;
mod handlers;
mod models;
mod pool_manager;
mod rpc;
mod settings;

// This controls JSON-RPC dispatch concurrency rather than database
// connection concurrency, which is configured separately by max_pool_size.
const WORKER_POOL_SIZE: usize = 8;

// Queued and active requests are separately bounded. Control requests bypass
// admission so cancellation remains available while the database queue is full.
const REQUEST_QUEUE_CAPACITY: usize = 64;
const RESPONSE_QUEUE_CAPACITY: usize = 64;

// The TDS client's async call chains produce large futures (especially in
// debug builds). A local SQL Server 2022 debug execute_query probe overflowed
// tokio's default 2 MiB stack while 4 MiB completed. The full release live
// suite also passed at 4 MiB in SS-045, but that does not remove the debug or
// cross-platform risk. Keep 16 MiB as a deliberate 4x margin until the preview
// client flattens those polling chains or CI stress proves less everywhere.
const WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(WORKER_STACK_SIZE)
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(run());
    runtime.shutdown_timeout(Duration::from_secs(1));
}

async fn run() {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let cleanup_handle = tokio::spawn(run_pool_cleanup(shutdown_rx.clone()));

    let (resp_tx, resp_rx) = mpsc::channel::<String>(RESPONSE_QUEUE_CAPACITY);
    let writer_handle = tokio::spawn(run_writer(resp_rx, shutdown_tx.clone()));
    let mut tasks = JoinSet::new();
    run_reader(resp_tx.clone(), &mut tasks, shutdown_rx).await;
    driver::service::registry::cancel_all();

    let _ = shutdown_tx.send(true);

    while tasks.join_next().await.is_some() {}
    driver::service::sessions::shutdown().await;
    pool_manager::shutdown().await;
    drop(resp_tx);
    let _ = writer_handle.await;
    let _ = cleanup_handle.await;
}

async fn run_pool_cleanup(mut shutdown_rx: watch::Receiver<bool>) {
    let mut settings_rx = settings::subscribe();
    let mut service_cleanup = tokio::time::interval(Duration::from_secs(10));
    loop {
        let cleanup_interval = settings::current().pool_idle_eviction_interval();
        tokio::select! {
            _ = service_cleanup.tick() => {
                driver::service::sessions::cleanup().await;
                driver::service::registry::cleanup();
                driver::service::pool::cleanup().await;
            },
            _ = sleep(cleanup_interval) => pool_manager::cleanup_idle_pools().await,
            // Reset the timer immediately when initialize supplies an override.
            result = settings_rx.changed() => {
                if result.is_err() {
                    break;
                }
            },
            _ = shutdown_rx.changed() => break,
        }
    }
}

async fn read_frame<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> std::io::Result<Option<Result<String, ()>>> {
    let mut frame = Vec::new();
    let mut oversized = false;
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok(if frame.is_empty() && !oversized {
                None
            } else {
                Some(Err(()))
            });
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let length = newline.map_or(buffer.len(), |index| index + 1);
        if !oversized {
            if frame.len() + length > 16 * 1024 * 1024 {
                oversized = true;
                frame.clear();
            } else {
                frame.extend_from_slice(&buffer[..length]);
            }
        }
        reader.consume(length);
        if newline.is_some() {
            return Ok(Some(if oversized {
                Err(())
            } else {
                String::from_utf8(frame).map_err(|_| ())
            }));
        }
    }
}
async fn run_reader(
    resp_tx: mpsc::Sender<String>,
    tasks: &mut JoinSet<()>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut reader = BufReader::new(tokio::io::stdin());
    let queued = Arc::new(Semaphore::new(REQUEST_QUEUE_CAPACITY));
    let active = Arc::new(Semaphore::new(WORKER_POOL_SIZE));

    loop {
        while tasks.try_join_next().is_some() {}
        let frame = tokio::select! { frame = read_frame(&mut reader) => frame, _ = shutdown_rx.changed() => break };
        let line = match frame {
            Ok(Some(Ok(line))) => line,
            Ok(None) | Err(_) => break,
            Ok(Some(Err(()))) => {
                if send_response(
                    &resp_tx,
                    rpc::error_response(
                        serde_json::Value::Null,
                        -32700,
                        "The driver frame is oversized or is not valid newline-delimited UTF-8",
                    ),
                )
                .await
                .is_err()
                {
                    break;
                }
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => {
                if send_response(
                    &resp_tx,
                    rpc::error_response(serde_json::Value::Null, -32700, "Invalid JSON-RPC frame"),
                )
                .await
                .is_err()
                {
                    break;
                }
                continue;
            }
        };
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let method = request["method"].as_str().unwrap_or("");
        if handlers::service::is_method(method) && !driver::service::types::valid_rpc_id(&id) {
            if send_response(
                &resp_tx,
                driver::service::ServiceError::argument(
                    "Service RPC id must be null, a bounded string, or a safe integer",
                )
                .response(serde_json::Value::Null),
            )
            .await
            .is_err()
            {
                break;
            }
            continue;
        }
        if matches!(
            method,
            "cancel_request"
                | "service_session_discard"
                | "service_invalidate_auth"
                | "shutdown"
                | "initialize"
        ) {
            let response = Box::pin(rpc::handle_line(trimmed)).await;
            if send_response(&resp_tx, response).await.is_err() {
                break;
            }
            continue;
        }
        let scope = if handlers::service::is_method(method) {
            match handlers::service::prepare(&request["params"]) {
                Ok(scope) => Some(scope),
                Err(error) => {
                    if send_response(&resp_tx, error.response(id)).await.is_err() {
                        break;
                    }
                    continue;
                }
            }
        } else {
            None
        };
        let queue_permit = if scope.is_some() {
            match queued.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    if send_response(
                        &resp_tx,
                        driver::service::ServiceError::new(
                            "RESOURCE_LIMIT",
                            "The driver request queue is full",
                        )
                        .response(id),
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                    continue;
                }
            }
        } else {
            tokio::select! { permit = queued.clone().acquire_owned() => match permit { Ok(permit) => permit, Err(_) => break }, _ = shutdown_rx.changed() => break }
        };
        let connection = scope
            .as_ref()
            .map(|scope| scope.connection_id().to_owned())
            .unwrap_or_else(|| {
                request["params"]["params"]["connection_id"]
                    .as_str()
                    .unwrap_or("legacy")
                    .to_owned()
            });
        let connection_limit = driver::service::registry::connection_limit(&connection);
        let active = active.clone();
        let resp_tx = resp_tx.clone();
        let line = trimmed.to_owned();
        tasks.spawn(async move {
            let permits = async { let connection = connection_limit.acquire_owned().await.unwrap(); let active = active.acquire_owned().await.unwrap(); (connection, active) };
            tokio::pin!(permits);
            let admission = if let Some(scope) = &scope {
                tokio::select! {
                    permits = &mut permits => Ok(permits),
                    _ = tokio::time::sleep_until(scope.state.deadline) => Err(driver::service::ServiceError::new("DEADLINE_EXCEEDED", "The queued request deadline was exceeded")),
                    _ = scope.wait_cancelled() => Err(driver::service::ServiceError::new("CANCELLED", "The queued request was cancelled")),
                }
            } else { Ok(permits.await) };
            drop(queue_permit);
            let response = match admission {
                Ok(_permits) => Box::pin(rpc::handle_line_with_scope(&line, scope)).await,
                Err(error) => error.response(id),
            };
            let _ = send_response(&resp_tx, response).await;
        });
    }
}
async fn send_response(
    sender: &mpsc::Sender<String>,
    response: serde_json::Value,
) -> Result<(), ()> {
    let body = bounded_response(response)?;
    sender.send(body).await.map_err(|_| ())
}
fn bounded_response(response: serde_json::Value) -> Result<String, ()> {
    let body = serde_json::to_string(&response).map_err(|_| ())?;
    if body.len() <= driver::service::types::FRAME_LIMIT {
        return Ok(body);
    }
    let mut error = driver::service::ServiceError::new(
        "RESOURCE_LIMIT",
        "The driver response exceeds its byte limit",
    );
    if response["error"]["data"]["outcome"] == "unknown"
        || response["error"]["data"]["code"] == "OUTCOME_UNKNOWN"
    {
        error.code = "OUTCOME_UNKNOWN";
        error.unknown = true;
    }
    let id = if error.unknown && !driver::service::types::valid_rpc_id(&response["id"]) {
        serde_json::Value::Null
    } else {
        response["id"].clone()
    };
    let mut body = serde_json::to_string(&error.clone().response(id)).map_err(|_| ())?;
    if body.len() > driver::service::types::FRAME_LIMIT {
        body = serde_json::to_string(&error.response(serde_json::Value::Null)).map_err(|_| ())?;
    }
    if body.len() > driver::service::types::FRAME_LIMIT {
        return Err(());
    }
    Ok(body)
}

async fn run_writer(mut resp_rx: mpsc::Receiver<String>, shutdown_tx: watch::Sender<bool>) {
    let mut stdout = tokio::io::stdout();
    while let Some(mut body) = resp_rx.recv().await {
        body.push('\n');
        if stdout.write_all(body.as_bytes()).await.is_err() {
            driver::service::registry::cancel_all();
            let _ = shutdown_tx.send(true);
            break;
        }
        if stdout.flush().await.is_err() {
            driver::service::registry::cancel_all();
            let _ = shutdown_tx.send(true);
            break;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    #[test]
    fn 큰_unknown_오류도_최종_frame_한계와_쓰기_결과를_보존한다() {
        // given
        let response = json!({"jsonrpc":"2.0","id":"x".repeat(16*1024*1024),"error":{"data":{"code":"OUTCOME_UNKNOWN","outcome":"unknown","details":{"large":"x".repeat(1024)}}}});
        // when
        let actual = bounded_response(response)
            .map(|text| (text.len(), serde_json::from_str::<Value>(&text).unwrap()))
            .unwrap();
        // then
        assert!(actual.0 <= driver::service::types::FRAME_LIMIT);
        assert_eq!(actual.1["id"], Value::Null);
        assert_eq!(actual.1["error"]["data"]["code"], "OUTCOME_UNKNOWN");
        assert_eq!(actual.1["error"]["data"]["outcome"], "unknown");
        assert_eq!(actual.1["error"]["data"]["details"], Value::Null);
    }
    #[test]
    fn legacy_큰_id의_최종_fallback도_실제_frame_한계를_지킨다() {
        // given
        let response =
            json!({"jsonrpc":"2.0","id":"x".repeat(16*1024*1024),"result":"x".repeat(1024)});
        // when
        let actual = bounded_response(response)
            .map(|text| (text.len(), serde_json::from_str::<Value>(&text).unwrap()))
            .unwrap();
        // then
        assert!(actual.0 <= driver::service::types::FRAME_LIMIT);
        assert_eq!(actual.1["id"], Value::Null);
        assert_eq!(actual.1["error"]["data"]["code"], "RESOURCE_LIMIT");
        assert_eq!(actual.1["error"]["data"]["outcome"], "not_applied");
    }
}
