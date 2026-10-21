use super::*;
use crate::{
    models::ConnectionParams,
    service_platform::{restore_saved_secrets, SavedSecrets},
    services::policy::ApprovalMode,
};

fn create_request(settings: Value) -> ServiceRequest {
    let mut request = request(
        Operation::ConnectionCreate,
        json!({"name":"Created","driver":"synthetic","settings":settings,"credential_ref":null}),
    );
    request.connection_id = None;
    request
}
fn update_request(version: u64, patch: Value) -> ServiceRequest {
    let mut request = request(Operation::ConnectionUpdate, json!({"patch":patch}));
    request.expected_version = Some(version);
    request
}
fn approval_config(profile: &Path, mode: &str) {
    persistence::service_write_atomic(
        &profile.join("config.json"),
        &serde_json::to_vec(&json!({"mcpReadonlyDefault":false,"mcpApprovalMode":mode})).unwrap(),
    )
    .unwrap();
}
async fn call_mcp(harness: &Harness, request: ServiceRequest) -> ServiceResponse {
    harness
        .runtime
        .call_validated(&request, &caller(CallerTransport::Mcp))
        .await
}
async fn approve_and_finish(
    harness: &Harness,
    request: ServiceRequest,
) -> (crate::ai_approval::PendingApproval, ServiceResponse) {
    let runtime = harness.runtime.clone();
    let task = tokio::spawn(async move {
        runtime
            .call_validated(&request, &caller(CallerTransport::Mcp))
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
    (pending, task.await.unwrap())
}

#[tokio::test]
async fn 비밀_필드와_비밀_같은_extra_키는_승인을_만들지_않고_거부한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let bytes = std::fs::read(harness.connections.path()).unwrap();
    let canary = "synthetic-secret-canary-7741";
    let variants = [
        json!({"password":canary}),
        json!({"ssh_password":canary}),
        json!({"ssh_key_passphrase":canary}),
        json!({"connection_uri":format!("postgres://user:{canary}@host/db")}),
        json!({"host":"h","extra":{"db_Password":canary}}),
        json!({"host":"h","extra":{"CLIENT_SECRET":canary}}),
        json!({"host":"h","extra":{"api_token":canary}}),
        json!({"host":"h","extra":{"ApiKey":canary}}),
    ];

    // when
    let mut responses = Vec::new();
    for settings in &variants {
        responses.push(call_mcp(&harness, create_request(settings.clone())).await);
        responses.push(
            call_mcp(
                &harness,
                update_request(saved.version, json!({"settings":settings})),
            )
            .await,
        );
    }
    responses.push(
        call_mcp(
            &harness,
            update_request(saved.version, json!({"password":canary})),
        )
        .await,
    );

    // then
    assert!(responses.iter().all(|response| {
        response.error.as_ref().unwrap().code == ErrorCode::SecretNotAccepted
            && !serde_json::to_string(response).unwrap().contains(canary)
            && response.job_id.is_none()
    }));
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
    assert_eq!(std::fs::read(harness.connections.path()).unwrap(), bytes);
    assert!(harness
        .runtime
        .job_history(&Default::default())
        .unwrap()
        .entries
        .is_empty());
}

#[tokio::test]
async fn 연결_생성과_수정은_승인_모드가_off여도_수정불가_승인을_기다린다() {
    // given
    let harness = Harness::new().await;
    approval_config(&harness.directory.0, "off");
    let saved = harness.connections.get("fixture").await.unwrap();
    let create = create_request(json!({"host":"localhost","database":"fixture"}));
    let create = ServiceRequest {
        input: json!({"name":"Created","driver":saved.driver,"settings":{"host":"localhost","database":"fixture"},"credential_ref":null}),
        ..create
    };
    let update = update_request(saved.version, json!({"name":"Renamed"}));

    // when
    let created = approve_and_finish(&harness, create).await;
    let updated = approve_and_finish(&harness, update).await;

    // then
    assert_eq!(created.1.status, JobStatus::Succeeded);
    assert_eq!(updated.1.status, JobStatus::Succeeded);
    for (pending, operation) in [
        (&created.0, "connection.create"),
        (&updated.0, "connection.update"),
    ] {
        assert!(!pending.editable);
        let marker = pending.service.as_ref().unwrap();
        assert_eq!(marker.operation, operation);
        assert_eq!(marker.request_hash.len(), 64);
        assert!(!marker.credential_retarget);
    }
    assert!(updated.0.query.contains("Renamed"));
}

#[tokio::test]
async fn 저장된_자격_증명이_있는_연결의_대상_변경만_retarget_경고를_싣는다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let plain = harness
        .connections
        .create(
            "Plain",
            &saved.driver,
            json!({"host":"localhost","database":"fixture"}),
            None,
        )
        .await
        .unwrap();

    // when
    let moved = approve_and_finish(
        &harness,
        update_request(
            saved.version,
            json!({"settings":{"host":"elsewhere.example","database":"fixture"}}),
        ),
    )
    .await;
    let renamed_only = approve_and_finish(
        &harness,
        update_request(
            harness.connections.get("fixture").await.unwrap().version,
            json!({"name":"Same target"}),
        ),
    )
    .await;
    let mut plain_update = update_request(
        plain.version,
        json!({"settings":{"host":"other.example","database":"fixture"}}),
    );
    plain_update.connection_id = Some(plain.id.clone());
    let without_secrets = approve_and_finish(&harness, plain_update).await;

    // then
    assert!(moved.0.service.as_ref().unwrap().credential_retarget);
    assert!(moved.0.query.contains("\"credential_retarget\":true"));
    assert!(!renamed_only.0.service.as_ref().unwrap().credential_retarget);
    assert!(
        !without_secrets
            .0
            .service
            .as_ref()
            .unwrap()
            .credential_retarget
    );
}

