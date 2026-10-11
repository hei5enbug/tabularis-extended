use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::watch;

use super::{
    persisted_history_items, reap_expired, result_metadata, sanitize_driver_error, snapshot_result,
    JobRecord, ManagedJobSpec, QueryAuditContext, QueryFilter, QueryHistoryItem, QueryManager,
    QueryState, QueryStatus, StoredResultSet, MAX_RETAINED_ROWS, MAX_SNAPSHOT_BYTES,
    MAX_SNAPSHOT_SLOTS, SNAPSHOT_TTL,
};
use crate::ai_activity::AiActivityEvent;
use crate::config::AppConfig;
use crate::drivers::driver_trait::ManagedQueryFailure;
use crate::drivers::sqlite::SqliteDriver;
use crate::models::{ConnectionParams, DatabaseSelection, QueryResult, SavedConnection};

fn query_result(
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    additional_results: Option<Vec<QueryResult>>,
) -> QueryResult {
    QueryResult {
        columns,
        rows,
        affected_rows: 0,
        truncated: false,
        pagination: None,
        additional_results,
    }
}

fn result_set(columns: Vec<String>, rows: Vec<Vec<Value>>) -> StoredResultSet {
    StoredResultSet {
        columns,
        rows,
        truncated: false,
        truncation_reason: None,
    }
}

fn synthetic_job(
    query_id: &str,
    created_at: &str,
    result_sets: Option<Vec<StoredResultSet>>,
    completed_ago: Duration,
) -> JobRecord {
    let now = tokio::time::Instant::now();
    let completed = now - completed_ago;
    let (cancel_tx, _cancel_rx) = watch::channel(false);
    let snapshot_bytes = result_sets
        .as_ref()
        .and_then(|sets| super::serialized_len(sets).ok())
        .unwrap_or(0);
    let retained_rows = result_sets
        .as_ref()
        .map(|sets| sets.iter().map(|set| set.rows.len()).sum())
        .unwrap_or(0);
    let status = QueryStatus {
        query_id: query_id.to_string(),
        state: QueryState::Succeeded,
        connection_id: "synthetic-connection".to_string(),
        created_at: created_at.to_string(),
        started_at: Some(created_at.to_string()),
        completed_at: Some(created_at.to_string()),
        elapsed_ms: 1,
        result_available: result_sets.is_some(),
        cancellable: false,
        result_sets: result_sets.as_deref().map(result_metadata),
        error: None,
    };
    JobRecord {
        status,
        query: "SELECT synthetic_value".to_string(),
        connection_name: "Synthetic connection".to_string(),
        audit: QueryAuditContext {
            session_id: "synthetic-session".to_string(),
            client_hint: None,
            connection_id: "synthetic-connection".to_string(),
            connection_name: "Synthetic connection".to_string(),
            query: "SELECT synthetic_value".to_string(),
            enabled: false,
            max_entries: 1,
        },
        fingerprint: "synthetic-fingerprint".to_string(),
        created_at: now,
        started_at: Some(now),
        completed_at: Some(completed),
        deadline: now + Duration::from_secs(60),
        cancel_tx,
        cancel_handle: None,
        result_sets,
        snapshot_bytes,
        retained_rows,
        reserved: false,
        cancel_requested_at: None,
    }
}

async fn insert_synthetic_job(
    manager: &QueryManager,
    query_id: &str,
    created_at: &str,
    result_sets: Option<Vec<StoredResultSet>>,
) {
    let job = synthetic_job(query_id, created_at, result_sets, Duration::ZERO);
    let mut state = manager.state.lock().await;
    state.reserved_bytes = state.reserved_bytes.saturating_add(job.snapshot_bytes);
    state.jobs.insert(query_id.to_string(), job);
}

