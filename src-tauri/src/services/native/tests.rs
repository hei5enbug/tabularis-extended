use super::*;
use crate::service_spatial::{ExportDestination, ExportStats, SpatialExportSinkFactory};
use crate::{
    drivers::driver_trait::*,
    models::*,
    services::{
        auth::{AuthConfig, OAuthGrant},
        backend::{CancellationSignal, ConnectionBackend, DriverContext},
        contracts::{JobStatus, Outcome},
        credentials::{Secret, SecretSlot, SecretStore},
        oauth::OAuthProvider,
        results::CursorBinding,
        transport::TransportArtifacts,
    },
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

pub(crate) struct Directory(pub PathBuf);
impl Directory {
    fn new() -> Self {
        #[cfg(target_os = "macos")]
        let root = PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let root = std::env::temp_dir();
        Self(root.join(format!("n-{}", uuid::Uuid::new_v4().simple())))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
/// What the scripted driver does and records. A query that is held waits for
/// `release` and, unless `ignore_cancel` is set, ends with `CANCELLED` when it
/// is asked to cancel.
pub(crate) struct DriverState {
    pub calls: Mutex<Vec<(ServiceRpcMethod, Value, bool)>>,
    pub legacy: AtomicUsize,
    pub capability_error: Mutex<Option<ErrorCode>>,
    pub unknown_count: AtomicBool,
    /// A driver that does not offer estimated plans.
    pub explain_unsupported: AtomicBool,
    /// A driver whose database gives no read-only guarantee.
    pub read_only_unavailable: AtomicBool,
    /// Databases the catalog lists; `fixture` when empty.
    pub databases: Mutex<Vec<String>>,
    /// The database each service call ran against.
    pub params_databases: Mutex<Vec<String>>,
    pub executions: AtomicUsize,
    /// Rows a query returns; ten when zero.
    pub rows: AtomicUsize,
    /// Bytes of text next to each row's number; none when zero.
    pub cell_bytes: AtomicUsize,
    /// Objects the catalog lists; two when zero.
    pub objects: AtomicUsize,
    /// Columns the catalog describes; three when zero.
    pub columns: AtomicUsize,
    /// The reply to `catalog.describe` when set.
    pub described: Mutex<Option<Value>>,
    pub hold: AtomicBool,
    pub entered: AtomicUsize,
    pub ignore_cancel: AtomicBool,
    pub cancel_fails: AtomicBool,
    pub cancels: AtomicUsize,
    released: tokio::sync::Semaphore,
    announce: tokio::sync::Notify,
    cancelled: Mutex<std::collections::HashSet<String>>,
    cancel_signal: tokio::sync::Notify,
}
impl Default for DriverState {
    fn default() -> Self {
        Self {
            calls: Default::default(),
            legacy: Default::default(),
            capability_error: Default::default(),
            unknown_count: Default::default(),
            explain_unsupported: Default::default(),
            read_only_unavailable: Default::default(),
            databases: Default::default(),
            params_databases: Default::default(),
            executions: Default::default(),
            rows: Default::default(),
            cell_bytes: Default::default(),
            objects: Default::default(),
            columns: Default::default(),
            described: Default::default(),
            hold: Default::default(),
            entered: Default::default(),
            ignore_cancel: Default::default(),
            cancel_fails: Default::default(),
            cancels: Default::default(),
            released: tokio::sync::Semaphore::new(0),
            announce: Default::default(),
            cancelled: Default::default(),
            cancel_signal: Default::default(),
        }
    }
}
impl DriverState {
    pub fn release(&self, jobs: usize) {
        self.released.add_permits(jobs);
    }
    pub async fn wait_entered(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = self.announce.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.entered.load(Ordering::SeqCst) >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("the held query did not reach the driver");
    }
    async fn cancel_requested(&self, request: &str) {
        loop {
            let notified = self.cancel_signal.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.cancelled.lock().unwrap().contains(request) {
                return;
            }
            notified.await;
        }
    }
    async fn hold_query(&self, request: &str) -> ServiceResult<()> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.announce.notify_waiters();
        let ignore = self.ignore_cancel.load(Ordering::SeqCst);
        tokio::select! {
            permit = self.released.acquire() => { permit.unwrap().forget(); Ok(()) },
            _ = self.cancel_requested(request), if !ignore => Err(failure(ErrorCode::Cancelled)),
        }
    }
}
struct MockDriver {
    manifest: PluginManifest,
    state: Arc<DriverState>,
}
impl MockDriver {
    fn legacy<T>(&self) -> Result<T, String> {
        self.state.legacy.fetch_add(1, Ordering::SeqCst);
        Err("Legacy path was not expected".into())
    }
}
#[async_trait]
impl DatabaseDriver for MockDriver {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    fn get_data_types(&self) -> Vec<DataTypeInfo> {
        vec![]
    }
    fn build_connection_url(&self, _: &ConnectionParams) -> Result<String, String> {
        self.legacy()
    }
    async fn test_connection(&self, _: &ConnectionParams) -> Result<(), String> {
        self.legacy()
    }
    async fn get_databases(&self, _: &ConnectionParams) -> Result<Vec<String>, String> {
        self.legacy()
    }
    async fn get_schemas(&self, _: &ConnectionParams) -> Result<Vec<String>, String> {
        self.legacy()
    }
    async fn get_tables(
        &self,
        _: &ConnectionParams,
        _: Option<&str>,
    ) -> Result<Vec<TableInfo>, String> {
        self.legacy()
    }
    async fn get_columns(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: Option<&str>,
    ) -> Result<Vec<TableColumn>, String> {
        self.legacy()
    }
    async fn get_foreign_keys(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: Option<&str>,
    ) -> Result<Vec<ForeignKey>, String> {
        self.legacy()
    }
    async fn get_indexes(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: Option<&str>,
    ) -> Result<Vec<Index>, String> {
        self.legacy()
    }
    async fn get_views(
        &self,
        _: &ConnectionParams,
        _: Option<&str>,
    ) -> Result<Vec<ViewInfo>, String> {
        self.legacy()
    }
    async fn get_view_definition(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: Option<&str>,
    ) -> Result<String, String> {
        self.legacy()
    }
    async fn get_view_columns(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: Option<&str>,
    ) -> Result<Vec<TableColumn>, String> {
        self.legacy()
    }
    async fn create_view(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<(), String> {
        self.legacy()
    }
    async fn alter_view(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<(), String> {
        self.legacy()
    }
    async fn drop_view(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: Option<&str>,
    ) -> Result<(), String> {
        self.legacy()
    }
    async fn get_routines(
        &self,
        _: &ConnectionParams,
        _: Option<&str>,
    ) -> Result<Vec<RoutineInfo>, String> {
        self.legacy()
    }
    async fn get_routine_parameters(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<Vec<RoutineParameter>, String> {
        self.legacy()
    }
    async fn get_routine_definition(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, String> {
        self.legacy()
    }
    async fn execute_query(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: Option<u32>,
        _: u32,
        _: Option<&str>,
    ) -> Result<QueryResult, String> {
        self.legacy()
    }
    async fn insert_record(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: HashMap<String, Value>,
        _: Option<&str>,
        _: u64,
    ) -> Result<u64, String> {
        self.legacy()
    }
    async fn update_record(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: &HashMap<String, Value>,
        _: &str,
        _: Value,
        _: Option<&str>,
        _: u64,
    ) -> Result<u64, String> {
        self.legacy()
    }
    async fn delete_record(
        &self,
        _: &ConnectionParams,
        _: &str,
        _: &HashMap<String, Value>,
        _: Option<&str>,
    ) -> Result<u64, String> {
        self.legacy()
    }
    async fn get_schema_snapshot(
        &self,
        _: &ConnectionParams,
        _: Option<&str>,
    ) -> Result<Vec<TableSchema>, String> {
        self.legacy()
    }
    async fn get_all_columns_batch(
        &self,
        _: &ConnectionParams,
        _: Option<&str>,
    ) -> Result<HashMap<String, Vec<TableColumn>>, String> {
        self.legacy()
    }
    async fn get_all_foreign_keys_batch(
        &self,
        _: &ConnectionParams,
        _: Option<&str>,
    ) -> Result<HashMap<String, Vec<ForeignKey>>, String> {
        self.legacy()
    }
    async fn service_capabilities(&self) -> ServiceResult<DriverServiceCapabilities> {
        if let Some(code) = *self.state.capability_error.lock().unwrap() {
            return Err(failure(code));
        }
        Ok(DriverServiceCapabilities {
            service_protocol: 1,
            exact_query: true,
            spatial: false,
            documents: false,
            query_page: false,
            cancel: true,
            auth_refresh: false,
            sessions: false,
            catalog: true,
            rows: true,
            explain: !self.state.explain_unsupported.load(Ordering::SeqCst),
            exact_affected_rows: true,
            read_only: if self.state.read_only_unavailable.load(Ordering::SeqCst) {
                DriverServiceReadOnly::Unavailable
            } else {
                DriverServiceReadOnly::ReadOnlyTransaction
            },
        })
    }
    async fn service_call(
        &self,
        method: ServiceRpcMethod,
        params: &ConnectionParams,
        context: &DriverContext,
        input: Value,
    ) -> ServiceResult<Value> {
        self.state
            .calls
            .lock()
            .unwrap()
            .push((method, input.clone(), context.read_only));
        self.state
            .params_databases
            .lock()
            .unwrap()
            .push(params.database.primary().to_owned());
        Ok(match method {
            ServiceRpcMethod::ExactQuery => {
                self.state.executions.fetch_add(1, Ordering::SeqCst);
                if self.state.hold.load(Ordering::SeqCst) {
                    self.state.hold_query(&context.request_id).await?;
                }
                let rows = match self.state.rows.load(Ordering::SeqCst) {
                    0 => 10,
                    rows => rows,
                };
                let cell = match self.state.cell_bytes.load(Ordering::SeqCst) {
                    0 => Value::Null,
                    bytes => json!("x".repeat(bytes)),
                };
                json!({"columns":["duplicate","duplicate"],"column_metadata":[{"native_type":"int4"},{"native_type":"text"}],"rows":(0..rows).map(|n|json!([n,cell])).collect::<Vec<_>>(),"affected_rows":null})
            }
            ServiceRpcMethod::Catalog => match input["operation"].as_str().unwrap_or("") {
                "catalog.databases" => {
                    let databases = self.state.databases.lock().unwrap().clone();
                    if databases.is_empty() {
                        json!({"databases":["fixture"]})
                    } else {
                        json!({"databases":databases})
                    }
                }
                "catalog.schemas" => json!({"schemas":["public"]}),
                "catalog.objects" => match self.state.objects.load(Ordering::SeqCst) {
                    0 => {
                        json!({"objects":[{"name":"records","kind":"table"},{"name":"records_view","kind":"view"}]})
                    }
                    count => {
                        json!({"objects":(0..count).map(|n|json!({"name":format!("object_{n:04}"),"kind":"table"})).collect::<Vec<_>>()})
                    }
                },
                "catalog.describe" => match self.state.columns.load(Ordering::SeqCst) {
                    _ if self.state.described.lock().unwrap().is_some() => {
                        self.state.described.lock().unwrap().clone().unwrap_or_default()
                    }
                    0 => {
                        json!({"columns":[{"name":"b","native_type":"text","is_primary_key":true,"primary_key_position":2},{"name":"a","native_type":"int4","is_primary_key":true,"primary_key_position":1},{"name":"payload","native_type":"jsonb"}]})
                    }
                    count => {
                        json!({"columns":(0..count).map(|n|json!({"name":format!("column_{n:03}"),"native_type":"varchar","is_nullable":true})).collect::<Vec<_>>()})
                    }
                },
                _ => return Err(failure(ErrorCode::UnsupportedOperation)),
            },
            ServiceRpcMethod::Row => {
                json!({"affected_rows":if self.state.unknown_count.load(Ordering::SeqCst){Value::Null}else{json!(1)}})
            }
            ServiceRpcMethod::Test => json!({"success":true}),
            ServiceRpcMethod::Explain => {
                if self.manifest.capabilities.sql_dialect == Some(SqlDialect::Mssql) {
                    json!({"engine":"sqlserver","format":"sqlserver-showplan-xml","payload":"<ShowPlanXML xmlns=\"http://schemas.microsoft.com/sqlserver/2004/07/showplan\"><RelOp NodeId=\"0\" PhysicalOp=\"Table Scan\" EstimateRows=\"1000\" EstimatedTotalSubtreeCost=\"3.5\"><TableScan><Object Schema=\"[dbo]\" Table=\"[orders]\"/></TableScan></RelOp></ShowPlanXML>"})
                } else {
                    json!({"engine":"postgres","format":"postgres-json","payload":[{"Plan":{"Node Type":"Seq Scan","Relation Name":"orders","Total Cost":35.5,"Plan Rows":1000}}]})
                }
            }
            ServiceRpcMethod::InvalidateAuth => json!({}),
            _ => return Err(failure(ErrorCode::UnsupportedOperation)),
        })
    }
    async fn service_cancel(&self, _: &str, request: &str) -> ServiceResult<()> {
        self.state.cancels.fetch_add(1, Ordering::SeqCst);
        if self.state.cancel_fails.load(Ordering::SeqCst) {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
        self.state.cancelled.lock().unwrap().insert(request.into());
        self.state.cancel_signal.notify_waiters();
        Ok(())
    }
}
#[derive(Default)]
pub(crate) struct SyntheticSecrets(pub AtomicUsize);
#[async_trait]
impl SecretStore for SyntheticSecrets {
    async fn get(&self, _: &str, _: SecretSlot) -> ServiceResult<Option<Secret>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
    async fn set(&self, _: &str, _: SecretSlot, _: Secret) -> ServiceResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn delete(&self, _: &str, _: SecretSlot) -> ServiceResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct SyntheticOAuth;
#[async_trait]
impl OAuthProvider for SyntheticOAuth {
    async fn device_begin(
        &self,
        _: &AuthConfig,
    ) -> ServiceResult<oauth2::StandardDeviceAuthorizationResponse> {
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn device_finish(
        &self,
        _: &AuthConfig,
        _: oauth2::StandardDeviceAuthorizationResponse,
        _: CancellationSignal,
    ) -> ServiceResult<OAuthGrant> {
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn client_credentials(&self, _: &AuthConfig, _: &Secret) -> ServiceResult<OAuthGrant> {
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn refresh(
        &self,
        _: &AuthConfig,
        _: &Secret,
        _: Option<&str>,
    ) -> ServiceResult<OAuthGrant> {
        Err(failure(ErrorCode::AuthRequired))
    }
}
pub(crate) struct Harness {
    pub directory: Directory,
    pub core: Arc<NativeCore>,
    pub id: String,
    pub state: Arc<DriverState>,
    pub secrets: Arc<SyntheticSecrets>,
}
impl Harness {
    pub async fn new() -> Self {
        Self::with_manifest(|_| {}).await
    }
    /// The same fixture with the driver's manifest edited before it is
    /// registered, to stand in for another kind of driver.
    pub async fn with_manifest(edit: impl FnOnce(&mut PluginManifest)) -> Self {
        let directory = Directory::new();
        let state = Arc::new(DriverState::default());
        let secrets = Arc::new(SyntheticSecrets::default());
        let mut manifest = crate::drivers::postgres::PostgresDriver::new()
            .manifest()
            .clone();
        manifest.id = format!("native-{}", uuid::Uuid::new_v4());
        edit(&mut manifest);
        let driver_id = manifest.id.clone();
        crate::drivers::registry::register_driver(MockDriver {
            manifest,
            state: state.clone(),
        })
        .await;
        crate::persistence::service_write_atomic(
            &directory.0.join("config.json"),
            br#"{"mcpReadonlyDefault":false}"#,
        )
        .unwrap();
        let platform = ServicePlatform::isolated(
            directory.0.clone(),
            uuid::Uuid::new_v4().to_string(),
            secrets.clone(),
            Arc::new(SyntheticOAuth),
            Arc::new(crate::service_platform::NoopParamExpander),
        )
        .unwrap();
        let connection = platform
            .connections
            .create(
                "Fixture",
                &driver_id,
                json!({"host":"127.0.0.1","database":"fixture","save_in_keychain":false}),
                None,
            )
            .await
            .unwrap();
        let core = NativeCore::compose(platform, directory.0.clone(), EndpointKind::Cli, None)
            .await
            .unwrap();
        Self {
            directory,
            core,
            id: connection.id,
            state,
            secrets,
        }
    }
    pub fn raw(&self, operation: Operation, input: Value) -> Value {
        let mut request = catalog::request(operation, Some(self.id.clone()), input);
        request.deadline_ms = 10_000;
        serde_json::to_value(request).unwrap()
    }
    pub fn query(&self, text: &str) -> Value {
        self.raw(
            Operation::QueryExecute,
            json!({"language":"sql","text":text,"parameters":[],"page_size":2,"mode":"exact"}),
        )
    }
    pub async fn allow_write(&self) {
        let connection = self.core.platform.connections.get(&self.id).await.unwrap();
        self.core
            .platform
            .connections
            .update(&self.id, connection.version, json!({"allow_writes":true}))
            .await
            .unwrap();
    }
}
pub(crate) async fn approve_waiting(h: &Harness) {
    let pending = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(pending) = crate::ai_approval::list_pending_in(&h.directory.0)
                .unwrap()
                .into_iter()
                .next()
            {
                break pending;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    h.core
        .approve(
            &pending.id,
            "approve",
            None,
            &h.core.caller(CallerTransport::Gui).unwrap(),
        )
        .unwrap();
}

#[test]
fn 실제_등록과_내부_주소와_탭이_일치하는_창만_서비스_권한을_얻는다() {
    // given
    let cases = [
        ("main", "tauri://localhost/", None, true),
        (
            "results-window-a",
            "tauri://localhost/results-window?tab=a",
            None,
            true,
        ),
        (
            "results-window-a",
            "http://localhost:5173/results-window?tab=a",
            Some("http://localhost:5173"),
            true,
        ),
        ("main", "https://foreign.invalid/", None, true),
        (
            "results-window-a",
            "tauri://localhost/results-window?tab=b",
            None,
            true,
        ),
        (
            "results-window-a",
            "tauri://localhost/other?tab=a",
            None,
            true,
        ),
        ("main", "tauri://localhost/", None, false),
    ];
    // when
    let actual =
        cases.map(|(label, url, dev, registered)| window_authority(label, url, dev, registered));
    // then
    assert_eq!(
        actual.iter().map(Result::is_ok).collect::<Vec<_>>(),
        [true, true, true, false, false, false, false]
    );
    assert_eq!(actual[1].as_ref().unwrap(), &WindowAuthority::Detached);
}
#[test]
fn 분리된_결과_창은_읽기와_작업_제어만_허용한다() {
    // given
    let authority = WindowAuthority::Detached;
    let operations = [
        Operation::ResultGet,
        Operation::QueryNext,
        Operation::JobCancel,
        Operation::MapList,
        Operation::QueryExecute,
        Operation::MapOpen,
        Operation::CredentialImport,
    ];
    // when
    let actual = (
        operations.map(|operation| authority.operation(operation).is_ok()),
        authority.main_only().unwrap_err().code,
    );
    // then
    assert_eq!(actual.0, [true, true, true, true, false, false, false]);
    assert_eq!(actual.1, ErrorCode::PermissionDenied);
}
async fn immutable_roundtrip(h: &Harness) -> (ServiceResponse, ServiceResponse) {
    let first = h
        .core
        .transport
        .run_one(
            h.query("select '$1; untouched'"),
            h.core.caller(CallerTransport::Cli).unwrap(),
            false,
            None,
        )
        .await
        .unwrap();
    let read = h.raw(
        Operation::ResultGet,
        json!({"result_id":first.result_id,"result_set_index":0,"offset":0,"limit":2}),
    );
    let second = h
        .core
        .transport
        .run_one(
            read,
            h.core.caller(CallerTransport::Mcp).unwrap(),
            false,
            None,
        )
        .await
        .unwrap();
    (first, second)
}
#[tokio::test]
async fn 네이티브_합성은_정확한_쿼리와_불변_참조를_같은_사용자에게_공유한다() {
    // given
    let h = Harness::new().await;
    // when
    let actual = immutable_roundtrip(&h).await;
    // then
    assert_eq!(actual.0.status, JobStatus::Succeeded);
    assert_eq!(actual.0.data["rows"], actual.1.data["rows"]);
    assert_eq!(
        h.state
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.0 == ServiceRpcMethod::ExactQuery)
            .count(),
        1
    );
    assert_eq!(
        h.state.calls.lock().unwrap()[0].1["text"],
        "select '$1; untouched'"
    );
    assert!(h.state.calls.lock().unwrap()[0].2);
    assert_eq!(h.state.legacy.load(Ordering::SeqCst), 0);
    assert_eq!(h.secrets.0.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn 인증_협상_오류는_구형_드라이버로_우회하지_않는다() {
    // given
    let h = Harness::new().await;
    *h.state.capability_error.lock().unwrap() = Some(ErrorCode::AuthRequired);
    // when
    let actual = h
        .core
        .capabilities(&h.id, &h.core.caller(CallerTransport::Gui).unwrap())
        .await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::AuthRequired);
    assert_eq!(h.state.legacy.load(Ordering::SeqCst), 0);
    assert_eq!(h.secrets.0.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn 읽기_전용_연결이면_명시적_의도도_큐에_넣기_전에_거부한다() {
    // given
    let h = Harness::new().await;
    crate::persistence::service_write_atomic(
        &h.directory.0.join("config.json"),
        br#"{"mcpReadonlyDefault":true}"#,
    )
    .unwrap();
    // when
    let actual = h
        .core
        .transport
        .run_one(
            h.query("delete from records"),
            h.core.caller(CallerTransport::Cli).unwrap(),
            true,
            None,
        )
        .await
        .unwrap();
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert!(h.state.calls.lock().unwrap().is_empty());
}
async fn approved_query(h: &Harness) -> ServiceResponse {
    let core = h.core.clone();
    let raw = h.query("delete from records where a=$1");
    let caller = core.caller(CallerTransport::Cli).unwrap();
    let task = tokio::spawn(async move {
        core.transport
            .run_one(raw, caller, true, None)
            .await
            .unwrap()
    });
    approve_waiting(h).await;
    task.await.unwrap()
}
#[tokio::test]
async fn 쓰기_의도와_저장된_허용과_사람의_승인을_모두_거쳐야_쓰기_경로로_간다() {
    // given
    let h = Harness::new().await;
    h.allow_write().await;
    // when
    let actual = approved_query(&h).await;
    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(h.state.calls.lock().unwrap().len(), 1);
    assert!(!h.state.calls.lock().unwrap()[0].2);
    assert!(crate::ai_approval::list_pending_in(&h.directory.0)
        .unwrap()
        .is_empty());
}
#[test]
fn 복합_기본키의_실제_순서와_중첩_문서값은_보존한다() {
    // given
    let description = json!({"columns":[{"name":"b","native_type":"text","primary_key_position":2},{"name":"a","native_type":"int4","primary_key_position":1}]});
    let keys = HashMap::from([("b".into(), json!("second")), ("a".into(), json!(1))]);
    let values = HashMap::from([(
        "payload".into(),
        json!({"nested":["ST_Point(1,2)","__USE_DEFAULT__"]}),
    )]);
    // when
    let actual = (
        catalog::identity(&catalog::columns(&description).unwrap(), &keys).unwrap(),
        catalog::values(&values).unwrap(),
    );
    // then
    assert_eq!(actual.0, json!({"columns":["a","b"],"values":[1,"second"]}));
    assert_eq!(actual.1["payload"], values["payload"]);
}
#[test]
fn 원시_표현식과_불완전한_기본키는_매개변수_쓰기_전에_거부한다() {
    // given
    let values = [
        HashMap::from([("v".into(), json!("__USE_DEFAULT__"))]),
        HashMap::from([("v".into(), json!("ST_Point(1,2)"))]),
    ];
    // when
    let actual = values.map(|values| catalog::values(&values).unwrap_err().code);
    // then
    assert_eq!(actual, [ErrorCode::InvalidArgument; 2]);
}
async fn approved_rows(h: &Harness) -> Vec<ServiceResult<u64>> {
    let mut results = vec![];
    for operation in [
        Operation::RowInsert,
        Operation::RowUpdate,
        Operation::RowDelete,
    ] {
        let core = h.core.clone();
        let id = h.id.clone();
        let task = tokio::spawn(async move {
            catalog::row(
                &core,
                &id,
                operation,
                catalog::NativeRowInput {
                    table: "records".into(),
                    schema: Some("public".into()),
                    database: None,
                    keys: HashMap::from([("b".into(), json!("second")), ("a".into(), json!(1))]),
                    changes: HashMap::from([("payload".into(), json!({"nested":[1,2]}))]),
                },
            )
            .await
        });
        approve_waiting(h).await;
        results.push(task.await.unwrap());
    }
    results
}
#[tokio::test]
async fn 행_작성과_수정과_삭제는_승인된_원본값과_전체_기본키를_전달한다() {
    // given
    let h = Harness::new().await;
    h.allow_write().await;
    // when
    let actual = approved_rows(&h).await;
    // then
    assert!(actual.iter().all(|result| result.as_ref() == Ok(&1)));
    assert_eq!(
        h.state
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.0 == ServiceRpcMethod::Row)
            .count(),
        3
    );
    assert!(h
        .state
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.0 == ServiceRpcMethod::Row)
        .all(|call| !call.2));
    assert_eq!(h.state.legacy.load(Ordering::SeqCst), 0);
}
async fn unknown_row(h: &Harness) -> ServiceResult<u64> {
    let core = h.core.clone();
    let id = h.id.clone();
    let task = tokio::spawn(async move {
        catalog::row(
            &core,
            &id,
            Operation::RowInsert,
            catalog::NativeRowInput {
                table: "records".into(),
                schema: None,
                database: None,
                keys: HashMap::new(),
                changes: HashMap::from([("payload".into(), json!(true))]),
            },
        )
        .await
    });
    approve_waiting(h).await;
    task.await.unwrap()
}
#[tokio::test]
async fn 확인되지_않은_영향_행수는_성공한_영행으로_바꾸지_않는다() {
    // given
    let h = Harness::new().await;
    h.allow_write().await;
    h.state.unknown_count.store(true, Ordering::SeqCst);
    // when
    let actual = unknown_row(&h).await;
    // then
    assert_eq!(
        actual.as_ref().unwrap_err().code,
        ErrorCode::CapabilityUnavailable
    );
    assert_eq!(actual.unwrap_err().outcome, Outcome::Applied);
}
async fn interrupted_read(timeout: bool) -> ErrorCode {
    let (reader, _writer) = tokio::io::duplex(16);
    let signal = CancellationSignal::default();
    if !timeout {
        signal.cancel();
    }
    cli::request_bytes(reader, &signal, Duration::from_millis(10))
        .await
        .unwrap_err()
        .code
}
#[tokio::test]
async fn 입력이_끝나지_않아도_취소는_즉시_반환한다() {
    // given
    let timeout = false;
    // when
    let actual = interrupted_read(timeout).await;
    // then
    assert_eq!(actual, ErrorCode::Cancelled);
}
#[tokio::test]
async fn 입력이_끝나지_않아도_제한시간이_지나면_반환한다() {
    // given
    let timeout = true;
    // when
    let actual = interrupted_read(timeout).await;
    // then
    assert_eq!(actual, ErrorCode::DeadlineExceeded);
}
/// The contract carries a descriptor as one byte, while parallel tests can hold more than 255
/// descriptors at once; move the pipe's read end to a free number the contract accepts.
#[cfg(unix)]
fn descriptor_below_256(file: std::fs::File) -> std::fs::File {
    use std::os::fd::{AsRawFd, FromRawFd};
    if file.as_raw_fd() <= 255 {
        return file;
    }
    for _ in 0..400 {
        let duplicate = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 3) };
        assert!(duplicate >= 0);
        if duplicate <= 255 {
            return unsafe { std::fs::File::from_raw_fd(duplicate) };
        }
        unsafe { libc::close(duplicate) };
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("no file descriptor at or below 255 became free");
}
#[cfg(unix)]
async fn protected_descriptor(h: &Harness) -> (String, ErrorCode, bool) {
    use std::{
        io::Write,
        os::fd::{AsRawFd, FromRawFd},
    };
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let reader = descriptor_below_256(unsafe { std::fs::File::from_raw_fd(fds[0]) });
    let mut writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    writer.write_all(b" SECRET_CANARY \n").unwrap();
    drop(writer);
    let raw=h.raw(Operation::CredentialImport,json!({"slot":"db_password","input_channel":{"kind":"file_descriptor","descriptor":reader.as_raw_fd()},"persistence":"session"}));
    let request = h.core.decode(&raw).unwrap();
    let caller = h.core.caller(CallerTransport::Cli).unwrap();
    let wrong = register_credential(
        &h.core,
        &request,
        &h.core.caller(CallerTransport::Mcp).unwrap(),
        CredentialSource::Descriptor(reader.as_raw_fd() as u8),
        CancellationSignal::default(),
    )
    .await
    .unwrap_err()
    .code;
    register_credential(
        &h.core,
        &request,
        &caller,
        CredentialSource::Descriptor(reader.as_raw_fd() as u8),
        CancellationSignal::default(),
    )
    .await
    .unwrap();
    let secret = h
        .core
        .platform
        .runtime
        .secret_inputs
        .take(
            &caller,
            &request.request_id,
            &request.input["input_channel"],
        )
        .unwrap();
    let replay = h
        .core
        .platform
        .runtime
        .secret_inputs
        .take(
            &caller,
            &request.request_id,
            &request.input["input_channel"],
        )
        .is_err();
    (secret.expose().into(), wrong, replay)
}
#[cfg(unix)]
#[tokio::test]
async fn 명시적_파일서술자는_단한번_비밀을_읽고_공통_요청에는_값을_넣지_않는다() {
    // given
    let h = Harness::new().await;
    // when
    let actual = protected_descriptor(&h).await;
    // then
    assert_eq!(actual.0, " SECRET_CANARY \n");
    assert_eq!(actual.1, ErrorCode::PermissionDenied);
    assert!(actual.2);
    assert_eq!(h.secrets.0.load(Ordering::SeqCst), 0);
    assert!(h.state.calls.lock().unwrap().is_empty());
}
async fn artifact_fixture(h: &Harness) -> String {
    let connection = h.core.platform.connections.get(&h.id).await.unwrap();
    let caller = h.core.caller(CallerTransport::Cli).unwrap();
    let permission = h
        .core
        .platform
        .runtime
        .policy
        .permission(&connection, &caller)
        .await
        .unwrap();
    let binding = CursorBinding {
        connection_id: h.id.clone(),
        permission_scope: permission.identity,
        auth_revision: connection.auth_revision,
        query_hash: "a".repeat(64),
        result_revision: uuid::Uuid::new_v4().to_string(),
        sdk_version: None,
        container_rid: None,
        endpoint_identity: None,
        partition_hash: None,
    };
    let mut sink = h
        .core
        .artifacts
        .reserve(ExportDestination {
            binding,
            caller_id: caller.caller_id,
            driver_id: connection.driver,
            filename: "owned.geojson".into(),
            max_bytes: 1024,
            deadline_at_ms: crate::services::storage::now_ms() + 30_000,
        })
        .unwrap();
    sink.append(br#"{"type":"FeatureCollection","features":[]}"#)
        .unwrap();
    sink.finish(ExportStats::default()).unwrap().artifact_id
}
async fn artifact_copy(
    h: &Harness,
    id: String,
) -> (
    ServiceResult<crate::services::artifacts::ArtifactMetadata>,
    ServiceResult<crate::services::artifacts::ArtifactMetadata>,
    Vec<u8>,
) {
    let path = h.directory.0.join("owned.geojson");
    let caller = h.core.caller(CallerTransport::Cli).unwrap();
    let first = h
        .core
        .artifacts
        .save(
            id.clone(),
            caller.clone(),
            path.clone(),
            CancellationSignal::default(),
        )
        .await;
    let second = h
        .core
        .artifacts
        .save(id, caller, path.clone(), CancellationSignal::default())
        .await;
    (first, second, std::fs::read(path).unwrap())
}
#[tokio::test]
async fn 산출물은_새_파일로_저장하고_기존_파일은_덮어쓰지_않는다() {
    // given
    let h = Harness::new().await;
    let id = artifact_fixture(&h).await;
    // when
    let actual = artifact_copy(&h, id).await;
    // then
    assert!(actual.0.is_ok());
    assert!(actual.1.is_err());
    assert_eq!(actual.2, br#"{"type":"FeatureCollection","features":[]}"#);
}
async fn revoked_artifact(h: &Harness, id: &str) -> ServiceResult<Value> {
    h.core
        .platform
        .connections
        .rotate_auth_revision(&h.id)
        .await
        .unwrap();
    h.core
        .artifacts
        .metadata(id, &h.core.caller(CallerTransport::Mcp).unwrap())
        .await
}
#[tokio::test]
async fn 인증이_회전하면_다른_프로세스의_산출물_참조도_거부한다() {
    // given
    let h = Harness::new().await;
    let id = artifact_fixture(&h).await;
    // when
    let actual = revoked_artifact(&h, &id).await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::AuthRequired);
    assert!(h.state.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn 종료하면_자기_끝점이_정리되고_약한_참조는_합성을_붙잡지_않는다() {
    // given
    let h = Harness::new().await;
    let weak = Arc::downgrade(&h.core);
    // when
    let actual = h.core.shutdown().await;
    // then
    assert!(actual.is_ok());
    assert!(h.core.ipc.lock().unwrap().is_none());
    assert_eq!(weak.strong_count(), 1);
}

#[tokio::test]
async fn 산출물_저장_취소는_새_파일을_게시하지_않는다() {
    // given
    let h = Harness::new().await;
    let id = artifact_fixture(&h).await;
    let destination = h.directory.0.join("cancelled.geojson");
    let signal = CancellationSignal::default();
    signal.cancel();
    // when
    let actual = h
        .core
        .artifacts
        .save(
            id,
            h.core.caller(CallerTransport::Cli).unwrap(),
            destination.clone(),
            signal,
        )
        .await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::Cancelled);
    assert!(!destination.exists());
}
struct AckPublisher {
    core: Mutex<Weak<NativeCore>>,
    published: Mutex<Vec<GuiApplyRequest>>,
}
impl GuiPublisher for AckPublisher {
    fn publish(&self, request: &GuiApplyRequest) -> ServiceResult<()> {
        self.published.lock().unwrap().push(request.clone());
        self.core
            .lock()
            .unwrap()
            .upgrade()
            .ok_or_else(|| failure(ErrorCode::GuiUnavailable))?
            .acks
            .acknowledge(
                &request.map_id,
                GuiApplyAck {
                    state_version: request.state_version,
                    rendered_version: Some(request.state_version),
                    generation: request.generation,
                    gui_applied: true,
                },
            )
    }
}
async fn native_map_ack(h: &Harness) -> (ServiceResponse, Vec<GuiApplyRequest>) {
    let publisher = Arc::new(AckPublisher {
        core: Mutex::new(Weak::new()),
        published: Mutex::new(vec![]),
    });
    let platform = ServicePlatform::isolated(
        h.directory.0.clone(),
        uuid::Uuid::new_v4().to_string(),
        h.secrets.clone(),
        Arc::new(SyntheticOAuth),
        Arc::new(crate::service_platform::NoopParamExpander),
    )
    .unwrap();
    let gui = NativeCore::compose(
        platform,
        h.directory.0.clone(),
        EndpointKind::Gui,
        Some(publisher.clone()),
    )
    .await
    .unwrap();
    *publisher.core.lock().unwrap() = Arc::downgrade(&gui);
    let caller = h.core.caller(CallerTransport::Cli).unwrap();
    let create = serde_json::to_value(catalog::request(
        Operation::MapCreate,
        None,
        json!({"name":"Owned map"}),
    ))
    .unwrap();
    let created = h
        .core
        .transport
        .run_one(create, caller.clone(), false, None)
        .await
        .unwrap();
    let mut open = catalog::request(
        Operation::MapOpen,
        None,
        json!({"gui_instance_id":gui.platform.runtime.jobs.instance_id()}),
    );
    open.map_id = created.data["map"]["map_id"].as_str().map(str::to_owned);
    open.expected_version = Some(0);
    let response = h
        .core
        .transport
        .run_one(serde_json::to_value(open).unwrap(), caller, false, None)
        .await
        .unwrap();
    gui.shutdown().await.unwrap();
    let published = publisher.published.lock().unwrap().clone();
    (response, published)
}
#[tokio::test]
async fn 다른_끝점의_지도_열기는_실제_게시_후_정확한_렌더_승인을_기다린다() {
    // given
    let h = Harness::new().await;
    // when
    let actual = native_map_ack(&h).await;
    // then
    assert_eq!(actual.0.status, JobStatus::Succeeded);
    assert_eq!(actual.0.data["gui_applied"], true);
    assert_eq!(actual.1.len(), 1);
    assert_eq!(actual.1[0].state_version, 0);
    assert_eq!(actual.1[0].generation, 1);
    assert_eq!(actual.1[0].state["action"], "open");
    assert!(h.state.calls.lock().unwrap().is_empty());
}
