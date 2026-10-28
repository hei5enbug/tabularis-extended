use crate::{
    client,
    handlers::{
        connection,
        spatial::{
            service,
            types::{DriverContext, RowIdentity},
        },
    },
    models::ConnectionParams,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// Tests run in parallel and the cancel registry rejects a request ID that is
/// already active on the connection, so every request gets its own ID.
fn params(input: Value, read_only: bool) -> Value {
    let request_id = format!(
        "synthetic-request-{}",
        NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
    );
    json!({"params":{"database":"fixture","username":"synthetic"},"driver_context":{"protocol_version":1,"connection_id":"synthetic-connection","request_id":request_id,"read_only":read_only},"input":input})
}

#[tokio::test]
async fn 명시한_서비스_프로토콜에서만_고정_capabilities를_공개한다() {
    // given
    let _settings = crate::settings::test_support::lock_and_reset().await;
    let input = json!({"settings":{"poolMaxSize":"7"},"service_protocol":1});
    // when
    let result = connection::initialize(json!(1), &input).await;
    // then
    assert_eq!(
        result["result"]["service_capabilities"],
        json!({"service_protocol":1,"exact_query":true,"spatial_v1":true,"sessions":true,"cancel_v1":true,"catalog":true,"rows":true,"explain":true,"readonly_guarantee":"read_only_transaction","exact_affected_rows":true})
    );
    assert_eq!(crate::settings::pool_max_size(), 7);
}

#[tokio::test]
async fn 기존_initialize는_설정을_유지하고_null을_반환한다() {
    // given
    let _settings = crate::settings::test_support::lock_and_reset().await;
    let input = json!({"settings":{"poolMaxSize":4}});
    // when
    let result = connection::initialize(json!(2), &input).await;
    // then
    assert_eq!(result["result"], Value::Null);
    assert_eq!(crate::settings::pool_max_size(), 4);
}

#[tokio::test]
async fn 지원하지_않는_initialize_프로토콜은_typed_error로_거부한다() {
    // given
    let _settings = crate::settings::test_support::lock_and_reset().await;
    let input = json!({"settings":{},"service_protocol":2});
    // when
    let result = connection::initialize(json!(3), &input).await;
    // then
    assert_eq!(result["error"]["data"]["code"], "PROTOCOL_MISMATCH");
}

#[test]
fn 서비스_dsn의_주소와_비밀번호를_정상화하고_tls_설정을_유지한다() {
    // given
    let input = ConnectionParams::from_value(
        &json!({"connection_string":"postgresql://synthetic:dsn-canary@localhost:5433/fixture","ssl_mode":"verify-full","ssl_ca":"synthetic-ca","ssl_cert":"synthetic-cert","ssl_key":"synthetic-key"}),
    );
    // when
    let actual = client::normalize_service_params(&input).unwrap();
    // then
    assert_eq!(actual.host.as_deref(), Some("localhost"));
    assert_eq!(actual.port, Some(5433));
    assert_eq!(actual.database.as_deref(), Some("fixture"));
    assert_eq!(actual.username.as_deref(), Some("synthetic"));
    assert_eq!(actual.password.as_deref(), Some("dsn-canary"));
    assert!(actual.connection_string.is_none());
    assert_eq!(actual.ssl_mode.as_deref(), Some("verify-full"));
    assert_eq!(actual.ssl_ca.as_deref(), Some("synthetic-ca"));
    assert_eq!(actual.ssl_cert.as_deref(), Some("synthetic-cert"));
    assert_eq!(actual.ssl_key.as_deref(), Some("synthetic-key"));
}

fn normalized_auth(
    input: &Value,
) -> Result<ConnectionParams, crate::handlers::spatial::SpatialError> {
    let (_, parsed) = DriverContext::parse(input, false)?;
    client::normalize_service_params(&parsed)
}

#[test]
fn transient_비밀번호는_dsn의_이전_비밀번호를_물리_연결_설정에서_대체한다() {
    // given
    let mut input = params(json!({}), true);
    input["params"]["connection_string"] =
        json!("postgresql://synthetic:old-canary@localhost/fixture");
    input["driver_context"]["auth"] =
        json!({"kind":"db_password","password":"new-canary","identity":"opaque-revision-2"});
    // when
    let actual = normalized_auth(&input).unwrap();
    // then
    assert_eq!(actual.password.as_deref(), Some("new-canary"));
    assert!(actual.connection_string.is_none());
    assert_eq!(input["driver_context"]["auth"]["password"], "new-canary");
}

#[test]
fn 잘못된_인증_context는_비밀번호를_오류에_포함하지_않는다() {
    // given
    let mut input = params(json!({}), true);
    input["driver_context"]["auth"] =
        json!({"kind":"db_password","password":"error-canary","identity":"","extra":"forbidden"});
    // when
    let actual = DriverContext::parse(&input, false).err().unwrap();
    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("error-canary"));
}

#[test]
fn 다른_인증_kind는_명시적으로_지원불가를_반환한다() {
    // given
    let mut input = params(json!({}), true);
    input["driver_context"]["auth"] = json!({"kind":"entra_token","access_token":"token-canary"});
    // when
    let actual = DriverContext::parse(&input, false).err().unwrap();
    // then
    assert_eq!(actual.code, "UNSUPPORTED_OPERATION");
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("token-canary"));
}

#[tokio::test]
async fn 서비스_startup_script는_연결을_만들기_전에_거부한다() {
    // given
    let input = ConnectionParams::from_value(
        &json!({"host":"unreachable.invalid","startup_script":"CREATE TABLE side_effect(i integer)"}),
    );
    // when
    let actual = client::build_service_pool(&input).await.err().unwrap();
    // then
    assert_eq!(actual.code, "UNSUPPORTED_OPERATION");
    assert_eq!(
        input.startup_script.as_deref(),
        Some("CREATE TABLE side_effect(i integer)")
    );
}

