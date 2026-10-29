use super::*;
use crate::{
    ai_activity::AiActivityEvent,
    service_platform::{HistoryRecord, Phase, QueryHistory},
    services::history::{HistoryEntry, HistoryMeta, HistoryQuery, HistoryStore, LiveJobs},
};
use std::collections::HashMap;

fn event(id: &str, connection: &str, status: &str, query: &str) -> AiActivityEvent {
    AiActivityEvent {
        id: id.into(),
        session_id: "session".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        tool: "query.execute".into(),
        connection_id: Some(connection.into()),
        connection_name: None,
        database: None,
        query: Some(query.into()),
        query_kind: Some("select".into()),
        duration_ms: 5,
        status: status.into(),
        rows: Some(1),
        error: None,
        client_hint: None,
        approval_id: None,
    }
}
fn record(job: &str, phase: Phase, at_ms: u64, connection: &str, status: &str) -> HistoryRecord {
    HistoryRecord {
        event: event(
            &uuid::Uuid::new_v4().to_string(),
            connection,
            status,
            "SELECT 1",
        ),
        job_id: job.into(),
        phase,
        owner_instance_id: "me".into(),
        timestamp_ms: at_ms,
    }
}
fn update_request(version: u64, patch: Value) -> ServiceRequest {
    let mut request = request(Operation::ConnectionUpdate, json!({"patch":patch}));
    request.expected_version = Some(version);
    request
}
fn history(directory: &Directory) -> QueryHistory {
    QueryHistory::new(directory.0.clone(), "me".into())
}
fn write_config(directory: &Directory, value: Value) {
    std::fs::create_dir_all(&directory.0).unwrap();
    std::fs::write(directory.0.join("config.json"), value.to_string()).unwrap();
}
fn lines(directory: &Directory) -> Vec<serde_json::Value> {
    let mut names = vec!["service_history.jsonl".to_owned()];
    names.extend((1..=5).map(|index| format!("service_history.{index}.jsonl")));
    names
        .iter()
        .filter_map(|name| std::fs::read_to_string(directory.0.join(name)).ok())
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .map(|line| serde_json::from_str(&line).unwrap())
        .collect()
}
fn list(history: &QueryHistory, query: HistoryQuery) -> Vec<HistoryEntry> {
    history.list(&query, &LiveJobs::default()).unwrap().entries
}

fn query_input(text: &str) -> ServiceRequest {
    request(
        Operation::QueryExecute,
        json!({"language":"sql","text":text,"parameters":[],"page_size":10}),
    )
}
async fn run(harness: &Harness, input: &ServiceRequest) -> ServiceResponse {
    harness
        .runtime
        .call_validated(input, &caller(CallerTransport::Gui))
        .await
}

#[tokio::test]
async fn 모든_작업은_같은_작업_id로_시작과_종료_이벤트를_남긴다() {
    // given
    let harness = Harness::new().await;

    // when
    let response = run(&harness, &query_input("SELECT 1")).await;
    let job_id = response.job_id.clone().unwrap();
    let files = lines(&harness.directory);
    let entries = harness
        .runtime
        .job_history(&HistoryQuery::default())
        .unwrap()
        .entries;

    // then
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|line| line["jobId"] == job_id.as_str()));
    assert_eq!(files[0]["phase"], "start");
    assert_eq!(files[1]["phase"], "terminal");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].job_id, job_id);
    assert_eq!(entries[0].status, JobStatus::Succeeded);
    assert_eq!(entries[0].query.as_deref(), Some("SELECT 1"));
    assert_eq!(entries[0].connection_id.as_deref(), Some("fixture"));
    assert_eq!(entries[0].row_count, Some(1));
    assert!(entries[0].elapsed_ms.is_some() && entries[0].finished_at_ms.is_some());
    assert!(!entries[0].legacy && !entries[0].cancellable);
}

#[tokio::test]
async fn 이력은_드라이버_오류의_비밀과_응답_본문을_저장하지_않는다() {
    // given
    let harness = Harness::new().await;
    harness
        .driver
        .fail_with_secret
        .store(true, Ordering::SeqCst);

    // when
    let failed = run(&harness, &query_input("SELECT 1")).await;
    let text = std::fs::read_to_string(harness.directory.0.join("service_history.jsonl")).unwrap();
    let entries = harness
        .runtime
        .job_history(&HistoryQuery::default())
        .unwrap()
        .entries;

    // then
    assert_eq!(failed.status, JobStatus::Failed);
    assert!(!text.contains("canary"));
    assert!(!text.contains("\"data\""));
    assert!(!text.contains("\"response\""));
    assert!(entries[0]
        .error
        .as_deref()
        .unwrap()
        .starts_with("DRIVER_EXITED"));
}