#[tokio::test]
async fn connection_test는_저장된_연결_id_외의_입력과_재정의를_받지_않는다() {
    // given
    let harness = Harness::new().await;
    let inputs = [
        json!({"password":"override"}),
        json!({"host":"evil.example"}),
        json!({"params":{"host":"evil.example"}}),
        json!({"connection_id":"fixture"}),
    ];

    // when
    let mut responses = Vec::new();
    for input in inputs {
        responses.push(call_mcp(&harness, request(Operation::ConnectionTest, input)).await);
    }
    let accepted = call_mcp(&harness, request(Operation::ConnectionTest, json!({}))).await;

    // then
    assert!(responses
        .iter()
        .all(|response| response.error.as_ref().unwrap().code == ErrorCode::InvalidArgument));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(accepted.status, JobStatus::Succeeded);
}

#[tokio::test]
async fn mcp는_연결_삭제와_자격_증명_가져오기를_큐에_넣기_전에_거부한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let bytes = std::fs::read(harness.connections.path()).unwrap();
    let mut delete = request(Operation::ConnectionDelete, json!({}));
    delete.expected_version = Some(saved.version);
    let import = request(
        Operation::CredentialImport,
        json!({"slot":"db_password","input_channel":{"kind":"file_descriptor","descriptor":7},"persistence":"session"}),
    );

    // when
    let responses = [
        call_mcp(&harness, delete).await,
        call_mcp(&harness, import).await,
    ];

    // then
    assert!(responses.iter().all(|response| {
        response.error.as_ref().unwrap().code == ErrorCode::OperationNotAllowed
            && response.job_id.is_none()
    }));
    assert_eq!(std::fs::read(harness.connections.path()).unwrap(), bytes);
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
}

#[tokio::test]
async fn 승인_모드는_설정_파일의_mcp_approval_mode를_따른다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let mcp = caller(CallerTransport::Mcp);
    let cases = [
        (None, ApprovalMode::WritesOnly),
        (Some("off"), ApprovalMode::Off),
        (Some("writes_only"), ApprovalMode::WritesOnly),
        (Some("all"), ApprovalMode::All),
        (Some("unexpected"), ApprovalMode::WritesOnly),
    ];

    // when
    let mut actual = Vec::new();
    for (mode, _) in cases {
        match mode {
            Some(mode) => approval_config(&harness.directory.0, mode),
            None => config(&harness.directory.0, false),
        }
        actual.push(harness.policy.approval_mode(&saved, &mcp).await.unwrap());
    }

    // then
    assert_eq!(actual, cases.map(|(_, expected)| expected));
}

struct FakeSecrets {
    password: Option<String>,
    uri: Result<Option<String>, ()>,
}
impl SavedSecrets for FakeSecrets {
    fn db_password(&self, _: &str) -> Option<String> {
        self.password.clone()
    }
    fn connection_uri(&self, _: &str, _: bool) -> Result<Option<String>, ()> {
        self.uri.clone()
    }
}
fn saved_params() -> ConnectionParams {
    serde_json::from_value(
        json!({"driver":"synthetic","host":"h","database":"d","save_in_keychain":true}),
    )
    .unwrap()
}

#[tokio::test]
async fn 저장된_연결의_키체인_비밀번호와_uri를_복원한다() {
    // given
    let secrets = Arc::new(FakeSecrets {
        password: Some("keychain-password".into()),
        uri: Ok(Some("synthetic://restored".into())),
    });
    let mut params = saved_params();
    params.connection_uri_in_keychain = Some(true);

    // when
    let actual = restore_saved_secrets(secrets, params, "fixture")
        .await
        .unwrap();

    // then
    assert_eq!(actual.password.as_deref(), Some("keychain-password"));
    assert_eq!(
        actual.connection_uri.as_deref(),
        Some("synthetic://restored")
    );
}

