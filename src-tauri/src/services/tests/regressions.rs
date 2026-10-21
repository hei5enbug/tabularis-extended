use super::{
    super::{
        backend::{ConnectionBackend, SessionFinish},
        contracts::*,
        credentials::{Secret, SecretSlot, SecretStore},
        jobs::JobManager,
        results::ResultStore,
        runtime::ServiceRuntime,
        sessions::SessionManager,
    },
    support::*,
};
use serde_json::{json, Value};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

async fn terminal(harness: &Harness, job_id: &str) -> ServiceResponse {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = harness
                .runtime
                .jobs
                .get(job_id, "owner", &caller())
                .unwrap();
            if !matches!(job.status, JobStatus::Queued | JobStatus::Running) {
                return job.response.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

async fn cancel_one_connection(
    harness: &Harness,
    first: ServiceRequest,
    second: ServiceRequest,
    gates: (Arc<TestGate>, Arc<TestGate>),
) -> (ServiceResponse, ServiceResponse, ServiceResponse, JobStatus) {
    let a = harness.runtime.submit(first, caller()).await;
    let b = harness.runtime.submit(second, caller()).await;
    tokio::join!(gates.0.wait_entered(), gates.1.wait_entered());
    let cancelled = harness
        .call(&request(
            Operation::JobCancel,
            json!({"job_id":a.job_id,"owner_instance_id":"owner"}),
        ))
        .await;
    let b_running = harness
        .runtime
        .jobs
        .get(b.job_id.as_deref().unwrap(), "owner", &caller())
        .unwrap()
        .status;
    gates.1.release();
    (
        cancelled,
        terminal(harness, a.job_id.as_deref().unwrap()).await,
        terminal(harness, b.job_id.as_deref().unwrap()).await,
        b_running,
    )
}

#[tokio::test]
async fn 같은_request_id라도_연결이_다르면_동시에_실행하고_한쪽만_취소한다() {
    // given
    let harness = Harness::new();
    let a = TestGate::new();
    let b = TestGate::new();
    harness
        .driver
        .gates
        .lock()
        .unwrap()
        .extend([("gate-a".into(), a.clone()), ("gate-b".into(), b.clone())]);
    let first = query("gate-a");
    let mut second = query("gate-b");
    second.request_id = first.request_id.clone();
    second.connection_id = Some("conn-b".into());

    // when
    let (cancelled, first, second, other_status) =
        cancel_one_connection(&harness, first, second, (a, b)).await;

    // then
    assert_eq!(cancelled.status, JobStatus::Succeeded);
    assert_eq!(first.status, JobStatus::Cancelled);
    assert_eq!(second.status, JobStatus::Succeeded);
    assert_eq!(other_status, JobStatus::Running);
    assert_eq!(harness.driver.peak.load(Ordering::SeqCst), 2);
    assert_eq!(
        *harness.driver.cancelled_connections.lock().unwrap(),
        ["conn-a"]
    );
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 2);
}

async fn logout_during_query(
    harness: &Harness,
    gate: Arc<TestGate>,
    write: bool,
) -> ServiceResponse {
    let runtime = harness.runtime.clone();
    let input = if write {
        write_query("gate-query")
    } else {
        query("gate-query")
    };
    let task = tokio::spawn(async move { runtime.call_validated(&input, &caller()).await });
    gate.wait_entered().await;
    harness.runtime.auth.logout("conn-a").await.unwrap();
    gate.release();
    task.await.unwrap()
}

#[tokio::test]
async fn 진행_중인_쿼리에서_로그아웃하면_이전_주체의_결과를_게시하지_않는다() {
    // given
    let harness = Harness::new();
    let gate = TestGate::new();
    harness
        .driver
        .gates
        .lock()
        .unwrap()
        .insert("gate-query".into(), gate.clone());

    // when
    let actual = logout_during_query(&harness, gate, false).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::AuthRequired);
    assert_eq!(actual.data, Value::Null);
    assert!(actual.result_id.is_none());
    assert!(actual.page.next_token.is_none());
    assert_ne!(
        harness.connections.saved.lock().unwrap()["conn-a"].auth_revision,
        "auth-1"
    );
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn 다른_store에서_auth_revision을_회전하면_이전_revision의_게시를_거부한다() {
    // given
    let directory = TestDirectory::new();
    let original = ResultStore::new(directory.0.clone()).unwrap();
    let other = ResultStore::new(directory.0.clone()).unwrap();
    other
        .invalidate_connection_revision("conn-a", "auth-2")
        .unwrap();

    // when
    let actual = original.insert(binding(), snapshot(vec![json!([1])]));

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::AuthRequired);
}

