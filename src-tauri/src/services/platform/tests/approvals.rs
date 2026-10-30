use super::*;

async fn consume_with_decision(
    harness: &Harness,
    request: ServiceRequest,
    change: impl FnOnce(&mut crate::ai_approval::ApprovalDecision),
) -> (
    ServiceResult<crate::services::policy::ApprovalGrant>,
    crate::ai_approval::PendingApproval,
) {
    let connection = harness.connections.get("fixture").await.unwrap();
    let remote = caller(CallerTransport::Mcp);
    let hash = value_hash(&serde_json::to_value(&request).unwrap());
    let approvals = harness.approvals.clone();
    let task = tokio::spawn(async move {
        approvals
            .consume(&connection, &remote, &request, &hash)
            .await
    });
    let pending = pending(&harness.approvals).await;
    let mut answer = decision(&pending.id, true);
    change(&mut answer);
    persistence::service_write_atomic(
        &harness
            .directory
            .0
            .join("pending_approvals")
            .join(format!("{}.decision.json", pending.id)),
        &serde_json::to_vec(&answer).unwrap(),
    )
    .unwrap();
    (task.await.unwrap(), pending)
}

#[tokio::test]
async fn 동일_큐의_한번_승인은_전체_요청_바인딩을_유지하고_파일을_정리한다() {
    // given
    let harness = Harness::new().await;
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"UPDATE fixture SET value = $1","parameters":[{"name":"$1","value":"exact-parameter"}],"page_size":10}),
    );
    // when
    let actual = consume_with_decision(&harness, request.clone(), |_| {}).await;
    // then
    assert!(actual.0.is_ok());
    assert!(actual.1.query.contains("UPDATE fixture SET value = $1"));
    assert!(actual.1.query.contains("exact-parameter"));
    assert_eq!(actual.1.query_kind, "service:query.execute");
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn edited_query를_포함한_승인은_실행하지_않는다() {
    // given
    let harness = Harness::new().await;
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    // when
    let actual = consume_with_decision(&harness, request, |decision| {
        decision.edited_query = Some("SELECT 1".into())
    })
    .await;
    // then
    assert_eq!(actual.0.err().unwrap().code, ErrorCode::WriteNotAllowed);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 거부와_잘못된_id와_기한밖_결정은_승인으로_소비하지_않는다() {
    // given
    let harness = Harness::new().await;
    // when
    let actual = invalid_decisions(&harness).await;
    // then
    assert!(actual
        .iter()
        .all(|error| error.code == ErrorCode::WriteNotAllowed));
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}
#[tokio::test]
async fn 승인_창에서_거부한_쓰기는_거부되었다는_원인을_알려준다() {
    // given
    let harness = Harness::new().await;
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    // when
    let actual = consume_with_decision(&harness, request, |decision| {
        decision.decision = "deny".into()
    })
    .await;
    // then
    let error = actual.0.err().unwrap();
    assert_eq!(error.code, ErrorCode::WriteNotAllowed);
    assert_eq!(error.details, json!({"reason":"approval_denied"}));
    assert!(error.message.contains("denied"), "{}", error.message);
}
async fn invalid_decisions(harness: &Harness) -> Vec<ServiceError> {
    let mut errors = vec![];
    for mode in 0..3 {
        let request = request(
            Operation::RowDelete,
            json!({"table":{"database":"fixture","schema":"public","name":"items"},"identity":{"columns":["id"],"values":[7]}}),
        );
        let actual = consume_with_decision(harness, request, |decision| match mode {
            0 => decision.decision = "deny".into(),
            1 => decision.approval_id = uuid::Uuid::new_v4().to_string(),
            _ => decision.decided_at = "2000-01-01T00:00:00Z".into(),
        })
        .await;
        errors.push(actual.0.err().unwrap());
    }
    errors
}

#[tokio::test]
async fn 문서와_행_승인_표시는_대상과_해시를_보존하고_본문을_노출하지_않는다() {
    // given
    let harness = Harness::new().await;
    let request = request(
        Operation::DocumentReplace,
        json!({"database":"db","container":"items","identity":{"id":"doc-7","partition_key":[{"type":"string","value":"partition-7"}]},"if_match":"etag-7","document":{"id":"doc-7","coordinates":[127.12,37.45],"secret_body_canary":"synthetic-body"}}),
    );
    // when
    let actual = consume_with_decision(&harness, request, |_| {}).await;
    // then
    assert!(actual.0.is_ok());
    assert!(actual.1.query.contains("doc-7"));
    assert!(actual.1.query.contains("partition-7"));
    assert!(actual.1.query.contains("etag-7"));
    assert!(actual.1.query.contains("body_hash"));
    assert!(actual.1.query.contains("change_count"));
    assert!(!actual.1.query.contains("127.12"));
    assert!(!actual.1.query.contains("synthetic-body"));
}

#[tokio::test]
async fn pending_표시를_수정하면_유효한_결정도_거부한다() {
    // given
    let harness = Harness::new().await;
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    // when
    let actual = tamper_pending(&harness, request).await;
    // then
    assert_eq!(actual.code, ErrorCode::WriteNotAllowed);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}
async fn tamper_pending(harness: &Harness, request: ServiceRequest) -> ServiceError {
    let connection = harness.connections.get("fixture").await.unwrap();
    let remote = caller(CallerTransport::Mcp);
    let approvals = harness.approvals.clone();
    let hash = value_hash(&serde_json::to_value(&request).unwrap());
    let task = tokio::spawn(async move {
        approvals
            .consume(&connection, &remote, &request, &hash)
            .await
    });
    let mut pending = pending(&harness.approvals).await;
    pending.query = "modified".into();
    persistence::service_write_atomic(
        &harness
            .directory
            .0
            .join("pending_approvals")
            .join(format!("{}.pending.json", pending.id)),
        &serde_json::to_vec(&pending).unwrap(),
    )
    .unwrap();
    task.await.unwrap().err().unwrap()
}

#[tokio::test]
async fn caller_owner_connection_request_hash_바인딩_변조는_모두_거부한다() {
    // given
    let harness = Harness::new().await;
    // when
    let actual = tamper_bindings(&harness).await;
    // then
    assert!(actual
        .iter()
        .all(|error| error.code == ErrorCode::WriteNotAllowed));
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}
async fn tamper_bindings(harness: &Harness) -> Vec<ServiceError> {
    let mut errors = vec![];
    for field in [
        "caller_id",
        "owner_instance_id",
        "connection_id",
        "request_id",
        "request_hash",
    ] {
        let connection = harness.connections.get("fixture").await.unwrap();
        let remote = caller(CallerTransport::Mcp);
        let request = request(
            Operation::QueryExecute,
            json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
        );
        let hash = value_hash(&serde_json::to_value(&request).unwrap());
        let approvals = harness.approvals.clone();
        let task = tokio::spawn(async move {
            approvals
                .consume(&connection, &remote, &request, &hash)
                .await
        });
        let pending = pending(&harness.approvals).await;
        let path = harness
            .directory
            .0
            .join("service-approval-bindings-v1")
            .join(format!("{}.json", pending.id));
        let mut binding: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        binding[field] = "wrong".into();
        persistence::service_write_atomic(&path, &serde_json::to_vec(&binding).unwrap()).unwrap();
        errors.push(task.await.unwrap().err().unwrap());
    }
    errors
}

#[cfg(unix)]
#[tokio::test]
async fn 공개_권한으로_쓴_legacy_결정은_공통_서비스가_소비하지_않는다() {
    // given
    let harness = Harness::new().await;
    // when
    let actual = unsafe_decision(&harness).await;
    // then
    assert_eq!(actual.code, ErrorCode::PermissionDenied);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}
#[cfg(unix)]
async fn unsafe_decision(harness: &Harness) -> ServiceError {
    use std::os::unix::fs::PermissionsExt;
    let connection = harness.connections.get("fixture").await.unwrap();
    let remote = caller(CallerTransport::Mcp);
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    let hash = value_hash(&serde_json::to_value(&request).unwrap());
    let approvals = harness.approvals.clone();
    let task = tokio::spawn(async move {
        approvals
            .consume(&connection, &remote, &request, &hash)
            .await
    });
    let pending = pending(&harness.approvals).await;
    let path = harness
        .directory
        .0
        .join("pending_approvals")
        .join(format!("{}.decision.json", pending.id));
    std::fs::write(
        &path,
        serde_json::to_vec(&decision(&pending.id, true)).unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
    task.await.unwrap().err().unwrap()
}

#[tokio::test]
async fn 승인_대기는_요청_deadline_ms가_지나도_유지되고_늦은_승인을_소비한다() {
    // given
    let harness = Harness::new().await;
    let mut request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    request.deadline_ms = 30;
    let connection = harness.connections.get("fixture").await.unwrap();
    let remote = caller(CallerTransport::Mcp);
    let hash = value_hash(&serde_json::to_value(&request).unwrap());
    let approvals = harness.approvals.clone();
    let task = tokio::spawn(async move {
        approvals
            .consume(&connection, &remote, &request, &hash)
            .await
    });
    let pending = pending(&harness.approvals).await;
    // when
    tokio::time::sleep(Duration::from_millis(300)).await;
    let waiting = (task.is_finished(), queue_counts(&harness.directory.0));
    let approved = harness.approvals.approve_pending(
        &pending.id,
        &decision(&pending.id, true),
        &caller(CallerTransport::Gui),
    );
    let actual = task.await.unwrap();
    // then
    assert_eq!(waiting, (false, (1, 0, 1)));
    assert!(approved.is_ok());
    assert!(actual.is_ok());
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}

#[tokio::test]
async fn 대기_중인_승인은_binding_lease를_갱신한다() {
    // given
    let harness = Harness::new().await;
    harness.approvals.set_timing(100, 100);
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    let connection = harness.connections.get("fixture").await.unwrap();
    let remote = caller(CallerTransport::Mcp);
    let hash = value_hash(&serde_json::to_value(&request).unwrap());
    let approvals = harness.approvals.clone();
    let task = tokio::spawn(async move {
        approvals
            .consume(&connection, &remote, &request, &hash)
            .await
    });
    let pending = pending(&harness.approvals).await;
    let first = binding_lease(&harness, &pending.id);
    // when
    tokio::time::sleep(Duration::from_millis(400)).await;
    let renewed = binding_lease(&harness, &pending.id);
    let approved = harness.approvals.approve_pending(
        &pending.id,
        &decision(&pending.id, true),
        &caller(CallerTransport::Gui),
    );
    let actual = task.await.unwrap();
    // then
    assert!(renewed > first);
    assert!(approved.is_ok());
    assert!(actual.is_ok());
}
fn binding_lease(harness: &Harness, id: &str) -> u64 {
    let path = harness
        .directory
        .0
        .join("service-approval-bindings-v1")
        .join(format!("{id}.json"));
    let binding: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    binding["deadline_at_ms"].as_u64().unwrap()
}

#[tokio::test]
async fn 대기하던_프로세스가_사라진_binding은_결정을_거부하고_다음_대기가_정리한다() {
    // given
    let harness = Harness::new().await;
    let connection = harness.connections.get("fixture").await.unwrap();
    let remote = caller(CallerTransport::Mcp);
    let first = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    let hash = value_hash(&serde_json::to_value(&first).unwrap());
    let approvals = harness.approvals.clone();
    let waiter = connection.clone();
    let waiter_remote = remote.clone();
    let task = tokio::spawn(async move {
        approvals
            .consume(&waiter, &waiter_remote, &first, &hash)
            .await
    });
    let pending = pending(&harness.approvals).await;
    let pending_path = harness
        .directory
        .0
        .join("pending_approvals")
        .join(format!("{}.pending.json", pending.id));
    let binding_path = harness
        .directory
        .0
        .join("service-approval-bindings-v1")
        .join(format!("{}.json", pending.id));
    let pending_bytes = std::fs::read(&pending_path).unwrap();
    let mut binding: Value =
        serde_json::from_slice(&std::fs::read(&binding_path).unwrap()).unwrap();
    task.abort();
    let _ = task.await;
    binding["deadline_at_ms"] = json!(crate::services::storage::now_ms() - 1);
    persistence::service_write_atomic(&pending_path, &pending_bytes).unwrap();
    persistence::service_write_atomic(&binding_path, &serde_json::to_vec(&binding).unwrap())
        .unwrap();
    // when
    let late = harness.approvals.approve_pending(
        &pending.id,
        &decision(&pending.id, true),
        &caller(CallerTransport::Gui),
    );
    let leftover = queue_counts(&harness.directory.0);
    let next = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    let next_hash = value_hash(&serde_json::to_value(&next).unwrap());
    let approvals = harness.approvals.clone();
    let next_task = tokio::spawn(async move {
        approvals
            .consume(&connection, &remote, &next, &next_hash)
            .await
    });
    let live = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let list = crate::ai_approval::list_pending_in(harness.approvals.profile()).unwrap();
            if list.len() == 1 && list[0].id != pending.id {
                return list[0].id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let counts = queue_counts(&harness.directory.0);
    next_task.abort();
    let _ = next_task.await;
    // then
    assert_eq!(late.unwrap_err().code, ErrorCode::WriteNotAllowed);
    assert_eq!(leftover, (1, 0, 1));
    assert_ne!(live, pending.id);
    assert_eq!(counts, (1, 0, 1));
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}

#[tokio::test]
async fn 대기_중인_승인의_pending_파일_수정_시각을_갱신한다() {
    // given
    let harness = Harness::new().await;
    harness.approvals.set_timing(100, 100);
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    let connection = harness.connections.get("fixture").await.unwrap();
    let hash = value_hash(&serde_json::to_value(&request).unwrap());
    let approvals = harness.approvals.clone();
    let task = tokio::spawn(async move {
        approvals
            .consume(&connection, &caller(CallerTransport::Mcp), &request, &hash)
            .await
    });
    let pending = pending(&harness.approvals).await;
    let path = harness
        .directory
        .0
        .join("pending_approvals")
        .join(format!("{}.pending.json", pending.id));
    let bytes = std::fs::read(&path).unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(3 * 3_600);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(old)).unwrap();
    // when
    tokio::time::sleep(Duration::from_millis(400)).await;
    let removed = crate::ai_approval::cleanup_expired_in(&harness.directory.0, 3_600).unwrap();
    let kept = std::fs::read(&path).ok();
    task.abort();
    let _ = task.await;
    // then
    assert_eq!(removed, 0);
    assert_eq!(kept, Some(bytes));
}

#[tokio::test]
async fn 승인대기_query_job_cancel은_큐를_즉시_정리하고_늦은_승인을_차단한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    harness
        .connections
        .update("fixture", saved.version, json!({"allow_writes":true}))
        .await
        .unwrap();
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    let remote = caller(CallerTransport::Mcp);
    harness
        .policy
        .register_query_write(&remote, &request)
        .unwrap();
    // when
    let actual = cancel_pending_job(&harness, request, remote).await;
    // then
    assert_eq!(actual.0.error.unwrap().code, ErrorCode::Cancelled);
    assert!(actual.1.is_err());
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.secrets.0.load(Ordering::SeqCst), 0);
    assert_eq!(harness.oauth.0.load(Ordering::SeqCst), 0);
}
async fn cancel_pending_job(
    harness: &Harness,
    original: ServiceRequest,
    remote: CallerContext,
) -> (ServiceResponse, ServiceResult<()>) {
    let queued = harness.runtime.submit(original, remote.clone()).await;
    let pending = pending(&harness.approvals).await;
    let job_id = queued.job_id.unwrap();
    let cancel = request(
        Operation::JobCancel,
        json!({"job_id":job_id,"owner_instance_id":"platform-owner"}),
    );
    harness.runtime.call_validated(&cancel, &remote).await;
    let response = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let record = harness
                .runtime
                .jobs
                .get(&job_id, "platform-owner", &remote)
                .unwrap();
            if let Some(response) = record.response {
                return response;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let late = harness.approvals.approve_pending(
        &pending.id,
        &decision(&pending.id, true),
        &caller(CallerTransport::Gui),
    );
    (response, late)
}

#[tokio::test]
async fn 지연된_승인_뒤_드라이버는_승인_대기_전에_남은_deadline을_그대로_받는다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    harness
        .connections
        .update("fixture", saved.version, json!({"allow_writes":true}))
        .await
        .unwrap();
    let mut request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    );
    request.deadline_ms = 3000;
    let remote = caller(CallerTransport::Mcp);
    harness
        .policy
        .register_query_write(&remote, &request)
        .unwrap();
    // when
    let queued = harness.runtime.submit(request, remote.clone()).await;
    let pending = pending(&harness.approvals).await;
    tokio::time::sleep(Duration::from_millis(3_500)).await;
    let job_id = queued.job_id.unwrap();
    let waiting = harness.runtime.jobs.awaiting_approval(&job_id);
    harness
        .approvals
        .approve_pending(
            &pending.id,
            &decision(&pending.id, true),
            &caller(CallerTransport::Gui),
        )
        .unwrap();
    let response = finished_job(&harness, &job_id, &remote).await;
    // then
    let granted = harness.driver.last_deadline_ms.load(Ordering::SeqCst);
    let expected = 3000 + crate::services::runtime::CANCEL_GRACE_MS;
    assert!(waiting);
    assert_eq!(response.status, JobStatus::Succeeded);
    assert!(granted <= expected && granted + 500 >= expected);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}
