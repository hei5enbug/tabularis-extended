use super::{
    auth::{AuthLifecycle, AuthManager, REFRESH_MARGIN_MS},
    backend::{
        ConnectionBackend, DriverBackend, DriverContext, DriverOutput, PermissionScope,
        PolicyBackend, ReadOnlyGuarantee, SavedConnection, SessionFinish, TransientAuth,
    },
    contracts::{
        ErrorCode, JobStatus, Limits, Metrics, Operation, Outcome, Page, ServiceRequest,
        ServiceResponse, MAX_DEADLINE_MS,
    },
    credentials::{SecretInputs, SecretSlot, SecretStore, SessionSecretStore},
    dispatch::{CallerContext, CallerTransport, ServiceDispatcher, ServiceHandler},
    error::{
        failure, redact_error, refusal, ServiceResult, ALLOW_WRITES_GUI_ONLY, CONNECTION_READ_ONLY,
    },
    history::{HistoryPage, HistoryQuery, HistoryStore},
    jobs::{JobLease, JobManager},
    oauth::OAuthProvider,
    policy::{ApprovalMode, ChangeAnnotations},
    results::{CursorBinding, NextPage, ResultStore, StoredPage},
    security::{sanitized_settings, validate_settings, value_hash},
    sessions::SessionManager,
    sql_classifier::{prove_read, read_dialect, ReadDialect},
    storage::now_ms,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    time::Duration,
};

/// How long a request waits after its deadline for the driver to answer the
/// cancellation it was asked to perform.
pub const CANCEL_GRACE_MS: u64 = 5_000;
/// How long before a driver would reject its Entra token (`REFRESH_MARGIN_MS`
/// before expiry) the host hands it a newer one during a running call.
pub const AUTH_REFRESH_LEAD_MS: u64 = 60_000;
/// How long a failed token refresh waits before it tries again.
const AUTH_REFRESH_RETRY_MS: u64 = 15_000;
/// The deadline of one token refresh call to the driver.
const AUTH_REFRESH_CALL_MS: u64 = 30_000;

pub const SERVICE_OPERATIONS: &[Operation] = &[
    Operation::ConnectionList,
    Operation::ConnectionCreate,
    Operation::ConnectionUpdate,
    Operation::ConnectionDelete,
    Operation::ConnectionTest,
    Operation::CatalogDatabases,
    Operation::CatalogSchemas,
    Operation::CatalogObjects,
    Operation::CatalogDescribe,
    Operation::QueryExecute,
    Operation::QueryNext,
    Operation::QueryExplain,
    Operation::ResultGet,
    Operation::ResultRelease,
    Operation::RowInsert,
    Operation::RowUpdate,
    Operation::RowDelete,
    Operation::DocumentRead,
    Operation::DocumentCreate,
    Operation::DocumentReplace,
    Operation::DocumentDelete,
    Operation::SessionOpen,
    Operation::SessionCommit,
    Operation::SessionRollback,
    Operation::SessionClose,
    Operation::JobGet,
    Operation::JobResult,
    Operation::JobCancel,
    Operation::AuthBegin,
    Operation::AuthStatus,
    Operation::AuthCancel,
    Operation::AuthLogout,
    Operation::CredentialImport,
];

pub struct ServiceRuntime {
    pub connections: Arc<dyn ConnectionBackend>,
    pub policy: Arc<dyn PolicyBackend>,
    pub driver: Arc<dyn DriverBackend>,
    pub results: Arc<ResultStore>,
    pub jobs: Arc<JobManager>,
    pub sessions: Arc<SessionManager>,
    pub auth: Arc<AuthManager>,
    pub secret_inputs: Arc<SecretInputs>,
    history: std::sync::OnceLock<Arc<dyn HistoryStore>>,
    validator: super::contracts::SchemaValidator,
    tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    stopping: AtomicBool,
    extensions: Mutex<HashMap<Operation, Arc<dyn super::extensions::OperationExtension>>>,
    me: Weak<ServiceRuntime>,
    cancel_grace_ms: AtomicU64,
}

struct RuntimeAuthLifecycle {
    connections: Arc<dyn ConnectionBackend>,
    driver: Arc<dyn DriverBackend>,
    results: Arc<ResultStore>,
    sessions: Arc<SessionManager>,
}
#[async_trait]
impl AuthLifecycle for RuntimeAuthLifecycle {
    async fn principal_changed(
        &self,
        connection_id: &str,
        previous: Option<&str>,
        _current: &str,
    ) -> ServiceResult<()> {
        self.invalidate(connection_id, previous).await
    }
    async fn logout(&self, connection_id: &str, principal: Option<&str>) -> ServiceResult<()> {
        self.invalidate(connection_id, principal).await
    }
}
impl RuntimeAuthLifecycle {
    async fn invalidate(&self, connection_id: &str, principal: Option<&str>) -> ServiceResult<()> {
        let revision = match self.connections.rotate_auth_revision(connection_id).await {
            Ok(revision) => revision,
            Err(error) if error.code == ErrorCode::ConnectionNotFound => {
                uuid::Uuid::new_v4().to_string()
            }
            Err(error) => return Err(error),
        };
        self.results
            .invalidate_connection_revision(connection_id, &revision)?;
        self.driver.invalidate_auth(connection_id, principal).await;
        self.sessions
            .invalidate_connection(
                connection_id,
                DriverContext {
                    connection_id: connection_id.into(),
                    request_id: format!("invalidate-{}", uuid::Uuid::new_v4()),
                    deadline_ms: 30_000,
                    signal: Default::default(),
                    transient_auth: None,
                    read_only: true,
                    session_handle: None,
                },
            )
            .await
    }
}

