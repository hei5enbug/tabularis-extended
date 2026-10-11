use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn synthetic_audit(enabled: bool) -> QueryAuditContext {
    QueryAuditContext {
        session_id: "synthetic-session".to_string(),
        client_hint: None,
        connection_id: "synthetic-connection".to_string(),
        connection_name: "Synthetic connection".to_string(),
        query: "SELECT 1".to_string(),
        enabled,
        max_entries: 20,
    }
}

fn synthetic_job(
    query_id: &str,
    state: QueryState,
    audit: QueryAuditContext,
    cancel_handle: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
) -> JobRecord {
    let now = tokio::time::Instant::now();
    let (cancel_tx, _cancel_rx) = watch::channel(false);
    let created_at = ai_activity::now_iso8601();
    JobRecord {
        status: QueryStatus {
            query_id: query_id.to_string(),
            state: state.clone(),
            connection_id: "synthetic-connection".to_string(),
            created_at,
            started_at: (state != QueryState::Queued).then(|| ai_activity::now_iso8601()),
            completed_at: None,
            elapsed_ms: 0,
            result_available: false,
            cancellable: !state.is_terminal(),
            result_sets: None,
            error: None,
        },
        query: "SELECT 1".to_string(),
        connection_name: "Synthetic connection".to_string(),
        audit,
        fingerprint: "synthetic-fingerprint".to_string(),
        created_at: now,
        started_at: (state != QueryState::Queued).then_some(now),
        completed_at: None,
        deadline: now + Duration::from_secs(60),
        cancel_tx,
        cancel_handle,
        result_sets: None,
        snapshot_bytes: 0,
        retained_rows: 0,
        reserved: state == QueryState::Queued,
        cancel_requested_at: None,
    }
}

#[tokio::test]
async fn 주어진_대기중_작업을_취소하면_데이터베이스_호출없이_종료한다() {
    // Given
    let manager = QueryManager::new();
    let job = synthetic_job(
        "queued-cancel",
        QueryState::Queued,
        synthetic_audit(false),
        None,
    );
    let cancel_rx = job.cancel_tx.subscribe();
    {
        let mut state = manager.state.lock().await;
        state.reserved_bytes = MAX_SNAPSHOT_BYTES;
        state.jobs.insert("queued-cancel".to_string(), job);
    }

    // When
    let cancelled = manager
        .cancel("queued-cancel")
        .await
        .expect("queued cancellation succeeds");

    // Then
    assert!(cancelled.accepted);
    assert!(!cancelled.already_terminal);
    assert_eq!(cancelled.state, QueryState::Cancelled);
    assert!(*cancel_rx.borrow());
    let state = manager.state.lock().await;
    assert_eq!(state.reserved_bytes, 0);
    assert_eq!(
        state.jobs.get("queued-cancel").unwrap().status.state,
        QueryState::Cancelled
    );
}

#[tokio::test]
async fn 주어진_서버종료시_취소응답이없으면_미확인상태를기록하고_native취소를요청한다() {
    // Given
    let manager = QueryManager::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let cancel_calls = calls.clone();
    let cancel_handle = Arc::new(move || {
        cancel_calls.fetch_add(1, Ordering::SeqCst);
        true
    });
    let job = synthetic_job(
        "shutdown-running",
        QueryState::Running,
        synthetic_audit(false),
        Some(cancel_handle),
    );
    {
        let mut state = manager.state.lock().await;
        state.jobs.insert("shutdown-running".to_string(), job);
    }

    // When
    manager.shutdown().await;

    // Then
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let status = manager
        .status("shutdown-running")
        .await
        .expect("shutdown keeps the in-memory record");
    assert_eq!(status.state, QueryState::CancelRequested);
    assert!(!status.cancellable);
    assert_eq!(
        status.error.as_ref().map(|error| error.code.as_str()),
        Some("CANCEL_NOT_CONFIRMED")
    );
}