async fn shutdown_and_read(
    harness: &Harness,
    result: String,
) -> (ServiceResponse, String, bool, bool) {
    harness.runtime.shutdown().await.unwrap();
    let other = ServiceRuntime::new(
        harness.directory.0.clone(),
        "other-runtime".into(),
        harness.connections.clone(),
        harness.policy.clone(),
        Arc::new(TestDriver::new()),
        harness.secrets.clone(),
        Arc::new(TestOAuth::new()),
    )
    .unwrap();
    let response = other
        .call_validated(
            &request(
                Operation::ResultGet,
                json!({"result_id":result,"result_set_index":0,"offset":0,"limit":10}),
            ),
            &caller(),
        )
        .await;
    let revision = harness
        .connections
        .get("conn-a")
        .await
        .unwrap()
        .auth_revision;
    let persistent = harness
        .secrets
        .get("conn-a", SecretSlot::DbPassword)
        .await
        .unwrap()
        .is_some();
    let ephemeral = harness
        .runtime
        .auth
        .session_store()
        .get("conn-a", SecretSlot::DbPassword)
        .await
        .unwrap()
        .is_some();
    other.shutdown().await.unwrap();
    (response, revision, persistent, ephemeral)
}

#[tokio::test]
async fn 정상_종료_뒤에도_다른_runtime이_ttl_안의_결과와_영구_자격_증명을_사용한다() {
    // given
    let harness = Harness::new();
    harness
        .runtime
        .auth
        .invalidate("conn-a", false)
        .await
        .unwrap();
    let revision = harness
        .connections
        .get("conn-a")
        .await
        .unwrap()
        .auth_revision;
    harness
        .secrets
        .set(
            "conn-a",
            SecretSlot::DbPassword,
            Secret::new(b"synthetic-persistent".to_vec()).unwrap(),
        )
        .await
        .unwrap();
    harness
        .runtime
        .auth
        .session_store()
        .set(
            "conn-a",
            SecretSlot::DbPassword,
            Secret::new(b"synthetic-session".to_vec()).unwrap(),
        )
        .await
        .unwrap();
    let first = harness.call(&query("SELECT 1")).await;

    // when
    let (actual, current, persistent, ephemeral) =
        shutdown_and_read(&harness, first.result_id.unwrap()).await;

    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(actual.data["rows"][0], json!([0]));
    assert_eq!(current, revision);
    assert!(persistent);
    assert!(!ephemeral);
}

async fn invalidate_open(
    manager: Arc<SessionManager>,
    saved: super::super::backend::SavedConnection,
    gate: Arc<TestGate>,
) -> super::super::error::ServiceResult<String> {
    let task_manager = manager.clone();
    let task_saved = saved.clone();
    let task = tokio::spawn(async move {
        task_manager
            .open(
                &task_saved,
                "fixture",
                true,
                &caller(),
                context(&task_saved.id),
            )
            .await
    });
    gate.wait_entered().await;
    manager
        .invalidate_connection(&saved.id, context(&saved.id))
        .await
        .unwrap();
    gate.release();
    task.await.unwrap()
}

