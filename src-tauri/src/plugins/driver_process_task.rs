//! The management task that owns a plugin's child process: it writes requests
//! to the child, routes its responses back to the callers, and enforces
//! shutdown. A process that was started on its first call is also stopped
//! after it has been idle for the configured period (see
//! [`crate::plugins::idle_stop`]) and started again by the next call, so the
//! task runs one loop per child process ("generation").

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::{
    next_command, read_plugin_frame, start_on_first_call, PendingPluginRequest, PluginCommand,
    ProcessStart, MAX_PENDING_REQUESTS, MAX_RPC_FRAME_BYTES, PLUGIN_INIT_TIMEOUT,
};
use crate::plugins::rpc::{
    cancel_notification_line, JsonRpcRequest, JsonRpcResponse, PluginCallError,
};

/// The pid of the running child; `None` while no child runs.
pub(super) type SharedPid = Arc<Mutex<Option<u32>>>;

pub(super) fn set_pid(pid: &SharedPid, value: Option<u32>) {
    if let Ok(mut guard) = pid.lock() {
        *guard = value;
    }
}

pub(super) struct ProcessTask {
    pub plugin_id: String,
    pub executable_path: PathBuf,
    pub interpreter: Option<String>,
    pub start: ProcessStart,
    pub pid: SharedPid,
    pub rx: mpsc::Receiver<PluginCommand>,
    pub shutdown_rx: oneshot::Receiver<Option<Duration>>,
}

/// How one child process's loop ended.
enum GenerationEnd {
    /// Shutdown, a crash or a broken transport: the task is over.
    Finished,
    /// The child sat idle and was stopped: wait for the next call.
    IdleStopped,
    /// The restarted child did not accept its `initialize`: wait for the next call.
    ReinitializeFailed,
}

struct Generation {
    number: u32,
    first_command: Option<PluginCommand>,
}

/// State that outlives a child process.
/// `Number.MAX_SAFE_INTEGER`: a Node plugin rejects a larger numeric id.
const FIRST_REPLAY_ID: u64 = (1 << 53) - 1;

struct TaskMemory {
    /// The last `initialize` request a child answered successfully, replayed
    /// to a restarted child so the caller that wakes it finds it initialized.
    last_initialize: Option<JsonRpcRequest>,
    /// Ids for the replayed request, counting down from the largest integer a
    /// JavaScript plugin accepts as an id, so they stay valid for Node plugins
    /// and never collide with the ids callers draw counting up from 1.
    next_replay_id: u64,
}

pub(super) async fn run(mut task: ProcessTask, running_child: Option<tokio::process::Child>) {
    let mut memory = TaskMemory {
        last_initialize: None,
        next_replay_id: FIRST_REPLAY_ID,
    };
    let mut next = running_child.map(|child| (child, None));
    let mut number = 0;
    loop {
        let (child, first_command) = match next.take() {
            Some(started) => started,
            None => match start_on_first_call(
                &mut task.rx,
                &mut task.shutdown_rx,
                &task.executable_path,
                task.interpreter.as_deref(),
            )
            .await
            {
                Some((child, command)) => (child, Some(command)),
                None => return,
            },
        };
        number += 1;
        set_pid(&task.pid, Some(child.id().unwrap_or_default()));
        let generation = Generation {
            number,
            first_command,
        };
        match run_generation(&mut task, &mut memory, child, generation).await {
            GenerationEnd::Finished => return,
            GenerationEnd::IdleStopped | GenerationEnd::ReinitializeFailed => {
                set_pid(&task.pid, None);
            }
        }
    }
}

