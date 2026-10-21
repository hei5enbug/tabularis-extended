use super::super::{SpatialDataDomain, SpatialDriverBackend};
use super::support;
use crate::{
    drivers::driver_trait::ServiceRpcMethod,
    services::{
        backend::{DriverContext, SavedConnection},
        contracts::{ErrorCode, Operation, ServiceRequest, ServiceResponse},
        error::{failure, ServiceResult},
        results::{ResultKind, ResultSet, ResultSnapshot},
    },
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

pub struct Spatial {
    pub calls: Mutex<Vec<(String, ServiceRpcMethod, Value)>>,
    pub responses: Mutex<VecDeque<ServiceResult<Value>>>,
    pub mode: AtomicUsize,
    pub gate: Mutex<Option<Arc<tokio::sync::Barrier>>>,
    pub entered: AtomicUsize,
    pub cancelled: Mutex<Vec<(String, String)>>,
}
impl Spatial {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(vec![]),
            responses: Mutex::new(VecDeque::new()),
            mode: AtomicUsize::new(0),
            gate: Mutex::new(None),
            entered: AtomicUsize::new(0),
            cancelled: Mutex::new(vec![]),
        })
    }
}
#[async_trait]
impl SpatialDriverBackend for Spatial {
    async fn call(
        &self,
        connection: &SavedConnection,
        method: ServiceRpcMethod,
        input: Value,
        context: DriverContext,
    ) -> ServiceResult<Value> {
        self.calls
            .lock()
            .unwrap()
            .push((connection.id.clone(), method, input.clone()));
        self.entered.fetch_add(1, Ordering::SeqCst);
        assert!(context.read_only);
        assert!(context.session_handle.is_none());
        assert_eq!(context.connection_id, connection.id);
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            tokio::select! {_=gate.wait()=>{},_=context.signal.cancelled()=>return Err(failure(ErrorCode::Cancelled))}
        }
        if let Some(response) = self.responses.lock().unwrap().pop_front() {
            return response;
        }
        match method {
            ServiceRpcMethod::SpatialTransformValues => {
                let values = input["values"].as_array().unwrap();
                let mode = self.mode.load(Ordering::SeqCst);
                if mode == 1 && values.len() > 1 {
                    return Err(failure(ErrorCode::ResourceLimit));
                }
                let geometries = values
                    .iter()
                    .map(|value| {
                        if value.is_null() || value["is_empty"] == true {
                            Value::Null
                        } else {
                            json!({"type":"Point","coordinates":[1,2]})
                        }
                    })
                    .collect::<Vec<_>>();
                Ok(
                    json!({"geometries":geometries,"feature_errors":[],"warnings":[],"limits":{"truncated":false,"reasons":[]}}),
                )
            }
            ServiceRpcMethod::SpatialColumns => Ok(
                json!({"columns":[{"column":"geom","column_index":1,"native_type":"geometry","srid":4326,"srid_status":"known","nullable":true,"primary_key":["id"]}]}),
            ),
            ServiceRpcMethod::SpatialReadFeature => Ok(
                json!({"row":{"id":1,"name":"new"},"identity":input["identity"],"source":"current_row"}),
            ),
            _ => Err(failure(ErrorCode::UnsupportedOperation)),
        }
    }
    async fn cancel(&self, connection: &str, request: &str) -> ServiceResult<()> {
        self.cancelled
            .lock()
            .unwrap()
            .push((connection.into(), request.into()));
        Ok(())
    }
}
pub struct Harness {
    pub base: support::Harness,
    pub spatial: Arc<Spatial>,
    pub domain: Arc<SpatialDataDomain>,
}
impl Harness {
    pub fn new() -> Self {
        let base = support::Harness::new();
        let spatial = Spatial::new();
        let domain = SpatialDataDomain::attach(
            &base.runtime,
            spatial.clone(),
            Some(base.domain.store().clone()),
        )
        .unwrap();
        Self {
            base,
            spatial,
            domain,
        }
    }
    pub fn snapshot(&self, columns: Vec<Value>, rows: Vec<Value>) -> String {
        let binding = support::binding();
        self.base
            .runtime
            .results
            .insert(
                binding.clone(),
                ResultSnapshot {
                    sets: vec![ResultSet {
                        kind: ResultKind::Tabular,
                        columns,
                        rows,
                    }],
                    revision: binding.result_revision,
                    limits: Default::default(),
                    allow_truncation: false,
                    warnings: vec![],
                },
            )
            .unwrap()
    }
    pub async fn call(&self, request: &ServiceRequest) -> ServiceResponse {
        self.base.call(request).await
    }
}
pub fn request(operation: Operation, input: Value) -> ServiceRequest {
    let mut request = support::request(operation, None, None, input);
    request.connection_id = Some("conn-a".into());
    request
}
pub fn value() -> Value {
    json!({"kind":"spatial","native_type":"geometry","srid":4326,"dimensions":"XY","encoding":"ewkb-base64","value":"AQEAAAABAAAAAgAAAA==","is_empty":false})
}
pub fn columns() -> Vec<Value> {
    vec![
        json!({"name":"duplicate","column_index":0,"native_type":"geometry","srid":4326}),
        json!({"name":"duplicate","column_index":1,"native_type":"text"}),
    ]
}
pub fn query(id: &str, size: usize) -> ServiceRequest {
    request(
        Operation::SpatialQueryResult,
        json!({"result_id":id,"result_set_index":0,"column_index":0,"page_size":size}),
    )
}
pub fn table(size: usize) -> ServiceRequest {
    request(
        Operation::SpatialTableQuery,
        json!({"table":{"database":"fixture","schema":"public","table":"places"},"column":"geom","viewport":{"west":-10,"east":10,"south":-10,"north":10},"page_size":size}),
    )
}
pub fn feature_page(count: usize, payload: usize, next: Option<&str>) -> Value {
    let snapshot = "fixture-snapshot";
    let features=(0..count).map(|index|json!({"type":"Feature","id":format!("{snapshot}:{index}"),"geometry":{"type":"Point","coordinates":[1,2]},"properties":{"id":index,"payload":"x".repeat(payload)}})).collect::<Vec<_>>();
    let refs=(0..count).map(|index|json!({"feature_id":format!("{snapshot}:{index}"),"identity":null,"snapshot_id":snapshot,"row_ordinal":index})).collect::<Vec<_>>();
    json!({"features":features,"row_references":refs,"page":{"next_token":next,"has_more":next.is_some(),"resume_mode":if next.is_some(){"native"}else{"none"}},"limits":{"truncated":false,"reasons":[]},"warnings":[],"feature_errors":[]})
}

