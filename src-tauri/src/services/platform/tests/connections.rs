use super::*;

#[tokio::test]
async fn 연결_crud는_version과_조직_정보_및_미지의_필드를_보존한다() {
    // given
    let harness = Harness::new().await;
    let original = harness.connections.get("fixture").await.unwrap();
    // when
    let actual = update_and_read(&harness, original.version).await;
    // then
    assert_eq!(actual.0.version, original.version + 1);
    assert!(actual.0.allow_writes);
    assert_ne!(actual.0.auth_revision, original.auth_revision);
    assert_eq!(actual.1["future_top"], "retained");
    assert_eq!(actual.1["connections"][0]["group_id"], "group");
    assert_eq!(actual.1["connections"][0]["tag_ids"], json!(["tag"]));
    assert_eq!(actual.1["connections"][0]["future_connection"], "retained");
    assert_eq!(
        actual.1["connections"][0]["params"]["future_param"],
        json!({"v":4})
    );
    assert!(!serde_json::to_string(&actual.0)
        .unwrap()
        .contains("synthetic-legacy-password"));
}
async fn update_and_read(harness: &Harness, version: u64) -> (SavedConnection, Value) {
    let saved = harness
        .connections
        .update(
            "fixture",
            version,
            json!({"name":"Updated","allow_writes":true}),
        )
        .await
        .unwrap();
    let raw = serde_json::from_slice(&std::fs::read(harness.connections.path()).unwrap()).unwrap();
    (saved, raw)
}

#[tokio::test]
async fn 외부_legacy_설정_변경은_version과_auth_revision을_한번_회전한다() {
    // given
    let harness = Harness::new().await;
    let original = harness.connections.get("fixture").await.unwrap();
    let mut legacy = persistence::load_connections_file(harness.connections.path()).unwrap();
    legacy.connections[0].params.host = Some("changed.example".into());
    persistence::save_connections_file(harness.connections.path(), &legacy).unwrap();
    // when
    let actual = read_twice(&harness.connections).await;
    // then
    assert_eq!(actual.0.version, original.version + 1);
    assert_eq!(actual.0.auth_revision, actual.1.auth_revision);
    assert_ne!(actual.0.auth_revision, original.auth_revision);
    assert_eq!(actual.0.settings["host"], "changed.example");
}
async fn read_twice(connections: &ProfileConnections) -> (SavedConnection, SavedConnection) {
    (
        connections.get("fixture").await.unwrap(),
        connections.get("fixture").await.unwrap(),
    )
}

#[tokio::test]
async fn auth_revision만_회전하면_연결_version은_증가하지_않는다() {
    // given
    let harness = Harness::new().await;
    let original = harness.connections.get("fixture").await.unwrap();
    // when
    let actual = rotate_and_read(&harness.connections).await;
    // then
    assert_ne!(actual.auth_revision, original.auth_revision);
    assert_eq!(actual.version, original.version);
}
async fn rotate_and_read(connections: &ProfileConnections) -> SavedConnection {
    connections.rotate_auth_revision("fixture").await.unwrap();
    connections.get("fixture").await.unwrap()
}

#[tokio::test]
async fn 잘못된_설정과_다른_연결_자격증명_ref는_저장하지_않는다() {
    // given
    let harness = Harness::new().await;
    let original = harness.connections.get("fixture").await.unwrap();
    let bytes = std::fs::read(harness.connections.path()).unwrap();
    // when
    let actual = reject_changes(&harness, &original).await;
    // then
    assert!(actual.iter().all(Result::is_err));
    assert_eq!(std::fs::read(harness.connections.path()).unwrap(), bytes);
}
async fn reject_changes(
    harness: &Harness,
    connection: &SavedConnection,
) -> Vec<ServiceResult<SavedConnection>> {
    let mut results = vec![];
    for settings in [
        json!({"driver":"other"}),
        json!({"azure_access_token":"synthetic-secret"}),
        json!({"host":"https://user:pass@example.com"}),
        json!({"unknown":true}),
        json!({"host":77}),
    ] {
        results.push(
            harness
                .connections
                .create("bad", &connection.driver, settings, None)
                .await,
        );
    }
    results.push(
        harness
            .connections
            .create(
                "bad",
                &connection.driver,
                json!({"host":"localhost"}),
                Some("fixture:db_password".into()),
            )
            .await,
    );
    results.push(
        harness
            .connections
            .update(
                "fixture",
                connection.version,
                json!({"credential_ref":"other:db_password"}),
            )
            .await,
    );
    results
}

