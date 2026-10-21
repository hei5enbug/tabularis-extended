use super::super::{
    backend::CancellationSignal,
    contracts::{ErrorCode, ServiceError},
    dispatch::{CallerContext, CallerTransport},
    error::{failure, redact_error, ServiceResult},
};
use super::{request, tool_catalog, LegacyFailure, ServiceTransport};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, watch, Semaphore},
    task::JoinSet,
    time::{timeout_at, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Cli,
    Mcp,
}

enum Frame {
    Eof,
    Line(Vec<u8>),
    Oversized,
}
async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Frame> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if bytes.is_empty() {
                Frame::Eof
            } else {
                Frame::Line(bytes)
            });
        }
        let length = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if bytes.len().saturating_add(length) > request::MAX_FRAME_BYTES + 1 {
            return Ok(Frame::Oversized);
        }
        bytes.extend_from_slice(&available[..length]);
        reader.consume(length);
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(Frame::Line(bytes));
        }
        if bytes.len() > request::MAX_FRAME_BYTES {
            return Ok(Frame::Oversized);
        }
    }
}

async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
    deadlines: &mut watch::Receiver<Instant>,
) -> ServiceResult<()> {
    let output = async {
        writer.write_all(bytes).await?;
        writer.flush().await
    };
    tokio::pin!(output);
    let mut watch_open = true;
    loop {
        let deadline = *deadlines.borrow();
        tokio::select! {
            result=&mut output => return result.map_err(|_|failure(ErrorCode::DriverExited)),
            _=tokio::time::sleep_until(deadline) => return Err(failure(ErrorCode::DeadlineExceeded)),
            changed=deadlines.changed(), if watch_open => {if changed.is_err(){watch_open=false;}}
        }
    }
}

/// Why an MCP request failed: a service error, or an error the legacy server
/// answers with and that is passed on unchanged.
enum Failure {
    Service(ServiceError),
    Legacy(LegacyFailure),
}
impl From<ServiceError> for Failure {
    fn from(error: ServiceError) -> Self {
        Self::Service(error)
    }
}

pub fn legacy_error_frame(id: Value, failure: LegacyFailure) -> Value {
    let mut error = json!({"code":failure.code,"message":failure.message});
    if let Some(data) = failure.data {
        error["data"] = data;
    }
    json!({"jsonrpc":"2.0","id":id,"error":error})
}

fn rpc_error(id: Value, error: ServiceError) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"Service request failed","data":redact_error(error)}})
}

fn rpc_id(raw: &Value) -> ServiceResult<Option<Value>> {
    let object = raw
        .as_object()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "jsonrpc" | "id" | "method" | "params"))
        || raw["jsonrpc"] != "2.0"
        || raw["method"].as_str().is_none_or(|name| {
            name.is_empty() || name.len() > 128 || name.chars().any(char::is_control)
        })
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    match object.get("id") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Value::Null)),
        Some(Value::String(id)) if id.len() <= 128 && !id.chars().any(char::is_control) => {
            Ok(Some(json!(id)))
        }
        Some(Value::Number(id))
            if id.as_f64().is_some_and(|number| {
                number.is_finite()
                    && number.fract() == 0.0
                    && number.abs() <= 9_007_199_254_740_991.0
            }) =>
        {
            Ok(Some(Value::Number(id.clone())))
        }
        _ => Err(failure(ErrorCode::InvalidArgument)),
    }
}

fn only_keys(value: &Value, keys: &[&str]) -> ServiceResult<()> {
    if value
        .as_object()
        .is_none_or(|object| object.keys().any(|key| !keys.contains(&key.as_str())))
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(())
}