#[test]
fn 주어진_종료이벤트를만들면_query_id와_알수없는상태를저장하고_행을기록하지않는다() {
    // Given
    let audit = synthetic_audit(false);
    let error = QueryError {
        code: "CANCEL_NOT_CONFIRMED".to_string(),
        message: "The server is shutting down before the query stopped".to_string(),
    };

    // When
    let event = build_query_event(
        &audit,
        "synthetic-query-id",
        "terminal",
        "unknown",
        125,
        Some(&error),
        None,
    );

    // Then
    assert_eq!(event.query_id.as_deref(), Some("synthetic-query-id"));
    assert_eq!(event.event_phase.as_deref(), Some("terminal"));
    assert_eq!(event.status, "unknown");
    assert_eq!(event.rows, None);
    assert_eq!(event.duration_ms, 125);
    assert!(event.error.unwrap().contains("CANCEL_NOT_CONFIRMED"));
}

#[test]
fn 주어진_취소종료확인이없으면_오류를성공한취소로표현하지않는다() {
    // Given
    let unconfirmed = ManagedQueryFailure::Cancelled {
        server_termination_confirmed: false,
    };
    let confirmed = ManagedQueryFailure::Cancelled {
        server_termination_confirmed: true,
    };

    // When
    let unconfirmed_error = sanitize_driver_error(&unconfirmed);
    let confirmed_error = sanitize_driver_error(&confirmed);

    // Then
    assert_eq!(unconfirmed_error.code, "CANCEL_NOT_CONFIRMED");
    assert!(unconfirmed_error.message.contains("did not confirm"));
    assert_eq!(confirmed_error.code, "QUERY_CANCELLED");
}

#[test]
fn 주어진_승인요청정리가필요하면_pending과_decision파일을함께지운다() {
    // Given
    let directory = tempfile::tempdir().unwrap();
    let pending = directory.path().join("synthetic.pending.json");
    let decision = directory.path().join("synthetic.decision.json");
    std::fs::write(&pending, "synthetic").unwrap();
    std::fs::write(&decision, "synthetic").unwrap();

    // When
    cleanup_pending_approval_in(directory.path(), "synthetic");

    // Then
    assert!(!pending.exists());
    assert!(!decision.exists());
}

#[test]
fn 주어진_대기중_승인정책이_all로바뀌면_실행전에승인을요구한다() {
    // Given
    let mode_before_queue = "safe";
    let mode_after_queue = "all";

    // When
    let approval_required = approval_required_after_refresh(mode_before_queue, mode_after_queue);

    // Then
    assert!(approval_required);
    assert!(!approval_required_after_refresh("all", "all"));
    assert!(!approval_required_after_refresh("all", "safe"));
}

#[tokio::test]
async fn 주어진_완료된_결과를_해제하면_큰_열_메타데이터를_지우고_행수는남긴다() {
    // Given
    let manager = QueryManager::new();
    let sets = vec![StoredResultSet {
        columns: vec!["wide_column_name".repeat(64)],
        rows: vec![vec![json!(1)], vec![json!(2)]],
        truncated: false,
        truncation_reason: None,
    }];
    let bytes = serialized_len(&sets).expect("snapshot is bounded");
    let mut job = synthetic_job(
        "release-metadata",
        QueryState::Succeeded,
        synthetic_audit(false),
        None,
    );
    job.result_sets = Some(sets.clone());
    job.status.result_sets = Some(result_metadata(&sets));
    job.status.result_available = true;
    job.snapshot_bytes = bytes;
    job.retained_rows = 2;
    job.completed_at = Some(tokio::time::Instant::now());
    {
        let mut state = manager.state.lock().await;
        state.reserved_bytes = bytes;
        state.jobs.insert("release-metadata".to_string(), job);
    }

    // When
    let release = manager
        .release("release-metadata")
        .await
        .expect("release succeeds");

    // Then
    assert_eq!(release["released"], true);
    let status = manager
        .status("release-metadata")
        .await
        .expect("metadata remains available");
    assert!(!status.result_available);
    assert!(status.result_sets.is_none());
    assert_eq!(
        manager.state.lock().await.reserved_bytes,
        0,
        "released bytes return to the shared budget"
    );
    let history = manager
        .list_history(history_filter_for_test(), Vec::new(), None)
        .await
        .expect("scalar history remains available");
    assert_eq!(history.items[0].rows, Some(2));
}

fn history_filter_for_test() -> QueryFilter {
    QueryFilter {
        connection_id: None,
        status: None,
        since: None,
        until: None,
        limit: 10,
    }
}
