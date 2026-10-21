use super::*;

fn result_binding(saved: &SavedConnection, permission: &PermissionScope) -> CursorBinding {
    CursorBinding {
        connection_id: saved.id.clone(),
        permission_scope: permission.identity.clone(),
        auth_revision: saved.auth_revision.clone(),
        query_hash: "owned-query".into(),
        result_revision: "owned-result".into(),
        sdk_version: None,
        container_rid: None,
        endpoint_identity: None,
        partition_hash: None,
    }
}
fn document_snapshot() -> ResultSnapshot {
    ResultSnapshot {
        sets: vec![ResultSet {
            kind: ResultKind::Documents,
            columns: vec![],
            rows: vec![
                json!({"document":{"id":"doc"},"identity":{"id":"doc","partition_key":[{"type":"null"}]},"etag":"etag"}),
            ],
        }],
        revision: "owned-result".into(),
        limits: Limits::default(),
        allow_truncation: false,
        warnings: vec![],
    }
}

#[tokio::test]
async fn 같은_사용자의_gui_결과는_mcp에서도_동일_권한으로_읽는다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let gui = harness
        .policy
        .permission(&saved, &caller(CallerTransport::Gui))
        .await
        .unwrap();
    let scope = result_binding(&saved, &gui);
    let id = harness
        .runtime
        .results
        .insert_reserved_with_origin(
            scope,
            document_snapshot(),
            None,
            Some(ResultOrigin {
                database: "database".into(),
                container: "container".into(),
            }),
        )
        .unwrap();
    let request = request(
        Operation::ResultGet,
        json!({"result_id":id,"result_set_index":0,"offset":0,"limit":10}),
    );
    // when
    let actual = harness
        .runtime
        .call_validated(&request, &caller(CallerTransport::Mcp))
        .await;
    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(
        actual.data["origin"],
        json!({"database":"database","container":"container"})
    );
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 정책_파일이_바뀌면_이전_결과_ref를_거부한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let gui = harness
        .policy
        .permission(&saved, &caller(CallerTransport::Gui))
        .await
        .unwrap();
    let id = harness
        .runtime
        .results
        .insert(result_binding(&saved, &gui), document_snapshot())
        .unwrap();
    config(&harness.directory.0, true);
    let request = request(
        Operation::ResultGet,
        json!({"result_id":id,"result_set_index":0,"offset":0,"limit":10}),
    );
    // when
    let actual = harness
        .runtime
        .call_validated(&request, &caller(CallerTransport::Mcp))
        .await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::InvalidPageToken);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn transport와_gui_write_권한은_읽기_identity를_바꾸지_않는다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    config(&harness.directory.0, true);
    // when
    let actual = transport_permissions(&harness, &saved).await;
    // then
    assert!(actual
        .iter()
        .all(|scope| scope.identity == actual[0].identity));
    assert!(actual[0].allow_writes);
    assert!(actual[1..].iter().all(|scope| !scope.allow_writes));
}
async fn transport_permissions(harness: &Harness, saved: &SavedConnection) -> Vec<PermissionScope> {
    let mut scopes = vec![];
    for transport in [
        CallerTransport::Gui,
        CallerTransport::Mcp,
        CallerTransport::Cli,
        CallerTransport::LocalIpc,
    ] {
        scopes.push(
            harness
                .policy
                .permission(saved, &caller(transport))
                .await
                .unwrap(),
        );
    }
    scopes
}

#[tokio::test]
async fn 누락된_config는_readonly이고_잘못된_config는_fail_closed다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    std::fs::remove_file(harness.directory.0.join("config.json")).unwrap();
    // when
    let actual = config_failures(&harness, &saved).await;
    // then
    assert!(!actual.0.allow_writes);
    assert_eq!(actual.1.unwrap_err().code, ErrorCode::PermissionDenied);
}
async fn config_failures(
    harness: &Harness,
    saved: &SavedConnection,
) -> (PermissionScope, ServiceResult<PermissionScope>) {
    let missing = harness
        .policy
        .permission(saved, &caller(CallerTransport::Mcp))
        .await
        .unwrap();
    persistence::service_write_atomic(&harness.directory.0.join("config.json"), b"{broken")
        .unwrap();
    (
        missing,
        harness
            .policy
            .permission(saved, &caller(CallerTransport::Gui))
            .await,
    )
}

