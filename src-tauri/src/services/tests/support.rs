use super::super::{
    auth::{AuthConfig, AuthLifecycle, OAuthGrant},
    backend::*,
    contracts::*,
    credentials::{Secret, SecretStore, SessionSecretStore},
    dispatch::*,
    error::{failure, ServiceResult},
    oauth::OAuthProvider,
    results::*,
    runtime::ServiceRuntime,
    storage::now_ms,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

pub struct TestDirectory(pub PathBuf);
impl TestDirectory {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!("tabularis-h1-test-{}", uuid::Uuid::new_v4())))
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn caller() -> CallerContext {
    CallerContext {
        transport: CallerTransport::Cli,
        caller_id: "caller".into(),
        owner_instance_id: "owner".into(),
    }
}
pub fn connection(id: &str) -> SavedConnection {
    SavedConnection {
        id: id.into(),
        name: id.into(),
        driver: "postgresql".into(),
        settings: json!({"host":"localhost","database":"fixture"}),
        credential_ref: None,
        version: 1,
        allow_writes: false,
        auth_revision: "auth-1".into(),
    }
}
pub fn request(operation: Operation, input: Value) -> ServiceRequest {
    ServiceRequest {
        protocol_version: 1,
        operation,
        request_id: uuid::Uuid::new_v4().to_string(),
        connection_id: Some("conn-a".into()),
        session_id: None,
        map_id: None,
        expected_version: None,
        deadline_ms: 30000,
        input,
    }
}
pub fn query(text: &str) -> ServiceRequest {
    request(
        Operation::QueryExecute,
        json!({"language":"sql","text":text,"parameters":[],"page_size":10}),
    )
}
pub fn snapshot(rows: Vec<Value>) -> ResultSnapshot {
    ResultSnapshot {
        sets: vec![ResultSet {
            kind: ResultKind::Tabular,
            columns: vec![json!({"name":"value","type":"integer"})],
            rows,
        }],
        revision: "revision-1".into(),
        limits: Limits::default(),
        allow_truncation: true,
        warnings: Vec::new(),
    }
}
pub fn binding() -> CursorBinding {
    CursorBinding {
        connection_id: "conn-a".into(),
        permission_scope: "caller".into(),
        auth_revision: "auth-1".into(),
        query_hash: "query-hash".into(),
        result_revision: "revision-1".into(),
        sdk_version: None,
        container_rid: None,
        endpoint_identity: None,
        partition_hash: None,
    }
}
pub fn context(id: &str) -> DriverContext {
    DriverContext {
        connection_id: id.into(),
        request_id: uuid::Uuid::new_v4().to_string(),
        deadline_ms: 30000,
        signal: Default::default(),
        transient_auth: None,
        read_only: true,
        session_handle: None,
    }
}

