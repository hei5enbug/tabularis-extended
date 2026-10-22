//! PostgreSQL plugin for Tabularis — JSON-RPC driver over stdin/stdout.
//!
//! # Protocol
//!
//! Reads newline-delimited JSON-RPC 2.0 requests from stdin and writes
//! responses (one JSON object per line) to stdout. All handler logic is
//! async (tokio) since the database pool requires an async runtime.
//!
//! Requests are fanned out to a small worker pool so a slow query on one
//! connection does not block a `ping` or metadata call on another. Responses
//! are funneled through a single writer task so concurrent handlers never
//! interleave bytes on stdout. A dedicated background task periodically
//! evicts idle connection pools (see `client::cleanup_idle_pools`) so a
//! long-running session that has connected to many distinct targets doesn't
//! pin idle TCP connections and pool memory for the plugin's lifetime.
//! Matches the sqlserver/dynamodb sibling plugins' architecture.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch, Mutex};
use tokio::time::interval;

const WORKER_POOL_SIZE: usize = 4;

// Bounded so a burst of requests applies backpressure to the stdin reader
// instead of buffering unboundedly in memory.
const REQUEST_QUEUE_CAPACITY: usize = 64;

const POOL_CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

struct Request {
    line: String,
    scope: Option<postgresql_plugin::cancel::ScopedGuard>,
}

#[tokio::main]
async fn main() {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let cleanup_handle = tokio::spawn(run_pool_cleanup(shutdown_rx));

    let (req_tx, req_rx) = mpsc::channel::<Request>(REQUEST_QUEUE_CAPACITY);
    let req_rx = Arc::new(Mutex::new(req_rx));

    let (resp_tx, resp_rx) = mpsc::unbounded_channel::<String>();
    let writer_handle = tokio::spawn(run_writer(resp_rx));

    let worker_handles: Vec<_> = (0..WORKER_POOL_SIZE)
        .map(|_| tokio::spawn(run_worker(req_rx.clone(), resp_tx.clone())))
        .collect();
    run_reader(req_tx, resp_tx).await;

    let _ = shutdown_tx.send(true);

    for handle in worker_handles {
        let _ = handle.await;
    }
    let _ = writer_handle.await;
    let _ = cleanup_handle.await;
}

async fn run_pool_cleanup(mut shutdown_rx: watch::Receiver<bool>) {
    let mut timer = interval(POOL_CLEANUP_INTERVAL);
    loop {
        tokio::select! {
            _ = timer.tick() => {
                postgresql_plugin::client::cleanup_idle_pools();
                postgresql_plugin::session::sweep_idle().await;
            }
            _ = shutdown_rx.changed() => break,
        }
    }
}

async fn run_reader(req_tx: mpsc::Sender<Request>, resp_tx: mpsc::UnboundedSender<String>) {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut controls = tokio::task::JoinSet::new();

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

        let request = serde_json::from_str::<serde_json::Value>(trimmed).ok();
        let method = request
            .as_ref()
            .and_then(|r| r["method"].as_str())
            .unwrap_or("");
        if matches!(
            method,
            "cancel" | "cancel_request" | "service_invalidate_auth"
        ) {
            let line = trimmed.to_string();
            let sender = resp_tx.clone();
            controls.spawn(async move {
                if let Some(response) = postgresql_plugin::rpc::handle_line(&line).await {
                    send_response(&sender, response);
                }
            });
            while controls.try_join_next().is_some() {}
            continue;
        }
        let scope = if matches!(
            method,
            "execute_query_exact"
                | "spatial_columns"
                | "spatial_transform_values"
                | "spatial_query_table"
                | "spatial_read_feature"
                | "service_session_open"
                | "service_session_finish"
                | "service_session_discard"
                | "service_test"
                | "service_catalog"
                | "service_row"
                | "service_explain"
        ) {
            let context = request.as_ref().map(|r| &r["params"]["driver_context"]);
            match context
                .and_then(|c| Some((c["connection_id"].as_str()?, c["request_id"].as_str()?)))
            {
                Some((connection, request_id)) => {
                    match postgresql_plugin::cancel::ScopedGuard::reserve(connection, request_id) {
                        Ok(scope) => Some(scope),
                        Err(error) => {
                            if let Some(request) =
                                request.as_ref().filter(|r| r.get("id").is_some())
                            {
                                send_response(&resp_tx, error.response(request["id"].clone()));
                            }
                            continue;
                        }
                    }
                }
                None => None,
            }
        } else {
            None
        };
        match req_tx.try_send(Request {
            line: trimmed.to_string(),
            scope,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => break,
            Err(mpsc::error::TrySendError::Full(_)) => {
                if let Some(request) = request.as_ref().filter(|r| r.get("id").is_some()) {
                    send_response(
                        &resp_tx,
                        postgresql_plugin::handlers::spatial::SpatialError::new(
                            "RATE_LIMITED",
                            "Driver request queue is full",
                        )
                        .response(request["id"].clone()),
                    );
                }
            }
        }
    }
    while controls.join_next().await.is_some() {}
}

fn send_response(sender: &mpsc::UnboundedSender<String>, response: serde_json::Value) {
    if let Ok(body) = serde_json::to_string(&response) {
        let _ = sender.send(body);
    }
}

async fn run_worker(
    req_rx: Arc<Mutex<mpsc::Receiver<Request>>>,
    resp_tx: mpsc::UnboundedSender<String>,
) {
    loop {
        let line = {
            let mut rx = req_rx.lock().await;
            rx.recv().await
        };
        let Some(line) = line else { break };

        // `handle_line` returns `None` for a JSON-RPC notification (a request
        // with no top-level `id` field, e.g. the host's `cancel` notification
        // on a plugin-call timeout, #126). A notification gets no response
        // per JSON-RPC convention, and writing one would corrupt the protocol
        // stream — so only forward a `Some` response to the writer.
        if let Some(response) =
            postgresql_plugin::rpc::handle_line_with_scope(&line.line, line.scope).await
        {
            let body = match serde_json::to_string(&response) {
                Ok(s) => s,
                Err(err) => format!(
                    "{{\"jsonrpc\":\"2.0\",\"error\":{{\"code\":-32603,\"message\":\"serialization failed: {err}\"}},\"id\":null}}"
                ),
            };

            if resp_tx.send(body).is_err() {
                break;
            }
        }
    }
}

async fn run_writer(mut resp_rx: mpsc::UnboundedReceiver<String>) {
    let mut stdout = tokio::io::stdout();
    while let Some(mut body) = resp_rx.recv().await {
        body.push('\n');
        if stdout.write_all(body.as_bytes()).await.is_err() {
            break;
        }
        let _ = stdout.flush().await;
    }
}
