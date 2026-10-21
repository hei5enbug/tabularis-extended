use super::*;
use crate::drivers::driver_trait::{DatabaseDriver, PluginSettingDefinition};
use crate::services::jobs::{JobLease, JobRecord};

struct ValidationGate {
    calls: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
impl ValidationGate {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}
#[async_trait]
impl crate::service_platform::connections::PreparationHook for ValidationGate {
    async fn before_validation(&self) {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
        }
    }
}
async fn cancel_during_preparation(
    harness: &Harness,
    request: ServiceRequest,
    gate: &ValidationGate,
) -> JobRecord {
    let gui = caller(CallerTransport::Gui);
    let connection_id = request.connection_id.clone();
    let queued = harness.runtime.submit(request, gui.clone()).await;
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    let job_id = queued.job_id.unwrap();
    let mut cancel = request_for_cancel(&job_id);
    cancel.connection_id = connection_id;
    let response = harness.runtime.call_validated(&cancel, &gui).await;
    assert_eq!(response.status, JobStatus::Succeeded);
    gate.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let record = harness
                .runtime
                .jobs
                .get(&job_id, "platform-owner", &gui)
                .unwrap();
            if record.response.is_some() {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
fn request_for_cancel(job_id: &str) -> ServiceRequest {
    request(
        Operation::JobCancel,
        json!({"job_id":job_id,"owner_instance_id":"platform-owner"}),
    )
}

#[tokio::test]
async fn 생성의_마지막_검증_대기중_취소하면_파일을_게시하지_않는다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let bytes = std::fs::read(harness.connections.path()).unwrap();
    let gate = Arc::new(ValidationGate::new());
    harness.connections.set_preparation_hook(gate.clone());
    let mut request = request(
        Operation::ConnectionCreate,
        json!({"name":"Cancelled","driver":saved.driver,"settings":{"host":"localhost","database":"fixture"},"credential_ref":null}),
    );
    request.connection_id = None;
    // when
    let actual = cancel_during_preparation(&harness, request, &gate).await;
    // then
    assert_eq!(
        actual.response.unwrap().error.unwrap().code,
        ErrorCode::Cancelled
    );
    assert!(!actual.write_dispatched);
    assert!(!actual.write_confirmed);
    assert_eq!(std::fs::read(harness.connections.path()).unwrap(), bytes);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.secrets.0.load(Ordering::SeqCst), 0);
    assert_eq!(harness.oauth.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 수정의_마지막_검증_대기중_취소하면_기존_파일을_보존한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let bytes = std::fs::read(harness.connections.path()).unwrap();
    let gate = Arc::new(ValidationGate::new());
    harness.connections.set_preparation_hook(gate.clone());
    let mut request = request(
        Operation::ConnectionUpdate,
        json!({"patch":{"name":"Cancelled","settings":{"host":"changed.example","database":"fixture"}}}),
    );
    request.expected_version = Some(saved.version);
    // when
    let actual = cancel_during_preparation(&harness, request, &gate).await;
    // then
    assert_eq!(
        actual.response.unwrap().error.unwrap().code,
        ErrorCode::Cancelled
    );
    assert!(!actual.write_dispatched);
    assert!(!actual.write_confirmed);
    assert_eq!(std::fs::read(harness.connections.path()).unwrap(), bytes);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.secrets.0.load(Ordering::SeqCst), 0);
    assert_eq!(harness.oauth.0.load(Ordering::SeqCst), 0);
}

async fn guarded_publications(
    harness: &Harness,
    driver: &str,
    lease: &JobLease,
) -> (SavedConnection, SavedConnection, bool) {
    let created = harness
        .connections
        .create_guarded(
            "Created",
            driver,
            json!({"host":"localhost","database":"fixture"}),
            None,
            lease,
        )
        .await
        .unwrap();
    let updated = harness
        .connections
        .update_guarded(
            &created.id,
            created.version,
            json!({"name":"Updated"}),
            lease,
        )
        .await
        .unwrap();
    let published = harness.connections.get(&created.id).await.unwrap().name == "Updated";
    (created, updated, published)
}
#[tokio::test]
async fn 정상_guard는_게시전에_dispatch를_기록하고_version을_증가한다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let request = request(
        Operation::ConnectionUpdate,
        json!({"patch":{"name":"Updated"}}),
    );
    let lease = harness
        .runtime
        .jobs
        .begin(&request, &caller(CallerTransport::Gui), false, false)
        .await
        .unwrap();
    // when
    let actual = guarded_publications(&harness, &saved.driver, &lease).await;
    // then
    assert!(lease.write_dispatched());
    assert_eq!(actual.0.version, 1);
    assert_eq!(actual.1.version, 2);
    assert!(actual.2);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