pub struct TestConnections {
    pub saved: Mutex<HashMap<String, SavedConnection>>,
    pub writes: AtomicUsize,
}
impl TestConnections {
    pub fn new() -> Self {
        Self {
            saved: Mutex::new(
                [
                    ("conn-a".into(), connection("conn-a")),
                    ("conn-b".into(), connection("conn-b")),
                ]
                .into(),
            ),
            writes: AtomicUsize::new(0),
        }
    }
}
#[async_trait]
impl ConnectionBackend for TestConnections {
    async fn rotate_auth_revision(&self, id: &str) -> ServiceResult<String> {
        let mut saved = self.saved.lock().unwrap();
        let connection = saved
            .get_mut(id)
            .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))?;
        connection.auth_revision = uuid::Uuid::new_v4().to_string();
        Ok(connection.auth_revision.clone())
    }
    async fn validate_settings(&self, _driver: &str, settings: &Value) -> ServiceResult<()> {
        let object = settings
            .as_object()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        if object.iter().any(|(key, value)| {
            !matches!(
                key.as_str(),
                "host" | "database" | "ssl_mode" | "extra" | "endpoint"
            ) || (key != "extra" && !value.is_string())
        }) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if let Some(extra) = object.get("extra") {
            if extra.as_object().map_or(true, |object| {
                object.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "auth_mode" | "tenant_id" | "client_id" | "trust_server_certificate"
                    )
                })
            }) {
                return Err(failure(ErrorCode::InvalidArgument));
            }
        }
        Ok(())
    }
    async fn list(&self) -> ServiceResult<Vec<SavedConnection>> {
        Ok(self.saved.lock().unwrap().values().cloned().collect())
    }
    async fn get(&self, id: &str) -> ServiceResult<SavedConnection> {
        self.saved
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))
    }
    async fn create(
        &self,
        name: &str,
        driver: &str,
        settings: Value,
        credential_ref: Option<String>,
    ) -> ServiceResult<SavedConnection> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        let mut value = connection(&uuid::Uuid::new_v4().to_string());
        value.name = name.into();
        value.driver = driver.into();
        value.settings = settings;
        value.credential_ref = credential_ref;
        self.saved
            .lock()
            .unwrap()
            .insert(value.id.clone(), value.clone());
        Ok(value)
    }
    async fn update(
        &self,
        id: &str,
        expected: u64,
        patch: Value,
    ) -> ServiceResult<SavedConnection> {
        let mut saved = self.saved.lock().unwrap();
        let value = saved
            .get_mut(id)
            .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))?;
        if value.version != expected {
            let mut error = failure(ErrorCode::VersionConflict);
            error.details = json!({"current_version":value.version});
            return Err(error);
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        if let Some(name) = patch["name"].as_str() {
            value.name = name.into();
        }
        if let Some(settings) = patch.get("settings") {
            value.settings = settings.clone();
        }
        value.version += 1;
        Ok(value.clone())
    }
    async fn delete(&self, id: &str, expected: u64) -> ServiceResult<()> {
        let mut saved = self.saved.lock().unwrap();
        if saved
            .get(id)
            .map_or(true, |value| value.version != expected)
        {
            return Err(failure(ErrorCode::VersionConflict));
        }
        saved.remove(id);
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

pub struct TestPolicy {
    pub allowed: AtomicBool,
    pub writes_allowed: AtomicBool,
    pub approved: AtomicBool,
    pub write_intent: AtomicBool,
    pub approvals: AtomicUsize,
}
impl TestPolicy {
    pub fn new() -> Self {
        Self {
            allowed: AtomicBool::new(true),
            writes_allowed: AtomicBool::new(false),
            approved: AtomicBool::new(false),
            write_intent: AtomicBool::new(false),
            approvals: AtomicUsize::new(0),
        }
    }
}
#[async_trait]
impl PolicyBackend for TestPolicy {
    async fn permission(
        &self,
        _connection: &SavedConnection,
        caller: &CallerContext,
    ) -> ServiceResult<PermissionScope> {
        if !self.allowed.load(Ordering::SeqCst) {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        Ok(PermissionScope {
            identity: caller.caller_id.clone(),
            allow_writes: self.writes_allowed.load(Ordering::SeqCst),
        })
    }
    async fn authorize_connection_change(
        &self,
        _caller: &CallerContext,
        _request: &ServiceRequest,
    ) -> ServiceResult<()> {
        if self.allowed.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(failure(ErrorCode::PermissionDenied))
        }
    }
    async fn consume_write_approval(
        &self,
        _connection: &SavedConnection,
        _caller: &CallerContext,
        _request: &ServiceRequest,
        _hash: &str,
    ) -> ServiceResult<()> {
        self.approvals.fetch_add(1, Ordering::SeqCst);
        if self.approved.swap(false, Ordering::SeqCst) {
            Ok(())
        } else {
            Err(failure(ErrorCode::WriteNotAllowed))
        }
    }
    async fn query_is_write(
        &self,
        _caller: &CallerContext,
        _request: &ServiceRequest,
    ) -> ServiceResult<bool> {
        Ok(self.write_intent.swap(false, Ordering::SeqCst))
    }
}

pub struct TestDriver {
    pub calls: AtomicUsize,
    pub cancels: AtomicUsize,
    pub discarded: AtomicUsize,
    pub invalidations: AtomicUsize,
    pub readonly: AtomicBool,
    pub rollback_failure: AtomicBool,
    pub active: AtomicUsize,
    pub peak: AtomicUsize,
    pub contexts: Mutex<Vec<(String, bool, Option<String>)>>,
    sessions: Mutex<HashMap<String, Arc<AtomicUsize>>>,
    pub session_peak: AtomicUsize,
    pub gates: Mutex<HashMap<String, Arc<TestGate>>>,
    pub cancelled_connections: Mutex<Vec<String>>,
}
impl TestDriver {
    pub fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            discarded: AtomicUsize::new(0),
            invalidations: AtomicUsize::new(0),
            readonly: AtomicBool::new(true),
            rollback_failure: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            contexts: Mutex::new(Vec::new()),
            sessions: Mutex::new(HashMap::new()),
            session_peak: AtomicUsize::new(0),
            gates: Mutex::new(HashMap::new()),
            cancelled_connections: Mutex::new(Vec::new()),
        }
    }
}
pub struct TestGate {
    entered: tokio::sync::Notify,
    count: AtomicUsize,
    release: tokio::sync::Semaphore,
}
impl TestGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Notify::new(),
            count: AtomicUsize::new(0),
            release: tokio::sync::Semaphore::new(0),
        })
    }
    pub async fn wait_entered(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = self.entered.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.count.load(Ordering::SeqCst) > 0 {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
    async fn enter(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_waiters();
        let permit = self.release.acquire().await.unwrap();
        permit.forget();
    }
    pub fn release(&self) {
        self.release.add_permits(1);
    }
}
struct ActiveCall<'a>(&'a AtomicUsize, Option<Arc<AtomicUsize>>);
impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
        if let Some(session) = &self.1 {
            session.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
#[async_trait]
impl DriverBackend for TestDriver {
    async fn supports(
        &self,
        _connection: &SavedConnection,
        _operation: Operation,
    ) -> ServiceResult<bool> {
        Ok(true)
    }
    async fn read_only_guarantee(
        &self,
        _connection: &SavedConnection,
    ) -> ServiceResult<ReadOnlyGuarantee> {
        Ok(if self.readonly.load(Ordering::SeqCst) {
            ReadOnlyGuarantee::ReadOnlyTransaction
        } else {
            ReadOnlyGuarantee::Unavailable
        })
    }
    async fn execute(
        &self,
        _connection: &SavedConnection,
        request: &ServiceRequest,
        context: DriverContext,
    ) -> ServiceResult<DriverOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.contexts.lock().unwrap().push((
            context.request_id.clone(),
            context.read_only,
            context.session_handle.clone(),
        ));
        let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(count, Ordering::SeqCst);
        let session = context
            .session_handle
            .as_ref()
            .map(|handle| self.sessions.lock().unwrap().get(handle).unwrap().clone());
        if let Some(session) = &session {
            let count = session.fetch_add(1, Ordering::SeqCst) + 1;
            self.session_peak.fetch_max(count, Ordering::SeqCst);
        }
        let _active = ActiveCall(&self.active, session);
        let gate = request.input["text"]
            .as_str()
            .and_then(|text| self.gates.lock().unwrap().get(text).cloned());
        if let Some(gate) = gate {
            tokio::select! { _ = gate.enter() => {}, _ = context.signal.cancelled() => return Ok(DriverOutput { data: Value::Null, snapshot: None, cursor: None, confirmed_cancelled: true, session_closed: false, metrics: Metrics::default(), limits: Limits::default(), warnings: vec![] }) }
        }
        match request.input["text"].as_str() {
            Some("wait") => {
                context.signal.cancelled().await;
                return Ok(DriverOutput {
                    data: Value::Null,
                    snapshot: None,
                    cursor: None,
                    confirmed_cancelled: true,
                    session_closed: false,
                    metrics: Metrics::default(),
                    limits: Limits::default(),
                    warnings: vec![],
                });
            }
            Some("slow") => tokio::time::sleep(Duration::from_millis(40)).await,
            Some("error") => {
                let mut error = failure(ErrorCode::DriverExited);
                error.message = "canary-password-token-593".into();
                error.details = json!({"access_token":"canary-password-token-593"});
                return Err(error);
            }
            Some("invalid") => return Err(failure(ErrorCode::InvalidArgument)),
            Some("session-cap") => {
                let mut error = failure(ErrorCode::ResourceLimit);
                error.outcome = Outcome::NotApplied;
                error.details = json!({"session_closed":true,"private_token":"synthetic-private"});
                return Err(error);
            }
            Some("native") => {
                return Ok(native_output(
                    vec![json!(1), json!(2)],
                    vec![json!(3), json!(4)],
                    2,
                ))
            }
            Some("native-oversized") => {
                return Ok(native_output(vec![json!(1), json!(2), json!(3)], vec![], 2))
            }
            Some("native-byte-cap") => {
                return Ok(native_output(
                    vec![
                        json!("a".repeat(MAX_PAGE_BYTES / 2 + 1)),
                        json!("b".repeat(MAX_PAGE_BYTES / 2 + 1)),
                    ],
                    vec![],
                    2,
                ))
            }
            _ => {}
        }
        Ok(DriverOutput {
            data: if request.input["text"] == "oversized" {
                json!({"blob":"a".repeat(MAX_PAGE_BYTES+1)})
            } else {
                json!({"affected_rows":1})
            },
            snapshot: if request.operation == Operation::QueryExecute
                && request.input["text"] != "oversized"
            {
                Some(snapshot((0..30).map(|row| json!([row])).collect()))
            } else {
                None
            },
            cursor: None,
            confirmed_cancelled: false,
            session_closed: false,
            metrics: Metrics {
                request_charge: Some(2.0),
                ..Metrics::default()
            },
            limits: Limits {
                truncated: true,
                reasons: vec!["source_limit".into()],
            },
            warnings: vec!["source_warning".into()],
        })
    }
    async fn continue_cursor(
        &self,
        _connection: &SavedConnection,
        state: &CursorState,
        _context: DriverContext,
    ) -> ServiceResult<DriverOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(native_output(state.tail.clone(), vec![], state.page_size))
    }
    async fn cancel(&self, connection_id: &str, _request_id: &str) -> ServiceResult<()> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        self.cancelled_connections
            .lock()
            .unwrap()
            .push(connection_id.into());
        Ok(())
    }
    async fn reserve_session(
        &self,
        connection: &SavedConnection,
        _database: &str,
        _context: DriverContext,
    ) -> ServiceResult<String> {
        let gate = self
            .gates
            .lock()
            .unwrap()
            .get(&format!("open:{}", connection.id))
            .cloned();
        if let Some(gate) = gate {
            gate.enter().await;
        }
        let handle = uuid::Uuid::new_v4().to_string();
        self.sessions
            .lock()
            .unwrap()
            .insert(handle.clone(), Arc::new(AtomicUsize::new(0)));
        Ok(handle)
    }
    async fn finish_session(
        &self,
        _connection: &SavedConnection,
        handle: &str,
        _finish: SessionFinish,
        _context: DriverContext,
    ) -> ServiceResult<()> {
        if self.rollback_failure.load(Ordering::SeqCst) {
            return Err(failure(ErrorCode::OutcomeUnknown));
        }
        self.sessions.lock().unwrap().remove(handle);
        Ok(())
    }
    async fn discard_session(&self, _connection: &str, handle: &str) {
        self.discarded.fetch_add(1, Ordering::SeqCst);
        self.sessions.lock().unwrap().remove(handle);
    }
    async fn invalidate_auth(&self, _connection_id: &str, _principal: Option<&str>) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}

