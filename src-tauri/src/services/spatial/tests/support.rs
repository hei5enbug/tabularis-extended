use super::super::{MapDomain, MapGuiBackend, MapSpatialCapability, MapStore};
use crate::services::local_ipc::{GuiApplyAck, GuiApplyRequest, InstanceMetadata};
use crate::services::{
    auth::{AuthConfig, OAuthGrant},
    backend::*,
    contracts::*,
    credentials::{Secret, SessionSecretStore},
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
};
pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!("tabularis-map-test-{}", uuid::Uuid::new_v4())))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn caller() -> CallerContext {
    CallerContext {
        transport: CallerTransport::Cli,
        caller_id: "local_user:fixture".into(),
        owner_instance_id: "map-owner".into(),
    }
}
pub fn request(
    op: Operation,
    map: Option<&str>,
    version: Option<u64>,
    input: Value,
) -> ServiceRequest {
    ServiceRequest {
        protocol_version: 1,
        operation: op,
        request_id: uuid::Uuid::new_v4().to_string(),
        connection_id: None,
        session_id: None,
        map_id: map.map(str::to_owned),
        expected_version: version,
        deadline_ms: 30_000,
        input,
    }
}
pub fn saved(id: &str) -> SavedConnection {
    SavedConnection {
        id: id.into(),
        name: id.into(),
        driver: "postgresql".into(),
        settings: json!({}),
        credential_ref: None,
        version: 1,
        allow_writes: false,
        auth_revision: "auth-1".into(),
    }
}
pub struct Connections(pub Mutex<HashMap<String, SavedConnection>>);
#[async_trait]
impl ConnectionBackend for Connections {
    async fn validate_settings(&self, _: &str, _: &Value) -> ServiceResult<()> {
        Ok(())
    }
    async fn rotate_auth_revision(&self, _: &str) -> ServiceResult<String> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn list(&self) -> ServiceResult<Vec<SavedConnection>> {
        Ok(self.0.lock().unwrap().values().cloned().collect())
    }
    async fn get(&self, id: &str) -> ServiceResult<SavedConnection> {
        self.0
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))
    }
    async fn create(
        &self,
        _: &str,
        _: &str,
        _: Value,
        _: Option<String>,
    ) -> ServiceResult<SavedConnection> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn update(&self, _: &str, _: u64, _: Value) -> ServiceResult<SavedConnection> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn delete(&self, _: &str, _: u64) -> ServiceResult<()> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
}
pub struct Policy {
    pub denied: Mutex<Vec<String>>,
    pub identity: Mutex<String>,
}
#[async_trait]
impl PolicyBackend for Policy {
    async fn permission(
        &self,
        connection: &SavedConnection,
        _: &CallerContext,
    ) -> ServiceResult<PermissionScope> {
        if self.denied.lock().unwrap().contains(&connection.id) {
            Err(failure(ErrorCode::PermissionDenied))
        } else {
            Ok(PermissionScope {
                identity: self.identity.lock().unwrap().clone(),
                allow_writes: false,
            })
        }
    }
    async fn authorize_connection_change(
        &self,
        _: &CallerContext,
        _: &ServiceRequest,
    ) -> ServiceResult<()> {
        Err(failure(ErrorCode::PermissionDenied))
    }
    async fn consume_write_approval(
        &self,
        _: &SavedConnection,
        _: &CallerContext,
        _: &ServiceRequest,
        _: &str,
    ) -> ServiceResult<()> {
        Err(failure(ErrorCode::WriteNotAllowed))
    }
}
pub struct Driver(pub AtomicUsize);
#[async_trait]
impl DriverBackend for Driver {
    async fn supports(&self, _: &SavedConnection, _: Operation) -> ServiceResult<bool> {
        Ok(false)
    }
    async fn read_only_guarantee(&self, _: &SavedConnection) -> ServiceResult<ReadOnlyGuarantee> {
        Ok(ReadOnlyGuarantee::Unavailable)
    }
    async fn execute(
        &self,
        _: &SavedConnection,
        _: &ServiceRequest,
        _: DriverContext,
    ) -> ServiceResult<DriverOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn continue_cursor(
        &self,
        _: &SavedConnection,
        _: &CursorState,
        _: DriverContext,
    ) -> ServiceResult<DriverOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn cancel(&self, _: &str, _: &str) -> ServiceResult<()> {
        Ok(())
    }
    async fn reserve_session(
        &self,
        _: &SavedConnection,
        _: &str,
        _: DriverContext,
    ) -> ServiceResult<String> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn finish_session(
        &self,
        _: &SavedConnection,
        _: &str,
        _: SessionFinish,
        _: DriverContext,
    ) -> ServiceResult<()> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn discard_session(&self, _: &str, _: &str) {}
    async fn invalidate_auth(&self, _: &str, _: Option<&str>) {}
}
struct OAuth;
#[async_trait]
impl OAuthProvider for OAuth {
    async fn device_begin(
        &self,
        _: &AuthConfig,
    ) -> ServiceResult<oauth2::StandardDeviceAuthorizationResponse> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn device_finish(
        &self,
        _: &AuthConfig,
        _: oauth2::StandardDeviceAuthorizationResponse,
        _: CancellationSignal,
    ) -> ServiceResult<OAuthGrant> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn client_credentials(&self, _: &AuthConfig, _: &Secret) -> ServiceResult<OAuthGrant> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn refresh(
        &self,
        _: &AuthConfig,
        _: &Secret,
        _: Option<&str>,
    ) -> ServiceResult<OAuthGrant> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
}
pub struct Capability {
    pub supported: AtomicBool,
    pub gate: Mutex<Option<Arc<tokio::sync::Barrier>>>,
    pub entered: AtomicUsize,
}
#[async_trait]
impl MapSpatialCapability for Capability {
    async fn supports(&self, _: &SavedConnection, context: &DriverContext) -> ServiceResult<bool> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            tokio::select! {_=gate.wait()=>{},_=context.signal.cancelled()=>return Err(failure(ErrorCode::Cancelled))}
        }
        Ok(self.supported.load(Ordering::SeqCst))
    }
}
pub struct Gui {
    pub calls: Mutex<Vec<(String, GuiApplyRequest)>>,
    pub mode: AtomicUsize,
}
#[async_trait]
impl MapGuiBackend for Gui {
    async fn discover(&self) -> ServiceResult<Vec<InstanceMetadata>> {
        Ok(vec![])
    }
    async fn apply(&self, id: &str, request: GuiApplyRequest) -> ServiceResult<GuiApplyAck> {
        let ack = GuiApplyAck {
            state_version: request.state_version,
            rendered_version: Some(request.state_version),
            generation: request.generation,
            gui_applied: true,
        };
        self.calls.lock().unwrap().push((id.into(), request));
        match self.mode.load(Ordering::SeqCst) {
            1 => Err(failure(ErrorCode::ApplyTimeout)),
            2 => Ok(GuiApplyAck {
                generation: ack.generation + 1,
                ..ack
            }),
            3 => Err(failure(ErrorCode::GuiUnavailable)),
            4 => {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                Ok(ack)
            }
            _ => Ok(ack),
        }
    }
}
pub struct Harness {
    pub directory: Directory,
    pub runtime: Arc<ServiceRuntime>,
    pub domain: Arc<MapDomain>,
    pub connections: Arc<Connections>,
    pub policy: Arc<Policy>,
    pub driver: Arc<Driver>,
    pub gui: Arc<Gui>,
    pub capability: Arc<Capability>,
}
impl Harness {
    pub fn new() -> Self {
        let directory = Directory::new();
        let connections = Arc::new(Connections(Mutex::new(
            [
                ("conn-a".into(), saved("conn-a")),
                ("conn-b".into(), saved("conn-b")),
            ]
            .into(),
        )));
        let policy = Arc::new(Policy {
            denied: Mutex::new(vec![]),
            identity: Mutex::new("fixture-policy".into()),
        });
        let driver = Arc::new(Driver(AtomicUsize::new(0)));
        let gui = Arc::new(Gui {
            calls: Mutex::new(vec![]),
            mode: AtomicUsize::new(0),
        });
        let capability = Arc::new(Capability {
            supported: AtomicBool::new(true),
            gate: Mutex::new(None),
            entered: AtomicUsize::new(0),
        });
        let runtime = ServiceRuntime::new(
            directory.0.clone(),
            "map-owner".into(),
            connections.clone(),
            policy.clone(),
            driver.clone(),
            Arc::new(SessionSecretStore::default()),
            Arc::new(OAuth),
        )
        .unwrap();
        let store = Arc::new(MapStore::new(&directory.0).unwrap());
        let domain = MapDomain::attach(&runtime, store, gui.clone(), capability.clone()).unwrap();
        Self {
            directory,
            runtime,
            domain,
            connections,
            policy,
            driver,
            gui,
            capability,
        }
    }
    pub async fn call(&self, request: &ServiceRequest) -> ServiceResponse {
        self.runtime.call_validated(request, &caller()).await
    }
    pub async fn create(&self) -> String {
        self.call(&request(
            Operation::MapCreate,
            None,
            None,
            json!({"name":"fixture"}),
        ))
        .await
        .data["map"]["map_id"]
            .as_str()
            .unwrap()
            .into()
    }
    pub fn snapshot(&self) -> String {
        self.runtime
            .results
            .insert(
                binding(),
                ResultSnapshot {
                    sets: vec![ResultSet {
                        kind: ResultKind::Tabular,
                        columns: vec![json!({"name":"geom","column_index":0})],
                        rows: vec![json!(["POINT(1 2)"])],
                    }],
                    revision: "fixture-result".into(),
                    limits: Limits::default(),
                    allow_truncation: false,
                    warnings: vec![],
                },
            )
            .unwrap()
    }
}
pub fn binding() -> CursorBinding {
    CursorBinding {
        connection_id: "conn-a".into(),
        permission_scope: "fixture-policy".into(),
        auth_revision: "auth-1".into(),
        query_hash: "fixture-query".into(),
        result_revision: "fixture-result".into(),
        sdk_version: None,
        container_rid: None,
        endpoint_identity: None,
        partition_hash: None,
    }
}
pub fn layer(map: &str, version: u64, connection: &str, source: Value) -> ServiceRequest {
    let mut request = request(
        Operation::MapLayerAdd,
        Some(map),
        Some(version),
        json!({"source":source,"style":{}}),
    );
    request.connection_id = Some(connection.into());
    request
}
pub fn table() -> Value {
    json!({"kind":"table","table":{"database":"fixture","schema":"public","table":"places"},"column":"geom","viewport":{"west":-10,"east":10,"south":-10,"north":10},"source_srid":4326,"longitude_mode":"preserve","skip_invalid":false})
}
pub fn query_source(id: &str) -> Value {
    json!({"kind":"query_result","result_id":id,"result_set_index":0,"column_index":0,"source_srid":4326,"longitude_mode":"preserve","skip_invalid":false})
}
pub fn expire_result(h: &Harness, id: &str) {
    let path = h.runtime.results.root().join(format!("{id}.json"));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["record"]["expires_at_ms"] = json!(now_ms().saturating_sub(1));
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
}
