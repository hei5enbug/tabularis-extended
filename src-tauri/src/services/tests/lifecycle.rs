use super::{
    super::{
        backend::SessionFinish,
        contracts::*,
        jobs::{running_expiry_ms, JobManager, JOB_TTL_MS},
        results::reservation_expiry_ms,
        runtime::CANCEL_GRACE_MS,
        sessions::SessionManager,
        storage::now_ms,
    },
    support::*,
};
use serde_json::json;
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

#[tokio::test]
async fn 진행_중인_request_id를_중복_등록하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let input = query("SELECT 1");
    let _lease = manager
        .begin(&input, &caller(), false, false)
        .await
        .unwrap();

    // when
    let actual = manager.begin(&input, &caller(), false, false).await;

    // then
    assert!(matches!(actual, Err(error) if error.code == ErrorCode::InvalidArgument));
}

#[tokio::test]
async fn 작업_소유자와_호출자가_다르면_상태를_읽을_수_없다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let lease = manager
        .begin(&query("SELECT 1"), &caller(), false, false)
        .await
        .unwrap();
    let mut other = caller();
    other.caller_id = "other".into();

    // when
    let actual = manager.get(&lease.id(), "owner", &other);

    // then
    assert!(matches!(actual, Err(error) if error.code == ErrorCode::PermissionDenied));
}

#[tokio::test]
async fn 취소_요청을_접수해도_드라이버_확인_전에는_running을_유지한다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let lease = manager
        .begin(&query("SELECT 1"), &caller(), false, false)
        .await
        .unwrap();

    // when
    let actual = manager.cancel(&lease.id(), "owner", &caller()).unwrap();

    // then
    assert_eq!(actual.status, JobStatus::Running);
    assert!(actual.cancel_requested);
    assert!(lease.signal().is_cancelled());
}

#[tokio::test]
async fn 현재_연결_권한이_없으면_취소_신호를_보내지_않는다() {
    // given
    let harness = Harness::new();
    let lease = harness
        .runtime
        .jobs
        .begin(&query("SELECT 1"), &caller(), false, false)
        .await
        .unwrap();
    harness.policy.allowed.store(false, Ordering::SeqCst);
    let input = request(
        Operation::JobCancel,
        json!({"job_id":lease.id(),"owner_instance_id":"owner"}),
    );

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::PermissionDenied);
    assert!(!lease.signal().is_cancelled());
    assert_eq!(harness.driver.cancels.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 종료된_읽기는_interrupted로_기록된다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let lease = manager
        .begin(&query("SELECT 1"), &caller(), false, false)
        .await
        .unwrap();
    let id = lease.id();

    // when
    drop(lease);

    // then
    assert_eq!(
        manager.get(&id, "owner", &caller()).unwrap().status,
        JobStatus::Interrupted
    );
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test]
async fn 응답을_받지_못한_쓰기는_outcome_unknown으로_기록된다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let lease = manager
        .begin(&query("write"), &caller(), true, false)
        .await
        .unwrap();
    let id = lease.id();
    lease.mark_write_dispatched().unwrap();

    // when
    drop(lease);

    // then
    assert_eq!(
        manager.get(&id, "owner", &caller()).unwrap().status,
        JobStatus::OutcomeUnknown
    );
}

#[tokio::test]
async fn 재시작_복구는_진행중인_조회와_쓰기를_구분한다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let read = manager
        .begin(&query("read"), &caller(), false, false)
        .await
        .unwrap();
    let write = manager
        .begin(&query("write"), &caller(), true, false)
        .await
        .unwrap();
    write.mark_write_dispatched().unwrap();
    let read_id = read.id();
    let write_id = write.id();

    // when
    manager.recover_owner("owner").unwrap();

    // then
    assert_eq!(
        manager.get(&read_id, "owner", &caller()).unwrap().status,
        JobStatus::Interrupted
    );
    assert_eq!(
        manager.get(&write_id, "owner", &caller()).unwrap().status,
        JobStatus::OutcomeUnknown
    );
}