fn native_output(rows: Vec<Value>, tail: Vec<Value>, page_size: usize) -> DriverOutput {
    let mut scope = binding();
    scope.sdk_version = Some("synthetic-sdk-1".into());
    scope.container_rid = Some("synthetic-container".into());
    scope.endpoint_identity = Some("synthetic-endpoint".into());
    scope.partition_hash = Some("synthetic-partition".into());
    DriverOutput {
        data: Value::Null,
        snapshot: Some(ResultSnapshot {
            sets: vec![ResultSet {
                kind: ResultKind::JsonValues,
                columns: vec![],
                rows,
            }],
            revision: "revision-1".into(),
            limits: Limits::default(),
            allow_truncation: false,
            warnings: vec![],
        }),
        cursor: Some(CursorState {
            binding: scope,
            native_state: json!({"continuation":"synthetic-sdk-private"}),
            tail,
            columns: vec![],
            page_size,
            expires_at_ms: now_ms() + RESULT_TTL_MS,
        }),
        confirmed_cancelled: false,
        session_closed: false,
        metrics: Metrics::default(),
        limits: Limits::default(),
        warnings: vec![],
    }
}

pub struct TestOAuth {
    pub exchanges: AtomicUsize,
    pub refreshes: AtomicUsize,
    pub short_expiry: AtomicBool,
    pub principal: Mutex<String>,
}
impl TestOAuth {
    pub fn new() -> Self {
        Self {
            exchanges: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
            short_expiry: AtomicBool::new(false),
            principal: Mutex::new("11111111-1111-4111-8111-111111111111".into()),
        }
    }
    fn grant(&self) -> OAuthGrant {
        OAuthGrant {
            access_token: Secret::new(b"synthetic-access-canary-581".to_vec()).unwrap(),
            refresh_token: Some(Secret::new(b"synthetic-refresh-canary-582".to_vec()).unwrap()),
            expires_at_ms: now_ms()
                + if self.short_expiry.load(Ordering::SeqCst) {
                    1000
                } else {
                    3_600_000
                },
            principal_id: self.principal.lock().unwrap().clone(),
        }
    }
}
#[async_trait]
impl OAuthProvider for TestOAuth {
    async fn device_begin(
        &self,
        _config: &AuthConfig,
    ) -> ServiceResult<oauth2::StandardDeviceAuthorizationResponse> {
        serde_json::from_value(json!({"device_code":"synthetic-device","user_code":"TEST-CODE","verification_uri":"https://microsoft.com/devicelogin","expires_in":900,"interval":1})).map_err(|_| failure(ErrorCode::AuthRequired))
    }
    async fn device_finish(
        &self,
        _config: &AuthConfig,
        _device: oauth2::StandardDeviceAuthorizationResponse,
        signal: CancellationSignal,
    ) -> ServiceResult<OAuthGrant> {
        tokio::select! { _ = signal.cancelled() => Err(failure(ErrorCode::Cancelled)), _ = tokio::time::sleep(Duration::from_millis(20)) => Ok(self.grant()) }
    }
    async fn client_credentials(
        &self,
        _config: &AuthConfig,
        _secret: &Secret,
    ) -> ServiceResult<OAuthGrant> {
        self.exchanges.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(self.grant())
    }
    async fn refresh(
        &self,
        _config: &AuthConfig,
        _refresh: &Secret,
        _previous: Option<&str>,
    ) -> ServiceResult<OAuthGrant> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(self.grant())
    }
}

