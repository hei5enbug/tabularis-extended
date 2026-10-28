#![allow(non_snake_case)]
use super::*;
use crate::services::{
    contracts::{Outcome, MAX_JOB_DEADLINE_MS},
    credentials::Secret,
};

pub(crate) async fn child_driver(id: &str, mode: &str, extra: Value) -> RpcDriver {
    let mut manifest = test_manifest();
    manifest.id = id.into();
    let mut settings = HashMap::from([("mode".into(), json!(mode))]);
    if let Some(extra) = extra.as_object() {
        settings.extend(
            extra
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    RpcDriver::new(
        manifest,
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/native_service_plugin.py"),
        Some("/usr/bin/python3".into()),
        vec![],
        settings,
    )
    .await
    .unwrap()
}

pub(crate) fn params(id: &str) -> ConnectionParams {
    serde_json::from_value(json!({"driver":id,"host":"localhost","port":5432,"username":"synthetic","password":null,"database":"original","ssl_mode":null,"ssl_ca":null,"ssl_cert":null,"ssl_key":null,"ssh_enabled":false,"ssh_connection_id":null,"ssh_host":null,"ssh_port":null,"ssh_user":null,"ssh_password":null,"ssh_key_file":null,"ssh_key_passphrase":null,"save_in_keychain":false,"k8s_enabled":false,"k8s_connection_id":null,"k8s_context":null,"k8s_namespace":null,"k8s_resource_type":null,"k8s_resource_name":null,"k8s_port":null,"k8s_kubectl_path":null,"k8s_kubeconfig_path":null,"ssm_enabled":false,"ssm_target":null,"ssm_profile":null,"ssm_region":null})).unwrap()
}
pub(crate) fn context(connection: &str, request: &str) -> DriverContext {
    DriverContext {
        connection_id: connection.into(),
        request_id: request.into(),
        deadline_ms: 2000,
        signal: CancellationSignal::default(),
        transient_auth: None,
        read_only: true,
        session_handle: None,
    }
}
pub(crate) fn query(text: &str) -> Value {
    json!({"text":text,"parameters":[],"max_rows":10000,"max_bytes":16*1024*1024-64*1024})
}

async fn negotiate_twice(
    driver: &RpcDriver,
) -> (DriverServiceCapabilities, DriverServiceCapabilities, Value) {
    let first = driver.service_capabilities().await.unwrap();
    let second = driver.service_capabilities().await.unwrap();
    let stats = driver
        .service_call(
            ServiceRpcMethod::Test,
            &params(&driver.manifest.id),
            &context("conn", "stats"),
            json!({}),
        )
        .await
        .unwrap();
    driver.shutdown().await;
    (first, second, stats)
}

#[tokio::test]
async fn 실제_자식의_프로토콜_협상은_프로세스당_한번_캐시한다() {
    // given
    let driver = child_driver("native-cache", "native", json!({})).await;
    // when
    let (first, second, stats) = negotiate_twice(&driver).await;
    // then
    assert_eq!(first, second);
    assert!(first.exact_query && first.sessions && first.cancel);
    assert_eq!(stats["initializations"], 1);
}

async fn negotiate_after_deadline(driver: &RpcDriver) -> (ServiceError, Value, Value) {
    let mut first = context("conn", "first");
    first.deadline_ms = 30;
    let error = driver
        .service_call(
            ServiceRpcMethod::ExactQuery,
            &params(&driver.manifest.id),
            &first,
            query("SELECT 1"),
        )
        .await
        .unwrap_err();
    let output = driver
        .service_call(
            ServiceRpcMethod::ExactQuery,
            &params(&driver.manifest.id),
            &context("conn", "second"),
            query("SELECT 1"),
        )
        .await
        .unwrap();
    driver.service_capabilities().await.unwrap();
    let stats = driver
        .service_call(
            ServiceRpcMethod::Test,
            &params(&driver.manifest.id),
            &context("conn", "stats"),
            json!({}),
        )
        .await
        .unwrap();
    driver.shutdown().await;
    (error, output, stats)
}
#[tokio::test]
async fn 첫_협상의_deadline은_캐시하지_않아_새_명시_요청이_성공한다() {
    // given
    let driver = child_driver(
        "negotiation-deadline",
        "native",
        json!({"slow_first_initialize":true}),
    )
    .await;
    driver
        .process
        .send_request("fixture_stats", json!({}), Some(Duration::from_secs(1)))
        .await
        .unwrap();
    // when
    let (error, output, stats) = negotiate_after_deadline(&driver).await;
    // then
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert_eq!(error.outcome, Outcome::NotStarted);
    assert_eq!(output["rows"][0][0], true);
    assert_eq!(stats["initializations"], 2);
    assert_eq!(stats["queries"], 1);
}

async fn rejected_initializations() -> Vec<(ErrorCode, u64)> {
    let mut result = vec![];
    for (mode, expected_count) in [
        ("legacy", 2),
        ("missing", 2),
        ("init_error", 1),
        ("protocol2", 1),
    ] {
        let driver = child_driver(&format!("reject-{mode}"), mode, json!({})).await;
        let code = driver.service_capabilities().await.unwrap_err().code;
        let stats = driver
            .process
            .send_request("fixture_stats", json!({}), Some(Duration::from_secs(1)))
            .await
            .unwrap();
        result.push((code, stats["initializations"].as_u64().unwrap()));
        assert_eq!(stats["initializations"], expected_count);
        driver.shutdown().await;
    }
    result
}
#[tokio::test]
async fn 구형과_거부된_협상은_네이티브_성공으로_간주하지_않는다() {
    // given
    let expected = vec![
        ErrorCode::CapabilityUnavailable,
        ErrorCode::CapabilityUnavailable,
        ErrorCode::InvalidArgument,
        ErrorCode::ProtocolMismatch,
    ];
    // when
    let actual = rejected_initializations().await;
    // then
    assert_eq!(
        actual.into_iter().map(|entry| entry.0).collect::<Vec<_>>(),
        expected
    );
}

#[tokio::test]
async fn SQL서버의_result_내부_협상도_정규화한다() {
    // given
    let driver = child_driver("wrapped-caps", "wrapped", json!({})).await;
    // when
    let actual = driver.service_capabilities().await;
    // then
    assert_eq!(
        actual.unwrap().read_only,
        DriverServiceReadOnly::RestrictedDatabasePrincipal
    );
}

#[tokio::test]
async fn 구조화된_검증_오류는_전송후에도_not_started와_안전한_세션_정보를_보존한다() {
    // given
    let driver = child_driver("structured-error", "native", json!({})).await;
    // when
    let actual = driver
        .service_call(
            ServiceRpcMethod::ExactQuery,
            &params("structured-error"),
            &context("conn", "request"),
            query("structured_error"),
        )
        .await
        .unwrap_err();
    // then
    assert_eq!(actual.code, ErrorCode::InvalidArgument);
    assert_eq!(actual.outcome, Outcome::NotStarted);
    assert_eq!(actual.details, json!({"session_closed":true}));
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("SECRET_CANARY"));
}

#[tokio::test]
async fn 플러그인_호출은_한_시간까지의_기한을_보내고_그보다_길면_송신하지_않는다() {
    // given
    let driver = child_driver("long-deadline", "native", json!({})).await;
    let call = |deadline_ms: u64| {
        let mut context = context("conn", &format!("deadline-{deadline_ms}"));
        context.deadline_ms = deadline_ms;
        let driver = &driver;
        async move {
            driver
                .service_call(
                    ServiceRpcMethod::ExactQuery,
                    &params("long-deadline"),
                    &context,
                    query("SELECT 1"),
                )
                .await
        }
    };

    // when
    let accepted = call(MAX_JOB_DEADLINE_MS).await.unwrap();
    let rejected = call(MAX_JOB_DEADLINE_MS + 1).await.unwrap_err();

    // then
    assert_eq!(accepted["rows"], json!([[true, null]]));
    assert_eq!(rejected.code, ErrorCode::InvalidArgument);
    driver.shutdown().await;
}

fn remote_failure(data: Value) -> NativeFailure {
    NativeFailure {
        error: PluginCallError::Remote(crate::plugins::rpc::JsonRpcError {
            code: -32000,
            message: "Driver operation failed".into(),
            data: Some(data),
        }),
        attempted: true,
    }
}

#[test]
fn 플러그인이_드라이버_종료로_보고한_문장_오류는_문장_오류_코드로_바꿔_문구까지_전달한다() {
    // given
    let text =
        "SQL Server error 241: Conversion failed [statement failure; severity 16; state 1; line 1]";
    let failure = remote_failure(json!({
        "code": "DRIVER_EXITED",
        "message": "SQL Server execution failed",
        "retryable": false,
        "outcome": "not_applied",
        "details": {"database_error": {"engine":"sqlserver","message":text,"errors":[{"number":241,"severity":16,"state":1,"line":1}]}}
    }));

    // when
    let actual = native_failure(failure);

    // then
    assert_eq!(actual.code, ErrorCode::DatabaseError);
    assert_eq!(actual.outcome, Outcome::NotApplied);
    assert_eq!(actual.message, text);
    assert_eq!(actual.details["database_error"]["engine"], "sqlserver");
}

#[test]
fn 플러그인이_잘못된_입력으로_보고한_구문_오류도_문장_오류_코드로_바꾼다() {
    // given
    let text = "ERROR: syntax error at or near \"WHERE\" (SQLSTATE 42601)\nPOSITION: 13";
    let failure = remote_failure(json!({
        "code": "INVALID_ARGUMENT",
        "message": "Database operation failed",
        "retryable": false,
        "outcome": "not_applied",
        "details": {"database_error": {"engine":"postgresql","message":text,"sqlstate":"42601","position":13}}
    }));

    // when
    let actual = native_failure(failure);

    // then
    assert_eq!(actual.code, ErrorCode::DatabaseError);
    assert_eq!(actual.message, text);
    assert_eq!(actual.details["database_error"]["sqlstate"], "42601");
}

#[test]
fn database_error가_없는_플러그인_오류는_고정_문구로_가린다() {
    // given
    let failure = remote_failure(json!({
        "code": "DRIVER_EXITED",
        "message": "SECRET_CANARY_REMOTE",
        "retryable": false,
        "outcome": "not_applied",
        "details": {"secret": "SECRET_CANARY_REMOTE"}
    }));

    // when
    let actual = native_failure(failure);

    // then
    assert_eq!(actual.code, ErrorCode::DriverExited);
    assert_eq!(
        actual.message,
        crate::services::error::failure(ErrorCode::DriverExited).message
    );
    assert_eq!(actual.details, Value::Null);
}

async fn transport_failures() -> Vec<ServiceError> {
    let mut failures = vec![];
    for kind in ["eof", "utf8", "malformed", "oversized"] {
        let driver = child_driver(&format!("transport-{kind}"), "native", json!({})).await;
        failures.push(
            driver
                .service_call(
                    ServiceRpcMethod::ExactQuery,
                    &params(&driver.manifest.id),
                    &context("conn", "request"),
                    query(&format!("transport_{kind}")),
                )
                .await
                .unwrap_err(),
        );
        driver.shutdown().await;
    }
    failures
}
#[tokio::test]
async fn EOF와_잘못된_프레임은_요청을_재실행하지_않고_unknown으로_끝난다() {
    // given
    let expected = 4;
    // when
    let actual = transport_failures().await;
    // then
    assert_eq!(actual.len(), expected);
    assert!(actual
        .iter()
        .all(|error| error.code == ErrorCode::OutcomeUnknown && error.outcome == Outcome::Unknown));
    assert!(actual
        .iter()
        .all(|error| !error.message.contains("SECRET_CANARY")));
}

#[tokio::test]
async fn 상한을_넘는_송신_프레임은_DB_송신전에_거부한다() {
    // given
    let driver = child_driver("send-limit", "native", json!({})).await;
    let input = query(&"x".repeat(MAX_RPC_FRAME_BYTES));
    // when
    let actual = driver
        .service_call(
            ServiceRpcMethod::ExactQuery,
            &params("send-limit"),
            &context("conn", "request"),
            input,
        )
        .await
        .unwrap_err();
    // then
    assert_eq!(actual.code, ErrorCode::ResourceLimit);
    assert_eq!(actual.outcome, Outcome::NotStarted);
}

pub(crate) async fn wait_active(driver: &RpcDriver, count: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let stats = driver
                .service_call(
                    ServiceRpcMethod::Test,
                    &params(&driver.manifest.id),
                    &context("stats", &uuid::Uuid::new_v4().to_string()),
                    json!({}),
                )
                .await
                .unwrap();
            if stats["active"].as_array().unwrap().len() == count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn cancelled_pair(driver: Arc<RpcDriver>) -> (ServiceResult<Value>, ServiceResult<Value>) {
    let first = tokio::spawn({
        let driver = driver.clone();
        async move {
            driver
                .service_call(
                    ServiceRpcMethod::ExactQuery,
                    &params(&driver.manifest.id),
                    &context("conn-a", "same-request"),
                    query("wait_pair"),
                )
                .await
        }
    });
    let second = tokio::spawn({
        let driver = driver.clone();
        async move {
            driver
                .service_call(
                    ServiceRpcMethod::ExactQuery,
                    &params(&driver.manifest.id),
                    &context("conn-b", "same-request"),
                    query("wait_pair"),
                )
                .await
        }
    });
    wait_active(&driver, 2).await;
    driver
        .service_cancel("conn-a", "same-request")
        .await
        .unwrap();
    let result = (first.await.unwrap(), second.await.unwrap());
    driver.shutdown().await;
    result
}
#[tokio::test]
async fn 같은_request_ID의_서로_다른_연결은_취소_대상이_분리된다() {
    // given
    let driver = Arc::new(child_driver("cancel-pair", "native", json!({})).await);
    // when
    let (first, second) = cancelled_pair(driver).await;
    // then
    assert_eq!(first.unwrap_err().outcome, Outcome::NotApplied);
    assert!(second.is_ok());
}
async fn ack_before_success(driver: Arc<RpcDriver>) -> ServiceResult<Value> {
    let task = tokio::spawn({
        let driver = driver.clone();
        async move {
            let mut context = context("conn", "write");
            context.read_only = false;
            driver
                .service_call(
                    ServiceRpcMethod::ExactQuery,
                    &params(&driver.manifest.id),
                    &context,
                    query("wait_ack"),
                )
                .await
        }
    });
    wait_active(&driver, 1).await;
    driver.service_cancel("conn", "write").await.unwrap();
    let result = task.await.unwrap();
    driver.shutdown().await;
    result
}
#[tokio::test]
async fn 취소_ACK만으로_쓰기_rollback을_확정하지_않는다() {
    // given
    let driver = Arc::new(child_driver("cancel-ack", "native", json!({})).await);
    // when
    let actual = ack_before_success(driver).await;
    // then
    assert_eq!(actual.unwrap()["rows"], json!([[true, null]]));
}
#[tokio::test]
async fn 쓰기_송신후_마감은_unknown이며_비밀을_노출하지_않는다() {
    // given
    let driver = child_driver("write-timeout", "native", json!({})).await;
    driver.service_capabilities().await.unwrap();
    let mut context = context("conn", "write");
    context.read_only = false;
    context.deadline_ms = 100;
    context.transient_auth = Some(TransientAuth::DbPassword {
        password: Secret::new(b"SECRET_CANARY_PASSWORD".to_vec()).unwrap(),
        identity: "revision-1".into(),
    });
    // when
    let actual = driver
        .service_call(
            ServiceRpcMethod::ExactQuery,
            &params("write-timeout"),
            &context,
            query("wait_timeout"),
        )
        .await
        .unwrap_err();
    // then
    assert_eq!(actual.outcome, Outcome::Unknown);
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("SECRET_CANARY"));
}
#[test]
fn 같은_주체의_토큰_갱신은_wire_identity를_바꾸지_않는다() {
    // given
    let mut before = context("conn", "request");
    before.transient_auth = Some(TransientAuth::EntraToken {
        access_token: Secret::new(b"SECRET_CANARY_OLD".to_vec()).unwrap(),
        expires_at_ms: 1,
        tenant_id: "tenant".into(),
        client_id: "client".into(),
        principal_id: "principal".into(),
        scope: "scope".into(),
    });
    let mut after = before.clone();
    after.transient_auth = Some(TransientAuth::EntraToken {
        access_token: Secret::new(b"SECRET_CANARY_NEW".to_vec()).unwrap(),
        expires_at_ms: 999,
        tenant_id: "tenant".into(),
        client_id: "client".into(),
        principal_id: "principal".into(),
        scope: "scope".into(),
    });
    // when
    let identities = [before, after].map(|context| {
        driver_wire(&params("driver"), &context, json!({}), 100)["driver_context"]["auth"]
            ["identity"]
            .clone()
    });
    // then
    assert_eq!(identities[0], identities[1]);
    assert!(identities[0].as_str().unwrap().len() <= 128);
    assert!(!identities[0].as_str().unwrap().contains("SECRET_CANARY"));
}

#[tokio::test]
async fn 내장_드라이버는_선택적_네이티브_서비스를_성공으로_꾸미지_않는다() {
    // given
    let driver = crate::drivers::sqlite::SqliteDriver::new();
    // when
    let actual = driver.service_capabilities().await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::CapabilityUnavailable);
}

