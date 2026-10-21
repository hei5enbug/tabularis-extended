use super::{
    auth::{AuthLifecycle, AuthManager},
    backend::{
        ConnectionBackend, DriverBackend, DriverContext, DriverOutput, PermissionScope,
        PolicyBackend, ReadOnlyGuarantee, SavedConnection, SessionFinish,
    },
    contracts::{
        ErrorCode, JobStatus, Limits, Metrics, Operation, Outcome, Page, ServiceRequest,
        ServiceResponse,
    },
    credentials::{SecretInputs, SecretSlot, SecretStore, SessionSecretStore},
    dispatch::{CallerContext, ServiceDispatcher, ServiceHandler},
    error::{failure, redact_error, ServiceResult},
    jobs::{JobLease, JobManager},
    oauth::OAuthProvider,
    results::{CursorBinding, NextPage, ResultStore, StoredPage},
    security::{sanitized_settings, validate_settings, value_hash},
    sessions::SessionManager,
    storage::now_ms,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

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
    validator: super::contracts::SchemaValidator,
    tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    stopping: AtomicBool,
    extensions: Mutex<HashMap<Operation, Arc<dyn super::extensions::OperationExtension>>>,
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
        let runtime = Arc::new(Self {
            connections,
            policy,
            driver,
            results,
            jobs,
            sessions,
            auth,
            secret_inputs: Arc::new(SecretInputs::default()),
            validator: super::contracts::SchemaValidator::new()
                .map_err(|_| failure(ErrorCode::InvalidArgument))?,
            tasks: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
            extensions: Mutex::new(HashMap::new()),
        });
        runtime.start_maintenance()?;
        Ok(runtime)
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
            | Operation::DocumentDelete => true,
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
                if session_write && !intent {
                    return Err(failure(ErrorCode::WriteNotAllowed));
                }
                intent || session_write
            }
            Operation::SessionOpen => request.input["mode"] == "write",
            Operation::SessionCommit => {
                self.sessions
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
                    .await?
            }
            _ => false,
        })
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
        if now_ms() >= lease.deadline_at_ms {
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
                    if self.policy.permission(&connection, caller).await.is_ok() {
                        list.push(public_connection(&connection));
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
                return Ok(success(request, public_connection(&connection)));
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
                if request.operation == Operation::JobCancel {
                    record = self.jobs.cancel(job_id, owner, caller)?;
                }
                if request.operation == Operation::JobCancel && record.cancel_requested {
                    if let Some(id) = &record.connection_id {
                        self.driver.cancel(id, &record.request_id).await?;
                    }
                }
                if request.operation == Operation::JobResult {
                    return Ok(success(
                        request,
                        json!({"status":record.status,"response":record.response}),
                    ));
                }
                return Ok(success(
                    request,
                    json!({"job_id":record.job_id,"status":record.status,"cancel_requested":record.cancel_requested,"owner_instance_id":record.owner_instance_id}),
                ));
            }
            _ => {}
        }
        let (connection, permission) = self.connection(request, caller).await?;
        if is_write {
            if !connection.allow_writes || !permission.allow_writes {
                return Err(failure(ErrorCode::WriteNotAllowed));
            }
            before_publication(
                lease,
                self.policy.consume_write_approval(
                    &connection,
                    caller,
                    request,
                    &value_hash(
                        &serde_json::to_value(request)
                            .map_err(|_| failure(ErrorCode::InvalidArgument))?,
                    ),
                ),
            )
            .await?;
        }
        let mut context = DriverContext {
            connection_id: connection.id.clone(),
            request_id: request.request_id.clone(),
            deadline_ms: lease.deadline_at_ms.saturating_sub(now_ms()),
            signal: lease.signal(),
            transient_auth: None,
            read_only: !is_write,
            session_handle: None,
        };
        match request.operation {
            Operation::ConnectionUpdate => {
                before_publication(
                    lease,
                    self.policy.authorize_connection_change(caller, request),
                )
                .await?;
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
                let settings = patch.get("settings").unwrap_or(&connection.settings);
                validate_connection_configuration(&connection.driver, settings)?;
                self.connections
                    .validate_settings(&connection.driver, settings)
                    .await?;
                let updated = self
                    .connections
                    .update_guarded(
                        &connection.id,
                        request
                            .expected_version
                            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
                        patch.clone(),
                        lease,
                    )
                    .await?;
                lease.mark_write_confirmed()?;
                if patch.get("settings").is_some()
                    || patch.get("credential_ref").is_some()
                    || patch.get("allow_writes").is_some()
                {
                    self.auth.invalidate(&connection.id, false).await?;
                }
                return Ok(success(request, public_connection(&updated)));
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
        if !is_write
            && self.driver.read_only_guarantee(&connection).await? == ReadOnlyGuarantee::Unavailable
        {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        context.transient_auth = self.auth.context(&connection, context.deadline_ms).await?;
        context.deadline_ms = lease.deadline_at_ms.saturating_sub(now_ms());
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
                )?;
                Ok(from_page(request, page))
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
                        || self.driver.read_only_guarantee(connection).await?
                            == ReadOnlyGuarantee::Unavailable
                    {
                        return Err(failure(ErrorCode::CapabilityUnavailable));
                    }
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
                    )?;
                    self.results.finish_next(token, &binding, page)
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
    async fn driver_call(
        &self,
        connection: &SavedConnection,
        request: &ServiceRequest,
        context: DriverContext,
        lease: &JobLease,
        cursor: Option<&super::results::CursorState>,
    ) -> ServiceResult<DriverOutput> {
        let call = async {
            match cursor {
                Some(cursor) => {
                    self.driver
                        .continue_cursor(connection, cursor, context)
                        .await
                }
                None => self.driver.execute(connection, request, context).await,
            }
        };
        match tokio::time::timeout(
            Duration::from_millis(lease.deadline_at_ms.saturating_sub(now_ms())),
            call,
        )
        .await
        {
            Ok(Ok(output)) if output.confirmed_cancelled => {
                let mut error = failure(ErrorCode::Cancelled);
                error.outcome = Outcome::NotApplied;
                if output.session_closed {
                    error.details = json!({"session_closed":true});
                }
                Err(error)
            }
            Ok(Err(mut error)) => {
                if lease.write_dispatched()
                    && (error.outcome == Outcome::Unknown
                        || (error.outcome == Outcome::NotStarted
                            && matches!(
                                error.code,
                                ErrorCode::DriverExited
                                    | ErrorCode::DeadlineExceeded
                                    | ErrorCode::OutcomeUnknown
                            )))
                {
                    error.code = ErrorCode::OutcomeUnknown;
                    error.outcome = Outcome::Unknown;
                }
                Err(error)
            }
            Ok(Ok(output)) => Ok(output),
            Err(_) => {
                lease.signal().cancel();
                let _ = self
                    .driver
                    .cancel(&connection.id, &request.request_id)
                    .await;
                let mut error = failure(ErrorCode::DeadlineExceeded);
                if lease.write_dispatched() {
                    error = failure(ErrorCode::OutcomeUnknown);
                    error.outcome = Outcome::Unknown;
                }
                Err(error)
            }
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
            let id = self.results.insert_reserved_with_origin(
                binding.clone(),
                snapshot,
                reservation,
                origin,
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
                | Operation::AuthStatus
                | Operation::AuthCancel
                | Operation::AuthLogout
        );
        let lease = match self.jobs.begin(request, caller, false, control).await {
            Ok(lease) => lease,
            Err(error) => return error_response(request, error),
        };
        let is_write = match self.classify(request, caller, &lease).await {
            Ok(value) => value,
            Err(error) => {
                return lease
                    .finish(error_response(request, error.clone()))
                    .unwrap_or_else(|_| error_response(request, error))
            }
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
        let execution = match tokio::time::timeout(
            Duration::from_millis(lease.deadline_at_ms.saturating_sub(now_ms())),
            self.execute(request, caller, &lease, is_write),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                lease.signal().cancel();
                if let Some(id) = &request.connection_id {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(2),
                        self.driver.cancel(id, &request.request_id),
                    )
                    .await;
                }
                Err(failure(ErrorCode::DeadlineExceeded))
            }
        };
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
                                    | ErrorCode::DeadlineExceeded
                                    | ErrorCode::OutcomeUnknown
                            )))
                {
                    error.code = ErrorCode::OutcomeUnknown;
                    error.outcome = Outcome::Unknown;
                }
                error_response(request, error)
            }
        };
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
        if let Err(error) = self.validate(&request) {
            self.secret_inputs.discard(&caller, &request.request_id);
            return error_response(&request, error);
        }
        let control = matches!(
            request.operation,
            Operation::JobGet
                | Operation::JobResult
                | Operation::JobCancel
                | Operation::AuthStatus
                | Operation::AuthCancel
                | Operation::AuthLogout
        );
        if control {
            return self.call_validated(&request, &caller).await;
        }
        let mut lease = match self.jobs.queue(&request, &caller, false, false) {
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
                Ok(()) => match runtime.classify(&request, &caller, &lease).await {
                    Ok(is_write) => {
                        runtime.complete(&request, &caller, lease, is_write).await;
                    }
                    Err(error) => {
                        let _ = lease.finish(error_response(&request, error));
                    }
                },
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
    async fn classify(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
    ) -> ServiceResult<bool> {
        tokio::time::timeout(
            Duration::from_millis(lease.deadline_at_ms.saturating_sub(now_ms())),
            self.is_write(request, caller),
        )
        .await
        .map_err(|_| failure(ErrorCode::DeadlineExceeded))?
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

struct SecretInputCleanup<'a> {
    inputs: &'a SecretInputs,
    caller: &'a CallerContext,
    request: &'a ServiceRequest,
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
fn public_connection(connection: &SavedConnection) -> Value {
    json!({"connection_id":connection.id,"name":connection.name,"driver":connection.driver,"settings":sanitized_settings(&connection.settings),"credential_ref":connection.credential_ref,"version":connection.version,"allow_writes":connection.allow_writes})
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
    if matches!(
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
    if extra.get("auth_source").is_some() {
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
