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

mod cancellation;
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
    run_reader(
        tokio::io::stdin(),
        resp_tx.clone(),
        &mut tasks,
        shutdown_rx,
        |line: String, scope, execution| async move {
            Box::pin(rpc::handle_line_with_scope(&line, scope, execution)).await
        },
    )
    .await;
    cancel_all_requests();

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
async fn run_reader<R, H, F>(
    input: R,
    resp_tx: mpsc::Sender<String>,
    tasks: &mut JoinSet<()>,
    mut shutdown_rx: watch::Receiver<bool>,
    handler: H,
) where
    R: tokio::io::AsyncRead + Unpin,
    H: Fn(
            String,
            Option<driver::service::registry::Scope>,
            Option<cancellation::ExecutionLease>,
        ) -> F
        + Clone
        + Send
        + 'static,
    F: std::future::Future<Output = serde_json::Value> + Send + 'static,
{
    let mut reader = BufReader::new(input);
    let queued = Arc::new(Semaphore::new(REQUEST_QUEUE_CAPACITY));
    // Caps legacy requests that are waiting for or holding a queue slot, so
    // the reader itself never blocks on admission.
    let legacy_pending = Arc::new(Semaphore::new(REQUEST_QUEUE_CAPACITY));
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
        // The pre-service `cancel` notification names the numeric id of a
        // running `execute_query` or `execute_query_batch`. Unknown, late or
        // malformed notifications are ignored, and none gets a response.
        if method == "cancel" {
            if request.get("id").is_some() {
                if send_response(
                    &resp_tx,
                    rpc::error_response(id, -32600, "cancel must be sent as a notification"),
                )
                .await
                .is_err()
                {
                    break;
                }
            } else if let Some(request_id) = request["params"]["id"].as_u64() {
                cancellation::global().cancel(request_id);
            }
            continue;
        }
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
        let (queue_permit, legacy_permit) = if scope.is_some() {
            match queued.clone().try_acquire_owned() {
                Ok(permit) => (Some(permit), None),
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
            match legacy_pending.clone().try_acquire_owned() {
                Ok(permit) => (queued.clone().try_acquire_owned().ok(), Some(permit)),
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
        // Registered here, before the call waits for a worker, so a `cancel`
        // notification that arrives while it is queued still reaches it.
        let execution = match (method, id.as_u64()) {
            ("execute_query" | "execute_query_batch", Some(request_id)) => {
                match cancellation::global().register(request_id) {
                    Ok(lease) => Some(lease),
                    Err(cancellation::RegisterError::DuplicateRequestId) => {
                        if send_response(
                            &resp_tx,
                            rpc::error_response(id, -32600, "request id is already active"),
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                }
            }
            _ => None,
        };
        let connection_limit = driver::service::registry::connection_limit(&connection);
        let active = active.clone();
        let queued = queued.clone();
        let resp_tx = resp_tx.clone();
        let line = trimmed.to_owned();
        let handler = handler.clone();
        tasks.spawn(async move {
            let _legacy_permit = legacy_permit;
            let queue_permit = match queue_permit {
                Some(permit) => permit,
                None => queued.acquire_owned().await.unwrap(),
            };
            let permits = async { let connection = connection_limit.acquire_owned().await.unwrap(); let active = active.acquire_owned().await.unwrap(); (connection, active) };
            tokio::pin!(permits);
            let admission = if let Some(scope) = &scope {
                tokio::select! {
                    permits = &mut permits => Ok(permits),
                    _ = tokio::time::sleep_until(scope.state.deadline) => Err(driver::service::ServiceError::new("DEADLINE_EXCEEDED", "The queued request deadline was exceeded").response(id.clone())),
                    _ = scope.wait_cancelled() => Err(driver::service::ServiceError::new("CANCELLED", "The queued request was cancelled").response(id.clone())),
                }
            } else if let Some(lease) = &execution {
                // A call cancelled while it waits for a worker never runs.
                tokio::select! {
                    permits = &mut permits => Ok(permits),
                    _ = lease.control().cancelled() => Err(rpc::query_cancelled_response(id.clone(), lease.request_id(), true)),
                }
            } else { Ok(permits.await) };
            drop(queue_permit);
            let response = match admission {
                Ok(_permits) => handler(line, scope, execution).await,
                Err(response) => response,
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

/// Stops every in-flight service request and legacy query call.
fn cancel_all_requests() {
    driver::service::registry::cancel_all();
    cancellation::global().cancel_all();
}

async fn run_writer(mut resp_rx: mpsc::Receiver<String>, shutdown_tx: watch::Sender<bool>) {
    let mut stdout = tokio::io::stdout();
    while let Some(mut body) = resp_rx.recv().await {
        body.push('\n');
        if stdout.write_all(body.as_bytes()).await.is_err() {
            cancel_all_requests();
            let _ = shutdown_tx.send(true);
            break;
        }
        if stdout.flush().await.is_err() {
            cancel_all_requests();
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

    fn on_worker_stack<F: std::future::Future<Output = ()> + Send + 'static>(future: F) {
        let _gate = driver::service::registry::SHUTDOWN_GATE.read().unwrap();
        std::thread::Builder::new()
            .stack_size(WORKER_STACK_SIZE)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(future)
            })
            .unwrap()
            .join()
            .unwrap();
    }

    fn service_line(connection: &str, request: &str, method: &str) -> String {
        json!({"jsonrpc":"2.0","id":request,"method":method,"params":{
            "params":{"driver":"sqlserver","host":"localhost","database":"fixture"},
            "driver_context":{"protocol_version":1,"connection_id":connection,"request_id":request,"deadline_ms":60_000},
            "input":{}
        }})
        .to_string()
    }
    fn cancel_line(connection: &str, request: &str) -> String {
        json!({"jsonrpc":"2.0","id":format!("cancel-{request}"),"method":"cancel_request","params":{"connection_id":connection,"request_id":request}}).to_string()
    }

    struct Harness {
        input: tokio::io::DuplexStream,
        output: mpsc::Receiver<String>,
        started: Arc<std::sync::atomic::AtomicUsize>,
        shutdown: watch::Sender<bool>,
        reader: tokio::task::JoinHandle<()>,
    }
    impl Harness {
        fn new() -> Self {
            let (input, server) = tokio::io::duplex(1 << 20);
            let (resp_tx, output) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
            let (shutdown, shutdown_rx) = watch::channel(false);
            let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = started.clone();
            let reader =
                tokio::spawn(async move {
                    let mut tasks = JoinSet::new();
                    run_reader(
                    server,
                    resp_tx,
                    &mut tasks,
                    shutdown_rx,
                    move |line: String,
                          scope: Option<driver::service::registry::Scope>,
                          execution: Option<cancellation::ExecutionLease>| {
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        async move {
                            let id = serde_json::from_str::<Value>(&line).unwrap()["id"].clone();
                            let Some(scope) = scope else {
                                let Some(lease) = execution else {
                                    return std::future::pending().await;
                                };
                                lease.control().cancelled().await;
                                return rpc::query_cancelled_response(id, lease.request_id(), false);
                            };
                            scope.wait_cancelled().await;
                            driver::service::ServiceError::new(
                                "CANCELLED",
                                "fake execution cancelled",
                            )
                            .response(id)
                        }
                    },
                )
                .await;
                    tasks.shutdown().await;
                });
            Self {
                input,
                output,
                started,
                shutdown,
                reader,
            }
        }
        async fn send(&mut self, line: &str) {
            self.input.write_all(line.as_bytes()).await.unwrap();
            self.input.write_all(b"\n").await.unwrap();
        }
        async fn receive(&mut self) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(5), self.output.recv())
                .await
                .expect("response within five seconds")
                .unwrap();
            serde_json::from_str(&line).unwrap()
        }
        async fn wait_started(&self, count: usize) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while self.started.load(std::sync::atomic::Ordering::SeqCst) < count {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("workers started");
        }
        async fn finish(self) {
            let _ = self.shutdown.send(true);
            let _ = self.reader.await;
        }
    }

    #[test]
    fn 모든_worker가_실행_중이어도_cancel_request는_queue를_거치지_않고_즉시_응답한다() {
        on_worker_stack(async {
            // given
            let mut harness = Harness::new();
            let connections = [
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
            ];
            for (index, connection) in connections.iter().enumerate() {
                for slot in 0..4 {
                    harness
                        .send(&service_line(
                            connection,
                            &format!("long-{index}-{slot}"),
                            "service_test",
                        ))
                        .await;
                }
            }
            harness.wait_started(WORKER_POOL_SIZE).await;
            // when
            let started = std::time::Instant::now();
            harness
                .send(&cancel_line(&connections[0], "long-0-0"))
                .await;
            let first = harness.receive().await;
            let elapsed = started.elapsed();
            // then
            assert_eq!(first["id"], "cancel-long-0-0");
            assert_eq!(first["result"]["acknowledged"], true);
            assert_eq!(first["result"]["cancellation_requested"], true);
            assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
            let cancelled = harness.receive().await;
            assert_eq!(cancelled["id"], "long-0-0");
            assert_eq!(cancelled["error"]["data"]["code"], "CANCELLED");
            harness.finish().await;
        });
    }

    #[test]
    fn worker_admission_전에_취소된_요청은_실행되지_않고_cancelled로_끝난다() {
        on_worker_stack(async {
            // given
            let mut harness = Harness::new();
            let connection = uuid::Uuid::new_v4().to_string();
            for slot in 0..4 {
                harness
                    .send(&service_line(
                        &connection,
                        &format!("busy-{slot}"),
                        "service_test",
                    ))
                    .await;
            }
            harness.wait_started(4).await;
            harness
                .send(&service_line(&connection, "queued", "service_test"))
                .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            // when
            harness.send(&cancel_line(&connection, "queued")).await;
            let mut responses = vec![harness.receive().await, harness.receive().await];
            responses.sort_by_key(|response| response["id"].to_string());
            // then
            let queued = responses
                .iter()
                .find(|r| r["id"] == "queued")
                .unwrap_or_else(|| panic!("{responses:?}"));
            assert_eq!(queued["error"]["data"]["code"], "CANCELLED");
            assert_eq!(queued["error"]["data"]["outcome"], "not_applied");
            assert_eq!(harness.started.load(std::sync::atomic::Ordering::SeqCst), 4);
            harness.finish().await;
        });
    }

    #[test]
    fn 알_수_없는_id와_다른_연결의_같은_id_취소는_실행_중인_요청을_건드리지_않는다() {
        on_worker_stack(async {
            // given
            let mut harness = Harness::new();
            let connection = uuid::Uuid::new_v4().to_string();
            let other = uuid::Uuid::new_v4().to_string();
            harness
                .send(&service_line(&connection, "same", "service_test"))
                .await;
            harness.wait_started(1).await;
            // when
            harness.send(&cancel_line(&other, "same")).await;
            harness.send(&cancel_line(&connection, "unknown")).await;
            let acks = [harness.receive().await, harness.receive().await];
            // then
            for ack in &acks {
                assert_eq!(ack["result"]["acknowledged"], true);
                assert_eq!(ack["result"]["cancellation_requested"], false);
            }
            assert!(harness.output.try_recv().is_err());
            harness.finish().await;
        });
    }

    #[test]
    fn id_없는_legacy_cancel_notification은_응답과_충돌_없이_무시된다() {
        on_worker_stack(async {
            // given
            let mut harness = Harness::new();
            let connection = uuid::Uuid::new_v4().to_string();
            harness
                .send(&service_line(&connection, "running", "service_test"))
                .await;
            harness.wait_started(1).await;
            // when
            harness
                .send(r#"{"jsonrpc":"2.0","method":"cancel","params":{"id":42}}"#)
                .await;
            harness.send(&cancel_line(&connection, "nothing")).await;
            let next = harness.receive().await;
            // then
            assert_eq!(next["id"], "cancel-nothing");
            assert!(harness.output.try_recv().is_err());
            assert!(!harness.reader.is_finished());
            harness.finish().await;
        });
    }

    fn legacy_query_line(id: u64, method: &str) -> String {
        json!({"jsonrpc":"2.0","id":id,"method":method,"params":{"params":{},"query":"SELECT 1"}})
            .to_string()
    }
    fn legacy_cancel_line(id: Value) -> String {
        json!({"jsonrpc":"2.0","method":"cancel","params":{"id":id}}).to_string()
    }

    #[test]
    fn 실행_중인_legacy_쿼리는_cancel_notification의_id로_취소되고_하나의_응답만_받는다() {
        on_worker_stack(async {
            for (id, method) in [(901, "execute_query"), (902, "execute_query_batch")] {
                // given
                let mut harness = Harness::new();
                harness.send(&legacy_query_line(id, method)).await;
                harness.wait_started(1).await;
                // when
                harness.send(&legacy_cancel_line(json!(id))).await;
                let response = harness.receive().await;
                // then
                assert_eq!(response["id"], id);
                assert_eq!(response["error"]["code"], -32800);
                assert_eq!(response["error"]["data"]["request_id"], id);
                assert!(harness.output.try_recv().is_err());
                harness.finish().await;
            }
        });
    }

    #[test]
    fn worker를_기다리다_취소된_legacy_쿼리는_실행되지_않고_취소_응답으로_끝난다() {
        on_worker_stack(async {
            // given
            let mut harness = Harness::new();
            let connections = [
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
            ];
            for (index, connection) in connections.iter().enumerate() {
                for slot in 0..4 {
                    harness
                        .send(&service_line(
                            connection,
                            &format!("busy-{index}-{slot}"),
                            "service_test",
                        ))
                        .await;
                }
            }
            harness.wait_started(WORKER_POOL_SIZE).await;
            harness.send(&legacy_query_line(903, "execute_query")).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            // when
            harness.send(&legacy_cancel_line(json!(903))).await;
            let response = harness.receive().await;
            // then
            assert_eq!(response["id"], 903);
            assert_eq!(response["error"]["code"], -32800);
            assert_eq!(
                response["error"]["data"]["server_termination_confirmed"],
                true
            );
            assert_eq!(
                harness.started.load(std::sync::atomic::Ordering::SeqCst),
                WORKER_POOL_SIZE
            );
            harness.finish().await;
        });
    }

    #[test]
    fn 이미_실행_중인_id의_legacy_쿼리는_무효_요청으로_거절되고_기존_요청은_계속된다() {
        on_worker_stack(async {
            // given
            let mut harness = Harness::new();
            harness.send(&legacy_query_line(904, "execute_query")).await;
            harness.wait_started(1).await;
            // when
            harness.send(&legacy_query_line(904, "execute_query")).await;
            let rejected = harness.receive().await;
            harness.send(&legacy_cancel_line(json!(904))).await;
            let cancelled = harness.receive().await;
            // then
            assert_eq!(rejected["id"], 904);
            assert_eq!(rejected["error"]["code"], -32600);
            assert_eq!(cancelled["error"]["code"], -32800);
            assert_eq!(harness.started.load(std::sync::atomic::Ordering::SeqCst), 1);
            harness.finish().await;
        });
    }

    #[test]
    fn 잘못된_legacy_cancel은_요청을_취소하지_않고_id가_있으면_무효_요청으로_응답한다() {
        on_worker_stack(async {
            // given
            let mut harness = Harness::new();
            harness.send(&legacy_query_line(905, "execute_query")).await;
            harness.wait_started(1).await;
            // when
            harness.send(&legacy_cancel_line(json!("905"))).await;
            harness.send(r#"{"jsonrpc":"2.0","method":"cancel"}"#).await;
            harness
                .send(r#"{"jsonrpc":"2.0","id":77,"method":"cancel","params":{"id":905}}"#)
                .await;
            let response = harness.receive().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            // then
            assert_eq!(response["id"], 77);
            assert_eq!(response["error"]["code"], -32600);
            assert!(harness.output.try_recv().is_err());
            harness.send(&legacy_cancel_line(json!(905))).await;
            assert_eq!(harness.receive().await["id"], 905);
            harness.finish().await;
        });
    }

    #[test]
    fn legacy_요청이_포화돼도_reader는_막히지_않고_cancel_request에_즉시_응답한다() {
        on_worker_stack(async {
            // given
            let mut harness = Harness::new();
            let legacy = |id: String, connection: &str| {
                json!({"jsonrpc":"2.0","id":id,"method":"get_tables","params":{"params":{"connection_id":connection}}}).to_string()
            };
            let connections = [
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
            ];
            for (index, connection) in connections.iter().enumerate() {
                for slot in 0..4 {
                    harness
                        .send(&legacy(format!("run-{index}-{slot}"), connection))
                        .await;
                }
            }
            harness.wait_started(WORKER_POOL_SIZE).await;
            let flood = REQUEST_QUEUE_CAPACITY + 36;
            for index in 0..flood {
                harness
                    .send(&legacy(format!("wait-{index}"), &connections[0]))
                    .await;
            }
            let target = uuid::Uuid::new_v4().to_string();
            harness
                .send(&service_line(&target, "service-target", "service_test"))
                .await;
            // when
            let started = std::time::Instant::now();
            harness.send(&cancel_line(&target, "service-target")).await;
            let mut rejected = Vec::new();
            let ack = loop {
                let response = harness.receive().await;
                if response["id"] == "cancel-service-target" {
                    break response;
                }
                rejected.push(response);
            };
            let elapsed = started.elapsed();
            // then
            assert_eq!(ack["result"]["cancellation_requested"], true);
            assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
            assert!(!rejected.is_empty());
            for response in &rejected {
                assert!(response["id"].as_str().unwrap().starts_with("wait-"));
                assert_eq!(response["error"]["data"]["code"], "RESOURCE_LIMIT");
            }
            harness.finish().await;
        });
    }
}
