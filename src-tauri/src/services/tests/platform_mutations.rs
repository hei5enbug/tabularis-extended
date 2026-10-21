use super::{
    super::{
        backend::*,
        contracts::*,
        credentials::{CallbackSecretStore, SecretStore},
        error::{failure, ServiceResult},
        runtime::ServiceRuntime,
    },
    support::*,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

struct FailingLifecycleConnections {
    base: Arc<TestConnections>,
    fail: AtomicBool,
}
#[async_trait]
impl ConnectionBackend for FailingLifecycleConnections {
    async fn validate_settings(&self, driver: &str, settings: &Value) -> ServiceResult<()> {
        self.base.validate_settings(driver, settings).await
    }
    async fn rotate_auth_revision(&self, id: &str) -> ServiceResult<String> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        self.base.rotate_auth_revision(id).await
    }
    async fn list(&self) -> ServiceResult<Vec<SavedConnection>> {
        self.base.list().await
    }
    async fn get(&self, id: &str) -> ServiceResult<SavedConnection> {
        self.base.get(id).await
    }
    async fn create(
        &self,
        name: &str,
        driver: &str,
        settings: Value,
        reference: Option<String>,
    ) -> ServiceResult<SavedConnection> {
        self.base.create(name, driver, settings, reference).await
    }
    async fn update(&self, id: &str, version: u64, patch: Value) -> ServiceResult<SavedConnection> {
        self.base.update(id, version, patch).await
    }
    async fn delete(&self, id: &str, version: u64) -> ServiceResult<()> {
        self.base.delete(id, version).await
    }
}
#[tokio::test]
async fn 연결_설정_게시후_무효화가_실패하면_applied를_반환한다() {
    // given
    let harness = Harness::new();
    let connections = Arc::new(FailingLifecycleConnections {
        base: harness.connections.clone(),
        fail: AtomicBool::new(true),
    });
    let runtime = ServiceRuntime::new(
        harness.directory.0.join("mutation-runtime"),
        "owner".into(),
        connections,
        harness.policy.clone(),
        harness.driver.clone(),
        harness.secrets.clone(),
        Arc::new(TestOAuth::new()),
    )
    .unwrap();
    let mut request = request(
        Operation::ConnectionUpdate,
        json!({"patch":{"name":"published","settings":{"host":"changed.example","database":"fixture"}}}),
    );
    request.expected_version = Some(1);
    // when
    let actual = runtime.call_validated(&request, &caller()).await;
    // then
    assert_eq!(
        actual.error.as_ref().unwrap().code,
        ErrorCode::ResourceLimit
    );
    assert_eq!(actual.error.unwrap().outcome, Outcome::Applied);
    assert_eq!(
        harness.connections.saved.lock().unwrap()["conn-a"].name,
        "published"
    );
    assert_eq!(harness.connections.writes.load(Ordering::SeqCst), 1);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 자격증명_저장이_실패해도_완료된_인증_회전은_applied다() {
    // given
    let harness = Harness::new();
    let secrets: Arc<dyn SecretStore> = Arc::new(CallbackSecretStore {
        read: Arc::new(|_, _| Ok(None)),
        write: Arc::new(|_, _, _| Err(failure(ErrorCode::ResourceLimit))),
        remove: Arc::new(|_, _| Ok(())),
    });
    let runtime = ServiceRuntime::new(
        harness.directory.0.join("credential-runtime"),
        "owner".into(),
        harness.connections.clone(),
        harness.policy.clone(),
        harness.driver.clone(),
        secrets,
        Arc::new(TestOAuth::new()),
    )
    .unwrap();
    let request = request(
        Operation::CredentialImport,
        json!({"slot":"db_password","input_channel":{"kind":"protected_stdin"},"persistence":"keychain"}),
    );
    runtime
        .secret_inputs
        .register(
            &caller(),
            &request.request_id,
            &request.input["input_channel"],
            b"synthetic-secret-canary".to_vec(),
        )
        .unwrap();
    // when
    let actual = runtime.call_validated(&request, &caller()).await;
    // then
    assert_eq!(
        actual.error.as_ref().unwrap().code,
        ErrorCode::ResourceLimit
    );
    assert_eq!(actual.error.as_ref().unwrap().outcome, Outcome::Applied);
    assert_ne!(
        harness.connections.saved.lock().unwrap()["conn-a"].auth_revision,
        "auth-1"
    );
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("synthetic-secret-canary"));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 자격증명_사전_무효화의_불명확한_실패는_재시도_가능한_notstarted로_꾸미지_않는다() {
    // given
    let harness = Harness::new();
    let connections = Arc::new(FailingLifecycleConnections {
        base: harness.connections.clone(),
        fail: AtomicBool::new(true),
    });
    let runtime = ServiceRuntime::new(
        harness.directory.0.join("uncertain-credential"),
        "owner".into(),
        connections,
        harness.policy.clone(),
        harness.driver.clone(),
        harness.secrets.clone(),
        Arc::new(TestOAuth::new()),
    )
    .unwrap();
    let request = request(
        Operation::CredentialImport,
        json!({"slot":"db_password","input_channel":{"kind":"protected_stdin"},"persistence":"keychain"}),
    );
    runtime
        .secret_inputs
        .register(
            &caller(),
            &request.request_id,
            &request.input["input_channel"],
            b"synthetic-secret-canary".to_vec(),
        )
        .unwrap();
    // when
    let actual = runtime.call_validated(&request, &caller()).await;
    // then
    assert_eq!(actual.status, JobStatus::OutcomeUnknown);
    assert_eq!(actual.error.unwrap().outcome, Outcome::Unknown);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 구체적인_version_충돌은_쓰기_표시후에도_notstarted를_보존한다() {
    // given
    let harness = Harness::new();
    let mut request = request(Operation::ConnectionDelete, json!({}));
    request.expected_version = Some(9);
    // when
    let actual = harness.runtime.call_validated(&request, &caller()).await;
    // then
    assert_eq!(
        actual.error.as_ref().unwrap().code,
        ErrorCode::VersionConflict
    );
    assert_eq!(actual.error.unwrap().outcome, Outcome::NotStarted);
    assert_eq!(harness.connections.writes.load(Ordering::SeqCst), 0);
}

async fn cancelled_default_guards(
    harness: &Harness,
    lease: &super::super::jobs::JobLease,
) -> (
    ServiceResult<SavedConnection>,
    ServiceResult<SavedConnection>,
) {
    lease.signal().cancel();
    let created = harness
        .connections
        .create_guarded(
            "Cancelled",
            "postgresql",
            json!({"host":"localhost","database":"fixture"}),
            None,
            lease,
        )
        .await;
    let updated = harness
        .connections
        .update_guarded("conn-a", 1, json!({"name":"Cancelled"}), lease)
        .await;
    (created, updated)
}
#[tokio::test]
async fn 기본_guard도_취소된_lease의_기존_backend를_호출하지_않는다() {
    // given
    let harness = Harness::new();
    let request = request(
        Operation::ConnectionUpdate,
        json!({"patch":{"name":"Cancelled"}}),
    );
    let lease = harness
        .runtime
        .jobs
        .begin(&request, &caller(), false, false)
        .await
        .unwrap();
    // when
    let actual = cancelled_default_guards(&harness, &lease).await;
    // then
    assert_eq!(actual.0.unwrap_err().code, ErrorCode::Cancelled);
    assert_eq!(actual.1.unwrap_err().code, ErrorCode::Cancelled);
    assert!(!lease.write_dispatched());
    assert_eq!(harness.connections.writes.load(Ordering::SeqCst), 0);
    assert_eq!(
        harness.connections.saved.lock().unwrap()["conn-a"].name,
        "conn-a"
    );
}