async fn setting_driver() -> String {
    let id = format!("settings-{}", uuid::Uuid::new_v4());
    let base = crate::plugins::driver::native_tests::child_driver(&id, "native", json!({})).await;
    let mut manifest = base.manifest().clone();
    base.shutdown().await;
    manifest.settings = [
        ("trust_server_certificate", "boolean"),
        ("max_pool_size", "number"),
    ]
    .into_iter()
    .map(|(key, kind)| PluginSettingDefinition {
        key: key.into(),
        label: key.into(),
        setting_type: kind.into(),
        default: None,
        description: None,
        required: false,
        options: vec![],
    })
    .collect();
    let driver = crate::plugins::driver::RpcDriver::new(
        manifest,
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/native_service_plugin.py"),
        Some("/usr/bin/python3".into()),
        vec![],
        std::collections::HashMap::from([("mode".into(), json!("native"))]),
    )
    .await
    .unwrap();
    crate::drivers::registry::register_driver(driver).await;
    id
}
#[tokio::test]
async fn 저장된_boolean과_number_문자열은_이름만_수정해도_유효하다() {
    // given
    let harness = Harness::new().await;
    let id = setting_driver().await;
    let saved=harness.connections.create("Typed",&id,json!({"host":"localhost","database":"fixture","extra":{"trust_server_certificate":false,"max_pool_size":4}}),None).await.unwrap();
    let mut request = request(
        Operation::ConnectionUpdate,
        json!({"patch":{"name":"Renamed"}}),
    );
    request.connection_id = Some(saved.id.clone());
    request.expected_version = Some(saved.version);
    // when
    let actual = harness
        .runtime
        .call_validated(&request, &caller(CallerTransport::Gui))
        .await;
    // then
    assert_eq!(
        actual.status,
        JobStatus::Succeeded,
        "Safe service error: {:?}",
        actual.error
    );
    assert_eq!(actual.data["name"], "Renamed");
    assert_eq!(
        actual.data["settings"]["extra"]["trust_server_certificate"],
        "false"
    );
    assert_eq!(actual.data["settings"]["extra"]["max_pool_size"], "4");
    assert_eq!(actual.data["allow_writes"], false);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}
async fn reject_noncanonical_settings(
    connections: &ProfileConnections,
    id: &str,
) -> Vec<ServiceResult<()>> {
    let mut results = vec![];
    for (key, value) in [
        ("trust_server_certificate", "TRUE"),
        ("max_pool_size", " 4 "),
        ("max_pool_size", "NaN"),
        ("max_pool_size", "Infinity"),
        ("max_pool_size", "{}"),
    ] {
        results.push(
            connections
                .validate_settings(id, &json!({"extra":{key:value}}))
                .await,
        );
    }
    results
}
#[tokio::test]
async fn boolean과_number의_비정규_문자열은_저장하지_않는다() {
    // given
    let harness = Harness::new().await;
    let id = setting_driver().await;
    harness.connections.get("fixture").await.unwrap();
    let bytes = std::fs::read(harness.connections.path()).unwrap();
    // when
    let actual = reject_noncanonical_settings(&harness.connections, &id).await;
    // then
    assert!(actual.iter().all(|result| result
        .as_ref()
        .is_err_and(|error| error.code == ErrorCode::InvalidArgument)));
    assert_eq!(std::fs::read(harness.connections.path()).unwrap(), bytes);
}

struct ObservationGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
#[async_trait]
impl ConnectionObservation for ObservationGate {
    async fn observe(&self, connection: &SavedConnection, _: bool) -> ServiceResult<bool> {
        if connection.id == "fixture" {
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
        }
        Ok(false)
    }
}
async fn independent_observation(
    connections: Arc<ProfileConnections>,
    second: &str,
    gate: &ObservationGate,
) -> (SavedConnection, SavedConnection) {
    let first_connections = connections.clone();
    let first = tokio::spawn(async move { first_connections.get("fixture").await.unwrap() });
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(1), connections.get(second))
        .await
        .unwrap()
        .unwrap();
    gate.release.add_permits(1);
    (first.await.unwrap(), second)
}
#[tokio::test]
async fn 다른_연결의_fresh_get은_관찰_hook의_대기에_막히지_않는다() {
    // given
    let harness = Harness::new().await;
    let saved = harness.connections.get("fixture").await.unwrap();
    let second = harness
        .connections
        .create(
            "Independent",
            &saved.driver,
            json!({"host":"localhost","database":"fixture"}),
            None,
        )
        .await
        .unwrap();
    let gate = Arc::new(ObservationGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
    });
    harness.connections.set_observation(gate.clone()).unwrap();
    // when
    let actual = independent_observation(harness.connections.clone(), &second.id, &gate).await;
    // then
    assert_eq!(actual.0.id, "fixture");
    assert_eq!(actual.1.id, second.id);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