fn history_event(
    id: &str,
    query_id: Option<&str>,
    event_phase: Option<&str>,
    tool: &str,
    timestamp: &str,
    status: &str,
) -> AiActivityEvent {
    AiActivityEvent {
        id: id.to_string(),
        session_id: "synthetic-session".to_string(),
        timestamp: timestamp.to_string(),
        tool: tool.to_string(),
        connection_id: Some("synthetic-connection".to_string()),
        connection_name: Some("Synthetic connection".to_string()),
        query: Some("SELECT synthetic_value".to_string()),
        query_kind: Some("select".to_string()),
        duration_ms: 17,
        status: status.to_string(),
        rows: Some(4),
        error: None,
        client_hint: None,
        approval_id: None,
        query_id: query_id.map(str::to_string),
        event_phase: event_phase.map(str::to_string),
    }
}

fn history_filter(limit: usize) -> QueryFilter {
    QueryFilter {
        connection_id: None,
        status: None,
        since: None,
        until: None,
        limit,
    }
}

fn history_by_id(
    items: Vec<QueryHistoryItem>,
) -> std::collections::HashMap<String, QueryHistoryItem> {
    items
        .into_iter()
        .map(|item| (item.query_id.clone(), item))
        .collect()
}

#[test]
fn given_중복_열과_유니코드_값_when_스냅샷을_만들면_then_결과_구조가_유지된다() {
    // Given: duplicate labels, positional values of several JSON types, and an empty middle result set.
    let result = query_result(
        vec![
            "이름".to_string(),
            "이름".to_string(),
            "활성".to_string(),
            "수".to_string(),
        ],
        vec![vec![
            json!("雪だるま ☃"),
            Value::Null,
            json!(true),
            json!(42),
        ]],
        Some(vec![
            query_result(Vec::new(), Vec::new(), None),
            query_result(vec!["decimal".to_string()], vec![vec![json!(3.125)]], None),
        ]),
    );

    // When: the result is captured into the immutable stored representation.
    let (sets, _bytes, retained_rows) = snapshot_result(result, 10).expect("snapshot fits");

    // Then: duplicate names, typed positions, the empty set, and all set boundaries survive.
    assert_eq!(retained_rows, 2);
    assert_eq!(sets.len(), 3);
    assert_eq!(
        sets[0].columns,
        vec![
            "이름".to_string(),
            "이름".to_string(),
            "활성".to_string(),
            "수".to_string()
        ]
    );
    assert_eq!(
        sets[0].rows,
        vec![vec![
            json!("雪だるま ☃"),
            Value::Null,
            json!(true),
            json!(42)
        ]]
    );
    assert!(sets[1].columns.is_empty());
    assert!(sets[1].rows.is_empty());
    assert_eq!(sets[2].columns, vec!["decimal".to_string()]);
    assert_eq!(sets[2].rows, vec![vec![json!(3.125)]]);
    assert_eq!(
        result_metadata(&sets)
            .iter()
            .map(|metadata| metadata.result_set_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn given_여러_결과_집합의_행이_만_개를_넘을_때_when_스냅샷을_만들면_then_전체_행_한도로_뒤_집합이_잘린다(
) {
    // Given: two result sets whose combined row count exceeds the global retention limit.
    let first = (0..6_000)
        .map(|value| vec![json!(value)])
        .collect::<Vec<_>>();
    let second = (6_000..11_000)
        .map(|value| vec![json!(value)])
        .collect::<Vec<_>>();
    let result = query_result(
        vec!["value".to_string()],
        first,
        Some(vec![query_result(vec!["value".to_string()], second, None)]),
    );

    // When: the caller asks to retain as many rows as possible.
    let (sets, _bytes, retained_rows) =
        snapshot_result(result, usize::MAX).expect("row cap is applied safely");

    // Then: rows are capped across result-set boundaries, with truncation recorded on the later set.
    assert_eq!(retained_rows, MAX_RETAINED_ROWS);
    assert_eq!(sets[0].rows.len(), 6_000);
    assert!(!sets[0].truncated);
    assert_eq!(sets[1].rows.len(), 4_000);
    assert!(sets[1].truncated);
    assert_eq!(sets[1].truncation_reason.as_deref(), Some("max_rows"));
}

#[test]
fn given_명시적인_작은_행_한도가_있을_때_when_스냅샷을_만들면_then_한도만큼만_저장된다() {
    // Given: a result with more rows than the request's explicit cap.
    let result = query_result(
        vec!["value".to_string()],
        (1..=4).map(|value| vec![json!(value)]).collect(),
        None,
    );

    // When: the caller requests one stored row.
    let (sets, _bytes, retained_rows) = snapshot_result(result, 1).expect("snapshot fits");

    // Then: the cap is applied and the snapshot explains why it was shortened.
    assert_eq!(retained_rows, 1);
    assert_eq!(sets[0].rows, vec![vec![json!(1)]]);
    assert!(sets[0].truncated);
    assert_eq!(sets[0].truncation_reason.as_deref(), Some("max_rows"));
}

#[test]
fn given_직렬화된_UTF8이_십육_MiB를_넘을_때_when_스냅샷을_만들면_then_크기_오류를_반환한다() {
    // Given: one serialized row that exceeds the per-snapshot byte budget.
    let oversized = "x".repeat(MAX_SNAPSHOT_BYTES);
    let result = query_result(
        vec!["payload".to_string()],
        vec![vec![json!(oversized)]],
        None,
    );

    // When: the manager measures the UTF-8 JSON representation.
    let snapshot = snapshot_result(result, 1);

    // Then: oversized content is rejected with the public capacity code.
    assert!(matches!(snapshot, Err("RESULT_TOO_LARGE")));
}

#[tokio::test]
async fn given_저장된_스냅샷이_있을_때_when_원본_픽스처가_바뀌면_then_원래_페이지를_반환한다() {
    // Given: a source fixture whose immutable snapshot is placed in manager state.
    let source_columns = vec!["value".to_string()];
    let source_rows = vec![vec![json!("first")], vec![json!("second")]];
    let mut source_fixture = query_result(source_columns.clone(), source_rows.clone(), None);
    let (stored, _bytes, _rows) =
        snapshot_result(query_result(source_columns, source_rows, None), 10)
            .expect("snapshot fits");
    let manager = QueryManager::new();
    insert_synthetic_job(
        &manager,
        "immutable-snapshot",
        "2026-01-01T00:00:00Z",
        Some(stored),
    )
    .await;

    // When: the original fixture is changed after snapshot creation.
    source_fixture.columns[0] = "mutated".to_string();
    source_fixture.rows[0][0] = json!("changed after capture");

    // Then: stored pages remain unchanged, and an offset past the end is an empty page.
    let first = manager
        .fetch("immutable-snapshot", 0, 0, 1)
        .await
        .ok()
        .expect("first page exists");
    let second = manager
        .fetch("immutable-snapshot", 0, 1, 1)
        .await
        .ok()
        .expect("second page exists");
    let beyond = manager
        .fetch("immutable-snapshot", 0, 20, 1)
        .await
        .ok()
        .expect("beyond-end page is empty");
    assert_eq!(first.columns, vec!["value".to_string()]);
    assert_eq!(first.rows, vec![vec![json!("first")]]);
    assert_eq!(second.rows, vec![vec![json!("second")]]);
    assert!(beyond.rows.is_empty());
    assert_eq!(beyond.offset, 20);
    assert_eq!(beyond.next_offset, 2);
    assert!(!beyond.has_more);
}

#[tokio::test]
async fn given_가져오기와_해제가_경쟁할_때_when_둘_다_완료되면_then_전체_페이지나_만료를_반환하고_행_수는_남긴다(
) {
    // Given: a completed two-row result with row-count metadata retained separately from its snapshot.
    let manager = QueryManager::new();
    insert_synthetic_job(
        &manager,
        "release-race",
        "2026-01-02T00:00:00Z",
        Some(vec![result_set(
            vec!["value".to_string()],
            vec![vec![json!(1)], vec![json!(2)]],
        )]),
    )
    .await;

    // When: a full-page fetch races with the idempotent release operation.
    let (fetch_result, release_result) = tokio::join!(
        manager.fetch("release-race", 0, 0, 10),
        manager.release("release-race")
    );

    // Then: a fetch sees either the complete page or the released state, never partial rows.
    release_result.ok().expect("release succeeds");
    match fetch_result {
        Ok(page) => assert_eq!(page.rows, vec![vec![json!(1)], vec![json!(2)]]),
        Err(error) => assert_eq!(error.code, "RESULT_EXPIRED"),
    }

    // And: repeated release is harmless and history keeps its completed row count.
    let repeated = manager
        .release("release-race")
        .await
        .ok()
        .expect("release is idempotent");
    assert_eq!(repeated["released"], json!(false));
    let history = manager
        .list_history(history_filter(10), Vec::new(), None)
        .await
        .ok()
        .expect("history is available");
    assert_eq!(history.items[0].rows, Some(2));
    assert_eq!(history.items[0].state, "succeeded");
    assert_eq!(history.items[0].created_at, "2026-01-02T00:00:00Z");
    assert_eq!(
        history.items[0].completed_at.as_deref(),
        Some("2026-01-02T00:00:00Z")
    );
    assert!(!history.items[0].result_available);
}

#[tokio::test]
async fn given_스냅샷이_만료되었을_때_when_정리하면_then_바이트만_해제하고_작업_메타데이터는_남긴다(
) {
    // Given: a completed snapshot older than the retention TTL.
    let manager = QueryManager::new();
    let sets = vec![result_set(vec!["value".to_string()], vec![vec![json!(9)]])];
    let job = synthetic_job(
        "expired-snapshot",
        "2026-01-03T00:00:00Z",
        Some(sets),
        SNAPSHOT_TTL + Duration::from_secs(1),
    );
    let expected_bytes = job.snapshot_bytes;
    {
        let mut state = manager.state.lock().await;
        state.reserved_bytes = expected_bytes;
        state.jobs.insert("expired-snapshot".to_string(), job);
    }

    // When: the regular expiry helper reaps the stale result bytes.
    {
        let mut state = manager.state.lock().await;
        reap_expired(&mut state, tokio::time::Instant::now());
        assert_eq!(state.reserved_bytes, 0);
        let retained = state
            .jobs
            .get("expired-snapshot")
            .expect("metadata remains");
        assert!(retained.result_sets.is_none());
        assert_eq!(retained.snapshot_bytes, 0);
        assert!(!retained.status.result_available);
    }

    // Then: metadata is queryable while result fetch reports expiration.
    let status = manager
        .status("expired-snapshot")
        .await
        .ok()
        .expect("job metadata remains");
    assert_eq!(status.state, QueryState::Succeeded);
    assert!(!status.result_available);
    assert_eq!(status.created_at, "2026-01-03T00:00:00Z");
    assert_eq!(status.completed_at.as_deref(), Some("2026-01-03T00:00:00Z"));
    assert_eq!(
        manager
            .fetch("expired-snapshot", 0, 0, 1)
            .await
            .err()
            .expect("expired results are unavailable")
            .code,
        "RESULT_EXPIRED"
    );
    let history = manager
        .list_history(history_filter(10), Vec::new(), None)
        .await
        .ok()
        .expect("history metadata remains after expiry");
    let item = history
        .items
        .first()
        .expect("expired query remains in history");
    assert_eq!(item.state, "succeeded");
    assert_eq!(item.rows, Some(1));
    assert_eq!(item.created_at, "2026-01-03T00:00:00Z");
    assert_eq!(item.completed_at.as_deref(), Some("2026-01-03T00:00:00Z"));
    assert!(!item.result_available);
}

#[tokio::test]
async fn given_작은_완료_스냅샷이_64개일_때_when_새_작업을_시작하면_then_용량_오류를_반환한다() {
    // Given: the snapshot slot table is full of small completed results, not active reservations.
    let manager = QueryManager::new();
    let small_set = vec![result_set(vec!["value".to_string()], vec![vec![json!(1)]])];
    {
        let mut state = manager.state.lock().await;
        for index in 0..MAX_SNAPSHOT_SLOTS {
            let query_id = format!("completed-{index}");
            let job = synthetic_job(
                &query_id,
                "2026-01-04T00:00:00Z",
                Some(small_set.clone()),
                Duration::ZERO,
            );
            state.reserved_bytes = state.reserved_bytes.saturating_add(job.snapshot_bytes);
            state.jobs.insert(query_id, job);
        }
        assert_eq!(
            state
                .jobs
                .values()
                .filter(|job| job.reserved || job.result_sets.is_some())
                .count(),
            MAX_SNAPSHOT_SLOTS
        );
    }
    let params = ConnectionParams {
        driver: "sqlite".to_string(),
        database: DatabaseSelection::Single("synthetic.db".to_string()),
        ..ConnectionParams::default()
    };
    let connection = SavedConnection {
        id: "capacity-check".to_string(),
        revision: String::new(),
        name: "Synthetic connection".to_string(),
        params: params.clone(),
        group_id: None,
        sort_order: None,
        detect_json_in_text_columns: None,
        appearance: None,
        tag_ids: None,
        environment: None,
    };
    let spec = ManagedJobSpec {
        connection,
        params,
        driver: Arc::new(SqliteDriver::new()),
        active_driver_ids: None,
        config: AppConfig::default(),
        query: "SELECT 1".to_string(),
        max_rows: 10,
        timeout: Duration::from_secs(30),
        fingerprint: "synthetic-fingerprint".to_string(),
        session_id: "synthetic-session".to_string(),
        client_hint: None,
        audit_enabled: false,
        max_audit_entries: 1,
    };

    // When: the real start API checks snapshot capacity before dispatching any work.
    let error = manager
        .start(spec)
        .await
        .err()
        .expect("all snapshot slots are occupied");

    // Then: a completed small result occupies the same slot as any other retained snapshot.
    assert_eq!(error.code, "RESULT_CAPACITY_EXCEEDED");
}

#[tokio::test]
async fn given_이력_커서가_있을_때_when_페이지_사이에_새_이벤트가_오면_then_키셋_페이지가_중복되지_않는다(
) {
    // Given: four persisted completed records, including an equal-timestamp keyset tie.
    let manager = QueryManager::new();
    let original = vec![
        history_event(
            "q4",
            None,
            None,
            "run_query",
            "2026-01-04T00:00:00Z",
            "success",
        ),
        history_event(
            "q3",
            None,
            None,
            "run_query",
            "2026-01-03T00:00:00Z",
            "success",
        ),
        history_event(
            "q2",
            None,
            None,
            "run_query",
            "2026-01-03T00:00:00Z",
            "success",
        ),
        history_event(
            "q1",
            None,
            None,
            "run_query",
            "2026-01-02T00:00:00Z",
            "success",
        ),
    ];
    let filter = history_filter(2);

    // When: page one is read, a newer event arrives, and page two uses the original keyset cursor.
    let first = manager
        .list_history(filter.clone(), original.clone(), None)
        .await
        .ok()
        .expect("first page succeeds");
    let cursor = first.next_cursor.clone().expect("more records exist");
    let mut with_newer = original.clone();
    with_newer.push(history_event(
        "newer-event",
        None,
        None,
        "run_query",
        "2026-01-05T00:00:00Z",
        "success",
    ));
    let second = manager
        .list_history(filter.clone(), with_newer.clone(), Some(&cursor))
        .await
        .ok()
        .expect("second page succeeds");

    // Then: the second page contains the remaining original keys once and excludes the new head item.
    let ids = first
        .items
        .iter()
        .chain(second.items.iter())
        .map(|item| item.query_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["q4", "q3", "q2", "q1"]);
    assert_eq!(
        ids.iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );

    // And: malformed and filter-mismatched cursors fail as invalid arguments.
    let malformed = manager
        .list_history(filter.clone(), with_newer.clone(), Some("not-a-cursor"))
        .await
        .err()
        .expect("malformed cursor is rejected");
    assert_eq!(malformed.code, "INVALID_ARGUMENT");
    let fresh = manager
        .list_history(filter.clone(), with_newer.clone(), None)
        .await
        .ok()
        .expect("a fresh first page succeeds");
    let filter_cursor = fresh.next_cursor.expect("cursor is generated");
    let mut different_filter = filter;
    different_filter.connection_id = Some("other-connection".to_string());
    let mismatch = manager
        .list_history(different_filter, with_newer, Some(&filter_cursor))
        .await
        .err()
        .expect("cursor cannot cross filters");
    assert_eq!(mismatch.code, "INVALID_ARGUMENT");
}

#[test]
fn given_시작_종료_고아_레거시_이벤트가_있을_때_when_이력을_복원하면_then_상태를_중복_없이_안전하게_표시한다(
) {
    // Given: a duplicate terminal record, an orphan start, and a completed legacy run_query event.
    let start = history_event(
        "managed-start-event",
        Some("managed-query"),
        Some("start"),
        "start_query",
        "2026-02-01T00:00:00Z",
        "running",
    );
    let terminal = history_event(
        "managed-terminal-event",
        Some("managed-query"),
        Some("terminal"),
        "start_query",
        "2026-02-01T00:00:01Z",
        "succeeded",
    );
    let orphan = history_event(
        "orphan-start-event",
        Some("orphan-query"),
        Some("start"),
        "start_query",
        "2026-02-01T00:00:02Z",
        "running",
    );
    let legacy = history_event(
        "legacy-query",
        None,
        None,
        "run_query",
        "2026-02-01T00:00:03Z",
        "success",
    );

    // When: persisted events are folded into user-visible history rows.
    let items = history_by_id(persisted_history_items(vec![
        start.clone(),
        terminal.clone(),
        terminal,
        orphan,
        legacy,
    ]));

    // Then: managed events deduplicate, orphan starts stay unavailable, and completed legacy history remains.
    let managed = items.get("managed-query").expect("managed pair is present");
    assert_eq!(managed.state, "succeeded");
    assert_eq!(managed.created_at, "2026-02-01T00:00:00Z");
    assert_eq!(
        managed.completed_at.as_deref(),
        Some("2026-02-01T00:00:01Z")
    );
    assert!(!managed.result_available);
    assert!(!managed.cancellable);

    let orphan = items.get("orphan-query").expect("orphan is retained");
    assert_eq!(orphan.state, "unknown");
    assert_eq!(orphan.completed_at, None);
    assert!(!orphan.result_available);
    assert!(!orphan.cancellable);

    let legacy = items
        .get("legacy-query")
        .expect("legacy record is retained");
    assert_eq!(legacy.state, "succeeded");
    assert_eq!(legacy.completed_at.as_deref(), Some("2026-02-01T00:00:03Z"));
    assert!(!legacy.result_available);
    assert!(!legacy.cancellable);
}

#[test]
fn given_비밀을_포함한_원격_오류가_있을_때_when_정제하면_then_토큰과_원문은_반환하지_않는다() {
    // Given: remote and transport failures contain synthetic credential-like text.
    let raw = "access_token=synthetic-secret database-password=raw-driver-detail";
    let remote = ManagedQueryFailure::Remote {
        code: 500,
        message: raw.to_string(),
        data: Some(json!({ "token": raw })),
    };
    let transport = ManagedQueryFailure::Transport(raw.to_string());

    // When: the manager converts driver failures to safe history errors.
    let remote_error = sanitize_driver_error(&remote);
    let transport_error = sanitize_driver_error(&transport);

    // Then: only stable public messages remain, with no raw error or secret material.
    for error in [&remote_error, &transport_error] {
        assert!(!error.message.contains("synthetic-secret"));
        assert!(!error.message.contains("raw-driver-detail"));
        assert!(!error.message.contains("access_token"));
    }
    assert_eq!(remote_error.code, "QUERY_FAILED");
    assert_eq!(transport_error.code, "DRIVER_UNAVAILABLE");
}