#[tokio::test]
async fn 무효화_중에_늦게_열린_물리_세션은_즉시_폐기한다() {
    // given
    let driver = Arc::new(TestDriver::new());
    let gate = TestGate::new();
    driver
        .gates
        .lock()
        .unwrap()
        .insert("open:conn-a".into(), gate.clone());
    let manager = Arc::new(SessionManager::new(driver.clone()));

    // when
    let actual = invalidate_open(manager, connection("conn-a"), gate).await;

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::AuthRequired);
    assert_eq!(driver.discarded.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 다른_연결의_세션이_사용_중이어도_로그아웃이_대기하지_않는다() {
    // given
    let driver = Arc::new(TestDriver::new());
    let manager = SessionManager::new(driver);
    let a = connection("conn-a");
    let b = connection("conn-b");
    manager
        .open(&a, "fixture", true, &caller(), context(&a.id))
        .await
        .unwrap();
    let id = manager
        .open(&b, "fixture", true, &caller(), context(&b.id))
        .await
        .unwrap();
    let _busy = manager
        .acquire(&id, &b, &caller(), false, &context(&b.id))
        .await
        .unwrap();

    // when
    let actual = tokio::time::timeout(
        Duration::from_millis(200),
        manager.invalidate_connection(&a.id, context(&a.id)),
    )
    .await;

    // then
    assert!(actual.is_ok_and(|result| result.is_ok()));
}

#[tokio::test]
async fn 한_연결의_무효화가_기다리는_동안_다른_연결의_세션을_종료한다() {
    // given
    let driver = Arc::new(TestDriver::new());
    let manager = Arc::new(SessionManager::new(driver));
    let a = connection("conn-a");
    let b = connection("conn-b");
    let first = manager
        .open(&a, "fixture", true, &caller(), context(&a.id))
        .await
        .unwrap();
    let second = manager
        .open(&b, "fixture", true, &caller(), context(&b.id))
        .await
        .unwrap();
    let _busy = manager
        .acquire(&first, &a, &caller(), false, &context(&a.id))
        .await
        .unwrap();
    let other = manager.clone();
    let invalidation = tokio::spawn(async move {
        other
            .invalidate_connection("conn-a", context("conn-a"))
            .await
    });

    // when
    let actual = tokio::time::timeout(
        Duration::from_millis(200),
        manager.finish(
            &second,
            &b,
            &caller(),
            SessionFinish::Rollback,
            context(&b.id),
        ),
    )
    .await;

    // then
    assert!(actual.is_ok_and(|result| result.is_ok()));
    assert!(!invalidation.is_finished());
}

#[tokio::test]
async fn 드라이버가_확정한_not_started_오류는_unknown으로_바꾸지_않는다() {
    // given
    let harness = Harness::new();
    harness.allow_write();
    harness.policy.write_intent.store(true, Ordering::SeqCst);

    // when
    let actual = harness.call(&write_query("invalid")).await;

    // then
    assert_eq!(
        actual.error.as_ref().unwrap().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(actual.error.unwrap().outcome, Outcome::NotStarted);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn db_쓰기_성공_후_결과_저장_실패는_applied로_보고하고_재실행하지_않는다() {
    // given
    let harness = Harness::new();
    harness.allow_write();
    harness.policy.write_intent.store(true, Ordering::SeqCst);

    // when
    let actual = harness.call(&write_query("oversized")).await;

    // then
    assert_eq!(
        actual.error.as_ref().unwrap().code,
        ErrorCode::ResourceLimit
    );
    assert_eq!(actual.error.unwrap().outcome, Outcome::Applied);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 성공이_확인된_쓰기는_응답_게시_전에_종료되어도_applied를_보존한다() {
    // given
    let directory = TestDirectory::new();
    let jobs = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let lease = jobs
        .begin(&query("write"), &caller(), true, false)
        .await
        .unwrap();
    let id = lease.id();
    lease.mark_write_dispatched().unwrap();
    lease.mark_write_confirmed().unwrap();

    // when
    drop(lease);

    // then
    assert_eq!(
        jobs.get(&id, "owner", &caller()).unwrap().status,
        JobStatus::Interrupted
    );
    assert_eq!(
        jobs.get(&id, "owner", &caller())
            .unwrap()
            .response
            .unwrap()
            .error
            .unwrap()
            .outcome,
        Outcome::Applied
    );
}

#[tokio::test]
async fn 드라이버가_닫은_세션은_host에서도_폐기하고_안전한_상태만_반환한다() {
    // given
    let harness = Harness::new();
    let opened = harness
        .call(&request(
            Operation::SessionOpen,
            json!({"database":"fixture","mode":"read_only"}),
        ))
        .await;
    let id = opened.data["session_id"].as_str().unwrap().to_owned();
    let mut input = query("session-cap");
    input.session_id = Some(id.clone());

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(
        actual.error.as_ref().unwrap().code,
        ErrorCode::ResourceLimit
    );
    assert_eq!(actual.error.as_ref().unwrap().outcome, Outcome::NotApplied);
    assert_eq!(
        actual.error.unwrap().details,
        json!({"session_closed":true})
    );
    assert_eq!(harness.driver.discarded.load(Ordering::SeqCst), 1);
    assert!(harness
        .runtime
        .sessions
        .is_write(&id, "conn-a", &caller())
        .await
        .is_err());
}

#[test]
fn 버전_충돌은_안전한_현재_버전만_오류에_보존한다() {
    // given
    let mut error = super::super::error::failure(ErrorCode::VersionConflict);
    error.message = "synthetic-private-password".into();
    error.details = json!({"current_version":7,"access_token":"synthetic-private"});

    // when
    let actual = super::super::error::redact_error(error);

    // then
    assert_eq!(actual.details, json!({"current_version":7}));
    assert!(!actual.message.contains("synthetic-private"));
}

#[test]
fn 화면_반영_timeout은_검증된_버전과_gui_applied_false만_보존한다() {
    // given
    let mut error = super::super::error::failure(ErrorCode::ApplyTimeout);
    error.details = json!({"state_version":5,"rendered_version":null,"gui_applied":false,"endpoint":"private-endpoint"});

    // when
    let actual = super::super::error::redact_error(error);

    // then
    assert_eq!(
        actual.details,
        json!({"state_version":5,"rendered_version":null,"gui_applied":false})
    );
}

#[test]
fn 안전한_버전_필드도_음수와_문자열과_js_정수_상한을_넘으면_제거한다() {
    // given
    let mut error = super::super::error::failure(ErrorCode::ApplyTimeout);
    error.details = json!({"state_version":-1,"rendered_version":"5","gui_applied":true});

    // when
    let actual = super::super::error::redact_error(error);

    // then
    assert_eq!(actual.details, Value::Null);
}

struct PolicyAccess;
#[async_trait::async_trait]
impl super::super::policy::AccessBackend for PolicyAccess {
    async fn permission(
        &self,
        _connection: &super::super::backend::SavedConnection,
        caller: &super::super::dispatch::CallerContext,
    ) -> super::super::error::ServiceResult<super::super::backend::PermissionScope> {
        Ok(super::super::backend::PermissionScope {
            identity: caller.caller_id.clone(),
            allow_writes: true,
        })
    }
    async fn authorize_connection_change(
        &self,
        _caller: &super::super::dispatch::CallerContext,
        _request: &ServiceRequest,
    ) -> super::super::error::ServiceResult<()> {
        Ok(())
    }
}
struct ReplayedApproval;
#[async_trait::async_trait]
impl super::super::policy::ApprovalBackend for ReplayedApproval {
    async fn consume(
        &self,
        connection: &super::super::backend::SavedConnection,
        caller: &super::super::dispatch::CallerContext,
        request: &ServiceRequest,
        hash: &str,
    ) -> super::super::error::ServiceResult<super::super::policy::ApprovalGrant> {
        Ok(super::super::policy::ApprovalGrant {
            approval_id: "same-approval".into(),
            connection_id: connection.id.clone(),
            caller_id: caller.caller_id.clone(),
            owner_instance_id: caller.owner_instance_id.clone(),
            request_id: request.request_id.clone(),
            request_hash: hash.into(),
        })
    }
}

#[tokio::test]
async fn 승인_저장소가_같은_승인을_다시_반환해도_재사용을_거부한다() {
    use super::super::backend::PolicyBackend;
    // given
    let policy = super::super::policy::ApprovalPolicy::new(
        Arc::new(PolicyAccess),
        Arc::new(ReplayedApproval),
    );
    let mut saved = connection("conn-a");
    saved.allow_writes = true;
    let first = query("DELETE FROM items");
    policy
        .consume_write_approval(&saved, &caller(), &first, "hash-1")
        .await
        .unwrap();
    let second = query("DELETE FROM items");

    // when
    let actual = policy
        .consume_write_approval(&saved, &caller(), &second, "hash-2")
        .await;

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::WriteNotAllowed);
}

#[tokio::test]
async fn 등록된_쓰기_의도는_다른_연결이나_변경된_입력으로_소비할_수_없다() {
    use super::super::backend::PolicyBackend;
    // given
    let policy = super::super::policy::ApprovalPolicy::new(
        Arc::new(PolicyAccess),
        Arc::new(ReplayedApproval),
    );
    let original = query("DELETE FROM items");
    policy.register_query_write(&caller(), &original).unwrap();
    let mut altered = original.clone();
    altered.connection_id = Some("conn-b".into());
    altered.input["text"] = json!("DELETE FROM other_items");

    // when
    let actual = policy.query_is_write(&caller(), &altered).await;

    // then
    assert!(!actual.unwrap());
}

async fn consume_intent_twice(
    policy: &super::super::policy::ApprovalPolicy,
    input: &ServiceRequest,
) -> (bool, bool) {
    use super::super::backend::PolicyBackend;
    (
        policy.query_is_write(&caller(), input).await.unwrap(),
        policy.query_is_write(&caller(), input).await.unwrap(),
    )
}

#[tokio::test]
async fn 등록된_쓰기_의도는_정확한_요청에서_한번만_소비한다() {
    // given
    let policy = super::super::policy::ApprovalPolicy::new(
        Arc::new(PolicyAccess),
        Arc::new(ReplayedApproval),
    );
    let input = query("DELETE FROM items");
    policy.register_query_write(&caller(), &input).unwrap();

    // when
    let actual = consume_intent_twice(&policy, &input).await;

    // then
    assert_eq!(actual, (true, false));
}

async fn native_pages(harness: &Harness) -> (ServiceResponse, ServiceResponse, ServiceResponse) {
    let first = harness.call(&query("native")).await;
    let token = first.page.next_token.clone().unwrap();
    let next = harness
        .call(&request(Operation::QueryNext, json!({"next_token":token})))
        .await;
    let replay = harness
        .call(&request(Operation::QueryNext, json!({"next_token":token})))
        .await;
    (first, next, replay)
}

#[tokio::test]
async fn native_cursor의_다음_페이지는_저장된_page_size와_sdk_tail을_보존한다() {
    // given
    let harness = Harness::new();
    harness
        .connections
        .saved
        .lock()
        .unwrap()
        .get_mut("conn-a")
        .unwrap()
        .driver = "cosmos-nosql".into();

    // when
    let (first, next, replay) = native_pages(&harness).await;

    // then
    assert_eq!(first.status, JobStatus::Succeeded);
    assert_eq!(first.data["values"], json!([1, 2]));
    assert_eq!(next.status, JobStatus::Succeeded);
    assert_eq!(next.data["values"], json!([3, 4]));
    assert_eq!(next.data, replay.data);
    assert_eq!(next.page, replay.page);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 2);
    assert!(!serde_json::to_string(&first)
        .unwrap()
        .contains("synthetic-sdk-private"));
}

#[tokio::test]
async fn native_snapshot이_한_공개_page보다_크면_행을_누락한_성공을_반환하지_않는다() {
    // given
    let harness = Harness::new();
    harness
        .connections
        .saved
        .lock()
        .unwrap()
        .get_mut("conn-a")
        .unwrap()
        .driver = "cosmos-nosql".into();

    // when
    let actual = harness.call(&query("native-oversized")).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ResourceLimit);
    assert!(actual.page.next_token.is_none());
    assert_eq!(actual.data, Value::Null);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn native_page가_byte_cap으로_줄어들면_native_token으로_덮지_않는다() {
    // given
    let harness = Harness::new();
    harness
        .connections
        .saved
        .lock()
        .unwrap()
        .get_mut("conn-a")
        .unwrap()
        .driver = "cosmos-nosql".into();

    // when
    let actual = harness.call(&query("native-byte-cap")).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ResourceLimit);
    assert!(actual.page.next_token.is_none());
    assert!(actual.result_id.is_none());
}

async fn release_native(harness: &Harness, first: ServiceResponse) -> ServiceResponse {
    harness
        .call(&request(
            Operation::ResultRelease,
            json!({"result_id":first.result_id.unwrap()}),
        ))
        .await;
    harness
        .call(&request(
            Operation::QueryNext,
            json!({"next_token":first.page.next_token.unwrap()}),
        ))
        .await
}

#[tokio::test]
async fn native_결과를_해제하면_그_결과에_묶인_sdk_cursor도_제거한다() {
    // given
    let harness = Harness::new();
    harness
        .connections
        .saved
        .lock()
        .unwrap()
        .get_mut("conn-a")
        .unwrap()
        .driver = "cosmos-nosql".into();
    let first = harness.call(&query("native")).await;

    // when
    let actual = release_native(&harness, first).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::InvalidPageToken);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn host_uri의_사용자_정보와_비밀을_연결_설정으로_저장하지_않는다() {
    // given
    let harness = Harness::new();
    let mut input = request(
        Operation::ConnectionCreate,
        json!({"name":"uri","driver":"postgresql","settings":{"host":"postgresql://user:synthetic-secret@localhost/database","database":"fixture"},"credential_ref":null}),
    );
    input.connection_id = None;

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::InvalidArgument);
    assert_eq!(harness.connections.writes.load(Ordering::SeqCst), 0);
}