async fn finished_job(harness: &Harness, job_id: &str, remote: &CallerContext) -> ServiceResponse {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let record = harness
                .runtime
                .jobs
                .get(job_id, "platform-owner", remote)
                .unwrap();
            if let Some(response) = record.response {
                return response;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn 승인대기_연결_수정_cancel은_파일_게시와_늦은_승인을_차단한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let mut request = request(
        Operation::ConnectionUpdate,
        json!({"patch":{"name":"must-not-publish"}}),
    );
    request.expected_version = Some(saved.version);
    let bytes = std::fs::read(harness.connections.path()).unwrap();
    // when
    let actual = cancel_pending_job(&harness, request, caller(CallerTransport::LocalIpc)).await;
    // then
    assert_eq!(actual.0.error.unwrap().code, ErrorCode::Cancelled);
    assert!(actual.1.is_err());
    assert_eq!(std::fs::read(harness.connections.path()).unwrap(), bytes);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn gui_직접_수정과_원격_승인_거부는_서로_독립이다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let mut request = request(
        Operation::ConnectionUpdate,
        json!({"patch":{"name":"direct-gui"}}),
    );
    request.expected_version = Some(saved.version);
    // when
    let actual = harness
        .runtime
        .call_validated(&request, &caller(CallerTransport::Gui))
        .await;
    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(actual.data["name"], "direct-gui");
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn binding의_pending_복사본을_변조하면_소비하지_않는다() {
    // given
    let harness = Harness::new().await;
    // when
    let actual = tamper_bound_pending(&harness).await;
    // then
    assert_eq!(actual.code, ErrorCode::WriteNotAllowed);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}
async fn tamper_bound_pending(harness: &Harness) -> ServiceError {
    let saved = harness.connections.get("fixture").await.unwrap();
    let remote = caller(CallerTransport::Mcp);
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":1}),
    );
    let hash = value_hash(&serde_json::to_value(&request).unwrap());
    let approvals = harness.approvals.clone();
    let task =
        tokio::spawn(async move { approvals.consume(&saved, &remote, &request, &hash).await });
    let pending = pending(&harness.approvals).await;
    let path = harness
        .directory
        .0
        .join("service-approval-bindings-v1")
        .join(format!("{}.json", pending.id));
    let mut binding: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    binding["pending"]["query"] = "forged".into();
    persistence::service_write_atomic(&path, &serde_json::to_vec(&binding).unwrap()).unwrap();
    task.await.unwrap().err().unwrap()
}

#[tokio::test]
async fn 소비한_승인의_다시_게시와_원격_승인자는_거부한다() {
    // given
    let harness = Harness::new().await;
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":1}),
    );
    // when
    let actual = replay_decision(&harness, request).await;
    // then
    assert_eq!(actual.0.unwrap_err().code, ErrorCode::WriteNotAllowed);
    assert_eq!(actual.1.unwrap_err().code, ErrorCode::PermissionDenied);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}