#[tokio::test]
async fn 같은_연결의_자격증명_slot_ref만_갱신할_수_있다() {
    // given
    let harness = Harness::new().await;
    let original = harness.connections.get("fixture").await.unwrap();
    // when
    let actual = harness
        .connections
        .update(
            "fixture",
            original.version,
            json!({"credential_ref":"fixture:db_password"}),
        )
        .await
        .unwrap();
    // then
    assert_eq!(
        actual.credential_ref.as_deref(),
        Some("fixture:db_password")
    );
    assert_eq!(actual.version, original.version + 1);
}

#[tokio::test]
async fn stale_version의_갱신과_삭제는_현재_version을_반환한다() {
    // given
    let harness = Harness::new().await;
    let original = harness.connections.get("fixture").await.unwrap();
    harness
        .connections
        .update("fixture", original.version, json!({"name":"current"}))
        .await
        .unwrap();
    let bytes = std::fs::read(harness.connections.path()).unwrap();
    // when
    let actual = stale_changes(&harness.connections, original.version).await;
    // then
    let update = actual.0.unwrap_err();
    let delete = actual.1.unwrap_err();
    assert_eq!(update.code, ErrorCode::ConnectionConflict);
    assert_eq!(delete.code, ErrorCode::ConnectionConflict);
    assert_eq!(update.details["current_version"], original.version + 1);
    assert_eq!(delete.details["current_version"], original.version + 1);
    assert_eq!(std::fs::read(harness.connections.path()).unwrap(), bytes);
}

#[tokio::test]
async fn gui가_바꾼_연결의_리비전은_서비스_version으로_이어지고_낡은_서비스_갱신은_충돌한다() {
    // given
    let harness = Harness::new().await;
    let original = harness.connections.get("fixture").await.unwrap();
    persistence::update_connections_file(harness.connections.path(), |file| {
        file.connections[0].name = "renamed by gui".into();
        Ok(())
    })
    .unwrap();
    let bytes = std::fs::read(harness.connections.path()).unwrap();
    // when
    let current = harness.connections.get("fixture").await.unwrap();
    let stale = harness
        .connections
        .update("fixture", original.version, json!({"name":"stale"}))
        .await;
    // then
    assert_eq!(current.version, original.version + 1);
    assert_eq!(stale.unwrap_err().code, ErrorCode::ConnectionConflict);
    assert_eq!(std::fs::read(harness.connections.path()).unwrap(), bytes);
}
async fn stale_changes(
    connections: &ProfileConnections,
    version: u64,
) -> (ServiceResult<SavedConnection>, ServiceResult<()>) {
    (
        connections
            .update("fixture", version, json!({"name":"stale"}))
            .await,
        connections.delete("fixture", version).await,
    )
}