impl ServiceRuntime {
    pub fn new(
        profile: PathBuf,
        instance_id: String,
        connections: Arc<dyn ConnectionBackend>,
        policy: Arc<dyn PolicyBackend>,
        driver: Arc<dyn DriverBackend>,
        secrets: Arc<dyn SecretStore>,
        oauth: Arc<dyn OAuthProvider>,
    ) -> ServiceResult<Arc<Self>> {
        let results = Arc::new(ResultStore::new(profile.join("results-v1"))?);
        let jobs = JobManager::new(instance_id, profile.join("jobs-v1"))?;
        let sessions = Arc::new(SessionManager::new(driver.clone()));
        let lifecycle = Arc::new(RuntimeAuthLifecycle {
            connections: connections.clone(),
            driver: driver.clone(),
            results: results.clone(),
            sessions: sessions.clone(),
        });
        let auth = AuthManager::new(
            oauth,
            secrets,
            Arc::new(SessionSecretStore::default()),
            lifecycle,
        );
        let validator = super::contracts::SchemaValidator::new()
            .map_err(|_| failure(ErrorCode::InvalidArgument))?;
        let runtime = Arc::new_cyclic(|me| Self {
            connections,
            policy,
            driver,
            results,
            jobs,
            sessions,
            auth,
            secret_inputs: Arc::new(SecretInputs::default()),
            history: std::sync::OnceLock::new(),
            validator,
            tasks: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
            extensions: Mutex::new(HashMap::new()),
            me: me.clone(),
            cancel_grace_ms: AtomicU64::new(CANCEL_GRACE_MS),
        });
        runtime.start_maintenance()?;
        Ok(runtime)
    }
    /// Running and recent jobs with persisted history, newest first. Only
    /// jobs this process owns are marked cancellable.
    pub fn job_history(&self, query: &HistoryQuery) -> ServiceResult<HistoryPage> {
        self.history
            .get()
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?
            .list(query, &self.jobs.live_jobs())
    }
    /// Shortens the wait for a driver to answer a cancellation after a
    /// deadline. Meant for fixtures; the default is `CANCEL_GRACE_MS`.
    pub fn set_cancel_grace_ms(&self, grace_ms: u64) {
        self.cancel_grace_ms.store(grace_ms, Ordering::SeqCst);
    }
    fn cancel_grace(&self) -> Duration {
        Duration::from_millis(self.cancel_grace_ms.load(Ordering::SeqCst))
    }
    /// Records every job from now on and serves `job_history`.
    pub fn set_history(&self, history: Arc<dyn HistoryStore>) {
        self.jobs.set_history(history.clone());
        let _ = self.history.set(history);
    }
    pub fn register(self: &Arc<Self>, dispatcher: &mut ServiceDispatcher) {
        for operation in SERVICE_OPERATIONS {
            dispatcher.register(*operation, self.clone());
        }
        if let Ok(extensions) = self.extensions.lock() {
            for operation in extensions.keys() {
                dispatcher.register(*operation, self.clone());
            }
        }
    }
    pub fn register_extension(
        self: &Arc<Self>,
        extension: Arc<dyn super::extensions::OperationExtension>,
    ) -> ServiceResult<()> {
        let operations = extension.operations();
        if operations.is_empty()
            || operations
                .iter()
                .any(|operation| !super::extensions::EXTENSION_OPERATIONS.contains(operation))
            || operations
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != operations.len()
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let mut extensions = self
            .extensions
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        if operations
            .iter()
            .any(|operation| extensions.contains_key(operation))
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        for operation in operations {
            extensions.insert(*operation, extension.clone());
        }
        Ok(())
    }
    async fn connection(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
    ) -> ServiceResult<(SavedConnection, PermissionScope)> {
        let connection = self
            .connections
            .get(
                request
                    .connection_id
                    .as_deref()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
            )
            .await?;
        let permission = self.policy.permission(&connection, caller).await?;
        if permission.identity.is_empty() || permission.identity.len() > 512 {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        Ok((connection, permission))
    }
    /// A write needs the caller's explicit `write: true` on the MCP surface,
    /// which reaches the policy as a registered intent.
    async fn require_write_intent(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
    ) -> ServiceResult<()> {
        let intent = self.policy.query_is_write(caller, request).await?;
        if caller.transport == CallerTransport::Mcp && !intent {
            return Err(failure(ErrorCode::WriteIntentRequired));
        }
        Ok(())
    }
    async fn is_write(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
    ) -> ServiceResult<bool> {
        Ok(match request.operation {
            Operation::RowInsert
            | Operation::RowUpdate
            | Operation::RowDelete
            | Operation::DocumentCreate
            | Operation::DocumentReplace
            | Operation::DocumentDelete => {
                self.require_write_intent(request, caller).await?;
                true
            }
            Operation::QueryExecute => {
                let intent = self.policy.query_is_write(caller, request).await?;
                let session_write = match &request.session_id {
                    Some(id) => {
                        self.sessions
                            .is_write(
                                id,
                                request
                                    .connection_id
                                    .as_deref()
                                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
                                caller,
                            )
                            .await?
                    }
                    None => false,
                };
                let text = request.input["text"].as_str().unwrap_or("");
                let in_session = request.session_id.is_some();
                let read = if is_proven_read(text, None, in_session) {
                    true
                } else {
                    let driver = match request.connection_id.as_deref() {
                        Some(id) => Some(self.connections.get(id).await?.driver),
                        None => None,
                    };
                    is_proven_read(text, driver.as_deref(), in_session)
                };
                match (session_write, read, intent) {
                    (true, _, true) => true,
                    (true, true, false) => false,
                    (true, false, false) => return Err(failure(ErrorCode::WriteIntentRequired)),
                    (false, true, false) => false,
                    (false, true, true) => return Err(failure(ErrorCode::InvalidArgument)),
                    (false, false, true) => true,
                    (false, false, false) => return Err(failure(ErrorCode::WriteIntentRequired)),
                }
            }
            Operation::SessionOpen => request.input["mode"] == "write",
            Operation::SessionCommit => {
                let write = self
                    .sessions
                    .is_write(
                        request
                            .session_id
                            .as_deref()
                            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
                        request
                            .connection_id
                            .as_deref()
                            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
                        caller,
                    )
                    .await?;
                if write {
                    self.require_write_intent(request, caller).await?;
                }
                write
            }
            _ => false,
        })
    }
    async fn effective_allow_writes(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
    ) -> bool {
        self.policy
            .permission(connection, caller)
            .await
            .map_or(false, |permission| permission.allow_writes)
    }
    /// `approval_mode` decides for MCP callers; every other caller approves
    /// writes only.
    async fn needs_approval(
        &self,
        request: &ServiceRequest,
        connection: &SavedConnection,
        caller: &CallerContext,
        is_write: bool,
    ) -> ServiceResult<bool> {
        if caller.transport != CallerTransport::Mcp {
            return Ok(is_write);
        }
        Ok(match self.policy.approval_mode(connection, caller).await? {
            ApprovalMode::Off => false,
            ApprovalMode::WritesOnly => is_write,
            ApprovalMode::All => is_write || request.operation == Operation::QueryExecute,
        })
    }
    /// A read-only connection rejects every write before it is queued.
    async fn write_gate(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        is_write: bool,
    ) -> ServiceResult<()> {
        if !is_write {
            return Ok(());
        }
        if let Ok((_, permission)) = self.connection(request, caller).await {
            if !permission.allow_writes {
                return Err(refusal(ErrorCode::WriteNotAllowed, CONNECTION_READ_ONLY));
            }
        }
        Ok(())
    }
    async fn execute(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
        is_write: bool,
    ) -> ServiceResult<ServiceResponse> {
        if lease.signal().is_cancelled() {
            return Err(failure(ErrorCode::Cancelled));
        }
        if now_ms() >= lease.deadline_at_ms() {
            return Err(failure(ErrorCode::DeadlineExceeded));
        }
        let extension = self
            .extensions
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .get(&request.operation)
            .cloned();
        if let Some(extension) = extension {
            let response = extension.execute(request, caller, lease).await?;
            if response.protocol_version != 1
                || response.request_id != request.request_id
                || response.connection_id != request.connection_id
                || !matches!(
                    response.status,
                    JobStatus::Succeeded
                        | JobStatus::Failed
                        | JobStatus::Cancelled
                        | JobStatus::OutcomeUnknown
                )
                || self
                    .validator
                    .validate_response(
                        &serde_json::to_value(&response)
                            .map_err(|_| failure(ErrorCode::InvalidArgument))?,
                    )
                    .is_err()
            {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            return Ok(response);
        }
        match request.operation {
            Operation::ConnectionList => {
                let mut list = Vec::new();
                for connection in self.connections.list().await? {
                    if let Ok(permission) = self.policy.permission(&connection, caller).await {
                        list.push(public_connection(&connection, permission.allow_writes));
                    }
                }
                return Ok(success(request, json!({"connections":list})));
            }
            Operation::ConnectionCreate => {
                before_publication(
                    lease,
                    self.policy.authorize_connection_change(caller, request),
                )
                .await?;
                validate_settings(&request.input["settings"])?;
                validate_connection_configuration(
                    string(&request.input, "driver")?,
                    &request.input["settings"],
                )?;
                self.connections
                    .validate_settings(
                        string(&request.input, "driver")?,
                        &request.input["settings"],
                    )
                    .await?;
                let connection = self
                    .connections
                    .create_guarded(
                        string(&request.input, "name")?,
                        string(&request.input, "driver")?,
                        request.input["settings"].clone(),
                        request.input["credential_ref"].as_str().map(str::to_owned),
                        lease,
                    )
                    .await?;
                lease.mark_write_confirmed()?;
                let allow_writes = self.effective_allow_writes(&connection, caller).await;
                return Ok(success(
                    request,
                    public_connection(&connection, allow_writes),
                ));
            }
            Operation::JobGet | Operation::JobResult | Operation::JobCancel => {
                let job_id = string(&request.input, "job_id")?;
                let owner = string(&request.input, "owner_instance_id")?;
                if owner != self.jobs.instance_id() {
                    return Err(failure(ErrorCode::CapabilityUnavailable));
                }
                let mut record = self.jobs.get(job_id, owner, caller)?;
                if let Some(connection_id) = &record.connection_id {
                    let connection = self.connections.get(connection_id).await?;
                    self.policy.permission(&connection, caller).await?;
                }
                let mut cancel_unsent = false;
                if request.operation == Operation::JobCancel {
                    record = self.jobs.cancel(job_id, owner, caller)?;
                    if record.cancel_requested && record.cancel_state == "requested" {
                        if let Some(id) = &record.connection_id {
                            let sent = tokio::time::timeout(
                                Duration::from_secs(5),
                                self.driver.cancel(id, &record.request_id),
                            )
                            .await;
                            if !matches!(sent, Ok(Ok(()))) {
                                self.jobs.mark_cancel_not_confirmed(job_id);
                                record.cancel_state = "not_confirmed".into();
                                cancel_unsent = true;
                            }
                        }
                    }
                }
                if request.operation == Operation::JobResult {
                    return Ok(success(
                        request,
                        json!({"status":record.status,"response":record.response}),
                    ));
                }
                let mut response = success(
                    request,
                    json!({"job_id":record.job_id,"status":record.status,"cancel_requested":record.cancel_requested,"owner_instance_id":record.owner_instance_id,"awaiting_approval":self.jobs.awaiting_approval(job_id),"cancel_state":if record.cancel_state.is_empty(){Value::Null}else{json!(record.cancel_state)},"cancel_code":if record.cancel_state=="not_confirmed"{json!("CANCEL_NOT_CONFIRMED")}else{Value::Null}}),
                );
                if cancel_unsent {
                    response.status = JobStatus::Failed;
                    response.error = Some(failure(ErrorCode::CancelNotConfirmed));
                }
                return Ok(response);
            }
            _ => {}
        }
        let (connection, permission) = self.connection(request, caller).await?;
        if is_write && !permission.allow_writes {
            return Err(refusal(ErrorCode::WriteNotAllowed, CONNECTION_READ_ONLY));
        }
        if self
            .needs_approval(request, &connection, caller, is_write)
            .await?
        {
            let hash = value_hash(
                &serde_json::to_value(request).map_err(|_| failure(ErrorCode::InvalidArgument))?,
            );
            let waited_from = now_ms();
            lease.begin_approval();
            let approved = if is_write {
                await_approval(
                    lease,
                    self.policy
                        .consume_write_approval(&connection, caller, request, &hash),
                )
                .await
            } else {
                await_approval(
                    lease,
                    self.policy
                        .consume_read_approval(&connection, caller, request, &hash),
                )
                .await
            };
            lease.end_approval(now_ms().saturating_sub(waited_from));
            approved?;
            self.recheck_after_approval(request, caller, &connection, is_write)
                .await?;
        }
        // A proven read in a write session skips write intent and approval,
        // but the driver still runs it in the session's write transaction.
        let write_session_read = !is_write
            && request.operation == Operation::QueryExecute
            && match &request.session_id {
                Some(id) => self.sessions.is_write(id, &connection.id, caller).await?,
                None => false,
            };
        let mut context = DriverContext {
            connection_id: connection.id.clone(),
            request_id: request.request_id.clone(),
            deadline_ms: lease.deadline_at_ms().saturating_sub(now_ms()),
            signal: lease.signal(),
            transient_auth: None,
            read_only: !is_write && !write_session_read,
            session_handle: None,
        };
        match request.operation {
            Operation::ConnectionUpdate => {
                validate_settings(&request.input["patch"])?;
                let patch = &request.input["patch"];
                if patch.as_object().is_none_or(|patch| {
                    patch.keys().any(|key| {
                        !matches!(
                            key.as_str(),
                            "name" | "settings" | "credential_ref" | "allow_writes"
                        )
                    })
                }) {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
                if caller.transport == CallerTransport::Mcp && patch.get("allow_writes").is_some() {
                    return Err(refusal(ErrorCode::InvalidArgument, ALLOW_WRITES_GUI_ONLY));
                }
                let expected_version = request
                    .expected_version
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
                let settings = patch.get("settings").unwrap_or(&connection.settings);
                validate_connection_configuration(&connection.driver, settings)?;
                self.connections
                    .validate_settings(&connection.driver, settings)
                    .await?;
                let credential_retarget = match patch.get("settings") {
                    Some(settings)
                        if super::security::credential_target_changed(
                            &connection.settings,
                            settings,
                        ) =>
                    {
                        self.connections.has_stored_secrets(&connection.id).await?
                    }
                    _ => false,
                };
                before_publication(
                    lease,
                    self.policy.authorize_connection_change_annotated(
                        caller,
                        request,
                        ChangeAnnotations {
                            credential_retarget,
                        },
                    ),
                )
                .await?;
                let updated = self
                    .connections
                    .update_guarded(&connection.id, expected_version, patch.clone(), lease)
                    .await?;
                lease.mark_write_confirmed()?;
                if patch.get("settings").is_some()
                    || patch.get("credential_ref").is_some()
                    || patch.get("allow_writes").is_some()
                {
                    self.auth.invalidate(&connection.id, false).await?;
                }
                let allow_writes = self.effective_allow_writes(&updated, caller).await;
                return Ok(success(request, public_connection(&updated, allow_writes)));
            }
            Operation::ConnectionDelete => {
                before_publication(
                    lease,
                    self.policy.authorize_connection_change(caller, request),
                )
                .await?;
                lease.mark_write_dispatched()?;
                self.connections
                    .delete(
                        &connection.id,
                        request
                            .expected_version
                            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
                    )
                    .await?;
                lease.mark_write_confirmed()?;
                self.auth.logout(&connection.id).await?;
                return Ok(success(request, json!({"deleted":true})));
            }
            Operation::ResultGet | Operation::ResultRelease => {
                let id = string(&request.input, "result_id")?;
                let binding = self.results.result_binding(id)?;
                authorize_binding(&binding, &connection, &permission)?;
                if request.operation == Operation::ResultRelease {
                    self.results.release(id, &binding)?;
                    return Ok(success(request, json!({"released":true})));
                }
                return Ok(from_page(
                    request,
                    self.results.page(
                        id,
                        &binding,
                        integer(&request.input, "result_set_index")?,
                        integer(&request.input, "offset")?,
                        integer(&request.input, "limit")?,
                    )?,
                ));
            }
            Operation::CredentialImport => {
                before_publication(
                    lease,
                    self.policy.authorize_connection_change(caller, request),
                )
                .await?;
                let slot = SecretSlot::parse(string(&request.input, "slot")?)?;
                let secret = self.secret_inputs.take_for_connection(
                    caller,
                    &request.request_id,
                    &request.input["input_channel"],
                    &connection.id,
                )?;
                lease.mark_write_dispatched()?;
                self.auth
                    .invalidate(&connection.id, false)
                    .await
                    .map_err(|mut error| {
                        if error.outcome != Outcome::Applied {
                            error.outcome = Outcome::Unknown;
                        }
                        error
                    })?;
                lease.mark_write_confirmed()?;
                if slot == SecretSlot::EntraRefreshToken {
                    let current = self.connections.get(&connection.id).await?;
                    self.auth
                        .import_refresh(
                            &current,
                            secret,
                            super::credentials::SecretPersistence::parse(
                                request.input["persistence"].as_str(),
                            )?,
                            context.deadline_ms,
                        )
                        .await?;
                } else {
                    let store = if request.input["persistence"] == "session" {
                        self.auth.session_store()
                    } else {
                        self.auth.persistent_store()
                    };
                    store.set(&connection.id, slot, secret).await?;
                }
                self.driver.invalidate_auth(&connection.id, None).await;
                self.results.invalidate_connection(&connection.id)?;
                return Ok(success(
                    request,
                    json!({"credential_ref":format!("{}:{}",connection.id,slot.name())}),
                ));
            }
            Operation::AuthBegin => {
                return Ok(success(
                    request,
                    tokio::time::timeout(
                        Duration::from_millis(context.deadline_ms),
                        self.auth.begin(
                            &connection,
                            string(&request.input, "auth_mode")?,
                            caller,
                            super::credentials::SecretPersistence::parse(
                                request.input["persistence"].as_str(),
                            )?,
                        ),
                    )
                    .await
                    .map_err(|_| failure(ErrorCode::DeadlineExceeded))??,
                ))
            }
            Operation::AuthStatus => {
                return Ok(success(
                    request,
                    self.auth.status(
                        &connection.id,
                        string(&request.input, "auth_job_id")?,
                        caller,
                    )?,
                ))
            }
            Operation::AuthCancel => {
                return Ok(success(
                    request,
                    self.auth
                        .cancel(
                            &connection.id,
                            string(&request.input, "auth_job_id")?,
                            caller,
                        )
                        .await?,
                ))
            }
            Operation::AuthLogout => {
                self.auth.logout(&connection.id).await?;
                return Ok(success(request, json!({"logged_out":true})));
            }
            Operation::QueryNext => {
                return self
                    .next_page(request, &connection, &permission, caller, context, lease)
                    .await
            }
            Operation::SessionOpen if connection.driver == "cosmos-nosql" => {
                return Err(failure(ErrorCode::UnsupportedOperation))
            }
            _ => {}
        }
        if !self.driver.supports(&connection, request.operation).await? {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
        let guarantee = if is_write || write_session_read {
            "not_applicable"
        } else {
            let guarantee = self.driver.read_only_guarantee(&connection).await?;
            if guarantee == ReadOnlyGuarantee::Unavailable
                && request.operation == Operation::QueryExecute
                && !is_proven_read(
                    request.input["text"].as_str().unwrap_or(""),
                    Some(&connection.driver),
                    request.session_id.is_some(),
                )
            {
                return Err(failure(ErrorCode::WriteNotAllowed));
            }
            guarantee_label(guarantee)
        };
        context.transient_auth = self.auth.context(&connection, context.deadline_ms).await?;
        context.deadline_ms = lease.deadline_at_ms().saturating_sub(now_ms());
        if context.deadline_ms == 0 {
            return Err(failure(ErrorCode::DeadlineExceeded));
        }
        match request.operation {
            Operation::SessionOpen => {
                if is_write {
                    lease.mark_write_dispatched()?;
                }
                let session_id = self
                    .sessions
                    .open(
                        &connection,
                        string(&request.input, "database")?,
                        !is_write,
                        caller,
                        context,
                    )
                    .await?;
                if let Err(error) = self.revalidate(&connection, &permission, caller).await {
                    self.sessions.discard(&session_id, &connection.id).await?;
                    return Err(error);
                }
                Ok(success(request, json!({"session_id":session_id})))
            }
            Operation::SessionCommit | Operation::SessionRollback | Operation::SessionClose => {
                if is_write {
                    lease.mark_write_dispatched()?;
                }
                let finish = if request.operation == Operation::SessionCommit {
                    SessionFinish::Commit
                } else {
                    SessionFinish::Rollback
                };
                self.sessions
                    .finish(
                        request
                            .session_id
                            .as_deref()
                            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
                        &connection,
                        caller,
                        finish,
                        context,
                    )
                    .await?;
                if is_write {
                    lease.mark_write_confirmed()?;
                }
                Ok(success(
                    request,
                    json!({"closed":true,"rollback_confirmed":finish==SessionFinish::Rollback}),
                ))
            }
            _ => {
                let session = if let Some(id) = &request.session_id {
                    Some(
                        self.sessions
                            .acquire(id, &connection, caller, is_write, &context)
                            .await?,
                    )
                } else {
                    None
                };
                if let Some(session) = &session {
                    context.session_handle = Some(session.handle().into());
                }
                if is_write {
                    lease.mark_write_dispatched()?;
                }
                let reservation = if request.operation == Operation::QueryExecute {
                    Some(
                        self.results
                            .reserve_query(&connection.id, context.deadline_ms)?,
                    )
                } else {
                    None
                };
                let output = self
                    .driver_call(&connection, request, context, lease, None)
                    .await;
                if is_write && output.is_ok() {
                    lease.mark_write_confirmed()?;
                }
                let session_closed = match &output {
                    Ok(output) => output.session_closed,
                    Err(error) => error.details["session_closed"] == true,
                };
                drop(session);
                if session_closed {
                    if let Some(id) = &request.session_id {
                        self.sessions.discard(id, &connection.id).await?;
                    }
                }
                let output = output?;
                self.revalidate(&connection, &permission, caller).await?;
                let page = self.store_output(
                    &connection,
                    &permission,
                    request,
                    output,
                    None,
                    reservation.as_ref(),
                    guarantee,
                )?;
                Ok(from_page(request, with_guarantee(page, guarantee)))
            }
        }
    }
    async fn next_page(
        &self,
        request: &ServiceRequest,
        connection: &SavedConnection,
        permission: &PermissionScope,
        caller: &CallerContext,
        mut context: DriverContext,
        lease: &JobLease,
    ) -> ServiceResult<ServiceResponse> {
        let token = string(&request.input, "next_token")?;
        let binding = self.results.token_binding(token)?;
        authorize_binding(&binding, connection, permission)?;
        let page = match self.results.begin_next(token, &binding)? {
            NextPage::Replay(page) => page,
            NextPage::Materialized {
                result_id,
                set_index,
                offset,
                limit,
            } => self.results.finish_next(
                token,
                &binding,
                self.results
                    .page(&result_id, &binding, set_index, offset, limit)?,
            )?,
            NextPage::Native(state) => {
                let result = async {
                    if !self
                        .driver
                        .supports(connection, Operation::QueryNext)
                        .await?
                    {
                        return Err(failure(ErrorCode::CapabilityUnavailable));
                    }
                    let guarantee =
                        guarantee_label(self.driver.read_only_guarantee(connection).await?);
                    context.transient_auth =
                        self.auth.context(connection, context.deadline_ms).await?;
                    let reservation = self
                        .results
                        .reserve_query(&connection.id, context.deadline_ms)?;
                    let output = self
                        .driver_call(connection, request, context, lease, Some(&state))
                        .await?;
                    self.revalidate(connection, permission, caller).await?;
                    let page = self.store_output(
                        connection,
                        permission,
                        request,
                        output,
                        Some(&binding),
                        Some(&reservation),
                        guarantee,
                    )?;
                    self.results
                        .finish_next(token, &binding, with_guarantee(page, guarantee))
                }
                .await;
                match result {
                    Ok(page) => page,
                    Err(error) => {
                        self.results.fail_next(token)?;
                        return Err(error);
                    }
                }
            }
        };
        Ok(from_page(request, page))
    }
    /// Keeps a driver call that outlives its Entra token supplied with a newer
    /// one: shortly before the driver would reject the token it holds, the host
    /// fetches a fresh token and hands it over. It never completes, so the
    /// caller drops it together with the call, and it stops for good when the
    /// driver cannot take a refreshed token.
    async fn refresh_auth_while_running(
        &self,
        connection: &SavedConnection,
        context: &DriverContext,
    ) -> std::convert::Infallible {
        let Some(TransientAuth::EntraToken {
            mut expires_at_ms, ..
        }) = context.transient_auth.clone()
        else {
            return std::future::pending().await;
        };
        let call_ends = now_ms().saturating_add(context.deadline_ms);
        loop {
            let refresh_at = expires_at_ms
                .saturating_sub(REFRESH_MARGIN_MS.saturating_add(AUTH_REFRESH_LEAD_MS));
            if refresh_at >= call_ends {
                return std::future::pending().await;
            }
            tokio::time::sleep(Duration::from_millis(refresh_at.saturating_sub(now_ms()))).await;
            if let Ok(Some(
                auth @ TransientAuth::EntraToken {
                    expires_at_ms: next,
                    ..
                },
            )) = self.auth.context(connection, MAX_DEADLINE_MS).await
            {
                if next > expires_at_ms {
                    let refresh = DriverContext {
                        connection_id: context.connection_id.clone(),
                        request_id: uuid::Uuid::new_v4().to_string(),
                        deadline_ms: AUTH_REFRESH_CALL_MS,
                        signal: context.signal.clone(),
                        transient_auth: Some(auth),
                        read_only: context.read_only,
                        session_handle: None,
                    };
                    match self.driver.refresh_auth(connection, refresh).await {
                        Ok(true) => {
                            expires_at_ms = next;
                            continue;
                        }
                        Ok(false) => return std::future::pending().await,
                        Err(_) => {}
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(AUTH_REFRESH_RETRY_MS)).await;
        }
    }
    async fn driver_call(
        &self,
        connection: &SavedConnection,
        request: &ServiceRequest,
        mut context: DriverContext,
        lease: &JobLease,
        cursor: Option<&super::results::CursorState>,
    ) -> ServiceResult<DriverOutput> {
        lease.mark_dispatched()?;
        context.deadline_ms = context
            .deadline_ms
            .saturating_add(self.cancel_grace().as_millis() as u64)
            .clamp(1, request.operation.max_deadline_ms());
        let refresh = self.refresh_auth_while_running(connection, &context);
        let call = async {
            match cursor {
                Some(cursor) => {
                    self.driver
                        .continue_cursor(connection, cursor, context.clone())
                        .await
                }
                None => {
                    self.driver
                        .execute(connection, request, context.clone())
                        .await
                }
            }
        };
        let result = tokio::select! {
            result = call => result,
            never = refresh => match never {},
        };
        match result {
            Ok(output) if output.confirmed_cancelled => {
                let mut error = failure(ErrorCode::Cancelled);
                error.outcome = if lease.write_dispatched() {
                    Outcome::Unknown
                } else {
                    Outcome::NotApplied
                };
                if output.session_closed {
                    error.details = json!({"session_closed":true});
                }
                Err(error)
            }
            Err(mut error) => {
                if lease.write_dispatched()
                    && (error.outcome == Outcome::Unknown
                        || error.code == ErrorCode::Cancelled
                        || (error.outcome == Outcome::NotStarted
                            && matches!(
                                error.code,
                                ErrorCode::DriverExited
                                    | ErrorCode::DatabaseError
                                    | ErrorCode::DeadlineExceeded
                                    | ErrorCode::OutcomeUnknown
                            )))
                {
                    if error.code != ErrorCode::Cancelled {
                        error.code = ErrorCode::OutcomeUnknown;
                    }
                    error.outcome = Outcome::Unknown;
                }
                Err(error)
            }
            Ok(output) => Ok(output),
        }
    }
    fn store_output(
        &self,
        connection: &SavedConnection,
        permission: &PermissionScope,
        request: &ServiceRequest,
        output: DriverOutput,
        previous: Option<&CursorBinding>,
        reservation: Option<&super::results::ResultReservation>,
        guarantee: &str,
    ) -> ServiceResult<StoredPage> {
        let origin = if connection.driver == "cosmos-nosql"
            && request.operation == Operation::QueryExecute
            && request.input["language"] == "cosmos_sql"
        {
            Some(super::results::ResultOrigin::from_input(&request.input)?)
        } else if connection.driver == "cosmos-nosql" && request.operation == Operation::QueryNext {
            self.results.token_origin(
                string(&request.input, "next_token")?,
                previous.ok_or_else(|| failure(ErrorCode::InvalidPageToken))?,
            )?
        } else {
            None
        };
        let DriverOutput {
            data,
            mut snapshot,
            cursor,
            metrics,
            limits,
            mut warnings,
            ..
        } = output;
        let affected_rows = if request.operation == Operation::QueryExecute && snapshot.is_some() {
            data.get("affected_rows")
                .filter(|value| {
                    value.is_null()
                        || value
                            .as_u64()
                            .is_some_and(|count| count <= 9_007_199_254_740_991)
                })
                .cloned()
        } else {
            None
        };
        let page_size = cursor
            .as_ref()
            .map(|cursor| cursor.page_size)
            .unwrap_or_else(|| request.input["page_size"].as_u64().unwrap_or(100) as usize);
        if let Some(cursor) = &cursor {
            if connection.driver != "cosmos-nosql"
                || cursor.page_size == 0
                || cursor.page_size > 1000
                || snapshot
                    .as_ref()
                    .is_none_or(|snapshot| snapshot.sets.len() != 1)
            {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            if snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.sets[0].rows.len() > page_size)
            {
                return Err(failure(ErrorCode::ResourceLimit));
            }
        }
        warnings.retain(|warning| {
            warning.len() <= 128
                && !warning.is_empty()
                && warning
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        });
        if let Some(snapshot) = &mut snapshot {
            snapshot.limits.truncated |= limits.truncated;
            for reason in &limits.reasons {
                if !snapshot.limits.reasons.contains(reason) {
                    snapshot.limits.reasons.push(reason.clone());
                }
            }
            snapshot.warnings.extend(warnings.clone());
        }
        let query_hash = previous
            .map(|binding| binding.query_hash.clone())
            .unwrap_or_else(|| value_hash(&request.input));
        let revision = previous
            .map(|binding| binding.result_revision.clone())
            .unwrap_or_else(|| {
                snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.revision.clone())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
            });
        let binding = CursorBinding {
            connection_id: connection.id.clone(),
            permission_scope: permission.identity.clone(),
            auth_revision: connection.auth_revision.clone(),
            query_hash,
            result_revision: revision,
            sdk_version: cursor
                .as_ref()
                .and_then(|cursor| cursor.binding.sdk_version.clone())
                .or_else(|| previous.and_then(|binding| binding.sdk_version.clone())),
            container_rid: cursor
                .as_ref()
                .and_then(|cursor| cursor.binding.container_rid.clone())
                .or_else(|| previous.and_then(|binding| binding.container_rid.clone())),
            endpoint_identity: cursor
                .as_ref()
                .and_then(|cursor| cursor.binding.endpoint_identity.clone())
                .or_else(|| previous.and_then(|binding| binding.endpoint_identity.clone())),
            partition_hash: cursor
                .as_ref()
                .and_then(|cursor| cursor.binding.partition_hash.clone())
                .or_else(|| previous.and_then(|binding| binding.partition_hash.clone())),
        };
        if let Some(previous) = previous {
            if cursor.is_some() && binding != *previous {
                return Err(failure(ErrorCode::InvalidPageToken));
            }
        }
        let mut page = if let Some(snapshot) = snapshot {
            let id = self.results.insert_labeled(
                binding.clone(),
                snapshot,
                reservation,
                origin,
                Some(guarantee),
            )?;
            let page = self.results.page(&id, &binding, 0, 0, page_size)?;
            if cursor.is_some() && page.page.has_more {
                self.results.release(&id, &binding)?;
                return Err(failure(ErrorCode::ResourceLimit));
            }
            page
        } else {
            StoredPage {
                data,
                page: Page::default(),
                result_id: None,
                limits,
                metrics: Metrics::default(),
                warnings,
            }
        };
        if let Some(affected_rows) = affected_rows {
            page.data["affected_rows"] = affected_rows;
        }
        if let Some(mut cursor) = cursor {
            if connection.driver != "cosmos-nosql" {
                return Err(failure(ErrorCode::InvalidPageToken));
            }
            cursor.binding = binding.clone();
            let token = self.results.cursor_token_reserved(cursor, reservation)?;
            if let Some(id) = &page.result_id {
                self.results.attach_cursor_result(&token, id, &binding)?;
            }
            page.page = Page {
                next_token: Some(token),
                has_more: true,
                resume_mode: super::contracts::ResumeMode::Native,
            };
        }
        if serde_json::to_vec(&page.data)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len()
            > super::results::MAX_PAGE_BYTES - 64 * 1024
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        page.metrics = metrics;
        Ok(page)
    }
    pub async fn call_validated(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
    ) -> ServiceResponse {
        let _secret_input = SecretInputCleanup {
            inputs: &self.secret_inputs,
            caller,
            request,
        };
        if let Err(error) = self.validate(request) {
            return error_response(request, error);
        }
        let control = matches!(
            request.operation,
            Operation::JobGet
                | Operation::JobResult
                | Operation::JobCancel
                | Operation::ResultGet
                | Operation::ResultRelease
                | Operation::AuthStatus
                | Operation::AuthCancel
                | Operation::AuthLogout
        );
        let is_write = match self.admit(request, caller).await {
            Ok(value) => value,
            Err(error) => return error_response(request, error),
        };
        let lease = match self.jobs.begin(request, caller, false, control).await {
            Ok(lease) => lease,
            Err(error) => return error_response(request, error),
        };
        self.complete(request, caller, lease, is_write).await
    }
    async fn complete(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: JobLease,
        is_write: bool,
    ) -> ServiceResponse {
        let _secret_input = SecretInputCleanup {
            inputs: &self.secret_inputs,
            caller,
            request,
        };
        let started = now_ms();
        let lease = Arc::new(lease);
        let (execution, deadline_cancel) =
            self.run_bounded(request, caller, &lease, is_write).await;
        // A job stopped before it reached the driver is already recorded as
        // cancelled; only a request the driver was asked about is judged here.
        let cancel_state = deadline_cancel.or_else(|| {
            if !lease.cancel_requested() {
                return None;
            }
            if !lease.dispatched() {
                return execution.is_ok().then_some("completed_first");
            }
            Some(match &execution {
                Ok(_) => "completed_first",
                Err(_) if lease.signal().is_cancelled() => "not_confirmed",
                Err(error) if error.code == ErrorCode::Cancelled => "confirmed",
                Err(_) => "not_confirmed",
            })
        });
        if let Some(state) = cancel_state {
            lease.set_cancel_state(state);
        }
        let mut response = match execution {
            Ok(response) => response,
            Err(mut error) => {
                if lease.write_confirmed() {
                    error.outcome = Outcome::Applied;
                } else if lease.write_dispatched()
                    && (error.outcome == Outcome::Unknown
                        || (error.outcome == Outcome::NotStarted
                            && matches!(
                                error.code,
                                ErrorCode::DriverExited
                                    | ErrorCode::DatabaseError
                                    | ErrorCode::DeadlineExceeded
                                    | ErrorCode::OutcomeUnknown
                            )))
                {
                    if error.code != ErrorCode::Cancelled {
                        error.code = ErrorCode::OutcomeUnknown;
                    }
                    error.outcome = Outcome::Unknown;
                }
                error_response(request, error)
            }
        };
        if cancel_state == Some("not_confirmed") {
            response.warnings.push("cancel_not_confirmed".into());
        }
        response.metrics.elapsed_ms = now_ms().saturating_sub(started);
        let mut preview = response.clone();
        preview.job_id = Some(lease.id());
        if serde_json::to_vec(&preview)
            .map_or(true, |bytes| bytes.len() > super::results::MAX_PAGE_BYTES)
        {
            let mut error = failure(ErrorCode::ResourceLimit);
            if lease.write_confirmed() {
                error.outcome = Outcome::Applied;
            }
            response = error_response(request, error);
        }
        match lease.finish(response) {
            Ok(response) => response,
            Err(mut error) => {
                if lease.write_confirmed() {
                    error.outcome = Outcome::Applied;
                }
                error_response(request, error)
            }
        }
    }
    /// Runs the operation until its deadline. At the deadline a call that
    /// reached the driver is asked to cancel and gets a grace period to
    /// answer; one that never reached it stops locally. The second value is
    /// what became of that cancellation request. A call the driver never
    /// confirmed keeps its slot until it ends on its own.
    async fn run_bounded(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &Arc<JobLease>,
        is_write: bool,
    ) -> (ServiceResult<ServiceResponse>, Option<&'static str>) {
        let Some(runtime) = self.me.upgrade() else {
            return (
                match run_until_deadline(lease, self.execute(request, caller, lease, is_write))
                    .await
                {
                    Some(result) => result,
                    None => Err(failure(ErrorCode::DeadlineExceeded)),
                },
                None,
            );
        };
        let mut task = {
            let (request, caller, lease) = (request.clone(), caller.clone(), lease.clone());
            ExecutionTask::spawn(async move {
                runtime.execute(&request, &caller, &lease, is_write).await
            })
        };
        if let Some(result) = run_until_deadline(lease, task.join()).await {
            return (result, None);
        }
        let grace = self.cancel_grace();
        if !lease.request_cancel() {
            lease.signal().cancel();
            let _ = tokio::time::timeout(grace.min(Duration::from_secs(2)), task.join()).await;
            return (Err(failure(ErrorCode::DeadlineExceeded)), None);
        }
        if let Some(id) = &request.connection_id {
            let _ = tokio::time::timeout(
                grace.max(Duration::from_millis(1)),
                self.driver.cancel(id, &request.request_id),
            )
            .await;
        }
        match tokio::time::timeout(grace, task.join()).await {
            Ok(Ok(response)) => (Ok(response), Some("completed_first")),
            Ok(Err(error)) if error.code == ErrorCode::Cancelled => {
                let mut error = failure(ErrorCode::DeadlineExceeded);
                error.outcome = if lease.write_dispatched() {
                    Outcome::Unknown
                } else {
                    Outcome::NotApplied
                };
                (Err(error), Some("confirmed"))
            }
            Ok(Err(error)) => (Err(error), Some("not_confirmed")),
            Err(_) => {
                task.detach();
                (
                    Err(failure(ErrorCode::DeadlineExceeded)),
                    Some("not_confirmed"),
                )
            }
        }
    }
    fn validate(&self, request: &ServiceRequest) -> ServiceResult<()> {
        if self.stopping.load(Ordering::SeqCst) {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
        let value =
            serde_json::to_value(request).map_err(|_| failure(ErrorCode::InvalidArgument))?;
        if serde_json::to_vec(&value)
            .map_err(|_| failure(ErrorCode::InvalidArgument))?
            .len()
            > 16 * 1024 * 1024
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        self.validator.validate_request(&value).map_err(|_| {
            failure(if request.protocol_version == 1 {
                ErrorCode::InvalidArgument
            } else {
                ErrorCode::ProtocolMismatch
            })
        })?;
        if !SERVICE_OPERATIONS.contains(&request.operation)
            && !self
                .extensions
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                .contains_key(&request.operation)
        {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
        match request.operation {
            Operation::ConnectionCreate => {
                super::security::reject_secret_fields(&request.input["settings"])?
            }
            Operation::ConnectionUpdate => {
                super::security::reject_secret_fields(&request.input["patch"])?;
                super::security::reject_secret_fields(&request.input["patch"]["settings"])?;
            }
            Operation::QueryExplain => {
                if !super::sql_classifier::is_plannable_statement(
                    request.input["sql"].as_str().unwrap_or(""),
                ) {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
            }
            _ => {}
        }
        if request.session_id.is_some()
            && !matches!(
                request.operation,
                Operation::QueryExecute
                    | Operation::RowInsert
                    | Operation::RowUpdate
                    | Operation::RowDelete
                    | Operation::SessionCommit
                    | Operation::SessionRollback
                    | Operation::SessionClose
            )
        {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        Ok(())
    }
    pub async fn submit(
        self: &Arc<Self>,
        request: ServiceRequest,
        caller: CallerContext,
    ) -> ServiceResponse {
        self.submit_as(request, caller, false).await
    }
    /// Like `submit`, for a job the service starts on its own behalf: it
    /// leaves no history entry.
    pub async fn submit_internal(
        self: &Arc<Self>,
        request: ServiceRequest,
        caller: CallerContext,
    ) -> ServiceResponse {
        self.submit_as(request, caller, true).await
    }
    async fn submit_as(
        self: &Arc<Self>,
        request: ServiceRequest,
        caller: CallerContext,
        internal: bool,
    ) -> ServiceResponse {
        if let Err(error) = self.validate(&request) {
            self.secret_inputs.discard(&caller, &request.request_id);
            return error_response(&request, error);
        }
        let control = matches!(
            request.operation,
            Operation::JobGet
                | Operation::JobResult
                | Operation::JobCancel
                | Operation::ResultGet
                | Operation::ResultRelease
                | Operation::AuthStatus
                | Operation::AuthCancel
                | Operation::AuthLogout
        );
        if control {
            return self.call_validated(&request, &caller).await;
        }
        let is_write = match self.admit(&request, &caller).await {
            Ok(value) => value,
            Err(error) => {
                self.secret_inputs.discard(&caller, &request.request_id);
                return error_response(&request, error);
            }
        };
        let mut lease = match self.jobs.queue_as(&request, &caller, false, false, internal) {
            Ok(lease) => lease,
            Err(error) => {
                self.secret_inputs.discard(&caller, &request.request_id);
                return error_response(&request, error);
            }
        };
        let id = lease.id();
        let mut queued = success(
            &request,
            json!({"job_id":id,"owner_instance_id":self.jobs.instance_id()}),
        );
        queued.job_id = Some(id.clone());
        queued.status = JobStatus::Queued;
        let runtime = self.clone();
        let task_id = id.clone();
        let (ready, start) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = start.await;
            let _cleanup = SecretInputCleanup {
                inputs: &runtime.secret_inputs,
                caller: &caller,
                request: &request,
            };
            match runtime.jobs.start(&mut lease).await {
                Ok(()) => {
                    runtime.complete(&request, &caller, lease, is_write).await;
                }
                Err(error) => {
                    let _ = lease.finish(error_response(&request, error));
                }
            }
            if let Ok(mut tasks) = runtime.tasks.lock() {
                tasks.remove(&task_id);
            }
        });
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.insert(id, task);
        } else {
            task.abort();
            queued.status = JobStatus::Failed;
            queued.data = Value::Null;
            queued.error = Some(failure(ErrorCode::ResourceLimit));
            return queued;
        }
        let _ = ready.send(());
        queued
    }
    pub async fn shutdown(&self) -> ServiceResult<()> {
        self.stopping.store(true, Ordering::SeqCst);
        let active = self.jobs.cancel_active();
        for job in &active {
            if let Some(connection) = &job.connection_id {
                let _ = self.driver.cancel(connection, &job.request_id).await;
            }
        }
        let tasks: Vec<_> = self
            .tasks
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .drain()
            .map(|(_, task)| task)
            .collect();
        for mut task in tasks {
            if tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        self.auth.teardown().await?;
        for connection in self.connections.list().await? {
            self.driver.invalidate_auth(&connection.id, None).await;
            self.sessions
                .invalidate_connection(
                    &connection.id,
                    DriverContext {
                        connection_id: connection.id.clone(),
                        request_id: format!("shutdown-{}", uuid::Uuid::new_v4()),
                        deadline_ms: 30000,
                        signal: Default::default(),
                        transient_auth: None,
                        read_only: true,
                        session_handle: None,
                    },
                )
                .await?;
        }
        self.jobs.recover_owner(self.jobs.instance_id())
    }
    /// An approval authorizes the permission and the saved connection it was
    /// requested for; neither may have changed while it was pending.
    async fn recheck_after_approval(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        connection: &SavedConnection,
        is_write: bool,
    ) -> ServiceResult<()> {
        let (current, permission) = self.connection(request, caller).await?;
        if is_write && !permission.allow_writes {
            return Err(refusal(ErrorCode::WriteNotAllowed, CONNECTION_READ_ONLY));
        }
        if current.version != connection.version {
            let mut error = failure(ErrorCode::ConnectionConflict);
            error.details = json!({"current_version":current.version});
            return Err(error);
        }
        Ok(())
    }
    async fn revalidate(
        &self,
        connection: &SavedConnection,
        permission: &PermissionScope,
        caller: &CallerContext,
    ) -> ServiceResult<()> {
        let current = self.connections.get(&connection.id).await?;
        if current.auth_revision != connection.auth_revision || current.driver != connection.driver
        {
            return Err(failure(ErrorCode::AuthRequired));
        }
        let current_permission = self.policy.permission(&current, caller).await?;
        if current_permission.identity != permission.identity
            || current_permission.allow_writes != permission.allow_writes
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        Ok(())
    }
    /// Decides whether a request is a write and rejects it before queueing
    /// when its intent is missing or the connection is read-only.
    async fn admit(&self, request: &ServiceRequest, caller: &CallerContext) -> ServiceResult<bool> {
        if caller.transport == CallerTransport::Mcp
            && super::policy::mcp_forbidden(request.operation)
        {
            return Err(failure(ErrorCode::OperationNotAllowed));
        }
        let is_write = tokio::time::timeout(
            Duration::from_millis(request.deadline_ms),
            self.is_write(request, caller),
        )
        .await
        .map_err(|_| failure(ErrorCode::DeadlineExceeded))??;
        self.write_gate(request, caller, is_write).await?;
        Ok(is_write)
    }
    fn start_maintenance(self: &Arc<Self>) -> ServiceResult<()> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| failure(ErrorCode::CapabilityUnavailable))?;
        let runtime = Arc::downgrade(self);
        let task = handle.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut ticks = 0u32;
            loop {
                interval.tick().await;
                let Some(runtime) = runtime.upgrade() else {
                    break;
                };
                if runtime.stopping.load(Ordering::SeqCst) {
                    break;
                }
                let context = DriverContext {
                    connection_id: String::new(),
                    request_id: format!("maintenance-{}", uuid::Uuid::new_v4()),
                    deadline_ms: 30000,
                    signal: Default::default(),
                    transient_auth: None,
                    read_only: true,
                    session_handle: None,
                };
                let _ = runtime.sessions.expire_idle(context).await;
                ticks += 1;
                if ticks % 30 == 0 {
                    let _ = runtime.results.cleanup();
                }
            }
        });
        self.tasks
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .insert("maintenance".into(), task);
        Ok(())
    }
}

/// An operation running as its own task. Dropping the handle stops the task
/// unless it was detached to let a call that ignored its cancellation end on
/// its own while it still holds the job's slot.
struct ExecutionTask(Option<tokio::task::JoinHandle<ServiceResult<ServiceResponse>>>);
impl ExecutionTask {
    fn spawn(
        future: impl std::future::Future<Output = ServiceResult<ServiceResponse>> + Send + 'static,
    ) -> Self {
        Self(Some(tokio::spawn(future)))
    }
    async fn join(&mut self) -> ServiceResult<ServiceResponse> {
        match self.0.as_mut() {
            Some(handle) => match handle.await {
                Ok(result) => {
                    self.0 = None;
                    result
                }
                Err(_) => {
                    self.0 = None;
                    Err(failure(ErrorCode::DriverExited))
                }
            },
            None => Err(failure(ErrorCode::DriverExited)),
        }
    }
    fn detach(&mut self) {
        self.0 = None;
    }
}
impl Drop for ExecutionTask {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

struct SecretInputCleanup<'a> {
    inputs: &'a SecretInputs,
    caller: &'a CallerContext,
    request: &'a ServiceRequest,
}
/// How often a job that waits for approval renews its persisted record.
const APPROVAL_RENEW_INTERVAL: Duration = Duration::from_secs(60);
/// How often the deadline is looked at again while a job waits for approval.
const APPROVAL_DEADLINE_RECHECK: Duration = Duration::from_millis(100);

/// Waits for `action` until the lease deadline. A job that awaits approval
/// has no deadline: its wait is not part of the execution time, so the
/// deadline is looked at again whenever it moves. `None` means it passed.
async fn run_until_deadline<T>(
    lease: &JobLease,
    action: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::pin!(action);
    loop {
        let remaining = if lease.awaiting_approval() {
            APPROVAL_DEADLINE_RECHECK
        } else {
            Duration::from_millis(lease.deadline_at_ms().saturating_sub(now_ms()))
        };
        if let Ok(result) = tokio::time::timeout(remaining, &mut action).await {
            return Some(result);
        }
        if !lease.awaiting_approval() && now_ms() >= lease.deadline_at_ms() {
            return None;
        }
    }
}
/// Waits for a human decision, which has no time limit, until the job is
/// cancelled. The persisted job record is renewed while the wait lasts.
async fn await_approval<T>(
    lease: &JobLease,
    action: impl std::future::Future<Output = ServiceResult<T>>,
) -> ServiceResult<T> {
    let signal = lease.signal();
    tokio::pin!(action);
    let mut renew = tokio::time::interval_at(
        tokio::time::Instant::now() + APPROVAL_RENEW_INTERVAL,
        APPROVAL_RENEW_INTERVAL,
    );
    loop {
        tokio::select! {
            biased;
            _ = signal.cancelled() => return Err(failure(ErrorCode::Cancelled)),
            result = &mut action => return result,
            _ = renew.tick() => lease.renew_approval(),
        }
    }
}
async fn before_publication<T>(
    lease: &JobLease,
    action: impl std::future::Future<Output = ServiceResult<T>>,
) -> ServiceResult<T> {
    let signal = lease.signal();
    tokio::select! { biased; _ = signal.cancelled() => Err(failure(ErrorCode::Cancelled)), result = action => result }
}
impl Drop for SecretInputCleanup<'_> {
    fn drop(&mut self) {
        if self.request.operation == Operation::CredentialImport {
            self.inputs.discard(self.caller, &self.request.request_id);
        }
    }
}

#[async_trait]
impl ServiceHandler for ServiceRuntime {
    async fn call(&self, request: &ServiceRequest, caller: &CallerContext) -> ServiceResponse {
        self.call_validated(request, caller).await
    }
    fn rejected(&self, request: &ServiceRequest, caller: &CallerContext) {
        if request.operation == Operation::CredentialImport {
            self.secret_inputs.discard(caller, &request.request_id);
        }
    }
}

/// Whether a query without write intent is a proven read. A PostgreSQL
/// wrapper does not count inside a session, whose transaction the caller
/// controls; only a plain read does.
fn is_proven_read(text: &str, driver: Option<&str>, in_session: bool) -> bool {
    let dialect = driver
        .and_then(read_dialect)
        .filter(|dialect| !(in_session && *dialect == ReadDialect::Postgres));
    prove_read(text, dialect).is_some()
}
fn success(request: &ServiceRequest, data: Value) -> ServiceResponse {
    ServiceResponse {
        protocol_version: 1,
        request_id: request.request_id.clone(),
        connection_id: request.connection_id.clone(),
        status: JobStatus::Succeeded,
        job_id: None,
        result_id: None,
        data,
        page: Page::default(),
        limits: Limits::default(),
        metrics: Metrics::default(),
        warnings: Vec::new(),
        error: None,
    }
}
fn guarantee_label(guarantee: ReadOnlyGuarantee) -> &'static str {
    match guarantee {
        ReadOnlyGuarantee::ReadOnlyTransaction => "read_only_transaction",
        ReadOnlyGuarantee::RestrictedDatabasePrincipal => "restricted_database_principal",
        ReadOnlyGuarantee::ReadOnlyDataPlaneRole => "read_only_data_plane_role",
        ReadOnlyGuarantee::ReadOnlyAccountKey => "read_only_account_key",
        ReadOnlyGuarantee::ReadOnlySdkOperations => "read_only_sdk_operations",
        ReadOnlyGuarantee::Unavailable => "classification_only",
    }
}
fn with_guarantee(mut page: StoredPage, guarantee: &str) -> StoredPage {
    if let Some(data) = page.data.as_object_mut() {
        data.entry("readonly_guarantee")
            .or_insert_with(|| json!(guarantee));
    }
    page
}
fn error_response(
    request: &ServiceRequest,
    error: super::contracts::ServiceError,
) -> ServiceResponse {
    let error = redact_error(error);
    let mut response = ServiceResponse::failure(request, error.code, &error.message);
    response.status = if error.outcome == Outcome::Unknown {
        JobStatus::OutcomeUnknown
    } else if error.code == ErrorCode::Cancelled {
        JobStatus::Cancelled
    } else {
        JobStatus::Failed
    };
    response.error = Some(error);
    response
}
fn from_page(request: &ServiceRequest, page: StoredPage) -> ServiceResponse {
    let mut response = success(request, page.data);
    response.result_id = page.result_id;
    response.page = page.page;
    response.limits = page.limits;
    response.metrics = page.metrics;
    response.warnings = page.warnings;
    response
}
fn string<'a>(value: &'a Value, key: &str) -> ServiceResult<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))
}
fn integer(value: &Value, key: &str) -> ServiceResult<usize> {
    value[key]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))
}
/// `allow_writes` is the effective permission from the profile's MCP
/// read-only settings, not the value saved on the connection.
fn public_connection(connection: &SavedConnection, allow_writes: bool) -> Value {
    json!({"connection_id":connection.id,"name":connection.name,"driver":connection.driver,"settings":sanitized_settings(&connection.settings),"credential_ref":connection.credential_ref,"version":connection.version,"allow_writes":allow_writes})
}
fn authorize_binding(
    binding: &CursorBinding,
    connection: &SavedConnection,
    permission: &PermissionScope,
) -> ServiceResult<()> {
    if binding.connection_id != connection.id
        || binding.permission_scope != permission.identity
        || binding.auth_revision != connection.auth_revision
    {
        Err(failure(ErrorCode::InvalidPageToken))
    } else {
        Ok(())
    }
}

