use super::{
    config::Config, credentials::Credentials, Error, Result, FRAME_BYTES, LOG_BYTES, REQUEST_MS,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

pub struct Response {
    pub result: Value,
    pub error: Option<&'static str>,
}
impl Response {
    pub fn parse(value: Value) -> Result<Self> {
        if value["jsonrpc"] != "2.0"
            || (value.get("result").is_some() == value.get("error").is_some())
        {
            return Err(Error("INVALID_RPC_FRAME"));
        }
        if value.get("error").is_some() {
            let code = match value["error"]["data"]["code"].as_str() {
                Some("OUTCOME_UNKNOWN") => "OUTCOME_UNKNOWN",
                Some("CANCELLED") => "CANCELLED",
                Some("DEADLINE_EXCEEDED") => "DEADLINE_EXCEEDED",
                Some("WRITE_NOT_ALLOWED") => "WRITE_NOT_ALLOWED",
                Some("INVALID_ARGUMENT") => "INVALID_ARGUMENT",
                Some("UNSUPPORTED_OPERATION") => "UNSUPPORTED_OPERATION",
                Some("AUTH_REQUIRED") => "AUTH_REQUIRED",
                Some("AUTH_EXPIRED") => "AUTH_EXPIRED",
                Some("RESOURCE_LIMIT") => "RESOURCE_LIMIT",
                Some("DRIVER_EXITED") => "DRIVER_EXITED",
                _ => "UNEXPECTED_NATIVE_ERROR",
            };
            Ok(Self {
                result: Value::Null,
                error: Some(code),
            })
        } else {
            Ok(Self {
                result: value["result"].clone(),
                error: None,
            })
        }
    }
}
pub trait Rpc {
    fn send(&mut self, request: Value) -> Result<()>;
    fn receive(&mut self, id: &str) -> Result<Response>;
    fn shutdown(&mut self) -> Result<()>;
    fn evidence_kind(&self) -> &'static str;
    fn call(&mut self, request: Value) -> Result<Response> {
        let id = request["id"]
            .as_str()
            .ok_or(Error("INVALID_REQUEST"))?
            .to_owned();
        self.send(request)?;
        self.receive(&id)
    }
}
pub fn read_frame(reader: &mut impl BufRead, cap: usize) -> Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let bytes = reader.fill_buf().map_err(|_| Error("RPC_IO_FAILED"))?;
        if bytes.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(Error("INVALID_RPC_FRAME"))
            };
        }
        let end = bytes.iter().position(|byte| *byte == b'\n');
        let length = end.map_or(bytes.len(), |index| index + 1);
        if length > cap.saturating_sub(frame.len()) {
            frame.fill(0);
            return Err(Error("FRAME_TOO_LARGE"));
        }
        frame.extend_from_slice(&bytes[..length]);
        reader.consume(length);
        if end.is_some() {
            return Ok(Some(frame));
        }
    }
}
pub fn frame_json(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > FRAME_BYTES || std::str::from_utf8(bytes).is_err() {
        return Err(Error("INVALID_RPC_FRAME"));
    }
    serde_json::from_slice(bytes).map_err(|_| Error("INVALID_RPC_FRAME"))
}
fn clear(value: &mut Value) {
    match value {
        Value::String(string) => {
            let mut bytes = std::mem::take(string).into_bytes();
            bytes.fill(0);
        }
        Value::Array(values) => values.iter_mut().for_each(clear),
        Value::Object(values) => values.values_mut().for_each(clear),
        _ => {}
    }
}
fn base64url_run(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'-' || **byte == b'_')
        .count()
}
/// A driver-acquired Entra token is unknown to the harness, so its output is
/// also screened for JWT-shaped values (`eyJ...` header, dot, payload).
pub fn token_shaped(bytes: &[u8]) -> bool {
    bytes
        .windows(3)
        .enumerate()
        .filter(|(_, window)| *window == b"eyJ")
        .any(|(start, _)| {
            let header = base64url_run(&bytes[start..]);
            let rest = &bytes[start + header..];
            header >= 16 && rest.first() == Some(&b'.') && base64url_run(&rest[1..]) >= 16
        })
}
struct Canaries(Vec<Vec<u8>>);
impl Canaries {
    fn found(&self, bytes: &[u8]) -> bool {
        token_shaped(bytes)
            || self.0.iter().any(|secret| {
                !secret.is_empty() && bytes.windows(secret.len()).any(|window| window == secret)
            })
    }
}
impl Drop for Canaries {
    fn drop(&mut self) {
        self.0.iter_mut().for_each(|bytes| bytes.fill(0));
    }
}
struct WriteJob {
    bytes: Vec<u8>,
    ack: mpsc::Sender<Result<()>>,
}
impl Drop for WriteJob {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn pause(&self, duration: Duration);
}
struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn pause(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}
pub struct Deadlines {
    whole: Instant,
    inflight: HashMap<String, Instant>,
    shutdown: Option<Instant>,
}
impl Deadlines {
    pub fn new(now: Instant) -> Self {
        Self {
            whole: now + Duration::from_secs(600),
            inflight: HashMap::new(),
            shutdown: None,
        }
    }
    pub fn begin(&mut self, id: &str, now: Instant) -> Result<()> {
        if self.inflight.len() >= 64 || self.inflight.contains_key(id) {
            return Err(Error("INFLIGHT_LIMIT"));
        }
        if now >= self.whole {
            return Err(Error("WHOLE_RUN_TIMEOUT"));
        }
        let mut deadline = (now + Duration::from_millis(REQUEST_MS)).min(self.whole);
        if let Some(shutdown) = self.shutdown {
            deadline = deadline.min(shutdown);
        }
        self.inflight.insert(id.to_owned(), deadline);
        Ok(())
    }
    pub fn remaining(&self, id: &str, now: Instant) -> Result<Duration> {
        let deadline = self.inflight.get(id).ok_or(Error("INVALID_RESPONSE_ID"))?;
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            Err(Error("RPC_TIMEOUT"))
        } else {
            Ok(remaining)
        }
    }
    pub fn shutdown(&mut self, now: Instant) -> Instant {
        let deadline = (now + Duration::from_secs(5)).min(self.whole);
        self.shutdown = Some(deadline);
        deadline
    }
    fn remove(&mut self, id: &str) {
        self.inflight.remove(id);
    }
}
/// Azure CLI state a live Entra run needs. The driver runs `az` itself, so the
/// sanitized child environment must still reach the signed-in CLI profile.
pub struct AzureEnv {
    pub config_dir: PathBuf,
    pub path: OsString,
}
impl AzureEnv {
    pub fn resolve(var: impl Fn(&str) -> Option<OsString>) -> Option<Self> {
        let absolute =
            |value: Option<OsString>| value.map(PathBuf::from).filter(|path| path.is_absolute());
        let config_dir = absolute(var("AZURE_CONFIG_DIR"))
            .or_else(|| absolute(var("HOME")).map(|home| home.join(".azure")))?;
        let path = var("PATH")
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
        Some(Self { config_dir, path })
    }
}
pub struct LiveRpc {
    child: Child,
    writer: Option<mpsc::SyncSender<WriteJob>>,
    responses: Option<mpsc::Receiver<Result<Value>>>,
    pending: HashMap<String, Value>,
    deadlines: Deadlines,
    fatal: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    clock: Arc<dyn Clock>,
    evidence: &'static str,
    root: PathBuf,
}
impl LiveRpc {
    pub fn spawn(config: &Config, credentials: &Credentials) -> Result<Self> {
        let binary = std::path::Path::new(env!("CARGO_BIN_EXE_sqlserver-plugin"));
        let azure = if config.principal.is_entra() {
            Some(
                AzureEnv::resolve(|key| std::env::var_os(key))
                    .ok_or(Error("AZURE_CONFIG_REQUIRED"))?,
            )
        } else {
            None
        };
        Self::spawn_process(
            binary,
            &[],
            credentials,
            azure,
            Arc::new(SystemClock),
            "actual_azure",
        )
    }
    pub fn spawn_process(
        binary: &Path,
        args: &[&OsStr],
        credentials: &Credentials,
        azure: Option<AzureEnv>,
        clock: Arc<dyn Clock>,
        evidence: &'static str,
    ) -> Result<Self> {
        if !binary.is_absolute() {
            return Err(Error("INVALID_DRIVER_BINARY"));
        }
        let deadlines = Deadlines::new(clock.now());
        let root = PathBuf::from(format!(
            "/tmp/tabularis-azure-harness-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&root).map_err(|_| Error("PRIVATE_PROFILE_FAILED"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(|_| Error("PRIVATE_PROFILE_FAILED"))?;
        }
        let mut command = Command::new(binary);
        command
            .args(args)
            .env_clear()
            .env("HOME", &root)
            .env("USERPROFILE", &root)
            .env("TMPDIR", &root)
            .env("TMP", &root)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(azure) = &azure {
            command
                .env("AZURE_CONFIG_DIR", &azure.config_dir)
                .env("PATH", &azure.path);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                let _ = fs::remove_dir_all(&root);
                return Err(Error("DRIVER_SPAWN_FAILED"));
            }
        };
        let mut stdin = child.stdin.take().ok_or(Error("DRIVER_PIPE_FAILED"))?;
        let stdout = child.stdout.take().ok_or(Error("DRIVER_PIPE_FAILED"))?;
        let mut stderr = child.stderr.take().ok_or(Error("DRIVER_PIPE_FAILED"))?;
        let canaries = Arc::new(Canaries(credentials.canaries()));
        let fatal = Arc::new(AtomicBool::new(false));
        let (writer, writes) = mpsc::sync_channel::<WriteJob>(64);
        let write_thread = std::thread::spawn(move || {
            while let Ok(mut job) = writes.recv() {
                let result = stdin
                    .write_all(&job.bytes)
                    .and_then(|_| stdin.flush())
                    .map_err(|_| Error("RPC_WRITE_FAILED"));
                job.bytes.fill(0);
                let failed = result.is_err();
                let _ = job.ack.send(result);
                if failed {
                    break;
                }
            }
        });
        let (sender, responses) = mpsc::sync_channel(64);
        let observed = canaries.clone();
        let out_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let result = read_frame(&mut reader, FRAME_BYTES).and_then(|frame| {
                    let Some(mut bytes) = frame else {
                        return Err(Error("DRIVER_EOF"));
                    };
                    let result = if observed.found(&bytes) {
                        Err(Error("SECRET_EXPOSURE_DETECTED"))
                    } else {
                        frame_json(&bytes)
                    };
                    bytes.fill(0);
                    result
                });
                let failed = result.is_err();
                if sender.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let flag = fatal.clone();
        let err_thread = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let read = (&mut stderr)
                .take(LOG_BYTES as u64 + 1)
                .read_to_end(&mut bytes);
            if read.is_err() || bytes.len() > LOG_BYTES || canaries.found(&bytes) {
                flag.store(true, Ordering::SeqCst);
            }
            bytes.fill(0);
        });
        Ok(Self {
            child,
            writer: Some(writer),
            responses: Some(responses),
            pending: HashMap::new(),
            deadlines,
            fatal,
            threads: vec![write_thread, out_thread, err_thread],
            clock,
            evidence,
            root,
        })
    }
    fn remaining(&self, id: &str) -> Result<Duration> {
        if self.fatal.load(Ordering::SeqCst) {
            return Err(Error("DRIVER_LOG_REJECTED"));
        }
        self.deadlines.remaining(id, self.clock.now())
    }
    fn send_request(&mut self, request: &Value) -> Result<()> {
        let now = self.clock.now();
        let id = request["id"]
            .as_str()
            .ok_or(Error("INVALID_REQUEST"))?
            .to_owned();
        self.deadlines.begin(&id, now)?;
        let result = (|| {
            let mut bytes = serde_json::to_vec(request).map_err(|_| Error("INVALID_REQUEST"))?;
            bytes.push(b'\n');
            let (sender, ack) = mpsc::channel();
            let job = WriteJob { bytes, ack: sender };
            if job.bytes.len() > FRAME_BYTES {
                return Err(Error("FRAME_TOO_LARGE"));
            }
            self.remaining(&id)?;
            self.writer
                .as_ref()
                .ok_or(Error("DRIVER_EOF"))?
                .try_send(job)
                .map_err(|_| Error("INFLIGHT_LIMIT"))?;
            ack.recv_timeout(self.remaining(&id)?)
                .map_err(|_| Error("RPC_WRITE_TIMEOUT"))?
        })();
        if let Err(error) = result {
            self.deadlines.remove(&id);
            if matches!(error.0, "RPC_WRITE_TIMEOUT" | "RPC_TIMEOUT") {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        result
    }
    fn join_threads(&mut self) {
        self.writer.take();
        self.responses.take();
        for (_, mut value) in self.pending.drain() {
            clear(&mut value);
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
impl Rpc for LiveRpc {
    fn send(&mut self, mut request: Value) -> Result<()> {
        let result = self.send_request(&request);
        clear(&mut request);
        result
    }
    fn receive(&mut self, id: &str) -> Result<Response> {
        loop {
            let duration = self.remaining(id)?;
            if let Some(value) = self.pending.remove(id) {
                self.deadlines.remove(id);
                return Response::parse(value);
            }
            match self
                .responses
                .as_ref()
                .ok_or(Error("DRIVER_EOF"))?
                .recv_timeout(duration.min(Duration::from_millis(250)))
            {
                Ok(value) => {
                    let value = value?;
                    let key = value["id"]
                        .as_str()
                        .ok_or(Error("INVALID_RESPONSE_ID"))?
                        .to_owned();
                    if !self.deadlines.inflight.contains_key(&key)
                        || self.pending.insert(key, value).is_some()
                    {
                        return Err(Error("INVALID_RESPONSE_ID"));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err(Error("DRIVER_EOF")),
            }
        }
    }
    fn shutdown(&mut self) -> Result<()> {
        let deadline = self.deadlines.shutdown(self.clock.now());
        let id = uuid::Uuid::new_v4().to_string();
        let result = self.call(json!({"jsonrpc":"2.0","id":id,"method":"shutdown","params":{}}));
        self.writer.take();
        self.responses.take();
        let waited = loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break Ok(()),
                Ok(None) => {}
                Err(_) => break Err(Error("DRIVER_WAIT_FAILED")),
            }
            if self.clock.now() >= deadline {
                break Err(Error("DRIVER_SHUTDOWN_TIMEOUT"));
            }
            self.clock.pause(Duration::from_millis(10));
        };
        if waited.is_err() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.join_threads();
        waited?;
        if self.fatal.load(Ordering::SeqCst) {
            return Err(Error("DRIVER_LOG_REJECTED"));
        }
        match result {
            Err(error)
                if matches!(
                    error.0,
                    "RPC_TIMEOUT" | "RPC_WRITE_TIMEOUT" | "WHOLE_RUN_TIMEOUT"
                ) =>
            {
                Err(Error("DRIVER_SHUTDOWN_TIMEOUT"))
            }
            Err(error) => Err(error),
            Ok(response) if response.error.is_none() => Ok(()),
            Ok(_) => Err(Error("DRIVER_SHUTDOWN_FAILED")),
        }
    }
    fn evidence_kind(&self) -> &'static str {
        self.evidence
    }
}
impl Drop for LiveRpc {
    fn drop(&mut self) {
        self.writer.take();
        self.responses.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.join_threads();
        let _ = fs::remove_dir_all(&self.root);
    }
}
