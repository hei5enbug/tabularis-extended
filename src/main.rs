//! Entry point: read JSON-RPC lines from stdin, dispatch, write responses.
//!
//! Requests are fanned out to a small worker pool so a slow query on one
//! connection does not block a `ping` or metadata call on another. Responses
//! are funneled through a single writer task so concurrent handlers never
//! interleave bytes on stdout.

use std::sync::Arc;

use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{mpsc, watch, Mutex},
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
const WORKER_POOL_SIZE: usize = 4;

// Both sides are bounded so backpressure reaches stdin even when the host is
// slow to consume responses. With four in-flight handlers this caps a burst at
// 134 queued or active payloads instead of moving it into an unbounded output
// queue (64 requests + 64 responses + 4 workers + reader + writer).
const REQUEST_QUEUE_CAPACITY: usize = 64;
const RESPONSE_QUEUE_CAPACITY: usize = 64;

// The TDS client's async call chains produce large futures (especially in
// debug builds). A local SQL Server 2022 debug execute_query probe overflowed
// tokio's default 2 MiB stack while 4 MiB completed. The full release live
// suite also passed at 4 MiB in SS-045, but that does not remove the debug or
// cross-platform risk. Keep 16 MiB as a deliberate 4x margin until the preview
// client flattens those polling chains or CI stress proves less everywhere.
const WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

struct QueuedRequest {
    line: String,
    execution: Option<cancellation::ExecutionLease>,
    response_id: Option<serde_json::Value>,
}

fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(WORKER_STACK_SIZE)
        .build()
        .expect("failed to build tokio runtime")
        .block_on(run());
}

async fn run() {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let cancellation_registry = Arc::new(cancellation::CancellationRegistry::default());

    let cleanup_handle = tokio::spawn(run_pool_cleanup(shutdown_rx));

    let (req_tx, req_rx) = mpsc::channel::<QueuedRequest>(REQUEST_QUEUE_CAPACITY);
    let req_rx = Arc::new(Mutex::new(req_rx));

    let (resp_tx, resp_rx) = mpsc::channel::<String>(RESPONSE_QUEUE_CAPACITY);
    let writer_handle = tokio::spawn(run_writer(resp_rx));
    let reader_resp_tx = resp_tx.clone();

    let worker_handles: Vec<_> = (0..WORKER_POOL_SIZE)
        .map(|_| tokio::spawn(run_worker(req_rx.clone(), resp_tx.clone())))
        .collect();
    drop(resp_tx);

    run_reader(
        BufReader::new(tokio::io::stdin()),
        req_tx,
        reader_resp_tx,
        cancellation_registry,
    )
    .await;

    let _ = shutdown_tx.send(true);

    for handle in worker_handles {
        let _ = handle.await;
    }
    let _ = writer_handle.await;
    let _ = cleanup_handle.await;
}

async fn run_pool_cleanup(mut shutdown_rx: watch::Receiver<bool>) {
    let mut settings_rx = settings::subscribe();
    loop {
        let cleanup_interval = settings::current().pool_idle_eviction_interval();
        tokio::select! {
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

async fn run_reader<R>(
    reader: R,
    req_tx: mpsc::Sender<QueuedRequest>,
    resp_tx: mpsc::Sender<String>,
    cancellation_registry: Arc<cancellation::CancellationRegistry>,
) where
    R: AsyncBufRead + Unpin,
{
    let mut lines = reader.lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                eprintln!("stdin read error, exiting: {err}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request_value = serde_json::from_str::<serde_json::Value>(trimmed).ok();
        let method = request_value
            .as_ref()
            .and_then(|request| request.get("method"))
            .and_then(serde_json::Value::as_str);

        if method == Some("cancel") {
            if let Some(response_id) = request_value
                .as_ref()
                .and_then(|request| request.get("id"))
                .cloned()
            {
                let response = rpc::error_response(
                    response_id,
                    -32600,
                    "cancel must be sent as a notification",
                );
                if !send_reader_response(&resp_tx, response, &cancellation_registry).await {
                    break;
                }
                continue;
            }
            if let Some(request_id) = request_value
                .as_ref()
                .and_then(|request| request.get("params"))
                .and_then(|params| params.get("id"))
                .and_then(serde_json::Value::as_u64)
            {
                cancellation_registry.cancel(request_id);
            }
            continue;
        }

        let response_id = request_value
            .as_ref()
            .and_then(|request| request.get("id"))
            .cloned();
        let execution = match (
            method,
            response_id.as_ref().and_then(serde_json::Value::as_u64),
        ) {
            (Some("execute_query" | "execute_query_batch"), Some(request_id)) => {
                match cancellation_registry.register(request_id) {
                    Ok(lease) => Some(lease),
                    Err(cancellation::RegisterError::DuplicateRequestId) => {
                        if let Some(response_id) = response_id {
                            let response = rpc::error_response(
                                response_id,
                                -32600,
                                "request id is already active",
                            );
                            if !send_reader_response(&resp_tx, response, &cancellation_registry)
                                .await
                            {
                                break;
                            }
                        }
                        continue;
                    }
                }
            }
            _ => None,
        };
        let queued = QueuedRequest {
            line: trimmed.to_string(),
            execution,
            response_id: response_id.clone(),
        };

        match req_tx.try_send(queued) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(queued)) => {
                drop(queued.execution);
                if let Some(response_id) = queued.response_id {
                    let response =
                        rpc::error_response(response_id, -32000, "request queue is full");
                    if !send_reader_response(&resp_tx, response, &cancellation_registry).await {
                        break;
                    }
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => break,
        }
    }
}

async fn send_reader_response(
    resp_tx: &mpsc::Sender<String>,
    response: serde_json::Value,
    cancellation_registry: &cancellation::CancellationRegistry,
) -> bool {
    let body = serde_json::to_string(&response).unwrap_or_else(|err| {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"error\":{{\"code\":-32603,\"message\":\"serialization failed: {err}\"}},\"id\":null}}"
        )
    });
    match resp_tx.try_send(body) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(body)) => {
            // Do not accept further requests once their response cannot be
            // queued. Cancel all work owned by this process, then wait for the
            // single writer to make room for this response before disconnecting
            // the reader.
            cancellation_registry.cancel_all();
            if resp_tx.send(body).await.is_err() {
                eprintln!("response writer closed while the reader was stopping");
            }
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            cancellation_registry.cancel_all();
            eprintln!("response writer is closed; cancelling in-flight queries");
            false
        }
    }
}