async fn replay_decision(
    harness: &Harness,
    request: ServiceRequest,
) -> (ServiceResult<()>, ServiceResult<()>) {
    let (grant, pending) = consume_with_decision(harness, request, |_| {}).await;
    grant.unwrap();
    (
        harness.approvals.approve_pending(
            &pending.id,
            &decision(&pending.id, true),
            &caller(CallerTransport::Gui),
        ),
        harness.approvals.approve_pending(
            &pending.id,
            &decision(&pending.id, true),
            &caller(CallerTransport::LocalIpc),
        ),
    )
}

#[tokio::test]
async fn 승인_queue는_64개로_제한하고_취소한_future의_파일을_정리한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    // when
    let actual = bounded_approval_queue(&harness, saved).await;
    // then
    assert_eq!(actual.0, (64, 0, 64));
    assert_eq!(actual.1.code, ErrorCode::ResourceLimit);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}
async fn bounded_approval_queue(
    harness: &Harness,
    saved: SavedConnection,
) -> ((usize, usize, usize), ServiceError) {
    let mut tasks = vec![];
    for _ in 0..64 {
        let saved = saved.clone();
        let approvals = harness.approvals.clone();
        let remote = caller(CallerTransport::Mcp);
        let mut request = request(
            Operation::QueryExecute,
            json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":1}),
        );
        request.deadline_ms = 30000;
        let hash = value_hash(&serde_json::to_value(&request).unwrap());
        tasks.push(tokio::spawn(async move {
            approvals.consume(&saved, &remote, &request, &hash).await
        }));
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while queue_counts(&harness.directory.0).0 != 64 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let counts = queue_counts(&harness.directory.0);
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":1}),
    );
    let hash = value_hash(&serde_json::to_value(&request).unwrap());
    let error = harness
        .approvals
        .consume(&saved, &caller(CallerTransport::Mcp), &request, &hash)
        .await
        .err()
        .unwrap();
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        let _ = task.await;
    }
    (counts, error)
}

