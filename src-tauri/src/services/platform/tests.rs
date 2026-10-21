use super::*;
use crate::{
    models::ConnectionParams,
    persistence,
    services::{
        auth::{AuthConfig, OAuthGrant},
        backend::*,
        contracts::*,
        credentials::{Secret, SecretSlot, SecretStore},
        dispatch::{CallerContext, CallerTransport},
        error::{failure, ServiceResult},
        oauth::OAuthProvider,
        policy::ApprovalBackend,
        results::*,
        security::value_hash,
    },
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

mod approvals;
mod connections;
mod guarded;
mod origin_policy;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("tabularis-platform-{}", uuid::Uuid::new_v4())))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn caller(transport: CallerTransport) -> CallerContext {
    crate::services::local_ipc::current_local_caller("platform-owner".into(), transport).unwrap()
}
fn request(operation: Operation, input: Value) -> ServiceRequest {
    ServiceRequest {
        protocol_version: 1,
        operation,
        request_id: uuid::Uuid::new_v4().to_string(),
        connection_id: Some("fixture".into()),
        session_id: None,
        map_id: None,
        expected_version: None,
        deadline_ms: 3000,
        input,
    }
}
fn seed(profile: &Path, driver: &str) {
    let raw = json!({"groups":[],"connections":[{"id":"fixture","name":"Fixture","params":{"driver":driver,"host":"localhost","database":"fixture","password":"synthetic-legacy-password","save_in_keychain":false,"future_param":{"v":4}},"group_id":"group","tag_ids":["tag"],"future_connection":"retained"}],"future_top":"retained"});
    persistence::service_write_atomic(
        &profile.join("connections.json"),
        &serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
}
async fn registered() -> String {
    let id = format!("platform-{}", uuid::Uuid::new_v4());
    crate::drivers::registry::register_driver(
        crate::plugins::driver::native_tests::child_driver(&id, "native", json!({})).await,
    )
    .await;
    id
}
fn config(profile: &Path, readonly: bool) {
    persistence::service_write_atomic(
        &profile.join("config.json"),
        &serde_json::to_vec(&json!({"mcpReadonlyDefault":readonly})).unwrap(),
    )
    .unwrap();
}

#[derive(Default)]
struct SyntheticSecrets(AtomicUsize);
#[async_trait]
impl SecretStore for SyntheticSecrets {
    async fn get(&self, _id: &str, _slot: SecretSlot) -> ServiceResult<Option<Secret>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
    async fn set(&self, _id: &str, _slot: SecretSlot, _secret: Secret) -> ServiceResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn delete(&self, _id: &str, _slot: SecretSlot) -> ServiceResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[derive(Default)]
struct SyntheticOAuth(AtomicUsize);
#[async_trait]
impl OAuthProvider for SyntheticOAuth {
    async fn device_begin(
        &self,
        _: &AuthConfig,
    ) -> ServiceResult<oauth2::StandardDeviceAuthorizationResponse> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn device_finish(
        &self,
        _: &AuthConfig,
        _: oauth2::StandardDeviceAuthorizationResponse,
        _: CancellationSignal,
    ) -> ServiceResult<OAuthGrant> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn client_credentials(&self, _: &AuthConfig, _: &Secret) -> ServiceResult<OAuthGrant> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn refresh(
        &self,
        _: &AuthConfig,
        _: &Secret,
        _: Option<&str>,
    ) -> ServiceResult<OAuthGrant> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::AuthRequired))
    }
}
#[derive(Default)]
struct CountExpander(AtomicUsize);
#[async_trait]
impl ServiceParamExpander for CountExpander {
    async fn expand(&self, params: &ConnectionParams, id: &str) -> ServiceResult<ConnectionParams> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let mut params = params.clone();
        params.connection_id = Some(id.into());
        Ok(params)
    }
}
#[derive(Default)]
struct CountDriver {
    calls: AtomicUsize,
    invalidations: AtomicUsize,
}
#[async_trait]
impl DriverBackend for CountDriver {
    async fn supports(&self, _: &SavedConnection, _: Operation) -> ServiceResult<bool> {
        Ok(true)
    }
    async fn read_only_guarantee(&self, _: &SavedConnection) -> ServiceResult<ReadOnlyGuarantee> {
        Ok(ReadOnlyGuarantee::ReadOnlyTransaction)
    }
    async fn execute(
        &self,
        _: &SavedConnection,
        _: &ServiceRequest,
        _: DriverContext,
    ) -> ServiceResult<DriverOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(DriverOutput {
            data: json!({"affected_rows":1}),
            snapshot: None,
            cursor: None,
            confirmed_cancelled: false,
            session_closed: false,
            metrics: Metrics::default(),
            limits: Limits::default(),
            warnings: vec![],
        })
    }
    async fn continue_cursor(
        &self,
        _: &SavedConnection,
        _: &CursorState,
        _: DriverContext,
    ) -> ServiceResult<DriverOutput> {
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
    async fn invalidate_auth(&self, _: &str, _: Option<&str>) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}