#[tokio::test]
async fn resolver는_명시_연결의_private_params만_확장한다() {
    // given
    let harness = Harness::new().await;
    let original = harness.connections.get("fixture").await.unwrap();
    let expander = Arc::new(CountExpander::default());
    let resolver = ProfileConnectionResolver::new(harness.connections.clone(), expander.clone());
    // when
    let actual =
        crate::service_registry_driver::ServiceConnectionResolver::params(&resolver, &original)
            .await
            .unwrap();
    // then
    assert_eq!(actual.connection_id.as_deref(), Some("fixture"));
    assert_eq!(
        actual.password.as_deref(),
        Some("synthetic-legacy-password")
    );
    assert_eq!(expander.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 다른_driver의_saved_claim은_private_자격증명_확장전에_거부한다() {
    // given
    let harness = Harness::new().await;
    let mut original = harness.connections.get("fixture").await.unwrap();
    original.driver = "other".into();
    let expander = Arc::new(CountExpander::default());
    let resolver = ProfileConnectionResolver::new(harness.connections.clone(), expander.clone());
    // when
    let actual =
        crate::service_registry_driver::ServiceConnectionResolver::params(&resolver, &original)
            .await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(expander.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 관찰되지_않은_외부_설정은_같은_version의_갱신과_삭제도_거부한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let mut raw: Value =
        serde_json::from_slice(&std::fs::read(harness.connections.path()).unwrap()).unwrap();
    raw["connections"][0]["params"]["host"] = json!("external.example");
    std::fs::write(harness.connections.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
    let bytes = std::fs::read(harness.connections.path()).unwrap();
    // when
    let actual = stale_changes(&harness.connections, saved.version).await;
    // then
    assert_eq!(actual.0.unwrap_err().code, ErrorCode::ConnectionConflict);
    assert_eq!(actual.1.unwrap_err().code, ErrorCode::ConnectionConflict);
    assert_eq!(std::fs::read(harness.connections.path()).unwrap(), bytes);
}

#[tokio::test]
async fn 신규_연결은_고유_id와_readonly_기본값으로_생성되고_삭제한다() {
    // given
    let harness = Harness::new().await;
    let driver = harness.connections.get("fixture").await.unwrap().driver;
    // when
    let actual = create_and_delete(&harness.connections, &driver).await;
    // then
    assert!(uuid::Uuid::parse_str(&actual.0.id).is_ok());
    assert!(!actual.0.allow_writes);
    assert_eq!(actual.0.version, 1);
    assert!(actual.1.is_err());
    assert_eq!(actual.0.settings["host"], "new.example");
}
async fn create_and_delete(
    connections: &ProfileConnections,
    driver: &str,
) -> (SavedConnection, ServiceResult<SavedConnection>) {
    let created = connections
        .create(
            "Created",
            driver,
            json!({"host":"new.example","database":"db"}),
            None,
        )
        .await
        .unwrap();
    connections
        .delete(&created.id, created.version)
        .await
        .unwrap();
    let deleted = connections.get(&created.id).await;
    (created, deleted)
}

#[tokio::test]
async fn cosmos_평면_설정과_nested_설정은_같은_private_params로_정규화한다() {
    // given
    let directory = Directory::new();
    let id = "cosmos-nosql";
    crate::drivers::registry::register_driver(
        crate::plugins::driver::native_tests::child_driver(id, "cosmos", json!({})).await,
    )
    .await;
    let connections = ProfileConnections::new(&directory.0);
    let flat = json!({"endpoint":"https://synthetic.documents.azure.com/","auth_mode":"account_key","database":"database"});
    let nested = json!({"extra":{"endpoint":"https://synthetic.documents.azure.com/","auth_mode":"account_key"},"database":"database"});
    // when
    let actual = normalize_cosmos(&connections, flat, nested).await;
    // then
    assert_eq!(actual.0, actual.1);
    assert_eq!(
        actual.0["extra"]["endpoint"],
        "https://synthetic.documents.azure.com/"
    );
    assert_eq!(actual.0["extra"]["auth_mode"], "account_key");
    assert_eq!(actual.0["database"], "database");
    assert!(actual.0["password"].is_null());
}
async fn normalize_cosmos(
    connections: &ProfileConnections,
    flat: Value,
    nested: Value,
) -> (Value, Value) {
    let first = connections
        .create("flat", "cosmos-nosql", flat, None)
        .await
        .unwrap();
    let second = connections
        .create("nested", "cosmos-nosql", nested, None)
        .await
        .unwrap();
    (
        serde_json::to_value(connections.private_params(&first.id).unwrap()).unwrap(),
        serde_json::to_value(connections.private_params(&second.id).unwrap()).unwrap(),
    )
}

struct FailingObservation;
#[async_trait]
impl ConnectionObservation for FailingObservation {
    async fn observe(&self, _: &SavedConnection, _: bool) -> ServiceResult<bool> {
        Err(failure(ErrorCode::AuthRequired))
    }
}
#[tokio::test]
async fn 외부_설정이_게시되어도_무효화_hook_실패는_fresh_get을_거부한다() {
    // given
    let harness = Harness::new().await;
    harness.connections.get("fixture").await.unwrap();
    harness
        .connections
        .set_observation(Arc::new(FailingObservation))
        .unwrap();
    // when
    let actual = harness.connections.get("fixture").await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::AuthRequired);
}