#[tokio::test]
async fn 실제_factory의_거부된_쓰기_승인은_resolver_secret_oauth_db를_호출하지_않는다() {
    // given
    let directory = Directory::new();
    let driver = registered().await;
    seed(&directory.0, &driver);
    config(&directory.0, false);
    let secrets = Arc::new(SyntheticSecrets::default());
    let oauth = Arc::new(SyntheticOAuth::default());
    let expander = Arc::new(CountExpander::default());
    let platform = ServicePlatform::isolated(
        directory.0.clone(),
        "platform-owner".into(),
        secrets.clone(),
        oauth.clone(),
        expander.clone(),
    )
    .unwrap();
    let saved = platform.connections.get("fixture").await.unwrap();
    platform
        .connections
        .update("fixture", saved.version, json!({"allow_writes":true}))
        .await
        .unwrap();
    let request = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":1}),
    );
    let remote = caller(CallerTransport::Mcp);
    platform
        .policy
        .register_query_write(&remote, &request)
        .unwrap();
    // when
    let actual = deny_platform_job(&platform, request, remote).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert_eq!(expander.0.load(Ordering::SeqCst), 0);
    assert_eq!(secrets.0.load(Ordering::SeqCst), 0);
    assert_eq!(oauth.0.load(Ordering::SeqCst), 0);
    assert_eq!(queue_counts(&directory.0), (0, 0, 0));
}
async fn deny_platform_job(
    platform: &ServicePlatform,
    request: ServiceRequest,
    remote: CallerContext,
) -> ServiceResponse {
    let queued = platform.runtime.submit(request, remote.clone()).await;
    let pending = pending(&platform.approvals).await;
    platform
        .approvals
        .approve_pending(
            &pending.id,
            &decision(&pending.id, false),
            &caller(CallerTransport::Gui),
        )
        .unwrap();
    let id = queued.job_id.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(response) = platform
                .runtime
                .jobs
                .get(&id, "platform-owner", &remote)
                .unwrap()
                .response
            {
                return response;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn 새_연결_승인은_공개_connection_id_null을_유지한다() {
    // given
    let harness = Harness::new().await;
    let mut request = request(
        Operation::ConnectionCreate,
        json!({"name":"Created","driver":"synthetic","settings":{"host":"localhost"},"credential_ref":null}),
    );
    request.connection_id = None;
    // when
    let actual = configuration_display(&harness, request).await;
    // then
    assert_eq!(actual.connection_id, "");
    assert_eq!(actual.query_kind, "service:connection.create");
    assert!(!actual.query.contains("__connection_create__"));
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}
async fn configuration_display(
    harness: &Harness,
    request: ServiceRequest,
) -> crate::ai_approval::PendingApproval {
    let approvals = harness.approvals.clone();
    let task = tokio::spawn(async move {
        approvals
            .configuration(&caller(CallerTransport::Mcp), &request)
            .await
    });
    let pending = pending(&harness.approvals).await;
    harness
        .approvals
        .approve_pending(
            &pending.id,
            &decision(&pending.id, true),
            &caller(CallerTransport::Gui),
        )
        .unwrap();
    task.await.unwrap().unwrap();
    pending
}

#[tokio::test]
async fn 설정_json의_비밀은_pending_표시에도_저장하지_않는다() {
    // given
    let harness = Harness::new().await;
    let mut request = request(
        Operation::ConnectionCreate,
        json!({"name":"Created","driver":"synthetic","settings":{"password":"synthetic-json-secret-canary"},"credential_ref":null}),
    );
    request.connection_id = None;
    // when
    let actual = harness
        .approvals
        .configuration(&caller(CallerTransport::Mcp), &request)
        .await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}

fn pending_write() -> ServiceRequest {
    request(
        Operation::QueryExecute,
        json!({"language":"sql","text":"DELETE FROM fixture","parameters":[],"page_size":10}),
    )
}
async fn approve_after(
    harness: &Harness,
    request: ServiceRequest,
    change: impl FnOnce(&Harness),
) -> ServiceResponse {
    let remote = caller(CallerTransport::Mcp);
    harness
        .policy
        .register_query_write(&remote, &request)
        .unwrap();
    let queued = harness.runtime.submit(request, remote.clone()).await;
    let pending = pending(&harness.approvals).await;
    change(harness);
    harness
        .approvals
        .approve_pending(
            &pending.id,
            &decision(&pending.id, true),
            &caller(CallerTransport::Gui),
        )
        .unwrap();
    let id = queued.job_id.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(response) = harness
                .runtime
                .jobs
                .get(&id, "platform-owner", &remote)
                .unwrap()
                .response
            {
                return response;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn 승인을_기다리는_동안_읽기전용이_켜지면_승인_뒤에도_쓰기를_막고_드라이버를_부르지_않는다() {
    // given
    let harness = Harness::new().await;

    // when
    let actual = approve_after(&harness, pending_write(), |harness| {
        config(&harness.directory.0, true)
    })
    .await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}

#[tokio::test]
async fn 승인을_기다리는_동안_연결을_수정하면_승인_뒤_connection_conflict로_끝나고_드라이버를_부르지_않는다()
{
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();

    // when
    let actual = approve_after(&harness, pending_write(), |harness| {
        futures::executor::block_on(harness.connections.update(
            "fixture",
            saved.version,
            json!({"name":"Renamed while pending"}),
        ))
        .unwrap();
    })
    .await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ConnectionConflict);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}

#[tokio::test]
async fn 승인정책은_승인이_끝난_뒤_권한을_다시_읽어_읽기전용_연결의_쓰기를_거부한다() {
    // given
    let harness = Harness::new().await;
    let connection = harness.connections.get("fixture").await.unwrap();
    let remote = caller(CallerTransport::Mcp);
    let request = pending_write();
    let hash = value_hash(&serde_json::to_value(&request).unwrap());
    let policy = harness.policy.clone();
    let task = {
        let (connection, remote, request, hash) =
            (connection.clone(), remote.clone(), request.clone(), hash);
        tokio::spawn(async move {
            crate::services::backend::PolicyBackend::consume_write_approval(
                policy.as_ref(),
                &connection,
                &remote,
                &request,
                &hash,
            )
            .await
        })
    };
    let pending = pending(&harness.approvals).await;

    // when
    config(&harness.directory.0, true);
    harness
        .approvals
        .approve_pending(
            &pending.id,
            &decision(&pending.id, true),
            &caller(CallerTransport::Gui),
        )
        .unwrap();
    let actual = task.await.unwrap();

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::WriteNotAllowed);
}