#[tokio::test]
async fn 다른_os_caller는_프로필_설정과_자격증명을_읽기전에_거부한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let mut caller = caller(CallerTransport::Gui);
    caller.caller_id = "local_user:other".into();
    // when
    let actual = harness.policy.permission(&saved, &caller).await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::PermissionDenied);
    assert_eq!(harness.secrets.0.load(Ordering::SeqCst), 0);
    assert_eq!(harness.oauth.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 외부_설정_변경의_관찰은_최신_auth_revision과_결과_marker를_함께_반환한다() {
    // given
    let harness = Harness::new().await;
    harness
        .connections
        .set_observation(Arc::new(RuntimeObservation {
            runtime: Arc::downgrade(&harness.runtime),
        }))
        .unwrap();
    let saved = harness.connections.get("fixture").await.unwrap();
    harness
        .runtime
        .results
        .invalidate_connection_revision("fixture", &saved.auth_revision)
        .unwrap();
    let permission = harness
        .policy
        .permission(&saved, &caller(CallerTransport::Gui))
        .await
        .unwrap();
    let scope = result_binding(&saved, &permission);
    let id = harness
        .runtime
        .results
        .insert(scope.clone(), document_snapshot())
        .unwrap();
    let mut legacy = persistence::load_connections_file(harness.connections.path()).unwrap();
    legacy.connections[0].params.host = Some("external.example".into());
    persistence::save_connections_file(harness.connections.path(), &legacy).unwrap();
    // when
    let actual = observe_concurrently(&harness, &id, &scope).await;
    // then
    assert_eq!(actual.0.version, saved.version + 1);
    assert_ne!(actual.0.auth_revision, saved.auth_revision);
    assert_eq!(actual.0.auth_revision, actual.1.auth_revision);
    assert_eq!(actual.2.as_deref(), Some(actual.0.auth_revision.as_str()));
    assert!(actual.3.is_err());
    assert_eq!(harness.driver.invalidations.load(Ordering::SeqCst), 1);
}
async fn observe_concurrently(
    harness: &Harness,
    id: &str,
    scope: &CursorBinding,
) -> (
    SavedConnection,
    SavedConnection,
    Option<String>,
    ServiceResult<ResultSnapshot>,
) {
    let (first, second) = tokio::join!(
        harness.connections.get("fixture"),
        harness.connections.get("fixture")
    );
    (
        first.unwrap(),
        second.unwrap(),
        harness
            .runtime
            .results
            .auth_revision_marker("fixture")
            .unwrap(),
        harness.runtime.results.snapshot(id, scope),
    )
}

#[tokio::test]
async fn 새_플랫폼은_이전_marker의_외부_revision을_관찰하고_재회전한다() {
    // given
    let directory = Directory::new();
    seed(&directory.0, "unexecuted-synthetic");
    let first = ServicePlatform::isolated(
        directory.0.clone(),
        "platform-first".into(),
        Arc::new(SyntheticSecrets::default()),
        Arc::new(SyntheticOAuth::default()),
        Arc::new(NoopParamExpander),
    )
    .unwrap();
    let saved = first.connections.get("fixture").await.unwrap();
    first
        .runtime
        .results
        .invalidate_connection_revision("fixture", &saved.auth_revision)
        .unwrap();
    let external = ProfileConnections::new(&directory.0);
    let changed = external.rotate_auth_revision("fixture").await.unwrap();
    let second = ServicePlatform::isolated(
        directory.0.clone(),
        "platform-second".into(),
        Arc::new(SyntheticSecrets::default()),
        Arc::new(SyntheticOAuth::default()),
        Arc::new(NoopParamExpander),
    )
    .unwrap();
    // when
    let actual = second.connections.get("fixture").await.unwrap();
    // then
    assert_eq!(actual.version, saved.version);
    assert_ne!(actual.auth_revision, changed);
    assert_eq!(
        second
            .runtime
            .results
            .auth_revision_marker("fixture")
            .unwrap()
            .as_deref(),
        Some(actual.auth_revision.as_str())
    );
}

#[tokio::test]
async fn 실제_외부_프로세스의_legacy_게시도_기존_결과를_무효화한다() {
    // given
    let directory = Directory::new();
    seed(&directory.0, "unexecuted-synthetic");
    let platform = ServicePlatform::isolated(
        directory.0.clone(),
        "parent-owner".into(),
        Arc::new(SyntheticSecrets::default()),
        Arc::new(SyntheticOAuth::default()),
        Arc::new(NoopParamExpander),
    )
    .unwrap();
    let saved = platform.connections.get("fixture").await.unwrap();
    platform
        .runtime
        .results
        .invalidate_connection_revision("fixture", &saved.auth_revision)
        .unwrap();
    // when
    let actual = external_process_observation(&platform, &directory.0).await;
    // then
    assert_eq!(actual.version, saved.version + 1);
    assert_ne!(actual.auth_revision, saved.auth_revision);
    assert_eq!(actual.settings["host"], "child.example");
    assert_eq!(
        platform
            .runtime
            .results
            .auth_revision_marker("fixture")
            .unwrap()
            .as_deref(),
        Some(actual.auth_revision.as_str())
    );
}
async fn external_process_observation(
    platform: &ServicePlatform,
    profile: &Path,
) -> SavedConnection {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "service_platform::tests::origin_policy::외부_legacy_writer_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("TABULARIS_OBSERVER_TEST_PROFILE", profile)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Owned child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("H3B1_OBSERVER_CHILD_COMPLETED"),
        "Owned child must actually execute"
    );
    platform.connections.get("fixture").await.unwrap()
}

#[test]
#[ignore = "Owned child-process fixture"]
fn 외부_legacy_writer_fixture() {
    // given
    let profile = PathBuf::from(
        std::env::var_os("TABULARIS_OBSERVER_TEST_PROFILE").expect("Owned profile required"),
    );
    let path = profile.join("connections.json");
    let mut loaded = persistence::load_connections_file(&path).unwrap();
    loaded.connections[0].params.host = Some("child.example".into());
    // when
    let actual = publish_external(&path, &loaded, &profile);
    // then
    assert!(actual.is_ok());
}
fn publish_external(
    path: &Path,
    loaded: &crate::models::ConnectionsFile,
    profile: &Path,
) -> ServiceResult<()> {
    persistence::save_connections_file(path, loaded).unwrap();
    let connections = ProfileConnections::new(profile);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(connections.get("fixture"))?;
    println!("H3B1_OBSERVER_CHILD_COMPLETED");
    Ok(())
}