async fn concurrent_queries(harness: &Harness, session_id: Option<String>) -> Vec<ServiceResponse> {
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..20 {
        let mut input = query("slow");
        input.session_id = session_id.clone();
        if session_id.is_none() && index % 2 == 1 {
            input.connection_id = Some("conn-b".into());
        }
        let runtime = harness.runtime.clone();
        tasks.spawn(async move { runtime.call_validated(&input, &caller()).await });
    }
    let mut responses = Vec::new();
    while let Some(response) = tasks.join_next().await {
        responses.push(response.unwrap());
    }
    responses
}

#[tokio::test]
async fn 서로_다른_연결은_전역_여덟_작업_상한_안에서_병렬로_실행된다() {
    // given
    let harness = Harness::new();

    // when
    let actual = concurrent_queries(&harness, None).await;

    // then
    assert!(actual
        .iter()
        .all(|response| response.status == JobStatus::Succeeded));
    assert!(harness.driver.peak.load(Ordering::SeqCst) <= 8);
    assert!(harness.driver.peak.load(Ordering::SeqCst) > 4);
    assert_eq!(harness.runtime.jobs.active_count(), 0);
}

#[tokio::test]
async fn 같은_sql_세션의_물리_연결은_직렬로_사용된다() {
    // given
    let harness = Harness::new();
    let opened = harness
        .call(&request(
            Operation::SessionOpen,
            json!({"database":"fixture","mode":"read_only"}),
        ))
        .await;
    let id = opened.data["session_id"].as_str().unwrap().to_owned();

    // when
    let actual = concurrent_queries(&harness, Some(id)).await;

    // then
    assert!(actual
        .iter()
        .all(|response| response.status == JobStatus::Succeeded));
    assert_eq!(harness.driver.session_peak.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 쓰기_세션에서도_이번_쓰기_의도와_승인_없는_쿼리는_거부한다() {
    // given
    let harness = Harness::new();
    harness.allow_write();
    let opened = harness
        .call(&request(
            Operation::SessionOpen,
            json!({"database":"fixture","mode":"write"}),
        ))
        .await;
    let mut input = query("DELETE FROM items");
    input.session_id = Some(opened.data["session_id"].as_str().unwrap().into());

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteIntentRequired);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 읽기_세션에_쓰기_쿼리를_넣으면_거부한다() {
    // given
    let harness = Harness::new();
    let opened = harness
        .call(&request(
            Operation::SessionOpen,
            json!({"database":"fixture","mode":"read_only"}),
        ))
        .await;
    harness.allow_write();
    harness.policy.write_intent.store(true, Ordering::SeqCst);
    let mut input = query("DELETE FROM items");
    input.session_id = Some(opened.data["session_id"].as_str().unwrap().into());

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 세션이_지원되지_않는_operation은_handle을_무시하지_않는다() {
    // given
    let harness = Harness::new();
    let mut input = request(Operation::CatalogDatabases, json!({}));
    input.session_id = Some("ignored-handle".into());

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::UnsupportedOperation);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 유휴_세션의_rollback이_불명확하면_연결을_풀에_반환하지_않는다() {
    // given
    let driver = Arc::new(TestDriver::new());
    let manager = SessionManager::with_idle(driver.clone(), 0);
    let saved = connection("conn-a");
    let id = manager
        .open(&saved, "fixture", true, &caller(), context("conn-a"))
        .await
        .unwrap();
    driver.rollback_failure.store(true, Ordering::SeqCst);

    // when
    let actual = manager
        .acquire(&id, &saved, &caller(), false, &context("conn-a"))
        .await;

    // then
    assert!(matches!(actual, Err(error) if error.code == ErrorCode::DeadlineExceeded));
    assert_eq!(driver.discarded.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cosmos는_sql_세션을_지원한다고_가장하지_않는다() {
    // given
    let driver = Arc::new(TestDriver::new());
    let manager = SessionManager::new(driver);
    let mut saved = connection("conn-a");
    saved.driver = "cosmos-nosql".into();

    // when
    let actual = manager
        .open(&saved, "fixture", true, &caller(), context("conn-a"))
        .await;

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::UnsupportedOperation);
}

#[tokio::test]
async fn 결과_용량이_예약되어_가득_차면_새_쿼리는_db를_호출하지_않는다() {
    // given
    let harness = Harness::new();
    let _reservations: Vec<_> = (0..12)
        .map(|_| {
            harness
                .runtime
                .results
                .reserve_query("conn-a", 30000)
                .unwrap()
        })
        .collect();

    // when
    let actual = harness.call(&query("SELECT 1")).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ResourceLimit);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 조회_deadline이_지나면_취소를_전달하고_자동_재실행하지_않는다() {
    // given
    let harness = Harness::new();
    let mut input = query("wait");
    input.deadline_ms = 1000;

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::DeadlineExceeded);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.driver.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(harness.driver.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 읽기_세션의_commit도_정상적으로_물리_연결을_종료한다() {
    // given
    let driver = Arc::new(TestDriver::new());
    let manager = SessionManager::new(driver.clone());
    let saved = connection("conn-a");
    let id = manager
        .open(&saved, "fixture", true, &caller(), context("conn-a"))
        .await
        .unwrap();

    // when
    let actual = manager
        .finish(
            &id,
            &saved,
            &caller(),
            SessionFinish::Commit,
            context("conn-a"),
        )
        .await;

    // then
    assert!(actual.is_ok());
    assert_eq!(driver.discarded.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 쓰기_세션의_rollback은_쓰기_의도_없이도_세션의_쓰기_모드로_드라이버에_보낸다() {
    // given
    let driver = Arc::new(TestDriver::new());
    let manager = SessionManager::new(driver.clone());
    let saved = connection("conn-a");
    let id = manager
        .open(&saved, "fixture", false, &caller(), context("conn-a"))
        .await
        .unwrap();

    // when
    let actual = manager
        .finish(
            &id,
            &saved,
            &caller(),
            SessionFinish::Rollback,
            context("conn-a"),
        )
        .await;

    // then
    assert!(actual.is_ok());
    assert_eq!(*driver.finish_read_only.lock().unwrap(), [false]);
    assert_eq!(driver.discarded.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 취소_제어_요청은_일반_작업_semaphore를_기다리지_않는다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let mut leases = Vec::new();
    for index in 0..8 {
        let mut input = query("slow");
        input.connection_id = Some(if index < 4 { "a" } else { "b" }.into());
        leases.push(
            manager
                .begin(&input, &caller(), false, false)
                .await
                .unwrap(),
        );
    }
    let input = request(
        Operation::JobGet,
        json!({"job_id":leases[0].id(),"owner_instance_id":"owner"}),
    );

    // when
    let actual = tokio::time::timeout(
        Duration::from_millis(100),
        manager.begin(&input, &caller(), false, true),
    )
    .await;

    // then
    assert!(actual.is_ok_and(|result| result.is_ok()));
}

#[test]
fn 작업_목록_조회와_작업_종료가_동시에_반복돼도_교착하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (done, finished) = std::sync::mpsc::channel();

    // when
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (manager, stop) = (manager.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let _ = manager.live_jobs();
                    let _ = manager.live_details();
                }
            })
        })
        .collect();
    let completers: Vec<_> = (0..4)
        .map(|_| {
            let (manager, done) = (manager.clone(), done.clone());
            std::thread::spawn(move || {
                let mut held = Vec::new();
                for _ in 0..3000 {
                    held.push(manager.queue(&query("SELECT 1"), &caller(), false, true).unwrap());
                    if held.len() >= 8 {
                        held.clear();
                    }
                }
                drop(held);
                let _ = done.send(());
            })
        })
        .collect();
    drop(done);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let completed = (0..completers.len())
        .filter(|_| {
            finished
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .is_ok()
        })
        .count();
    stop.store(true, Ordering::SeqCst);

    // then
    assert_eq!(completed, completers.len());
    assert_eq!(manager.active_count(), 0);
    for reader in readers {
        reader.join().unwrap();
    }
}

fn 성공_응답(request: &ServiceRequest, job_id: &str) -> ServiceResponse {
    ServiceResponse {
        protocol_version: 1,
        request_id: request.request_id.clone(),
        connection_id: request.connection_id.clone(),
        status: JobStatus::Succeeded,
        job_id: Some(job_id.into()),
        result_id: None,
        data: json!({}),
        page: Page::default(),
        limits: Limits::default(),
        metrics: Metrics::default(),
        warnings: Vec::new(),
        error: None,
    }
}

#[tokio::test]
async fn 한_시간짜리_작업은_15분이_지나도_기한과_취소_유예가_끝나기_전에는_읽을_수_있다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let mut input = query("SELECT 1");
    input.deadline_ms = MAX_JOB_DEADLINE_MS;
    let lease = manager
        .begin(&input, &caller(), false, false)
        .await
        .unwrap();

    // when
    let record = manager.get(&lease.id(), "owner", &caller()).unwrap();
    let created = record.created_at_ms;
    let minutes = |count: u64| created + count * 60 * 1000;
    let actual = [
        record.expired_at(minutes(20)),
        record.expired_at(created + MAX_JOB_DEADLINE_MS + CANCEL_GRACE_MS - 1),
        record.expired_at(created + MAX_JOB_DEADLINE_MS + CANCEL_GRACE_MS),
    ];

    // then
    assert_eq!(actual, [false, false, true]);
}

#[test]
fn 짧은_작업의_실행_중_만료는_기존_15분을_유지한다() {
    // given
    let created = 1_000_000;

    // when
    let actual = [
        running_expiry_ms(created, 30_000),
        running_expiry_ms(created, 120_000),
        running_expiry_ms(created, MAX_JOB_DEADLINE_MS),
    ];

    // then
    assert_eq!(
        actual,
        [
            created + JOB_TTL_MS,
            created + JOB_TTL_MS,
            created + MAX_JOB_DEADLINE_MS + CANCEL_GRACE_MS
        ]
    );
}

#[tokio::test]
async fn 끝난_작업은_끝난_시각부터_15분_동안_읽을_수_있다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let mut input = query("SELECT 1");
    input.deadline_ms = MAX_JOB_DEADLINE_MS;
    let lease = manager
        .begin(&input, &caller(), false, false)
        .await
        .unwrap();
    let id = lease.id();

    // when
    let before = now_ms();
    lease.finish(성공_응답(&input, &id)).unwrap();
    let after = now_ms();
    let record = manager.get(&id, "owner", &caller()).unwrap();

    // then
    assert_eq!(record.status, JobStatus::Succeeded);
    assert!(record.expires_at_ms >= before + JOB_TTL_MS);
    assert!(record.expires_at_ms <= after + JOB_TTL_MS);
    assert!(!record.expired_at(before + JOB_TTL_MS - 1));
    assert!(record.expired_at(after + JOB_TTL_MS));
}

#[tokio::test]
async fn 끝나기_전에_종료된_작업은_종료된_시각부터_15분_동안_읽을_수_있다() {
    // given
    let directory = TestDirectory::new();
    let manager = JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let mut input = query("SELECT 1");
    input.deadline_ms = MAX_JOB_DEADLINE_MS;
    let lease = manager
        .begin(&input, &caller(), false, false)
        .await
        .unwrap();
    let id = lease.id();

    // when
    let before = now_ms();
    drop(lease);
    let after = now_ms();
    let record = manager.get(&id, "owner", &caller()).unwrap();

    // then
    assert_eq!(record.status, JobStatus::Interrupted);
    assert!(record.expires_at_ms >= before + JOB_TTL_MS);
    assert!(record.expires_at_ms <= after + JOB_TTL_MS);
}

#[test]
fn 결과_예약은_한_시간짜리_기한을_넘겨서_유지되고_그보다_길게_잡지_않는다() {
    // given
    let now = 5_000_000;

    // when
    let actual = [
        reservation_expiry_ms(now, 30_000),
        reservation_expiry_ms(now, MAX_JOB_DEADLINE_MS),
        reservation_expiry_ms(now, MAX_JOB_DEADLINE_MS * 2),
    ];

    // then
    assert_eq!(
        actual,
        [
            now + 31_000,
            now + MAX_JOB_DEADLINE_MS + 1000,
            now + MAX_JOB_DEADLINE_MS + 1000
        ]
    );
}