impl ServiceTransport {
    async fn mcp_request(
        self: &Arc<Self>,
        raw: Value,
        mut caller: CallerContext,
        signal: CancellationSignal,
    ) -> (Option<Value>, i32) {
        let id = match rpc_id(&raw) {
            Ok(id) => id,
            Err(error) => return (Some(rpc_error(Value::Null, error)), 2),
        };
        caller.transport = CallerTransport::Mcp;
        let params = raw.get("params").cloned().unwrap_or_else(|| json!({}));
        let result: Result<Value, Failure> = async {
            match raw["method"].as_str().unwrap_or_default() {
                "initialize" => {
                    only_keys(&params, &["protocolVersion", "capabilities", "clientInfo"])?;
                    let version = params["protocolVersion"].as_str().filter(|version| matches!(*version, "2024-11-05" | "2025-03-26" | "2025-06-18")).unwrap_or("2025-06-18");
                    if let (Some(name), Some(legacy)) = (params["clientInfo"]["name"].as_str(), self.legacy.get()) {
                        legacy.client_connected(name);
                    }
                    Ok(json!({"protocolVersion":version,"capabilities":{"tools":{},"resources":{}},"serverInfo":{"name":"tabularis-extended","version":env!("CARGO_PKG_VERSION")}}))
                }
                "ping" => { only_keys(&params, &[])?; Ok(json!({})) }
                "tools/list" => { only_keys(&params, &[])?; Ok(self.tools()?) }
                "resources/list" => {
                    only_keys(&params,&[])?;
                    let value=match self.legacy.get() {Some(legacy)=>legacy.list_resources(&caller).await.unwrap_or_else(||json!({"resources":[]})),None=>json!({"resources":[]})};
                    if serde_json::to_vec(&value).map_err(|_|failure(ErrorCode::ResourceLimit))?.len()>request::MAX_FRAME_BYTES{return Err(failure(ErrorCode::ResourceLimit).into());}
                    Ok(value)
                }
                "tools/call" => {
                    only_keys(&params, &["name", "arguments"])?;
                    let name = params["name"].as_str().ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
                    match self.legacy_call(name, &params["arguments"], &caller).await {
                        Some(Ok(value)) => Ok(value),
                        Some(Err(legacy)) => Err(Failure::Legacy(legacy)),
                        None => Ok(self.call_tool(name, params["arguments"].clone(), caller, Some(signal)).await?),
                    }
                }
                "resources/read" => {
                    only_keys(&params, &["uri"])?;
                    let uri = params["uri"].as_str().ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
                    Ok(self.resource_with_legacy(uri, &caller).await?)
                }
                "notifications/initialized" if id.is_none() => { only_keys(&params, &[])?; Ok(json!({})) }
                _ => Err(failure(ErrorCode::UnsupportedOperation).into()),
            }
        }.await;
        match (id, result) {
            (None, _) => (None, 0),
            (Some(id), Ok(result)) => (Some(json!({"jsonrpc":"2.0","id":id,"result":result})), 0),
            (Some(id), Err(Failure::Legacy(failure))) => (Some(legacy_error_frame(id, failure)), 0),
            // A rejected request is an answer, not a server failure.
            (Some(id), Err(Failure::Service(error))) => (Some(rpc_error(id, error)), 0),
        }
    }