pub struct SignalGate {
    pub count: AtomicUsize,
    entered: tokio::sync::Notify,
    released: tokio::sync::Semaphore,
}
impl SignalGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            count: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            released: tokio::sync::Semaphore::new(0),
        })
    }
    pub async fn wait(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_waiters();
        self.released.acquire().await.unwrap().forget();
    }
    pub async fn entered(&self, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let notified = self.entered.notified();
                if self.count.load(Ordering::SeqCst) >= count {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
    pub fn release(&self) {
        self.released.add_permits(1);
    }
}
pub struct BlockingSpatial {
    pub gates: std::collections::HashMap<String, Arc<SignalGate>>,
    pub cancelled: Mutex<Vec<(String, String)>>,
    pub calls: AtomicUsize,
}
#[async_trait]
impl SpatialDriverBackend for BlockingSpatial {
    async fn call(
        &self,
        connection: &SavedConnection,
        method: ServiceRpcMethod,
        input: Value,
        context: DriverContext,
    ) -> ServiceResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let gate = self.gates.get(&connection.id).unwrap();
        tokio::select! {
            _ = gate.wait() => {},
            _ = context.signal.cancelled() => return Err(failure(ErrorCode::Cancelled)),
        }
        match method {
            ServiceRpcMethod::SpatialColumns => Ok(json!({"columns":[]})),
            ServiceRpcMethod::SpatialTransformValues => Ok(
                json!({"geometries":input["values"].as_array().unwrap().iter().map(|_|json!({"type":"Point","coordinates":[1,2]})).collect::<Vec<_>>(),"feature_errors":[],"warnings":[],"limits":{"truncated":false,"reasons":[]}}),
            ),
            _ => Err(failure(ErrorCode::UnsupportedOperation)),
        }
    }
    async fn cancel(&self, connection: &str, request: &str) -> ServiceResult<()> {
        self.cancelled
            .lock()
            .unwrap()
            .push((connection.into(), request.into()));
        Ok(())
    }
}
pub struct DelayedPolicy {
    pub inner: Arc<support::Policy>,
    pub count: AtomicUsize,
    pub pause_at: AtomicUsize,
    pub gate: Arc<SignalGate>,
}
#[async_trait]
impl crate::services::backend::PolicyBackend for DelayedPolicy {
    async fn permission(
        &self,
        connection: &SavedConnection,
        caller: &crate::services::dispatch::CallerContext,
    ) -> ServiceResult<crate::services::backend::PermissionScope> {
        let count = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if count == self.pause_at.load(Ordering::SeqCst) {
            self.gate.wait().await;
        }
        crate::services::backend::PolicyBackend::permission(self.inner.as_ref(), connection, caller)
            .await
    }
    async fn authorize_connection_change(
        &self,
        _: &crate::services::dispatch::CallerContext,
        _: &ServiceRequest,
    ) -> ServiceResult<()> {
        Err(failure(ErrorCode::PermissionDenied))
    }
    async fn consume_write_approval(
        &self,
        _: &SavedConnection,
        _: &crate::services::dispatch::CallerContext,
        _: &ServiceRequest,
        _: &str,
    ) -> ServiceResult<()> {
        Err(failure(ErrorCode::WriteNotAllowed))
    }
}
struct NoOAuth;
#[async_trait]
impl crate::services::oauth::OAuthProvider for NoOAuth {
    async fn device_begin(
        &self,
        _: &crate::services::auth::AuthConfig,
    ) -> ServiceResult<oauth2::StandardDeviceAuthorizationResponse> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn device_finish(
        &self,
        _: &crate::services::auth::AuthConfig,
        _: oauth2::StandardDeviceAuthorizationResponse,
        _: crate::services::backend::CancellationSignal,
    ) -> ServiceResult<crate::services::auth::OAuthGrant> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn client_credentials(
        &self,
        _: &crate::services::auth::AuthConfig,
        _: &crate::services::credentials::Secret,
    ) -> ServiceResult<crate::services::auth::OAuthGrant> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
    async fn refresh(
        &self,
        _: &crate::services::auth::AuthConfig,
        _: &crate::services::credentials::Secret,
        _: Option<&str>,
    ) -> ServiceResult<crate::services::auth::OAuthGrant> {
        Err(failure(ErrorCode::UnsupportedOperation))
    }
}
pub struct RaceHarness {
    pub base: support::Harness,
    pub runtime: Arc<crate::services::runtime::ServiceRuntime>,
    pub policy: Arc<DelayedPolicy>,
}
impl RaceHarness {
    pub fn new(driver: Arc<dyn SpatialDriverBackend>) -> Self {
        let base = support::Harness::new();
        let policy = Arc::new(DelayedPolicy {
            inner: base.policy.clone(),
            count: AtomicUsize::new(0),
            pause_at: AtomicUsize::new(0),
            gate: SignalGate::new(),
        });
        let runtime = crate::services::runtime::ServiceRuntime::new(
            base.directory.0.join("race"),
            "map-owner".into(),
            base.connections.clone(),
            policy.clone(),
            base.driver.clone(),
            Arc::new(crate::services::credentials::SessionSecretStore::default()),
            Arc::new(NoOAuth),
        )
        .unwrap();
        SpatialDataDomain::attach(&runtime, driver, Some(base.domain.store().clone())).unwrap();
        Self {
            base,
            runtime,
            policy,
        }
    }
    pub fn snapshot(&self) -> String {
        let binding = support::binding();
        self.runtime
            .results
            .insert(
                binding.clone(),
                ResultSnapshot {
                    sets: vec![ResultSet {
                        kind: ResultKind::Tabular,
                        columns: columns(),
                        rows: vec![json!([value(), "old"])],
                    }],
                    revision: binding.result_revision,
                    limits: Default::default(),
                    allow_truncation: false,
                    warnings: vec![],
                },
            )
            .unwrap()
    }
}
pub async fn terminal(
    runtime: &Arc<crate::services::runtime::ServiceRuntime>,
    job: &str,
) -> ServiceResponse {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let record = runtime
                .jobs
                .get(job, "map-owner", &support::caller())
                .unwrap();
            if !matches!(
                record.status,
                crate::services::contracts::JobStatus::Queued
                    | crate::services::contracts::JobStatus::Running
            ) {
                return record.response.unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap()
}
pub fn cache_count(runtime: &crate::services::runtime::ServiceRuntime, prefix: &str) -> usize {
    std::fs::read_dir(runtime.results.root())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        .count()
}
pub fn expire(runtime: &crate::services::runtime::ServiceRuntime, id: &str) {
    let path = runtime.results.root().join(format!("{id}.json"));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["record"]["expires_at_ms"] = json!(0);
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
}