#[tokio::test]
async fn iam_인증과_없는_uri는_복원하지_않고_인증_필요로_끝낸다() {
    // given
    let secrets = |uri| {
        Arc::new(FakeSecrets {
            password: Some("keychain-password".into()),
            uri,
        })
    };
    let mut iam = saved_params();
    iam.use_iam_auth = Some(true);
    let mut missing = saved_params();
    missing.connection_uri_in_keychain = Some(true);
    let mut unreadable = saved_params();
    unreadable.connection_uri_in_keychain = Some(true);

    // when
    let actual = [
        restore_saved_secrets(secrets(Ok(None)), iam, "fixture").await,
        restore_saved_secrets(secrets(Ok(None)), missing, "fixture").await,
        restore_saved_secrets(secrets(Err(())), unreadable, "fixture").await,
    ];

    // then
    assert!(actual
        .iter()
        .all(|result| result.as_ref().err().unwrap().code == ErrorCode::AuthRequired));
}

#[tokio::test]
async fn 키체인을_쓰지_않는_연결은_비밀번호를_채우지_않는다() {
    // given
    let secrets = Arc::new(FakeSecrets {
        password: Some("keychain-password".into()),
        uri: Ok(None),
    });
    let mut params = saved_params();
    params.save_in_keychain = Some(false);

    // when
    let actual = restore_saved_secrets(secrets, params, "fixture")
        .await
        .unwrap();

    // then
    assert!(actual.password.is_none());
    assert!(actual.connection_uri.is_none());
}

#[test]
fn sqlserver_plus_설정은_driver_extra_키를_client_id_없이_정규화한다() {
    // given
    let settings = json!({
        "host": "fixture.database.windows.net",
        "database": "fixture",
        "ssl_mode": "verify-full",
        "username": "user@example.invalid",
        "extra": {
            "auth_mode": "entra_user",
            "auth_source": "azure_cli",
            "tenant_id": "22222222-2222-4222-8222-222222222222",
            "entra_object_id": "44444444-4444-4444-8444-444444444444",
            "azure_cli_path": "/opt/azure/az"
        }
    });
    // when
    let actual =
        crate::service_platform::checked_params_for_tests("sqlserver-plus", &settings).unwrap();
    // then
    assert_eq!(actual.username.as_deref(), Some("user@example.invalid"));
    assert_eq!(
        actual.extra["entra_object_id"],
        "44444444-4444-4444-8444-444444444444"
    );
    assert_eq!(actual.extra["azure_cli_path"], "/opt/azure/az");
    assert_eq!(actual.extra["auth_source"], "azure_cli");
    assert!(!actual.extra.contains_key("client_id"));
}

#[tokio::test]
async fn mcp_설정_갱신은_키체인_저장_상태와_비밀을_그대로_유지한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let raw = json!({"groups":[],"connections":[{"id":"fixture","name":"Fixture","params":{
        "driver":saved.driver,"host":"localhost","port":5432,"database":"fixture",
        "save_in_keychain":true,"connectionUriInKeychain":true,
        "ssh_key_passphrase":"synthetic-passphrase",
        "extra":{"api_token":"synthetic-token"}}}]});
    persistence::service_write_atomic(
        harness.connections.path(),
        &serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
    let before = harness.connections.get("fixture").await.unwrap();
    let update = update_request(
        before.version,
        json!({"settings":{"host":"localhost","port":5433,"database":"fixture"}}),
    );
    let secrets = Arc::new(FakeSecrets {
        password: Some("keychain-password".into()),
        uri: Ok(Some("synthetic://restored".into())),
    });

    // when
    let updated = approve_and_finish(&harness, update).await;
    let stored = harness.connections.private_params("fixture").unwrap();
    let on_disk: Value =
        serde_json::from_slice(&std::fs::read(harness.connections.path()).unwrap()).unwrap();
    let restored = restore_saved_secrets(secrets, stored.clone(), "fixture")
        .await
        .unwrap();

    // then
    assert_eq!(updated.1.status, JobStatus::Succeeded);
    assert_eq!(stored.port, Some(5433));
    assert_eq!(stored.save_in_keychain, Some(true));
    assert_eq!(stored.connection_uri_in_keychain, Some(true));
    assert_eq!(
        stored.ssh_key_passphrase.as_deref(),
        Some("synthetic-passphrase")
    );
    assert_eq!(stored.extra["api_token"], "synthetic-token");
    assert_eq!(on_disk["connections"][0]["params"]["save_in_keychain"], true);
    assert_eq!(
        on_disk["connections"][0]["params"]["connection_uri_in_keychain"],
        true
    );
    assert_eq!(restored.password.as_deref(), Some("keychain-password"));
    assert_eq!(
        restored.connection_uri.as_deref(),
        Some("synthetic://restored")
    );
}