async fn passphrase_boundaries(
    harness: &Harness,
    canary: &str,
) -> (
    SavedConnection,
    ServiceResponse,
    ServiceResult<()>,
    ServiceResult<()>,
    Option<String>,
) {
    let saved = harness.connections.get("fixture").await.unwrap();
    let listed = harness
        .runtime
        .call_validated(
            &request(Operation::ConnectionList, json!({})),
            &caller(CallerTransport::Gui),
        )
        .await;
    let settings = json!({"ssh_key_passphrase":canary});
    let invalid = harness
        .connections
        .validate_settings(&saved.driver, &settings)
        .await;
    let mut change = request(
        Operation::ConnectionUpdate,
        json!({"patch":{"settings":settings}}),
    );
    change.expected_version = Some(saved.version);
    let approval = harness
        .approvals
        .configuration(&caller(CallerTransport::Mcp), &change)
        .await;
    let private = harness
        .connections
        .private_params("fixture")
        .unwrap()
        .ssh_key_passphrase;
    (saved, listed, invalid, approval, private)
}
#[tokio::test]
async fn legacy_passphrase는_private에만_남고_공개_응답과_승인에서_제거한다() {
    // given
    let harness = Harness::new().await;
    let canary = "synthetic-private-passphrase-canary";
    let mut legacy = persistence::load_connections_file(harness.connections.path()).unwrap();
    legacy.connections[0].params.ssh_key_passphrase = Some(canary.into());
    legacy.connections[0].params.ssh_allow_passphrase_prompt = Some(true);
    persistence::save_connections_file(harness.connections.path(), &legacy).unwrap();
    // when
    let actual = passphrase_boundaries(&harness, canary).await;
    // then
    assert!(!serde_json::to_string(&actual.0).unwrap().contains(canary));
    assert!(!serde_json::to_string(&actual.1).unwrap().contains(canary));
    assert_eq!(actual.0.settings["ssh_allow_passphrase_prompt"], true);
    assert_eq!(actual.1.status, JobStatus::Succeeded);
    assert_eq!(actual.2.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(actual.3.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(actual.4.as_deref(), Some(canary));
    assert_eq!(queue_counts(&harness.directory.0), (0, 0, 0));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.secrets.0.load(Ordering::SeqCst), 0);
    assert_eq!(harness.oauth.0.load(Ordering::SeqCst), 0);
}

async fn created_metadata(
    connections: &ProfileConnections,
    driver: &str,
) -> (SavedConnection, Value, SavedConnection) {
    let saved = connections
        .create(
            "Metadata",
            driver,
            json!({"host":"localhost","database":"fixture"}),
            None,
        )
        .await
        .unwrap();
    let raw: Value = serde_json::from_slice(&std::fs::read(connections.path()).unwrap()).unwrap();
    let record = raw["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["id"] == saved.id)
        .unwrap()
        .clone();
    let fresh = connections.get(&saved.id).await.unwrap();
    (saved, record, fresh)
}
#[tokio::test]
async fn 생성한_연결의_초기_metadata와_auth_revision은_첫_게시부터_보존한다() {
    // given
    let harness = Harness::new().await;
    let driver = harness.connections.get("fixture").await.unwrap().driver;
    // when
    let actual = created_metadata(&harness.connections, &driver).await;
    // then
    assert_eq!(actual.1["service_v1"]["version"], 1);
    assert_eq!(
        actual.1["service_v1"]["auth_revision"],
        actual.0.auth_revision
    );
    assert_eq!(actual.1["service_v1"]["allow_writes"], false);
    assert_eq!(
        actual.1["service_v1"]["settings_fingerprint"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(actual.2.auth_revision, actual.0.auth_revision);
    assert_eq!(actual.2.version, 1);
}

async fn repeated_extra_roundtrip(
    harness: &Harness,
    id: &str,
) -> (
    SavedConnection,
    Vec<SavedConnection>,
    ServiceResponse,
    Vec<SavedConnection>,
) {
    let created=harness.connections.create("Canonical",id,json!({"host":"localhost","database":"fixture","extra":{"trust_server_certificate":false,"max_pool_size":4}}),None).await.unwrap();
    let mut before = vec![];
    for _ in 0..12 {
        before.push(harness.connections.get(&created.id).await.unwrap());
    }
    let mut change = request(
        Operation::ConnectionUpdate,
        json!({"patch":{"name":"Renamed"}}),
    );
    change.connection_id = Some(created.id.clone());
    change.expected_version = Some(1);
    let updated = harness
        .runtime
        .call_validated(&change, &caller(CallerTransport::Gui))
        .await;
    let mut after = vec![];
    for _ in 0..12 {
        after.push(harness.connections.get(&created.id).await.unwrap());
    }
    (created, before, updated, after)
}
#[tokio::test]
async fn extra_두_필드의_반복_roundtrip은_이름_수정_외에_version을_바꾸지_않는다() {
    // given
    let harness = Harness::new().await;
    let driver = setting_driver().await;
    // when
    let actual = repeated_extra_roundtrip(&harness, &driver).await;
    // then
    assert!(actual
        .1
        .iter()
        .all(|saved| saved.version == 1 && saved.auth_revision == actual.0.auth_revision));
    assert_eq!(actual.2.status, JobStatus::Succeeded);
    assert_eq!(actual.2.data["version"], 2);
    assert!(actual
        .3
        .iter()
        .all(|saved| saved.version == 2 && saved.auth_revision == actual.0.auth_revision));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.driver.invalidations.load(Ordering::SeqCst), 0);
}

async fn external_key_order(harness: &Harness, second: &str) -> (SavedConnection, SavedConnection) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "service_platform::tests::guarded::외부_key_order_writer_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("TABULARIS_KEY_ORDER_TEST_PROFILE", &harness.directory.0)
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
            .contains("H3B1_KEY_ORDER_CHILD_COMPLETED"),
        "Owned child must actually execute"
    );
    (
        harness.connections.get("fixture").await.unwrap(),
        harness.connections.get(second).await.unwrap(),
    )
}
#[tokio::test]
async fn 실제_외부_프로세스가_객체_key_순서만_바꾸면_권한과_version을_유지한다() {
    // given
    let harness = Harness::new().await;
    let mut raw: Value =
        serde_json::from_slice(&std::fs::read(harness.connections.path()).unwrap()).unwrap();
    raw["connections"][0]["params"]["future_param"] =
        json!({"a":1,"nested":{"left":true,"right":[1,2]}});
    persistence::service_write_atomic(
        harness.connections.path(),
        &serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
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
    let second = harness
        .connections
        .create(
            "Independent",
            &saved.driver,
            json!({"host":"localhost","database":"fixture"}),
            None,
        )
        .await
        .unwrap();
    let second = harness.connections.get(&second.id).await.unwrap();
    harness
        .runtime
        .results
        .invalidate_connection_revision(&second.id, &second.auth_revision)
        .unwrap();
    // when
    let actual = external_key_order(&harness, &second.id).await;
    // then
    assert_eq!(actual.0.version, saved.version);
    assert_eq!(actual.0.auth_revision, saved.auth_revision);
    assert_eq!(actual.1.version, second.version);
    assert_eq!(actual.1.auth_revision, second.auth_revision);
    assert_eq!(harness.driver.invalidations.load(Ordering::SeqCst), 0);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}
fn reverse_object_keys(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .rev()
                .map(|(key, value)| (key.clone(), reverse_object_keys(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(reverse_object_keys).collect()),
        value => value.clone(),
    }
}
fn publish_key_order(profile: &Path) -> ServiceResult<()> {
    let path = profile.join("connections.json");
    let raw: Value = serde_json::from_slice(
        &persistence::service_read_file(&path, 16 * 1024 * 1024, true)?.unwrap(),
    )
    .unwrap();
    persistence::service_write_atomic(
        &path,
        &serde_json::to_vec(&reverse_object_keys(&raw)).unwrap(),
    )?;
    println!("H3B1_KEY_ORDER_CHILD_COMPLETED");
    Ok(())
}
#[test]
#[ignore = "Owned child-process fixture"]
fn 외부_key_order_writer_fixture() {
    // given
    let profile = PathBuf::from(
        std::env::var_os("TABULARIS_KEY_ORDER_TEST_PROFILE").expect("Owned profile required"),
    );
    // when
    let actual = publish_key_order(&profile);
    // then
    assert!(actual.is_ok());
}