#[test]
fn 기존_설정_응답에서도_접두사가_붙은_비밀과_uri_사용자_정보를_가린다() {
    // given
    let saved = json!({"host":"postgresql://user:synthetic-private@localhost/db","extra":{"azure_access_token":"synthetic-private","azure_client_secret":"synthetic-private","auth_mode":"entra_user"}});

    // when
    let actual = super::super::security::sanitized_settings(&saved);

    // then
    assert_eq!(actual["host"], Value::Null);
    assert_eq!(actual["extra"]["auth_mode"], "entra_user");
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("synthetic-private"));
}

#[tokio::test]
async fn 대기열_64개를_넘는_요청은_작업을_추가하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let jobs = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let _queued: Vec<_> = (0..64)
        .map(|_| {
            jobs.queue(&query("waiting"), &caller(), false, false)
                .unwrap()
        })
        .collect();

    // when
    let actual = jobs.queue(&query("overflow"), &caller(), false, false);

    // then
    assert!(matches!(actual,Err(error) if error.code==ErrorCode::RateLimited));
    assert_eq!(jobs.active_count(), 64);
}

#[tokio::test]
async fn 쓰기_성공_후_로그아웃이_결과를_막아도_applied를_보존한다() {
    // given
    let harness = Harness::new();
    harness.allow_write();
    harness.policy.write_intent.store(true, Ordering::SeqCst);
    let gate = TestGate::new();
    harness
        .driver
        .gates
        .lock()
        .unwrap()
        .insert("gate-query".into(), gate.clone());

    // when
    let actual = logout_during_query(&harness, gate, true).await;

    // then
    assert_eq!(actual.error.as_ref().unwrap().code, ErrorCode::AuthRequired);
    assert_eq!(actual.error.unwrap().outcome, Outcome::Applied);
    assert_eq!(actual.data, Value::Null);
    assert!(actual.result_id.is_none());
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn runtime_refresh_import는_회전한_revision에_issuer_주체를_묶는다() {
    // given
    let harness = Harness::new();
    harness
        .connections
        .saved
        .lock()
        .unwrap()
        .insert("conn-a".into(), entra_connection("entra_user"));
    let input = request(
        Operation::CredentialImport,
        json!({"slot":"entra_refresh_token","input_channel":{"kind":"protected_stdin"},"persistence":"session"}),
    );
    harness
        .runtime
        .secret_inputs
        .register(
            &caller(),
            &input.request_id,
            &input.input["input_channel"],
            b"synthetic-raw-refresh".to_vec(),
        )
        .unwrap();

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_ne!(
        harness.connections.saved.lock().unwrap()["conn-a"].auth_revision,
        "auth-1"
    );
    assert!(harness
        .runtime
        .auth
        .session_store()
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_some_and(
            |secret| serde_json::from_str::<Value>(secret.expose()).is_ok_and(|record| record
                ["credential_identity"]
                .as_str()
                .unwrap()
                .ends_with(&harness.connections.saved.lock().unwrap()["conn-a"].auth_revision))
        ));
    assert!(harness
        .secrets
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("synthetic-raw-refresh"));
}

#[test]
fn 결과의_32_mib_상한은_행을_절단하고_원인을_보존한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let rows = (0..10)
        .map(|_| json!(["a".repeat(4 * 1024 * 1024)]))
        .collect();
    let source = snapshot(rows);

    // when
    let actual = insert_bounded(&store, source);

    // then
    assert!(serde_json::to_vec(&actual).unwrap().len() <= super::super::results::MAX_RESULT_BYTES);
    assert!(actual.limits.truncated);
    assert!(actual.limits.reasons.contains(&"byte_limit".into()));
    assert!(actual.sets[0].rows.len() < 10);
}

