use super::super::super::test_support::{
    self as support, TestConnections, TestDirectory, TestDriver, TestGate, TestOAuth,
};
use super::*;
use crate::services::{
    backend::PermissionScope,
    credentials::SessionSecretStore,
    policy::{AccessBackend, ApprovalBackend, ApprovalGrant, ApprovalPolicy},
    runtime::ServiceRuntime,
};
use async_trait::async_trait;
use std::sync::atomic::Ordering;

struct Access;
#[async_trait]
impl AccessBackend for Access {
    async fn permission(
        &self,
        _connection: &SavedConnection,
        caller: &CallerContext,
    ) -> ServiceResult<PermissionScope> {
        Ok(PermissionScope {
            identity: caller.caller_id.clone(),
            allow_writes: false,
        })
    }
    async fn authorize_connection_change(
        &self,
        _caller: &CallerContext,
        _request: &ServiceRequest,
    ) -> ServiceResult<()> {
        Ok(())
    }
}
struct Approvals;
#[async_trait]
impl ApprovalBackend for Approvals {
    async fn consume(
        &self,
        _connection: &SavedConnection,
        _caller: &CallerContext,
        _request: &ServiceRequest,
        _hash: &str,
    ) -> ServiceResult<ApprovalGrant> {
        Err(failure(ErrorCode::WriteNotAllowed))
    }
}

struct Harness {
    _directory: TestDirectory,
    transport: Arc<ServiceTransport>,
    driver: Arc<TestDriver>,
}
impl Harness {
    fn new() -> Self {
        let directory = TestDirectory::new();
        let driver = Arc::new(TestDriver::new());
        let policy = Arc::new(ApprovalPolicy::new(Arc::new(Access), Arc::new(Approvals)));
        let runtime = ServiceRuntime::new(
            directory.0.clone(),
            "owner".into(),
            Arc::new(TestConnections::new()),
            policy.clone(),
            driver.clone(),
            Arc::new(SessionSecretStore::default()),
            Arc::new(TestOAuth::new()),
        )
        .unwrap();
        runtime.set_cancel_grace_ms(100);
        Self {
            _directory: directory,
            transport: ServiceTransport::new(
                runtime,
                policy,
                Arc::new(super::super::super::UnavailableArtifacts),
            )
            .unwrap(),
            driver,
        }
    }
    fn hold(&self, word: &str) -> Arc<TestGate> {
        let gate = TestGate::new();
        self.driver
            .gates
            .lock()
            .unwrap()
            .insert(word.into(), gate.clone());
        gate
    }
}

fn caller() -> CallerContext {
    CallerContext {
        transport: CallerTransport::Mcp,
        ..support::caller()
    }
}
fn read(text: &str, deadline_ms: u64) -> ReadQuery {
    ReadQuery {
        connection_id: "conn-a".into(),
        text: text.into(),
        page_size: 10,
        deadline_ms,
        database: None,
        schema: None,
    }
}
async fn until(condition: impl Fn() -> bool) -> bool {
    for _ in 0..200 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    condition()
}

#[tokio::test]
async fn 기다리던_호출이_중단되면_그_작업에_취소를_보낸다() {
    // given
    let h = Harness::new();
    let gate = h.hold("held");
    let waiting = tokio::spawn({
        let transport = h.transport.clone();
        async move {
            transport
                .run_read_query(read("SELECT 'held'", 60_000), caller())
                .await
        }
    });
    gate.wait_entered().await;
    // when
    waiting.abort();
    // then
    assert!(until(|| h.driver.cancels.load(Ordering::SeqCst) == 1).await);
    assert_eq!(h.driver.cancelled_connections.lock().unwrap()[0], "conn-a");
}

#[tokio::test]
async fn 끝까지_기다려_끝난_작업에는_취소를_보내지_않는다() {
    // given
    let h = Harness::new();
    // when
    let actual = h
        .transport
        .run_read_query(read("SELECT 'slow'", 30_000), caller())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // then
    assert_eq!(actual.response.status, JobStatus::Succeeded);
    assert_eq!(actual.response.data["rows"].as_array().unwrap().len(), 10);
    assert_eq!(h.driver.cancels.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 마감에_이른_작업은_서버에서_취소되어_확정_상태로_돌아온다() {
    // given
    let h = Harness::new();
    let _gate = h.hold("held");
    // when
    let actual = h
        .transport
        .run_read_query(read("SELECT 'held'", 100), caller())
        .await
        .unwrap();
    // then
    let error = actual.response.error.unwrap();
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert_eq!(actual.cancel_state.as_deref(), Some("confirmed"));
    assert_eq!(h.driver.cancels.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 서버가_취소를_확인하지_않으면_확인되지_않은_상태로_알린다() {
    // given
    let h = Harness::new();
    let _gate = h.hold("held");
    h.driver.cancel_confirms.store(false, Ordering::SeqCst);
    // when
    let actual = h
        .transport
        .run_read_query(read("SELECT 'held'", 100), caller())
        .await
        .unwrap();
    // then
    assert_eq!(
        actual.response.error.as_ref().map(|error| error.code),
        Some(ErrorCode::DeadlineExceeded)
    );
    assert_eq!(actual.cancel_state.as_deref(), Some("not_confirmed"));
    assert!(actual
        .response
        .warnings
        .contains(&"cancel_not_confirmed".to_string()));
}

#[tokio::test]
async fn 읽기로_증명되지_않은_문장은_실행하지_않고_쓰기_의도_오류로_거부한다() {
    // given
    let h = Harness::new();
    // when
    let actual = h
        .transport
        .run_read_query(read("UPDATE fixture SET value = 1", 30_000), caller())
        .await
        .unwrap();
    // then
    assert_eq!(
        actual.response.error.map(|error| error.code),
        Some(ErrorCode::WriteIntentRequired)
    );
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 허용_범위를_벗어난_마감은_잘못된_인자로_거부한다() {
    // given
    let h = Harness::new();
    // when
    let actual = h
        .transport
        .run_read_query(read("SELECT 1", 120_001), caller())
        .await;
    // then
    assert_eq!(
        actual.err().map(|error| error.code),
        Some(ErrorCode::InvalidArgument)
    );
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