async fn run_worker(
    req_rx: Arc<Mutex<mpsc::Receiver<QueuedRequest>>>,
    resp_tx: mpsc::Sender<String>,
) {
    loop {
        let line = {
            let mut rx = req_rx.lock().await;
            rx.recv().await
        };
        let Some(request) = line else { break };

        // Box the dispatch future itself: it embeds every handler's state
        // machine, so constructing only a boxed handler result later would
        // still leave the large dispatch enum on the worker stack.
        let response = Box::pin(rpc::handle_line_with_execution(
            &request.line,
            request.execution.as_ref(),
        ))
        .await;
        drop(request.execution);
        let body = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(err) => format!(
                "{{\"jsonrpc\":\"2.0\",\"error\":{{\"code\":-32603,\"message\":\"serialization failed: {err}\"}},\"id\":null}}",
            ),
        };

        if resp_tx.send(body).await.is_err() {
            break;
        }
    }
}

async fn run_writer(mut resp_rx: mpsc::Receiver<String>) {
    let mut stdout = tokio::io::stdout();
    while let Some(mut body) = resp_rx.recv().await {
        body.push('\n');
        if stdout.write_all(body.as_bytes()).await.is_err() {
            break;
        }
        let _ = stdout.flush().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn queued_placeholder() -> QueuedRequest {
        QueuedRequest {
            line: String::new(),
            execution: None,
            response_id: None,
        }
    }

    #[tokio::test]
    async fn 요청_큐가_가득해도_취소_알림을_처리하고_응답하지_않는다() {
        // given
        let (req_tx, mut req_rx) = mpsc::channel(REQUEST_QUEUE_CAPACITY);
        for _ in 0..REQUEST_QUEUE_CAPACITY {
            req_tx.try_send(queued_placeholder()).unwrap();
        }
        let (resp_tx, mut resp_rx) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
        let registry = Arc::new(cancellation::CancellationRegistry::default());
        let execution = registry.register(501).unwrap();

        // when
        run_reader(
            BufReader::new(
                br#"{"jsonrpc":"2.0","method":"cancel","params":{"id":501}}"#.as_slice(),
            ),
            req_tx,
            resp_tx,
            Arc::clone(&registry),
        )
        .await;

        // then
        assert!(execution.control().is_cancelled());
        assert!(resp_rx.try_recv().is_err());
        assert!(req_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn 잘못된_취소_알림은_응답하지_않고_요청도_취소하지_않는다() {
        // given
        let (req_tx, _req_rx) = mpsc::channel(REQUEST_QUEUE_CAPACITY);
        let (resp_tx, mut resp_rx) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
        let registry = Arc::new(cancellation::CancellationRegistry::default());
        let execution = registry.register(504).unwrap();
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"method\":\"cancel\",\"params\":{\"id\":\"504\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"cancel\"}\n"
        );

        // when
        run_reader(
            BufReader::new(input.as_bytes()),
            req_tx,
            resp_tx,
            Arc::clone(&registry),
        )
        .await;

        // then
        assert!(!execution.control().is_cancelled());
        assert!(resp_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn id가_있는_cancel은_요청을_취소하지_않고_무효_요청으로_응답한다() {
        // given
        let (req_tx, _req_rx) = mpsc::channel(REQUEST_QUEUE_CAPACITY);
        let (resp_tx, mut resp_rx) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
        let registry = Arc::new(cancellation::CancellationRegistry::default());
        let execution = registry.register(505).unwrap();
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"method\":\"cancel\",",
            "\"params\":{\"id\":505},\"id\":77}\n"
        );

        // when
        run_reader(
            BufReader::new(input.as_bytes()),
            req_tx,
            resp_tx,
            Arc::clone(&registry),
        )
        .await;
        let response: serde_json::Value =
            serde_json::from_str(&resp_rx.try_recv().unwrap()).unwrap();

        // then
        assert_eq!(response["id"], 77);
        assert_eq!(response["error"]["code"], -32600);
        assert!(!execution.control().is_cancelled());
    }

    #[tokio::test]
    async fn 포화된_요청_큐는_직렬화된_과부하_응답을_보내고_등록을_정리한다() {
        // given
        let (req_tx, _req_rx) = mpsc::channel(REQUEST_QUEUE_CAPACITY);
        for _ in 0..REQUEST_QUEUE_CAPACITY {
            req_tx.try_send(queued_placeholder()).unwrap();
        }
        let (resp_tx, mut resp_rx) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
        let registry = Arc::new(cancellation::CancellationRegistry::default());
        let input = json!({
            "jsonrpc": "2.0",
            "method": "execute_query",
            "params": {},
            "id": 502
        })
        .to_string();

        // when
        run_reader(
            BufReader::new(format!("{input}\n").as_bytes()),
            req_tx,
            resp_tx,
            Arc::clone(&registry),
        )
        .await;

        // then
        let response: serde_json::Value =
            serde_json::from_str(&resp_rx.try_recv().unwrap()).unwrap();
        assert_eq!(response["id"], 502);
        assert_eq!(response["error"]["code"], -32000);
        assert_eq!(response["error"]["message"], "request queue is full");
        assert!(!registry.cancel(502));
    }

    #[tokio::test]
    async fn 응답_큐_포화는_응답을_전달한_뒤_입력을_중단하고_모든_요청을_취소한다() {
        // given
        let (req_tx, _req_rx) = mpsc::channel(REQUEST_QUEUE_CAPACITY);
        for _ in 0..REQUEST_QUEUE_CAPACITY {
            req_tx.try_send(queued_placeholder()).unwrap();
        }
        let (resp_tx, mut resp_rx) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
        for index in 0..RESPONSE_QUEUE_CAPACITY {
            resp_tx.try_send(format!("prior-{index}")).unwrap();
        }
        let registry = Arc::new(cancellation::CancellationRegistry::default());
        let inflight = registry.register(506).unwrap();
        let input = json!({
            "jsonrpc": "2.0",
            "method": "execute_query",
            "params": {},
            "id": 507
        })
        .to_string();
        let input = format!("{input}\n");
        let reader_registry = Arc::clone(&registry);

        // when
        let reader = tokio::spawn(async move {
            run_reader(
                BufReader::new(input.as_bytes()),
                req_tx,
                resp_tx,
                reader_registry,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !inflight.control().is_cancelled() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reader failed to request cancellation on saturation");

        // then
        for index in 0..RESPONSE_QUEUE_CAPACITY {
            assert_eq!(resp_rx.recv().await, Some(format!("prior-{index}")));
        }
        let response: serde_json::Value =
            serde_json::from_str(&resp_rx.recv().await.unwrap()).unwrap();
        reader.await.unwrap();
        assert_eq!(response["id"], 507);
        assert_eq!(response["error"]["code"], -32000);
        assert_eq!(response["error"]["message"], "request queue is full");
        assert!(!registry.cancel(507));
        assert!(inflight.control().is_cancelled());
    }

    #[tokio::test]
    async fn 중복_활성_id의_취소는_먼저_등록된_요청에만_전달된다() {
        // given
        let (req_tx, _req_rx) = mpsc::channel(REQUEST_QUEUE_CAPACITY);
        let (resp_tx, mut resp_rx) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
        let registry = Arc::new(cancellation::CancellationRegistry::default());
        let original = registry.register(503).unwrap();
        let input = json!({
            "jsonrpc": "2.0",
            "method": "execute_query",
            "params": {},
            "id": 503
        })
        .to_string();

        // when
        run_reader(
            BufReader::new(format!("{input}\n").as_bytes()),
            req_tx,
            resp_tx,
            Arc::clone(&registry),
        )
        .await;
        let response: serde_json::Value =
            serde_json::from_str(&resp_rx.try_recv().unwrap()).unwrap();
        registry.cancel(503);

        // then
        assert_eq!(response["error"]["code"], -32600);
        assert!(original.control().is_cancelled());
    }
}
