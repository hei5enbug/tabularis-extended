use super::super::{
    contracts::{
        ErrorCode, SchemaValidator, ServiceError, ServiceRequest, ServiceResponse,
        MAX_JOB_DEADLINE_MS,
    },
    dispatch::{CallerContext, CallerTransport},
    error::{failure, refusal, ServiceResult, LOCAL_IPC_INSTANCES},
    runtime::ServiceRuntime,
    security::opaque_id,
    storage::PrivateStore,
};
use super::{
    gui::{apply_timeout, validate_ack, validate_apply_request},
    platform,
    wire::{encode_frame, read_frame, write_encoded, write_frame},
    GuiApplyAck, GuiApplyRequest,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    sync::{Notify, Semaphore},
    task::JoinHandle,
};

/// The longest an owner call may take: a job's deadline and the grace after it.
const MAX_OWNER_CALL_MS: u64 = MAX_JOB_DEADLINE_MS + 2_000;

pub(super) fn owner_call_timeout(deadline_ms: u64) -> Duration {
    Duration::from_millis(deadline_ms.saturating_add(2_000).min(MAX_OWNER_CALL_MS))
}
pub(super) fn served_deadline(deadline_ms: u64) -> Duration {
    Duration::from_millis(deadline_ms.min(MAX_JOB_DEADLINE_MS))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    Gui,
    Cli,
    Mcp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceMetadata {
    pub instance_id: String,
    pub process_id: u32,
    pub kind: EndpointKind,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    instance_id: String,
    process_id: u32,
    kind: EndpointKind,
    start_nonce: String,
}
impl Record {
    fn metadata(&self) -> InstanceMetadata {
        InstanceMetadata {
            instance_id: self.instance_id.clone(),
            process_id: self.process_id,
            kind: self.kind,
        }
    }
    fn validate(&self) -> ServiceResult<()> {
        opaque_id(&self.instance_id)?;
        if uuid::Uuid::parse_str(&self.start_nonce).is_err() || self.process_id == 0 {
            return Err(failure(ErrorCode::GuiUnavailable));
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    protocol_version: u8,
    target_nonce: String,
    client_instance: String,
    client_nonce: String,
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Call {
    Probe,
    Service(ServiceRequest),
    ApplyMap(GuiApplyRequest),
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Reply {
    Probe,
    Service(ServiceResponse),
    ApplyMap(ServiceResult<GuiApplyAck>),
}

#[async_trait]
pub trait LocalIpcRouter: Send + Sync {
    async fn call(&self, request: ServiceRequest, caller: CallerContext) -> ServiceResponse;
    async fn apply_map(
        &self,
        _request: GuiApplyRequest,
        _caller: CallerContext,
    ) -> ServiceResult<GuiApplyAck> {
        Err(failure(ErrorCode::GuiUnavailable))
    }
}

pub struct RuntimeRouter {
    runtime: Arc<ServiceRuntime>,
    validator: SchemaValidator,
}
impl RuntimeRouter {
    pub fn new(runtime: Arc<ServiceRuntime>) -> Result<Self, String> {
        Ok(Self {
            runtime,
            validator: SchemaValidator::new()?,
        })
    }
}
#[async_trait]
impl LocalIpcRouter for RuntimeRouter {
    async fn call(&self, request: ServiceRequest, caller: CallerContext) -> ServiceResponse {
        if serde_json::to_value(&request)
            .ok()
            .is_none_or(|value| self.validator.validate_request(&value).is_err())
        {
            super::super::dispatch::ServiceHandler::rejected(
                self.runtime.as_ref(),
                &request,
                &caller,
            );
            return ServiceResponse::failure(
                &request,
                if request.protocol_version == 1 {
                    ErrorCode::InvalidArgument
                } else {
                    ErrorCode::ProtocolMismatch
                },
                "The service request is invalid.",
            );
        }
        self.runtime.submit(request, caller).await
    }
}

pub struct LocalIpc {
    store: Arc<PrivateStore>,
    record: Record,
    endpoint: PathBuf,
    stopped: Arc<AtomicBool>,
    stop: Arc<Notify>,
    task: Option<JoinHandle<()>>,
}

impl LocalIpc {
    pub async fn start(
        profile: &Path,
        instance_id: String,
        kind: EndpointKind,
        router: Arc<dyn LocalIpcRouter>,
    ) -> ServiceResult<Self> {
        opaque_id(&instance_id)?;
        let store = Arc::new(PrivateStore::new(profile.join("local-ipc"), 16 * 1024)?);
        let record = Record {
            instance_id,
            process_id: std::process::id(),
            kind,
            start_nonce: uuid::Uuid::new_v4().to_string(),
        };
        let endpoint = endpoint_path(store.root(), &record);
        let mut listener;
        {
            let _lock = store.lock()?;
            if store.read::<Record>(&record.instance_id)?.is_some() {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            reap_exited_instances(&store)?;
            if store.ids()?.len() >= MAX_INSTANCES {
                return Err(refusal(ErrorCode::ResourceLimit, LOCAL_IPC_INSTANCES));
            }
            listener = platform::bind(&endpoint)?;
            if let Err(error) = store.write(&record.instance_id, &record) {
                remove_endpoint(&endpoint);
                return Err(error);
            }
        }
        own_instances().push(OwnInstance {
            store: store.clone(),
            record: record.clone(),
            endpoint: endpoint.clone(),
        });
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(Notify::new());
        let task_store = store.clone();
        let task_record = record.clone();
        let task_stop = stop.clone();
        let task = tokio::spawn(async move {
            let capacity = Arc::new(Semaphore::new(64));
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = task_stop.notified() => break,
                    accepted = platform::accept(&mut listener) => {
                        let Ok((stream, peer)) = accepted else { continue; };
                        let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                        let store = task_store.clone(); let record = task_record.clone(); let router = router.clone();
                        clients.spawn(async move { let _permit = permit; let _ = serve(stream, peer, store, record, router).await; });
                    },
                    _ = clients.join_next(), if !clients.is_empty() => {}
                }
            }
            clients.abort_all();
            while clients.join_next().await.is_some() {}
        });
        Ok(Self {
            store,
            record,
            endpoint,
            stopped,
            stop,
            task: Some(task),
        })
    }
    pub fn instance_id(&self) -> &str {
        &self.record.instance_id
    }
    pub async fn discover(&self, kind: EndpointKind) -> ServiceResult<Vec<InstanceMetadata>> {
        let mut instances = Vec::new();
        let mut probes = tokio::task::JoinSet::new();
        for id in self.store.ids()? {
            let Ok(Some(record)) = self.store.read::<Record>(&id) else {
                continue;
            };
            if probes.len() >= 64 {
                return Err(failure(ErrorCode::ResourceLimit));
            }
            if record.kind == kind && record.instance_id == id && record.validate().is_ok() {
                let root = self.store.root().to_owned();
                let client = self.record.clone();
                let stopped = self.stopped.clone();
                probes.spawn(async move {
                    if exchange_at(
                        &root,
                        &client,
                        &record,
                        &stopped,
                        Call::Probe,
                        Duration::from_secs(2),
                    )
                    .await
                    .is_ok()
                    {
                        Some(record.metadata())
                    } else {
                        None
                    }
                });
            }
        }
        while let Some(result) = probes.join_next().await {
            if let Ok(Some(instance)) = result {
                instances.push(instance);
            }
        }
        instances.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
        Ok(instances)
    }
    pub async fn call_owner(
        &self,
        owner_id: &str,
        request: ServiceRequest,
    ) -> ServiceResult<ServiceResponse> {
        let record = self.record_for(owner_id)?;
        let timeout = owner_call_timeout(request.deadline_ms);
        let request_id = request.request_id.clone();
        let connection_id = request.connection_id.clone();
        match self
            .exchange(&record, Call::Service(request), timeout)
            .await?
        {
            Reply::Service(response)
                if response.protocol_version == 1
                    && response.request_id == request_id
                    && response.connection_id == connection_id =>
            {
                Ok(response)
            }
            _ => {
                let mut error = failure(ErrorCode::OutcomeUnknown);
                error.outcome = super::super::contracts::Outcome::Unknown;
                Err(error)
            }
        }
    }
    pub async fn apply_map(
        &self,
        gui_id: &str,
        request: GuiApplyRequest,
    ) -> ServiceResult<GuiApplyAck> {
        validate_apply_request(&request)?;
        let record = match self.record_for(gui_id) {
            Ok(record) if record.kind == EndpointKind::Gui => record,
            _ => return Err(self.gui_unavailable().await?),
        };
        let ack = match self
            .exchange(
                &record,
                Call::ApplyMap(request.clone()),
                Duration::from_secs(5),
            )
            .await
        {
            Ok(Reply::ApplyMap(ack)) => ack?,
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::DeadlineExceeded | ErrorCode::OutcomeUnknown
                ) =>
            {
                return Err(apply_timeout(&request))
            }
            Err(error) if error.code == ErrorCode::GuiUnavailable => {
                return Err(self.gui_unavailable().await?)
            }
            Err(error) => return Err(error),
            _ => return Err(apply_timeout(&request)),
        };
        validate_ack(&request, &ack)?;
        Ok(ack)
    }
    pub async fn gui_unavailable(&self) -> ServiceResult<ServiceError> {
        let mut error = failure(ErrorCode::GuiUnavailable);
        error.details = serde_json::json!({"instances":self.discover(EndpointKind::Gui).await?});
        Ok(error)
    }
    fn record_for(&self, id: &str) -> ServiceResult<Record> {
        opaque_id(id)?;
        let record = self
            .store
            .read::<Record>(id)?
            .ok_or_else(|| failure(ErrorCode::GuiUnavailable))?;
        record.validate()?;
        if record.instance_id != id {
            return Err(failure(ErrorCode::GuiUnavailable));
        }
        Ok(record)
    }
    async fn exchange(
        &self,
        record: &Record,
        call: Call,
        deadline: Duration,
    ) -> ServiceResult<Reply> {
        exchange_at(
            self.store.root(),
            &self.record,
            record,
            &self.stopped,
            call,
            deadline,
        )
        .await
    }
    pub async fn shutdown(&mut self) -> ServiceResult<()> {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            self.stop.notify_one();
            if let Some(task) = self.task.take() {
                task.await.map_err(|_| failure(ErrorCode::DriverExited))?;
            }
            self.cleanup()?;
        }
        Ok(())
    }
    fn cleanup(&self) -> ServiceResult<()> {
        own_instances().retain(|own| own.record.start_nonce != self.record.start_nonce);
        remove_own_record(&self.store, &self.record, &self.endpoint)
    }
}

/// The most instance records one profile's `local-ipc` directory holds.
const MAX_INSTANCES: usize = 64;

/// An instance this process started. A signal handler that exits without
/// unwinding uses it to remove the record and endpoint `Drop` would have.
struct OwnInstance {
    store: Arc<PrivateStore>,
    record: Record,
    endpoint: PathBuf,
}

fn own_instances() -> std::sync::MutexGuard<'static, Vec<OwnInstance>> {
    static OWN: std::sync::Mutex<Vec<OwnInstance>> = std::sync::Mutex::new(Vec::new());
    OWN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Removes the records and endpoints of every instance this process started.
/// For a process about to call `exit`, which skips `Drop`.
pub fn remove_own_instances() {
    remove_own_instances_where(|_| true);
}

/// [`remove_own_instances`] for the instances whose store is `root` only.
#[cfg(test)]
pub(super) fn remove_own_instances_in(root: &Path) {
    remove_own_instances_where(|own| own.store.root() == root);
}

fn remove_own_instances_where(selected: impl Fn(&OwnInstance) -> bool) {
    let removed: Vec<OwnInstance> = {
        let mut own = own_instances();
        let (removed, kept) = std::mem::take(&mut *own).into_iter().partition(&selected);
        *own = kept;
        removed
    };
    for own in removed {
        let _ = remove_own_record(&own.store, &own.record, &own.endpoint);
    }
}

/// Removes `record` and its endpoint, unless another start replaced it.
fn remove_own_record(store: &PrivateStore, record: &Record, endpoint: &Path) -> ServiceResult<()> {
    let _lock = store.lock()?;
    if store
        .read::<Record>(&record.instance_id)?
        .is_some_and(|stored| stored.start_nonce == record.start_nonce)
    {
        store.remove(&record.instance_id)?;
        remove_endpoint(endpoint);
    }
    Ok(())
}

/// Removes the records, and their endpoints, of instances whose process has
/// exited or whose endpoint no longer accepts connections: a process killed
/// by a signal or a crash never removes its own. Runs under the store lock.
fn reap_exited_instances(store: &PrivateStore) -> ServiceResult<()> {
    for id in store.ids()? {
        let Ok(Some(record)) = store.read::<Record>(&id) else {
            continue;
        };
        if record.instance_id != id || record.validate().is_err() {
            continue;
        }
        let endpoint = endpoint_path(store.root(), &record);
        if !platform::instance_alive(record.process_id, &endpoint) {
            store.remove(&id)?;
            remove_endpoint(&endpoint);
        }
    }
    Ok(())
}
impl Drop for LocalIpc {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.stop.notify_one();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let _ = self.cleanup();
    }
}

async fn exchange_at(
    root: &Path,
    client: &Record,
    record: &Record,
    stopped: &AtomicBool,
    call: Call,
    deadline: Duration,
) -> ServiceResult<Reply> {
    if stopped.load(Ordering::Acquire) {
        return Err(failure(ErrorCode::GuiUnavailable));
    }
    let dispatched = AtomicBool::new(false);
    let result = tokio::time::timeout(deadline, async {
        let mut stream = tokio::time::timeout(
            Duration::from_secs(2),
            platform::connect(&endpoint_path(root, record), record.process_id),
        )
        .await
        .map_err(|_| failure(ErrorCode::GuiUnavailable))??;
        let hello = Hello {
            protocol_version: 1,
            target_nonce: record.start_nonce.clone(),
            client_instance: client.instance_id.clone(),
            client_nonce: client.start_nonce.clone(),
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            write_frame(&mut stream, &hello).await?;
            let returned: Record = read_frame(&mut stream).await?;
            if returned.start_nonce != record.start_nonce
                || returned.instance_id != record.instance_id
                || returned.process_id != record.process_id
            {
                return Err(failure(ErrorCode::GuiUnavailable));
            }
            Ok(())
        })
        .await
        .map_err(|_| failure(ErrorCode::GuiUnavailable))??;
        let encoded = encode_frame(&call)?;
        dispatched.store(true, Ordering::Release);
        write_encoded(&mut stream, &encoded).await?;
        read_frame(&mut stream).await
    })
    .await
    .map_err(|_| failure(ErrorCode::DeadlineExceeded))
    .and_then(|result| result);
    match result {
        Err(_) if dispatched.load(Ordering::Acquire) => {
            let mut error = failure(ErrorCode::OutcomeUnknown);
            error.outcome = super::super::contracts::Outcome::Unknown;
            Err(error)
        }
        other => other,
    }
}