async fn run_generation(
    task: &mut ProcessTask,
    memory: &mut TaskMemory,
    mut child: tokio::process::Child,
    generation: Generation,
) -> GenerationEnd {
    let Generation {
        number,
        mut first_command,
    } = generation;
    let plugin_id = task.plugin_id.clone();
    let idle_stop_allowed = task.start == ProcessStart::OnFirstCall;
    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let mut reader = BufReader::new(stdout);
    let mut line_buf = Vec::new();

    if number > 1 {
        if let Some(initialize) = &memory.last_initialize {
            let replay_id = memory.next_replay_id;
            memory.next_replay_id -= 1;
            let outcome = replay_initialize(
                &mut stdin,
                &mut reader,
                &mut line_buf,
                initialize,
                replay_id,
            )
            .await;
            if let Err(message) = outcome {
                log::warn!(
                    "Plugin {plugin_id} did not initialize after an idle restart: {message}"
                );
                answer_with_error(first_command.take(), message);
                let _ = child.kill().await;
                return GenerationEnd::ReinitializeFailed;
            }
            line_buf.clear();
        }
    }

    let rx = &mut task.rx;
    let shutdown_rx = &mut task.shutdown_rx;
    let mut pending_requests: HashMap<u64, PendingPluginRequest> = HashMap::new();
    let mut shutting_down = false;
    let mut shutdown_deadline: Option<tokio::time::Instant> = None;
    let mut channel_drained = false;
    let mut last_activity = tokio::time::Instant::now();
    let mut end = GenerationEnd::Finished;

    loop {
        pending_requests.retain(|_, pending| !pending.response.is_closed());
        if shutting_down && channel_drained && pending_requests.is_empty() {
            break;
        }
        let idle_deadline = if idle_stop_allowed
            && !shutting_down
            && pending_requests.is_empty()
            && first_command.is_none()
        {
            crate::plugins::idle_stop::for_plugin(&plugin_id).map(|idle| last_activity + idle)
        } else {
            None
        };
        tokio::select! {
            resolved = &mut *shutdown_rx, if !shutting_down => {
                // Existing callers with a reserved queue slot may still
                // enqueue; receive those requests before deciding we are
                // fully drained. No fresh reservations are allowed.
                rx.close();
                shutting_down = true;
                // A dropped sender is not a configured "wait forever",
                // so use the global grace, which is always bounded.
                let grace = resolved.unwrap_or_else(|_| {
                    Some(crate::plugins::shutdown_grace::dropped_signal_grace())
                });
                if let Some(grace) = grace {
                    shutdown_deadline = Some(tokio::time::Instant::now() + grace);
                    log::info!(
                        "Plugin {plugin_id} draining RPCs for {}s",
                        grace.as_secs()
                    );
                } else {
                    log::info!("Plugin {plugin_id} draining RPCs until they finish");
                }
            }
            _ = tokio::time::sleep_until(
                shutdown_deadline.unwrap_or_else(tokio::time::Instant::now)
            ), if shutdown_deadline.is_some() => {
                log::warn!("Plugin shutdown grace period elapsed; terminating child");
                break;
            }
            _ = tokio::time::sleep_until(
                idle_deadline.unwrap_or_else(tokio::time::Instant::now)
            ), if idle_deadline.is_some() => {
                if crate::plugins::idle_stop::is_held(&plugin_id) || !rx.is_empty() {
                    last_activity = tokio::time::Instant::now();
                    continue;
                }
                log::info!("Plugin {plugin_id} idle; stopping its process until the next call");
                end = GenerationEnd::IdleStopped;
                break;
            }
            msg = next_command(&mut first_command, rx), if !channel_drained => {
                last_activity = tokio::time::Instant::now();
                let (request, response, native) = match msg {
                    Some(PluginCommand::Call(request, response)) => (request, response, None),
                    Some(PluginCommand::NativeCall(request, response, dispatch)) => (request, response, Some(dispatch)),
                    Some(PluginCommand::DropNative(id)) => { pending_requests.remove(&id); continue; },
                    Some(PluginCommand::Cancel(id)) => {
                        if pending_requests.remove(&id).is_some() {
                            let line = cancel_notification_line(id);
                            if !matches!(tokio::time::timeout(PLUGIN_INIT_TIMEOUT, stdin.write_all(line.as_bytes())).await, Ok(Ok(()))) { break; }
                        }
                        continue;
                    },
                    None => {
                        if shutting_down {
                            channel_drained = true;
                            continue;
                        }
                        log::warn!("Plugin process channel closed without shutdown signal");
                        break;
                    }
                };
                if response.is_closed() { continue; }
                if pending_requests.len() >= MAX_PENDING_REQUESTS {
                    let _ = response.send(Err(PluginCallError::Transport("native_pending_limit".into()))); continue;
                }
                if let Some(dispatch) = &native {
                    if dispatch.abandoned.load(Ordering::SeqCst) || dispatch.signal.is_cancelled() {
                        let _ = response.send(Err(PluginCallError::Transport("native_cancelled".into()))); continue;
                    }
                    if tokio::time::Instant::now() >= dispatch.deadline {
                        let _ = response.send(Err(PluginCallError::Transport("native_deadline".into()))); continue;
                    }
                }
                let mut encoded = match serde_json::to_vec(&request) {
                    Ok(encoded) if encoded.len() <= MAX_RPC_FRAME_BYTES => encoded,
                    _ => { let _ = response.send(Err(PluginCallError::Transport("native_frame_limit".into()))); continue; },
                };
                encoded.push(b'\n');
                let id = request.id;
                pending_requests.insert(id, PendingPluginRequest {
                    response,
                    native: native.clone(),
                    initialize: (request.method == "initialize").then_some(request),
                });
                let written = if let Some(dispatch) = &native {
                    dispatch.attempted.store(true, Ordering::SeqCst);
                    tokio::select! {
                        result = stdin.write_all(&encoded) => result.is_ok(),
                        _ = tokio::time::sleep_until(dispatch.deadline) => false,
                        _ = dispatch.signal.cancelled() => false,
                    }
                } else { matches!(tokio::time::timeout(PLUGIN_INIT_TIMEOUT, stdin.write_all(&encoded)).await, Ok(Ok(()))) };
                if !written { break; }
            }
            frame = read_plugin_frame(&mut reader, &mut line_buf) => {
                match frame {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        last_activity = tokio::time::Instant::now();
                        if line_buf.last() == Some(&b'\n') { line_buf.pop(); }
                        if line_buf.len() > MAX_RPC_FRAME_BYTES { break; }
                        let response = match serde_json::from_slice::<JsonRpcResponse>(&line_buf) { Ok(response) => response, Err(_) => break };
                        line_buf.clear();
                        let (id, outcome) = match response {
                            JsonRpcResponse::Success { jsonrpc, result, id } if jsonrpc == "2.0" => (id, Ok(result)),
                            JsonRpcResponse::Error { jsonrpc, error, id } if jsonrpc == "2.0" => (id, Err(PluginCallError::Remote(error))),
                            _ => break,
                        };
                        match id {
                            Some(id) => {
                                if let Some(pending) = pending_requests.remove(&id) {
                                    if outcome.is_ok() {
                                        if let Some(initialize) = pending.initialize {
                                            memory.last_initialize = Some(initialize);
                                        }
                                    }
                                    let _ = pending.response.send(outcome);
                                }
                            }
                            None => {
                                if let Err(PluginCallError::Remote(error)) = outcome {
                                    log::error!(
                                        "Plugin returned an error without a request id ({}): {}",
                                        error.code,
                                        error.message
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // Give remaining callers a specific shutdown failure instead of
    // an ambiguous dropped-channel/no-response error.
    for (_, pending) in pending_requests.drain() {
        let _ = pending.native;
        let message = if shutting_down {
            "Plugin shutdown grace period elapsed before RPC completed"
        } else {
            "Plugin transport stopped"
        };
        let _ = pending
            .response
            .send(Err(PluginCallError::Transport(message.into())));
    }
    if matches!(end, GenerationEnd::IdleStopped) {
        // Closing stdin lets a well-behaved plugin exit by itself.
        drop(stdin);
        let grace = crate::plugins::shutdown_grace::for_plugin(&plugin_id)
            .unwrap_or(crate::plugins::shutdown_grace::DEFAULT_PLUGIN_SHUTDOWN_GRACE);
        let _ = tokio::time::timeout(grace, child.wait()).await;
    }
    let _ = child.kill().await;
    end
}

/// Writes the remembered `initialize` to a fresh child under a task-owned id
/// and waits for its answer.
async fn replay_initialize(
    stdin: &mut tokio::process::ChildStdin,
    reader: &mut BufReader<tokio::process::ChildStdout>,
    line_buf: &mut Vec<u8>,
    initialize: &JsonRpcRequest,
    replay_id: u64,
) -> Result<(), String> {
    let request = JsonRpcRequest {
        jsonrpc: initialize.jsonrpc.clone(),
        method: initialize.method.clone(),
        params: initialize.params.clone(),
        id: replay_id,
    };
    let mut encoded = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    encoded.push(b'\n');
    let exchange = async {
        stdin
            .write_all(&encoded)
            .await
            .map_err(|error| format!("could not send initialize: {error}"))?;
        loop {
            line_buf.clear();
            match read_plugin_frame(reader, line_buf).await {
                Ok(0) => return Err("the plugin exited before it answered initialize".to_string()),
                Err(error) => return Err(format!("could not read the initialize answer: {error}")),
                Ok(_) => {}
            }
            match serde_json::from_slice::<JsonRpcResponse>(line_buf.trim_ascii()) {
                Ok(JsonRpcResponse::Success { id, .. }) if id == Some(replay_id) => return Ok(()),
                Ok(JsonRpcResponse::Error { id, error, .. })
                    if id == Some(replay_id) || id.is_none() =>
                {
                    return Err(format!("initialize failed: {}", error.message))
                }
                Ok(_) => {}
                Err(error) => return Err(format!("unreadable initialize answer: {error}")),
            }
        }
    };
    match tokio::time::timeout(PLUGIN_INIT_TIMEOUT, exchange).await {
        Ok(result) => result,
        Err(_) => Err("initialize timed out".to_string()),
    }
}

/// Answers the call that woke a plugin that then failed to start properly.
fn answer_with_error(command: Option<PluginCommand>, reason: String) {
    let message =
        format!("Plugin could not be restarted after it was stopped while idle: {reason}");
    if let Some(PluginCommand::Call(_, response) | PluginCommand::NativeCall(_, response, _)) =
        command
    {
        let _ = response.send(Err(PluginCallError::Transport(message)));
    }
}
