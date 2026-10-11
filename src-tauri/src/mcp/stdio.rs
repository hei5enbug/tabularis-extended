use super::{handle_request, query_manager, JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use serde_json::json;
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Semaphore};
use tokio::task::JoinSet;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const ORDINARY_LIMIT: usize = 32;
const CONTROL_LIMIT: usize = 8;

enum Frame {
    Data(Vec<u8>),
    Oversized,
}

fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Frame>> {
    let mut bytes = Vec::new();
    let mut oversized = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(if oversized {
                Some(Frame::Oversized)
            } else if bytes.is_empty() {
                None
            } else {
                Some(Frame::Data(bytes))
            });
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let used = newline.map_or(buffer.len(), |index| index + 1);
        let payload = if newline.is_some() { used - 1 } else { used };
        if !oversized && bytes.len().saturating_add(payload) <= MAX_FRAME_BYTES {
            bytes.extend_from_slice(&buffer[..payload]);
        } else {
            oversized = true;
            bytes.clear();
        }
        reader.consume(used);
        if newline.is_some() {
            return Ok(Some(if oversized {
                Frame::Oversized
            } else {
                Frame::Data(bytes)
            }));
        }
    }
}

fn is_control(request: &JsonRpcRequest) -> bool {
    request.method == "tools/call"
        && matches!(
            request
                .params
                .as_ref()
                .and_then(|params| params.get("name"))
                .and_then(|name| name.as_str()),
            Some(
                "get_query_status"
                    | "cancel_query"
                    | "release_query_result"
                    | "fetch_query_result"
                    | "list_query_history"
            )
        )
}

fn error_response(
    id: Option<serde_json::Value>,
    code: i32,
    kind: &str,
    message: &str,
) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
            data: Some(json!({ "kind": kind })),
        }),
    }
}

pub(super) async fn serve() {
    let (input_tx, mut input_rx) = mpsc::channel(32);
    let (output_tx, mut output_rx) = mpsc::channel::<JsonRpcResponse>(256);
    let (closed_tx, mut closed_rx) = watch::channel(false);
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        while let Ok(Some(frame)) = read_frame(&mut reader) {
            if input_tx.blocking_send(frame).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        while let Some(response) = output_rx.blocking_recv() {
            let Ok(bytes) = serde_json::to_vec(&response) else {
                break;
            };
            if writer
                .write_all(&bytes)
                .and_then(|_| writer.write_all(b"\n"))
                .and_then(|_| writer.flush())
                .is_err()
            {
                break;
            }
        }
        let _ = closed_tx.send(true);
    });
    let ordinary = Arc::new(Semaphore::new(ORDINARY_LIMIT));
    let controls = Arc::new(Semaphore::new(CONTROL_LIMIT));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = closed_rx.changed() => break,
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
            frame = input_rx.recv() => {
                let Some(frame) = frame else { break; };
                let request = match frame {
                    Frame::Data(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => continue,
                    Frame::Data(bytes) => serde_json::from_slice::<JsonRpcRequest>(&bytes).ok(),
                    Frame::Oversized => None,
                };
                let Some(request) = request else {
                    if output_tx.try_send(error_response(None, -32700, "INVALID_REQUEST", "Invalid or oversized JSON-RPC request")).is_err() { break; }
                    continue;
                };
                if request.id.is_none() { continue; }
                if request.jsonrpc != "2.0" || !request.id.as_ref().is_some_and(|id| id.is_string() || id.is_number()) {
                    if output_tx.try_send(error_response(request.id, -32600, "INVALID_REQUEST", "Invalid JSON-RPC request")).is_err() { break; }
                    continue;
                }
                let slots = if is_control(&request) { controls.clone() } else { ordinary.clone() };
                let permit = match slots.try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        if output_tx.try_send(error_response(request.id, -32000, "QUEUE_FULL", "The MCP request capacity is full")).is_err() { break; }
                        continue;
                    }
                };
                let output = output_tx.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    if let Some(response) = handle_request(request).await { let _ = output.send(response).await; }
                });
            }
        }
    }
    drop(input_rx);
    query_manager::global_manager().shutdown().await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    drop(output_tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), closed_rx.changed()).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 너무_큰_프레임을_소진한_뒤_다음_요청을_읽는다() {
        // given
        let mut input = vec![b'x'; MAX_FRAME_BYTES + 1];
        input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\"}\n");
        let mut reader = io::BufReader::with_capacity(7, io::Cursor::new(input));
        // when
        let first = read_frame(&mut reader).unwrap();
        let second = read_frame(&mut reader).unwrap();
        // then
        assert!(matches!(first, Some(Frame::Oversized)));
        assert!(matches!(second, Some(Frame::Data(bytes)) if bytes == b"{\"jsonrpc\":\"2.0\"}"));
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn 줄바꿈이_없는_마지막_프레임도_한번만_읽는다() {
        // given
        let mut reader = io::Cursor::new(b"request");
        // when
        let frame = read_frame(&mut reader).unwrap();
        // then
        assert!(matches!(frame, Some(Frame::Data(bytes)) if bytes == b"request"));
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn 실행_요청은_취소_전용_슬롯을_사용하지_않는다() {
        // given
        let mut request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({"name":"start_query"})),
        };
        // when / then
        assert!(!is_control(&request));
        request.params = Some(json!({"name":"cancel_query"}));
        assert!(is_control(&request));
    }
}