    pub async fn run_stdio<R, W>(
        self: &Arc<Self>,
        mut reader: R,
        mut writer: W,
        mut caller: CallerContext,
        mode: Mode,
        explicit_write_intent: bool,
        shutdown_signal: CancellationSignal,
    ) -> ServiceResult<i32>
    where
        R: AsyncBufRead + Unpin + Send,
        W: AsyncWrite + Unpin + Send,
    {
        request::validate_caller(&caller)?;
        caller.transport = match mode {
            Mode::Cli => CallerTransport::Cli,
            Mode::Mcp => CallerTransport::Mcp,
        };
        let disconnected = CancellationSignal::default();
        let (sender, mut receiver) = mpsc::channel::<Value>(64);
        let (deadline_updates, mut output_deadlines) =
            watch::channel(Instant::now() + Duration::from_millis(request::MAX_FOREGROUND_MS));
        let write_signal = disconnected.clone();
        let write_loop = async {
            while let Some(value) = receiver.recv().await {
                let mut bytes =
                    serde_json::to_vec(&value).map_err(|_| failure(ErrorCode::ResourceLimit))?;
                if bytes.len() > request::MAX_FRAME_BYTES {
                    write_signal.cancel();
                    return Err(failure(ErrorCode::ResourceLimit));
                }
                bytes.push(b'\n');
                if let Err(error) = write_frame(&mut writer, &bytes, &mut output_deadlines).await {
                    write_signal.cancel();
                    return Err(error);
                }
            }
            Ok(())
        };
        let read_loop = async {
            let ordinary = Arc::new(Semaphore::new(80));
            let control = Arc::new(Semaphore::new(16));
            let mut tasks = JoinSet::new();
            let active = CancellationSignal::default();
            let mut drain_deadline = Instant::now() + Duration::from_secs(2);
            let mut exit = 0;
            let mut interrupted = false;
            loop {
                while let Some(result) = tasks.try_join_next() {
                    if let Ok(code) = result {
                        if code != 0 {
                            exit = code;
                        }
                    } else {
                        exit = 4;
                    }
                }
                let frame = tokio::select! {
                    _ = shutdown_signal.cancelled() => { interrupted=true; active.cancel(); break; },
                    _ = disconnected.cancelled() => { active.cancel(); break; },
                    frame = read_frame(&mut reader) => frame,
                };
                let bytes = match frame {
                    Ok(Frame::Eof) => break,
                    Ok(Frame::Line(bytes)) => bytes,
                    Ok(Frame::Oversized) => {
                        let error = failure(ErrorCode::ResourceLimit);
                        let response = if mode == Mode::Mcp {
                            rpc_error(Value::Null, error)
                        } else {
                            serde_json::to_value(request::rejected_raw(&Value::Null, error))
                                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                        };
                        let _ = sender.try_send(response);
                        exit = 2;
                        active.cancel();
                        break;
                    }
                    Err(_) => {
                        exit = 4;
                        active.cancel();
                        break;
                    }
                };
                if bytes.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let raw: Value = match serde_json::from_slice(&bytes) {
                    Ok(raw) => raw,
                    Err(_) => {
                        let error = failure(ErrorCode::InvalidArgument);
                        let response = if mode == Mode::Mcp {
                            rpc_error(Value::Null, error)
                        } else {
                            serde_json::to_value(request::rejected_raw(&Value::Null, error))
                                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                        };
                        if sender.try_send(response).is_err() {
                            active.cancel();
                            exit = 4;
                            break;
                        }
                        exit = 2;
                        continue;
                    }
                };
                let envelope = if mode == Mode::Mcp {
                    &raw["params"]["arguments"]
                } else {
                    &raw
                };
                let is_control = if mode == Mode::Mcp {
                    match raw["method"].as_str() {
                        Some(
                            "initialize"
                            | "ping"
                            | "tools/list"
                            | "resources/list"
                            | "resources/read"
                            | "notifications/initialized",
                        ) => true,
                        Some("tools/call") => raw["params"]["name"]
                            .as_str()
                            .is_some_and(tool_catalog::is_control_tool),
                        _ => false,
                    }
                } else {
                    envelope["operation"]
                        .as_str()
                        .and_then(|name| serde_json::from_value(json!(name)).ok())
                        .is_some_and(request::control_operation)
                };
                let permit = match if is_control {
                    control.clone()
                } else {
                    ordinary.clone()
                }
                .try_acquire_owned()
                {
                    Ok(permit) => permit,
                    Err(_) => {
                        let error = failure(ErrorCode::ResourceLimit);
                        let response = if mode == Mode::Mcp {
                            match rpc_id(&raw) {
                                Ok(None) => continue,
                                Ok(Some(id)) => rpc_error(id, error),
                                Err(error) => rpc_error(Value::Null, error),
                            }
                        } else {
                            serde_json::to_value(request::rejected_raw(&raw, error))
                                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                        };
                        if sender.try_send(response).is_err() {
                            active.cancel();
                            exit = 4;
                            break;
                        }
                        exit = 4;
                        continue;
                    }
                };
                let deadline_ms = envelope["deadline_ms"]
                    .as_u64()
                    .unwrap_or(30_000)
                    .clamp(1, 120_000)
                    .saturating_add(if mode == Mode::Mcp {
                        envelope["wait_ms"].as_u64().unwrap_or(5_000).min(60_000)
                    } else {
                        0
                    });
                let deadline = Instant::now() + Duration::from_millis(deadline_ms + 2_000);
                drain_deadline = drain_deadline.max(deadline);
                deadline_updates.send_replace(drain_deadline);
                let transport = self.clone();
                let caller = caller.clone();
                let signal = active.clone();
                let sender = sender.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let (response, code) = if mode == Mode::Mcp {
                        transport.mcp_request(raw, caller, signal).await
                    } else {
                        let response = match transport
                            .run_one(raw.clone(), caller, explicit_write_intent, Some(signal))
                            .await
                        {
                            Ok(response) => response,
                            Err(error) => request::rejected_raw(&raw, error),
                        };
                        let code = request::exit_code(&response);
                        (serde_json::to_value(response).ok(), code)
                    };
                    if let Some(response) = response {
                        if !matches!(
                            timeout_at(deadline, sender.send(response)).await,
                            Ok(Ok(()))
                        ) {
                            return 4;
                        }
                    }
                    code
                });
            }
            if interrupted || disconnected.is_cancelled() {
                drain_deadline = drain_deadline.min(Instant::now() + Duration::from_secs(2));
            }
            deadline_updates.send_replace(drain_deadline);
            loop {
                match timeout_at(drain_deadline, tasks.join_next()).await {
                    Ok(Some(Ok(code))) => {
                        if code != 0 {
                            exit = code;
                        }
                    }
                    Ok(Some(Err(_))) => exit = 4,
                    Ok(None) => break,
                    Err(_) => {
                        active.cancel();
                        tasks.abort_all();
                        while tasks.join_next().await.is_some() {}
                        exit = 4;
                        break;
                    }
                }
            }
            let shutdown_deadline = drain_deadline.max(Instant::now() + Duration::from_millis(1));
            if !matches!(
                timeout_at(shutdown_deadline, self.runtime.shutdown()).await,
                Ok(Ok(()))
            ) && exit == 0
            {
                exit = 4;
            }
            drop(sender);
            Ok(if interrupted { 130 } else { exit })
        };
        let (read, written) = tokio::join!(read_loop, write_loop);
        written?;
        read
    }
}