/// `sqlserver-plus` owns its Entra sign-in: the driver runs Azure CLI itself
/// and the host neither acquires nor forwards a token, so only the fields the
/// driver reads are checked and no `client_id` is involved.
fn validate_driver_owned_entra(settings: &Value, extra: &Value) -> ServiceResult<()> {
    let text = |key: &str| {
        settings
            .get(key)
            .or_else(|| extra.get(key))
            .filter(|value| !value.is_null())
    };
    let entra_keys = [
        "auth_source",
        "tenant_id",
        "entra_object_id",
        "azure_cli_path",
    ];
    match extra["auth_mode"].as_str() {
        Some("entra_user") => {}
        Some("entra_service_principal") => return Err(failure(ErrorCode::InvalidArgument)),
        _ if entra_keys.iter().any(|key| extra.get(*key).is_some()) => {
            return Err(failure(ErrorCode::InvalidArgument))
        }
        _ => return Ok(()),
    }
    let required = |key: &str| {
        text(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))
    };
    let tenant = required("tenant_id")?;
    if extra["auth_source"] != "azure_cli"
        || required("username").is_err()
        || required("entra_object_id").is_err()
        || tenant.starts_with('-')
        || !tenant
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        || extra["integrated"] == true
        || extra["integrated"] == "true"
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    if let Some(path) = extra.get("azure_cli_path") {
        let valid = path.as_str().is_some_and(|path| {
            !path.is_empty() && path.trim() == path && std::path::Path::new(path).is_absolute()
        });
        if !valid {
            return Err(failure(ErrorCode::InvalidArgument));
        }
    }
    Ok(())
}