#[tokio::test]
async fn 연결_설정_입력은_이력에_본문_없이_작업_이름만_남긴다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let input = update_request(saved.version, json!({"name":"history-private-name"}));

    // when
    let response = run(&harness, &input).await;
    let text = std::fs::read_to_string(harness.directory.0.join("service_history.jsonl")).unwrap();

    // then
    assert_eq!(response.status, JobStatus::Succeeded);
    assert!(text.contains("connection.update"));
    assert!(!text.contains("history-private-name"));
}

#[tokio::test]
async fn 감사를_끄면_이력을_쓰지_않는다() {
    // given
    let harness = Harness::new().await;
    write_config(&harness.directory, json!({"aiAuditEnabled":false}));

    // when
    let response = run(&harness, &query_input("SELECT 1")).await;
    let entries = harness
        .runtime
        .job_history(&HistoryQuery::default())
        .unwrap()
        .entries;

    // then
    assert_eq!(response.status, JobStatus::Succeeded);
    assert!(!harness.directory.0.join("service_history.jsonl").exists());
    assert!(entries.is_empty());
}

#[tokio::test]
async fn 읽기전용_연결의_쓰기는_작업을_만들지_않고_이력도_남기지_않는다() {
    // given
    let harness = Harness::new().await;
    config(&harness.directory.0, true);
    let mcp = caller(CallerTransport::Mcp);
    let input = query_input("DELETE FROM items");
    harness.policy.register_query_write(&mcp, &input).unwrap();

    // when
    let response = harness.runtime.call_validated(&input, &mcp).await;

    // then
    assert_eq!(response.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert!(response.job_id.is_none());
    assert!(!harness.directory.0.join("service_history.jsonl").exists());
}

#[test]
fn 이전_감사_기록은_완료된_레거시_이력으로_함께_읽는다() {
    // given
    let directory = Directory::new();
    let history = history(&directory);
    crate::ai_activity::append_event_in(
        &directory.0,
        &event("old", "conn-a", "success", "SELECT old"),
    )
    .unwrap();
    crate::ai_activity::append_event_in(&directory.0, &event("bad", "conn-a", "denied", "DROP x"))
        .unwrap();
    history
        .append(&record(
            "j-new",
            Phase::Start,
            4_000_000_000_000,
            "conn-a",
            "queued",
        ))
        .unwrap();

    // when
    let actual = list(&history, HistoryQuery::default());

    // then
    assert_eq!(actual.len(), 3);
    assert_eq!(actual[0].job_id, "j-new");
    let legacy: Vec<_> = actual.iter().filter(|entry| entry.legacy).collect();
    assert_eq!(legacy.len(), 2);
    assert!(legacy.iter().all(|entry| !entry.cancellable
        && entry.finished_at_ms.is_some()
        && entry.job_id.starts_with("legacy-")));
    assert!(legacy
        .iter()
        .any(|entry| entry.status == JobStatus::Succeeded
            && entry.query.as_deref() == Some("SELECT old")));
    assert!(legacy.iter().any(|entry| entry.status == JobStatus::Failed));
}

#[test]
fn 연결과_상태와_기간으로_거르고_불투명_커서로_빠짐없이_나눠_읽는다() {
    // given
    let directory = Directory::new();
    let history = history(&directory);
    for index in 0..5u64 {
        let connection = if index % 2 == 0 { "conn-a" } else { "conn-b" };
        let job = format!("j-{index}");
        history
            .append(&record(
                &job,
                Phase::Start,
                1_000 + index,
                connection,
                "queued",
            ))
            .unwrap();
        let status = if index == 4 { "failed" } else { "succeeded" };
        history
            .append(&record(
                &job,
                Phase::Terminal,
                1_100 + index,
                connection,
                status,
            ))
            .unwrap();
    }

    // when
    let by_connection = list(
        &history,
        HistoryQuery {
            connection_id: Some("conn-b".into()),
            ..Default::default()
        },
    );
    let by_status = list(
        &history,
        HistoryQuery {
            status: Some(JobStatus::Failed),
            ..Default::default()
        },
    );
    let by_time = list(
        &history,
        HistoryQuery {
            since_ms: Some(1_001),
            until_ms: Some(1_003),
            ..Default::default()
        },
    );
    let mut paged = Vec::new();
    let mut cursor = None;
    loop {
        let page = history
            .list(
                &HistoryQuery {
                    limit: Some(2),
                    cursor: cursor.clone(),
                    ..Default::default()
                },
                &LiveJobs::default(),
            )
            .unwrap();
        paged.extend(page.entries.into_iter().map(|entry| entry.job_id));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    // then
    assert_eq!(
        by_connection
            .iter()
            .map(|entry| entry.job_id.as_str())
            .collect::<Vec<_>>(),
        ["j-3", "j-1"]
    );
    assert_eq!(by_status.len(), 1);
    assert_eq!(by_status[0].job_id, "j-4");
    assert_eq!(
        by_time
            .iter()
            .map(|entry| entry.job_id.as_str())
            .collect::<Vec<_>>(),
        ["j-3", "j-2", "j-1"]
    );
    assert_eq!(paged, ["j-4", "j-3", "j-2", "j-1", "j-0"]);
}

#[test]
fn 한도는_1에서_200_사이이고_잘못된_커서는_거부한다() {
    // given
    let directory = Directory::new();
    let history = history(&directory);
    let call = |limit, cursor: Option<&str>| {
        history
            .list(
                &HistoryQuery {
                    limit,
                    cursor: cursor.map(str::to_owned),
                    ..Default::default()
                },
                &LiveJobs::default(),
            )
            .map(|page| page.entries.len())
    };

    // when
    let actual = [
        call(Some(0), None),
        call(Some(201), None),
        call(Some(200), None),
        call(Some(1), None),
    ];
    let invalid = call(Some(5), Some("not-a-cursor")).unwrap_err();

    // then
    assert_eq!(
        actual[0].as_ref().unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        actual[1].as_ref().unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert!(actual[2].is_ok() && actual[3].is_ok());
    assert_eq!(invalid.code, ErrorCode::InvalidPageToken);
}

#[test]
fn 종료_이벤트가_없는_시작은_재시작_뒤_결과를_알_수_없다() {
    // given
    let directory = Directory::new();
    let first = history(&directory);
    first.record_start(
        &HistoryMeta::new(
            "j-lost",
            "query.execute",
            Some("conn-a"),
            Some("UPDATE t SET a = 1"),
        ),
        JobStatus::Queued,
    );
    drop(first);

    // when
    let restarted = QueryHistory::new(directory.0.clone(), "after-restart".into());
    let actual = list(&restarted, HistoryQuery::default());

    // then
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].status, JobStatus::OutcomeUnknown);
    assert!(actual[0].finished_at_ms.is_none());
    assert!(!actual[0].cancellable);
}

#[test]
fn 취소_가능_표시는_작업을_소유한_현재_프로세스만_붙인다() {
    // given
    let directory = Directory::new();
    let history = history(&directory);
    history
        .append(&record("j-live", Phase::Start, 10, "conn-a", "running"))
        .unwrap();
    let live = |instance: &str| LiveJobs {
        instance_id: instance.into(),
        statuses: HashMap::from([("j-live".to_owned(), JobStatus::Running)]),
    };

    // when
    let owner = history.list(&HistoryQuery::default(), &live("me")).unwrap();
    let other = history
        .list(&HistoryQuery::default(), &live("someone-else"))
        .unwrap();

    // then
    assert!(owner.entries[0].cancellable);
    assert_eq!(owner.entries[0].status, JobStatus::Running);
    assert!(!other.entries[0].cancellable);
    assert_eq!(other.entries[0].status, JobStatus::OutcomeUnknown);
}

#[test]
fn 여러_기록자가_동시에_써도_줄이_섞이지_않고_회전도_유실이_없다() {
    // given
    let directory = Arc::new(Directory::new());
    write_config(&directory, json!({"aiAuditMaxEntries":50}));

    // when
    let workers: Vec<_> = (0..8)
        .map(|worker| {
            let directory = directory.clone();
            std::thread::spawn(move || {
                let history = QueryHistory::new(directory.0.clone(), format!("proc-{worker}"));
                for index in 0..25u64 {
                    let job = format!("j-{worker}-{index}");
                    history
                        .append(&record(&job, Phase::Start, index, "conn-a", "queued"))
                        .unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let stored = lines(&directory);

    // then
    assert_eq!(stored.len(), 200);
    let mut jobs: Vec<_> = stored
        .iter()
        .map(|line| line["jobId"].as_str().unwrap().to_owned())
        .collect();
    jobs.sort();
    jobs.dedup();
    assert_eq!(jobs.len(), 200);
    assert!(directory.0.join("service_history.1.jsonl").exists());
    assert!(!directory.0.join("service_history.6.jsonl").exists());
}

#[cfg(unix)]
#[test]
fn 이력_파일은_사용자만_읽을_수_있다() {
    // given
    use std::os::unix::fs::PermissionsExt;
    let directory = Directory::new();
    let history = history(&directory);

    // when
    history
        .append(&record("j-private", Phase::Start, 1, "conn-a", "queued"))
        .unwrap();
    let mode = std::fs::metadata(directory.0.join("service_history.jsonl"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;

    // then
    assert_eq!(mode, 0o600);
}