async fn wait_full_queue(driver: &RpcDriver) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let stats = driver
                .process
                .send_request("fixture_stats", json!({}), Some(Duration::from_secs(1)))
                .await
                .unwrap();
            if stats["active"].as_array().unwrap().len() == 64 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn full_queue_control(driver: Arc<RpcDriver>) -> (ServiceError, ServiceResult<Value>, Value) {
    let opened = driver
        .service_call(
            ServiceRpcMethod::SessionOpen,
            &params(&driver.manifest.id),
            &context("session", "open"),
            json!({"database":"synthetic","mode":"read_only"}),
        )
        .await
        .unwrap();
    let mut tasks = Vec::new();
    let mut signals = Vec::new();
    for index in 0..64 {
        let mut context = context("conn", &format!("queued-{index}"));
        context.deadline_ms = 120000;
        signals.push(context.signal.clone());
        tasks.push(tokio::spawn({
            let driver = driver.clone();
            async move {
                driver
                    .service_call(
                        ServiceRpcMethod::ExactQuery,
                        &params(&driver.manifest.id),
                        &context,
                        query("wait_ack"),
                    )
                    .await
            }
        }));
    }
    wait_full_queue(&driver).await;
    let mut extra = context("conn", "extra");
    extra.deadline_ms = 25;
    let denied = driver
        .service_call(
            ServiceRpcMethod::ExactQuery,
            &params(&driver.manifest.id),
            &extra,
            query("SELECT 1"),
        )
        .await
        .unwrap_err();
    let minimal = ConnectionParams {
        driver: driver.manifest.id.clone(),
        connection_id: Some("conn".into()),
        ..ConnectionParams::default()
    };
    driver
        .service_call(
            ServiceRpcMethod::InvalidateAuth,
            &minimal,
            &context("conn", "invalidate"),
            json!({}),
        )
        .await
        .unwrap();
    let mut discard = context("session", "discard");
    discard.session_handle = Some(opened["session_handle"].as_str().unwrap().into());
    driver
        .service_call(
            ServiceRpcMethod::SessionDiscard,
            &params(&driver.manifest.id),
            &discard,
            json!({}),
        )
        .await
        .unwrap();
    driver.service_cancel("conn", "queued-0").await.unwrap();
    let first = tasks.remove(0).await.unwrap();
    for signal in signals.iter().skip(1) {
        signal.cancel();
    }
    for task in tasks {
        let _ = task.await.unwrap();
    }
    let stats = driver
        .service_call(
            ServiceRpcMethod::Test,
            &params(&driver.manifest.id),
            &context("conn", "stats"),
            json!({}),
        )
        .await
        .unwrap();
    driver.shutdown().await;
    (denied, first, stats)
}
#[tokio::test]
async fn 네이티브_호출_상한에서_추가_SQL은_송신하지_않고_취소_제어는_계속_동작한다() {
    // given
    let driver = Arc::new(child_driver("bounded-pending", "native", json!({})).await);
    driver.service_capabilities().await.unwrap();
    // when
    let (denied, cancelled, stats) = full_queue_control(driver).await;
    // then
    assert_eq!(denied.code, ErrorCode::DeadlineExceeded);
    assert_eq!(denied.outcome, Outcome::NotStarted);
    assert!(cancelled.is_ok());
    assert_eq!(stats["queries"], 64);
}