struct Harness {
    directory: Directory,
    connections: Arc<ProfileConnections>,
    approvals: Arc<ServiceApprovals>,
    policy: Arc<ApprovalPolicy>,
    runtime: Arc<ServiceRuntime>,
    driver: Arc<CountDriver>,
    secrets: Arc<SyntheticSecrets>,
    oauth: Arc<SyntheticOAuth>,
}
impl Harness {
    async fn new() -> Self {
        let directory = Directory::new();
        let driver_id = registered().await;
        seed(&directory.0, &driver_id);
        config(&directory.0, false);
        let connections = Arc::new(ProfileConnections::new(&directory.0));
        let approvals =
            ServiceApprovals::new(directory.0.clone(), caller(CallerTransport::Gui).caller_id)
                .unwrap();
        let access = Arc::new(ProfileAccess::new(
            directory.0.clone(),
            caller(CallerTransport::Gui).caller_id,
            approvals.clone(),
        ));
        let policy = Arc::new(ApprovalPolicy::new(access, approvals.clone()));
        let driver = Arc::new(CountDriver::default());
        let secrets = Arc::new(SyntheticSecrets::default());
        let oauth = Arc::new(SyntheticOAuth::default());
        let runtime = ServiceRuntime::new(
            directory.0.clone(),
            "platform-owner".into(),
            connections.clone(),
            policy.clone(),
            driver.clone(),
            secrets.clone(),
            oauth.clone(),
        )
        .unwrap();
        Self {
            directory,
            connections,
            approvals,
            policy,
            runtime,
            driver,
            secrets,
            oauth,
        }
    }
}
async fn pending(approvals: &ServiceApprovals) -> crate::ai_approval::PendingApproval {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let list = crate::ai_approval::list_pending_in(approvals.profile()).unwrap();
            if let Some(pending) = list.into_iter().next() {
                return pending;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
fn decision(id: &str, approve: bool) -> crate::ai_approval::ApprovalDecision {
    crate::ai_approval::ApprovalDecision {
        approval_id: id.into(),
        decided_at: chrono::Utc::now().to_rfc3339(),
        decision: if approve { "approve" } else { "deny" }.into(),
        reason: None,
        edited_query: None,
    }
}
fn queue_counts(profile: &Path) -> (usize, usize, usize) {
    let entries = std::fs::read_dir(profile.join("pending_approvals"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let bindings = std::fs::read_dir(profile.join("service-approval-bindings-v1"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .count();
    (
        entries
            .iter()
            .filter(|name| name.ends_with(".pending.json"))
            .count(),
        entries
            .iter()
            .filter(|name| name.ends_with(".decision.json"))
            .count(),
        bindings,
    )
}

#[test]
fn 격리_경로는_비어있거나_상대_경로이면_접근_전에_거부한다() {
    // given
    let absolute = std::env::temp_dir().join("owned-profile");
    let absolute = absolute.to_str().unwrap();
    let values = [("", false), ("relative", false), (absolute, true)];
    // when
    let actual = values.map(|(value, _)| {
        checked_isolation_paths(
            Some(std::ffi::OsStr::new(value)),
            Some(std::ffi::OsStr::new(value)),
        )
        .is_ok()
    });
    // then
    assert_eq!(actual, [false, false, true]);
}