pub struct TestLifecycle(pub AtomicUsize);
#[async_trait]
impl AuthLifecycle for TestLifecycle {
    async fn principal_changed(
        &self,
        _connection: &str,
        _old: Option<&str>,
        _new: &str,
    ) -> ServiceResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn logout(&self, _connection: &str, _old: Option<&str>) -> ServiceResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

pub struct Harness {
    pub directory: TestDirectory,
    pub runtime: Arc<ServiceRuntime>,
    pub dispatcher: ServiceDispatcher,
    pub connections: Arc<TestConnections>,
    pub policy: Arc<TestPolicy>,
    pub driver: Arc<TestDriver>,
    pub secrets: Arc<SessionSecretStore>,
}
impl Harness {
    pub fn new() -> Self {
        let directory = TestDirectory::new();
        let connections = Arc::new(TestConnections::new());
        let policy = Arc::new(TestPolicy::new());
        let driver = Arc::new(TestDriver::new());
        let secrets = Arc::new(SessionSecretStore::default());
        let oauth = Arc::new(TestOAuth::new());
        let runtime = ServiceRuntime::new(
            directory.0.clone(),
            "owner".into(),
            connections.clone(),
            policy.clone(),
            driver.clone(),
            secrets.clone(),
            oauth.clone(),
        )
        .unwrap();
        let mut dispatcher = ServiceDispatcher::new().unwrap();
        runtime.register(&mut dispatcher);
        Self {
            directory,
            runtime,
            dispatcher,
            connections,
            policy,
            driver,
            secrets,
        }
    }
    pub fn allow_write(&self) {
        self.connections
            .saved
            .lock()
            .unwrap()
            .get_mut("conn-a")
            .unwrap()
            .allow_writes = true;
        self.policy.writes_allowed.store(true, Ordering::SeqCst);
        self.policy.approved.store(true, Ordering::SeqCst);
    }
    pub async fn call(&self, request: &ServiceRequest) -> ServiceResponse {
        self.dispatcher.dispatch(request, &caller()).await
    }
}

pub fn entra_connection(mode: &str) -> SavedConnection {
    let mut connection = connection("conn-a");
    connection.settings = json!({"host":"fixture.database.windows.net","database":"fixture","ssl_mode":"verify-full","extra":{"auth_mode":mode,"tenant_id":"22222222-2222-4222-8222-222222222222","client_id":"33333333-3333-4333-8333-333333333333"}});
    connection
}
pub async fn bound_refresh(
    secrets: &dyn SecretStore,
    connection: &SavedConnection,
    principal: &str,
) {
    let config = AuthConfig::from_connection(connection).unwrap().unwrap();
    secrets.set(&connection.id, super::super::credentials::SecretSlot::EntraRefreshToken, Secret::new(serde_json::to_vec(&json!({"refresh_token":"synthetic-initial-refresh","tenant_id":config.tenant_id,"client_id":config.client_id,"scope":config.scope,"credential_identity":config.credential_identity,"principal_id":principal})).unwrap()).unwrap()).await.unwrap();
}