fn insert_bounded(
    store: &ResultStore,
    source: super::super::results::ResultSnapshot,
) -> super::super::results::ResultSnapshot {
    let id = store.insert(binding(), source).unwrap();
    store.snapshot(&id, &binding()).unwrap()
}

async fn dispatcher_session_login(
    harness: &Harness,
    input: &ServiceRequest,
) -> (ServiceResponse, ServiceResponse) {
    let begin = harness.call(input).await;
    let id = begin.data["auth_job_id"].as_str().unwrap().to_owned();
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = harness
                .call(&request(Operation::AuthStatus, json!({"auth_job_id":id})))
                .await;
            if status.data["state"] == "authenticated" || status.data["state"] == "failed" {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    (begin, status)
}

#[tokio::test]
async fn dispatcher의_auth_begin_session_요청은_schema를_통과하고_session_slot만_저장한다() {
    // given
    let harness = Harness::new();
    harness
        .connections
        .saved
        .lock()
        .unwrap()
        .insert("conn-a".into(), entra_connection("entra_user"));
    let input = request(
        Operation::AuthBegin,
        json!({"auth_mode":"entra_user","persistence":"session"}),
    );

    // when
    let (begin, status) = dispatcher_session_login(&harness, &input).await;

    // then
    assert_eq!(begin.status, JobStatus::Succeeded);
    assert_eq!(status.status, JobStatus::Succeeded);
    assert_eq!(status.data["state"], "authenticated");
    assert!(harness
        .runtime
        .auth
        .session_store()
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_some());
    assert!(harness
        .secrets
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn auth_begin의_허용되지_않은_저장_방식은_인증을_시작하지_않는다() {
    // given
    let harness = Harness::new();
    harness
        .connections
        .saved
        .lock()
        .unwrap()
        .insert("conn-a".into(), entra_connection("entra_user"));
    let input = request(
        Operation::AuthBegin,
        json!({"auth_mode":"entra_user","persistence":"plain_file"}),
    );

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::InvalidArgument);
    assert!(harness
        .runtime
        .auth
        .session_store()
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
    assert!(harness
        .secrets
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
}