#[test]
fn 잘못된_dsn의_오류에_원문_uri를_포함하지_않는다() {
    // given
    let input = ConnectionParams::from_value(
        &json!({"connection_string":"postgresql://user:dsn-error-canary@host:invalid/db"}),
    );
    // when
    let actual = client::normalize_service_params(&input).unwrap_err();
    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("dsn-error-canary"));
}

#[test]
fn 복합_identity는_실제_pk의_전체_순서와_null없음을_요구한다() {
    // given
    let key = vec!["tenant".to_string(), "id".to_string()];
    let identity = RowIdentity {
        columns: vec!["id".into(), "tenant".into()],
        values: vec![json!(1), json!(2)],
    };
    // when
    let actual = service::validate_identity(&identity, &key).unwrap_err();
    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
}

#[test]
fn pk없는_테이블의_수정과_삭제는_지원불가를_반환한다() {
    // given
    let identity = RowIdentity {
        columns: vec![],
        values: vec![],
    };
    // when
    let actual = service::validate_identity(&identity, &[]).unwrap_err();
    // then
    assert_eq!(actual.code, "UNSUPPORTED_OPERATION");
}

#[test]
fn 고정_row는_읽기_전용_context에서_실행하지_않는다() {
    // given
    let input = params(
        json!({"operation":"row.insert","table":{"database":"fixture","schema":"public","table":"items"},"values":{"name":"x"}}),
        true,
    );
    let (context, connection) = DriverContext::parse(&input, false).unwrap();
    // when
    let actual = service::validate(&input, "service_row", &context, &connection).unwrap_err();
    // then
    assert_eq!(actual.code, "WRITE_NOT_ALLOWED");
}

#[test]
fn 고정_catalog는_다른_database로_몰래_전환하지_않는다() {
    // given
    let input = params(
        json!({"operation":"catalog.schemas","database":"different"}),
        true,
    );
    let (context, connection) = DriverContext::parse(&input, true).unwrap();
    // when
    let actual = service::validate(&input, "service_catalog", &context, &connection).unwrap_err();
    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
}

#[test]
fn 고정_catalog_describe는_종류를_선택적으로_받는다() {
    // given
    let table = json!({"database":"fixture","schema":"public","table":"items"});
    let inputs = [
        json!({"operation":"catalog.describe","table":table}),
        json!({"operation":"catalog.describe","table":table,"kind":"foreign_table"}),
        json!({"operation":"catalog.describe","table":table,"kind":"operator_family"}),
    ];

    // when
    let actual: Vec<bool> = inputs
        .iter()
        .map(|input| {
            crate::handlers::spatial::types::input::<service::CatalogInput>(
                &json!({ "input": input }),
            )
            .is_ok()
        })
        .collect();

    // then
    assert_eq!(actual, [true, true, true]);
}

#[test]
fn 고정_catalog_describe는_알_수_없는_종류와_추가_필드를_거부한다() {
    // given
    let table = json!({"database":"fixture","schema":"public","table":"items"});
    let inputs = [
        json!({"operation":"catalog.describe","table":table,"kind":"synonym"}),
        json!({"operation":"catalog.describe","table":table,"kind":"index"}),
        json!({"operation":"catalog.describe","table":table,"kind":"table","text":"DROP TABLE items"}),
    ];

    // when
    let actual: Vec<bool> = inputs
        .iter()
        .map(|input| {
            crate::handlers::spatial::types::input::<service::CatalogInput>(
                &json!({ "input": input }),
            )
            .is_err()
        })
        .collect();

    // then
    assert_eq!(actual, [true, true, true]);
}

#[test]
fn 고정_row는_사용자_sql_추가_필드를_받지_않는다() {
    // given
    let input = params(
        json!({"operation":"row.insert","table":{"database":"fixture","schema":"public","table":"items"},"values":{"name":"x"},"text":"DROP TABLE items"}),
        false,
    );
    // when
    let actual = types_input_row(&input).unwrap_err();
    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
}
fn types_input_row(input: &Value) -> Result<(), crate::handlers::spatial::SpatialError> {
    super::handlers::spatial::types::input::<service::RowInput>(input).map(|_| ())
}

#[tokio::test]
async fn 계획_요청은_쓰기_context면_연결하기_전에_거부한다() {
    // given
    let input = params(json!({"sql":"SELECT 1"}), false);
    // when
    let actual = crate::handlers::spatial::dispatch(json!(1), &input, "service_explain").await;
    // then
    assert_eq!(actual["error"]["data"]["code"], "WRITE_NOT_ALLOWED");
}

#[tokio::test]
async fn 계획_요청은_분석_같은_추가_필드를_연결하기_전에_거부한다() {
    // given
    let input = params(json!({"sql":"SELECT 1","analyze":true}), true);
    // when
    let actual = crate::handlers::spatial::dispatch(json!(2), &input, "service_explain").await;
    // then
    assert_eq!(actual["error"]["data"]["code"], "INVALID_ARGUMENT");
}

#[tokio::test]
async fn 계획_요청은_트랜잭션_제어_문장을_연결하기_전에_거부한다() {
    // given
    let input = params(json!({"sql":"COMMIT"}), true);
    // when
    let actual = crate::handlers::spatial::dispatch(json!(3), &input, "service_explain").await;
    // then
    assert_eq!(actual["error"]["data"]["code"], "UNSUPPORTED_OPERATION");
}