async fn serve(
    mut stream: platform::Stream,
    peer: platform::VerifiedPeer,
    store: Arc<PrivateStore>,
    record: Record,
    router: Arc<dyn LocalIpcRouter>,
) -> ServiceResult<()> {
    let caller = tokio::time::timeout(Duration::from_secs(2), async {
        let hello: Hello = read_frame(&mut stream).await?;
        if hello.protocol_version != 1 || hello.target_nonce != record.start_nonce {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let client = store
            .read::<Record>(&hello.client_instance)?
            .ok_or_else(|| failure(ErrorCode::PermissionDenied))?;
        client.validate()?;
        if client.process_id != peer.process_id
            || client.instance_id != hello.client_instance
            || client.start_nonce != hello.client_nonce
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        write_frame(&mut stream, &record).await?;
        Ok(CallerContext {
            transport: CallerTransport::LocalIpc,
            caller_id: format!("local_user:{}", peer.user_identity),
            owner_instance_id: record.instance_id.clone(),
        })
    })
    .await
    .map_err(|_| failure(ErrorCode::PermissionDenied))??;
    let call: Call = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut stream))
        .await
        .map_err(|_| failure(ErrorCode::DeadlineExceeded))??;
    let reply = match call {
        Call::Probe => Reply::Probe,
        Call::Service(request) => {
            let deadline = served_deadline(request.deadline_ms);
            let response = tokio::time::timeout(deadline, router.call(request.clone(), caller))
                .await
                .unwrap_or_else(|_| {
                    let mut response = ServiceResponse::failure(
                        &request,
                        ErrorCode::OutcomeUnknown,
                        "The service outcome could not be confirmed.",
                    );
                    if let Some(error) = response.error.as_mut() {
                        error.outcome = super::super::contracts::Outcome::Unknown;
                    }
                    response.status = super::super::contracts::JobStatus::OutcomeUnknown;
                    response
                });
            Reply::Service(response)
        }
        Call::ApplyMap(request) => {
            if record.kind != EndpointKind::Gui {
                return Err(failure(ErrorCode::GuiUnavailable));
            }
            validate_apply_request(&request)?;
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                router.apply_map(request.clone(), caller),
            )
            .await
            .unwrap_or_else(|_| Err(apply_timeout(&request)));
            Reply::ApplyMap(result)
        }
    };
    tokio::time::timeout(Duration::from_secs(2), write_frame(&mut stream, &reply))
        .await
        .map_err(|_| failure(ErrorCode::DeadlineExceeded))?
}
fn endpoint_path(root: &Path, record: &Record) -> PathBuf {
    #[cfg(unix)]
    {
        root.join(format!(
            "{}.sock",
            &super::super::security::digest(record.start_nonce.as_bytes())[..20]
        ))
    }
    #[cfg(windows)]
    {
        PathBuf::from(format!(
            r"\\.\pipe\tabularis-{}-{}",
            &super::super::security::digest(root.to_string_lossy().as_bytes())[..16],
            record.start_nonce
        ))
    }
}
fn remove_endpoint(path: &Path) {
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(path);
    }
    #[cfg(windows)]
    {
        let _ = path;
    }
}