pub fn validate_connection_configuration(driver: &str, settings: &Value) -> ServiceResult<()> {
    validate_settings(settings)?;
    if !settings.is_object() {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let extra = settings.get("extra").unwrap_or(settings);
    for host in [settings.get("host"), extra.get("host")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if host.contains("://")
            || host.contains(['@', '/', '?', '#'])
            || host.chars().any(char::is_whitespace)
            || host.chars().any(char::is_control)
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
    }
    if driver == "sqlserver-plus" {
        validate_driver_owned_entra(settings, extra)?;
    } else if matches!(
        extra["auth_mode"].as_str(),
        Some("entra_user" | "entra_service_principal")
    ) {
        if uuid::Uuid::parse_str(extra["tenant_id"].as_str().unwrap_or("")).is_err()
            || uuid::Uuid::parse_str(extra["client_id"].as_str().unwrap_or("")).is_err()
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if extra.get("username").is_some_and(|value| !value.is_null())
            || extra["integrated"] == true
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
    }
    if driver != "sqlserver-plus" && extra.get("auth_source").is_some() {
        if !matches!(extra["auth_source"].as_str(), Some("oauth" | "azure_cli"))
            || (extra["auth_source"] == "azure_cli"
                && (!matches!(driver, "sqlserver" | "cosmos-nosql")
                    || extra["auth_mode"] != "entra_user"
                    || extra["client_id"] != super::azure_cli::CLIENT_ID))
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
    }
    if driver == "cosmos-nosql" {
        let endpoint = settings["endpoint"]
            .as_str()
            .or_else(|| extra["endpoint"].as_str())
            .unwrap_or("");
        let url = reqwest::Url::parse(endpoint).map_err(|_| failure(ErrorCode::InvalidArgument))?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url
                .host_str()
                .is_none_or(|host| !host.ends_with(".documents.azure.com"))
            || url.port().is_some_and(|port| port != 443)
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if !matches!(
            extra["auth_mode"].as_str(),
            Some("account_key" | "entra_user" | "entra_service_principal")
        ) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
    }
    if settings["host"]
        .as_str()
        .is_some_and(|host| host.ends_with(".database.windows.net"))
    {
        if settings["ssl_mode"] != "verify-full"
            || extra["trust_server_certificate"] == true
            || settings["database"].as_str().is_none_or(str::is_empty)
        {
            return Err(failure(ErrorCode::TlsValidationFailed));
        }
    }
    Ok(())
}
