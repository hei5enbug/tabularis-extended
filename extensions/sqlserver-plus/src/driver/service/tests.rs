use super::{exact, explain, pool, registry, types::DriverContext};
use crate::driver::azure_cli_auth::fake_cli::{HOST, OBJECT, TENANT, USER};
use crate::{
    connection::resolve_connection_params,
    models::{ConnectionParams, DatabaseSelection},
    settings::PluginSettings,
};
use serde_json::json;
fn entra() -> (ConnectionParams, DriverContext) {
    let params = ConnectionParams {
        host: Some(HOST.into()),
        username: Some(USER.into()),
        database: DatabaseSelection::Single("fixture".into()),
        auth_mode: Some("entra_user".into()),
        extra: [
            ("auth_source".into(), "azure_cli".into()),
            ("tenant_id".into(), TENANT.into()),
            ("entra_object_id".into(), OBJECT.into()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let context = serde_json::from_value(json!({"protocol_version":1,"connection_id":uuid::Uuid::new_v4().to_string(),"request_id":"request"})).unwrap();
    (params, context)
}
fn request(params: &ConnectionParams, auth: serde_json::Value) -> serde_json::Value {
    json!({"params":params,"driver_context":{"protocol_version":1,"connection_id":"fixture","request_id":"request","auth":auth},"input":{}})
}
#[test]
fn azure_sql은_database와_검증하는_tls를_강제한다() {
    // given
    let (params, _) = entra();
    // when
    let actual = resolve_connection_params(&params).unwrap();
    // then
    assert_eq!(actual.ssl_mode.as_deref(), Some("verify-full"));
    assert!(!actual.integrated_auth);
}
#[test]
fn azure_sql에서_미검증_tls_override를_거부한다() {
    // given
    let (mut params, _) = entra();
    params.ssl_mode = Some("require".into());
    // when
    let actual = resolve_connection_params(&params);
    // then
    assert!(actual.is_err());
}
#[test]
fn azure_hostname의_주변_공백도_tls_검사를_우회하지_못한다() {
    // given
    let (mut params, _) = entra();
    params.host = Some(" synthetic.database.windows.net \t".into());
    let mut insecure = params.clone();
    insecure.ssl_mode = Some("require".into());
    // when
    let actual = [params, insecure].map(|params| resolve_connection_params(&params));
    // then
    assert_eq!(
        actual[0].as_ref().unwrap().ssl_mode.as_deref(),
        Some("verify-full")
    );
    assert!(actual[1].is_err());
}
#[test]
fn azure_sql에서_명시하지_않은_database를_거부한다() {
    // given
    let (mut params, _) = entra();
    params.database = DatabaseSelection::default();
    // when
    let actual = resolve_connection_params(&params);
    // then
    assert!(actual.is_err());
}
#[test]
fn entra와_다른_인증_정보가_충돌하면_거부한다() {
    // given
    let (mut params, _) = entra();
    params.password = Some("sql-secret".into());
    // when
    let actual = resolve_connection_params(&params);
    // then
    assert!(actual.is_err());
}
#[test]
fn entra_user는_빈_token_자리표시자와_검증하는_tls로_설정한다() {
    // given
    let (params, context) = entra();
    let params = resolve_connection_params(&params).unwrap();

    // when
    let actual = pool::config(&params, &PluginSettings::default(), &context)
        .unwrap()
        .to_client_context();

    // then
    assert_eq!(actual.access_token.as_deref(), Some(""));
    assert!(!actual.encryption_options.trust_server_certificate);
    assert!(actual.password.is_empty());
    assert!(actual.user_name.is_empty());
}
#[test]
fn host가_보낸_entra_token은_사용하지_않고_형식화된_오류로_거부한다() {
    // given
    let (params, _) = entra();
    let auth = json!({"kind":"entra_token","access_token":"synthetic-host-token-canary","expires_at_ms":4_102_444_800_000_u64,"identity":"principal","tenant_id":TENANT,"client_id":"client","principal_id":OBJECT,"scope":"https://database.windows.net/.default"});
    let variants = [
        request(&params, auth.clone()),
        request(&params, json!({"kind":"entra_user"})),
    ];

    // when
    let parsed = variants.clone().map(|value| DriverContext::parse(&value));
    let control = variants.map(|value| DriverContext::parse_control(&value));

    // then
    for error in parsed
        .into_iter()
        .map(|result| result.err().unwrap())
        .chain(control.into_iter().map(|result| result.err().unwrap()))
    {
        assert_eq!(error.code, "INVALID_ARGUMENT");
        assert!(!error.message.contains("canary"));
        assert!(!format!("{:?}", error).contains("canary"));
    }
}
#[test]
fn entra_user는_driver_자체_인증이라_transient_auth_없이만_통과한다() {
    // given
    let (params, context) = entra();
    let with_password: DriverContext = serde_json::from_value(json!({"protocol_version":1,"connection_id":"fixture","request_id":"request","auth":{"kind":"db_password","password":"synthetic-secret","identity":"principal"}})).unwrap();
    let mut other = params.clone();
    other.auth_mode = Some("sql_password".into());
    other.username = Some("sql-user".into());

    // when
    let accepted = context.validate_auth(&params);
    let carrying_password = with_password.validate_auth(&params);
    let sql_without_password = context.validate_auth(&other);

    // then
    assert!(accepted.is_ok());
    assert_eq!(carrying_password.unwrap_err().code, "INVALID_ARGUMENT");
    assert_eq!(sql_without_password.unwrap_err().code, "AUTH_REQUIRED");
}
#[test]
fn 불완전하거나_안전하지_않은_entra_설정은_인증_검증에서_거부한다() {
    // given
    let (params, context) = entra();
    let mut missing_object = params.clone();
    missing_object.extra.remove("entra_object_id");
    let mut private_host = params.clone();
    private_host.host = Some("sql.example.com".into());
    let mut password = params.clone();
    password.password = Some("synthetic-secret".into());

    // when
    let actual = [missing_object, private_host, password].map(|p| context.validate_auth(&p));

    // then
    assert!(actual
        .iter()
        .all(|result| result.as_ref().unwrap_err().code == "INVALID_ARGUMENT"));
}
#[test]
fn service_capabilities는_host_token을_광고하지_않는다() {
    // given
    let capabilities = crate::handlers::service::capabilities();

    // when
    let actual = &capabilities["entra_token"];

    // then
    assert_eq!(actual, &json!(false));
    assert_eq!(capabilities["service_protocol"], json!(1));
}
#[test]
fn service_capabilities는_readonly_guarantee를_광고하지_않는다() {
    // given
    let capabilities = crate::handlers::service::capabilities();

    // when
    let actual = capabilities.get("readonly_guarantee");

    // then
    assert_eq!(actual, None);
    assert_eq!(capabilities["catalog"], json!(true));
}
#[test]
fn 다른_연결의_같은_request_id는_별도로_취소한다() {
    let _gate = registry::SHUTDOWN_GATE.read().unwrap();
    // given
    let (_, first) = entra();
    let (_, mut second) = entra();
    second.request_id = first.request_id.clone();
    let a = registry::Scope::reserve(&first).unwrap();
    let b = registry::Scope::reserve(&second).unwrap();
    // when
    let actual = registry::cancel(&first.connection_id, &first.request_id);
    // then
    assert!(actual);
    assert!(a.cancelled());
    assert!(!b.cancelled());
}
#[test]
fn named_parameter의_중복과_잘못된_이름을_거부한다() {
    // given
    let parameters = [
        exact::Parameter {
            name: "@value".into(),
            value: json!(1),
        },
        exact::Parameter {
            name: "@Value".into(),
            value: json!(2),
        },
    ];
    // when
    let actual = exact::bind(&parameters);
    // then
    assert_eq!(actual.err().unwrap().code, "INVALID_ARGUMENT");
}
#[test]
fn 정밀도를_잃을_parameter와_객체는_지원한다고_하지_않는다() {
    // given
    let values = [json!(9_007_199_254_740_992_u64), json!({"nested":true})];
    // when
    let actual = values.map(|value| exact::value_type(&value));
    // then
    assert!(actual
        .iter()
        .all(|result| result.as_ref().err().unwrap().code == "UNSUPPORTED_TYPE"));
}
#[test]
fn 앞의_중첩_주석으로_transaction_control_검사를_우회하지_못한다() {
    // given
    let text = "-- leading\n/* outer /* nested */ done */ COMMIT";
    // when
    let actual = exact::validate_text(text);
    // then
    assert_eq!(actual.unwrap_err().code, "UNSUPPORTED_OPERATION");
}
#[test]
fn 비어_있지_않은_schema는_실행_전에_지원하지_않는_작업_오류로_거부한다() {
    // given
    let input: exact::ExactInput =
        serde_json::from_value(json!({"text":"SELECT 1","schema":"sales"})).unwrap();
    // when
    let actual = exact::validate(&input);
    // then
    let error = actual.unwrap_err();
    assert_eq!(error.code, "UNSUPPORTED_OPERATION");
    assert_eq!(
        error.message,
        "SQL Server has no per-session default schema; names must be schema-qualified"
    );
}
#[test]
fn schema가_없거나_비어_있으면_기존처럼_통과한다() {
    // given
    let absent: exact::ExactInput = serde_json::from_value(json!({"text":"SELECT 1"})).unwrap();
    let empty: exact::ExactInput =
        serde_json::from_value(json!({"text":"SELECT 1","schema":""})).unwrap();
    // when
    let actual = [exact::validate(&absent), exact::validate(&empty)];
    // then
    assert!(actual.iter().all(|result| result.is_ok()));
}
#[test]
fn 정확_쿼리_입력은_알_수_없는_필드를_계속_거부한다() {
    // given
    let value = json!({"text":"SELECT 1","schema":"sales","search_path":"x"});
    // when
    let actual = serde_json::from_value::<exact::ExactInput>(value);
    // then
    assert!(actual.is_err());
}
#[test]
fn 예약한_physical_session이_64개면_다음_예약을_거부한다() {
    // given
    let _permits = (0..64)
        .map(|_| super::sessions::reserve().unwrap())
        .collect::<Vec<_>>();
    // when
    let actual = super::sessions::reserve();
    // then
    assert_eq!(actual.unwrap_err().code, "RESOURCE_LIMIT");
}
#[test]
fn non_null_sdk_값의_decode_mismatch는_null로_변환하지_않는다() {
    // given
    let row = mssql_tiberius_bridge::Row::from_tds(
        &[],
        vec![mssql_tds::datatypes::column_values::ColumnValues::Int(42)],
    );
    // when
    let actual = exact::decode(&row, 0, mssql_tiberius_bridge::ColumnType::NVarchar);
    // then
    assert_eq!(actual.unwrap_err().code, "UNSUPPORTED_TYPE");
}
#[test]
fn invalidate는_최소_설정으로_문맥과_연결_바인딩만_검사한다() {
    // given
    let minimal = json!({"params":{"driver":"sqlserver","connection_id":"target"},"driver_context":{"protocol_version":1,"connection_id":"target","request_id":"logout","read_only":true},"input":{}});
    let mut mismatch = minimal.clone();
    mismatch["params"]["connection_id"] = json!("other");
    let mut invalid = minimal.clone();
    invalid["driver_context"]["permissions"] = json!(["write"]);
    let mut nonobject = minimal.clone();
    nonobject["params"] = json!("not-an-object");
    // when
    let actual =
        [minimal, mismatch, invalid, nonobject].map(|params| DriverContext::parse_control(&params));
    // then
    assert_eq!(actual[0].as_ref().unwrap().connection_id, "target");
    assert!(actual[0].as_ref().unwrap().auth.is_none());
    assert!(actual[1..]
        .iter()
        .all(|result| result.as_ref().err().unwrap().code == "INVALID_ARGUMENT"));
}

fn plan_context(read_only: bool, session: Option<&str>) -> DriverContext {
    serde_json::from_value(json!({
        "protocol_version":1,
        "connection_id":uuid::Uuid::new_v4().to_string(),
        "request_id":"plan",
        "read_only":read_only,
        "session_handle":session,
        "auth":{"kind":"db_password","password":"synthetic","identity":"opaque"},
    }))
    .unwrap()
}
fn plan_connection() -> ConnectionParams {
    ConnectionParams {
        host: Some("localhost".into()),
        username: Some("user".into()),
        database: DatabaseSelection::Single("fixture".into()),
        ..Default::default()
    }
}
async fn plan_error(
    sql: &str,
    context: &DriverContext,
    connection: &ConnectionParams,
) -> &'static str {
    let scope = registry::Scope::reserve(context).unwrap();
    let input = explain::Input {
        sql: sql.into(),
        schema: None,
    };
    explain::estimated_plan(input, context, connection, &scope)
        .await
        .unwrap_err()
        .code
}
#[test]
fn 계획_입력은_sql과_스키마만_받는다() {
    // given
    let inputs = [
        json!({"sql":"SELECT 1"}),
        json!({"sql":"SELECT 1","schema":"dbo"}),
        json!({"sql":"SELECT 1","analyze":true}),
        json!({"sql":"SELECT 1","database":"other"}),
        json!({"schema":"dbo"}),
    ];

    // when
    let actual = inputs.map(|input| serde_json::from_value::<explain::Input>(input).is_ok());

    // then
    assert_eq!(actual, [true, true, false, false, false]);
}
#[test]
fn service_capabilities는_계획_기능과_메서드를_광고한다() {
    // given
    let capabilities = crate::handlers::service::capabilities();

    // when
    let actual = (
        capabilities["explain"].clone(),
        crate::handlers::service::is_method("service_explain"),
    );

    // then
    assert_eq!(actual, (json!(true), true));
}
#[tokio::test]
async fn 계획_요청은_세션_제어_문장과_쓰기_context와_세션을_연결_전에_거부한다() {
    let _gate = registry::SHUTDOWN_GATE.read().unwrap();
    // given
    let connection = plan_connection();
    let read = plan_context(true, None);

    // when
    let control = plan_error("SET NOCOUNT ON", &read, &connection).await;
    let transaction = plan_error("BEGIN TRANSACTION", &read, &connection).await;
    let empty = plan_error("  ", &read, &connection).await;
    let write = plan_error("SELECT 1", &plan_context(false, None), &connection).await;
    let session = plan_error("SELECT 1", &plan_context(true, Some("handle")), &connection).await;

    // then
    assert_eq!(control, "UNSUPPORTED_OPERATION");
    assert_eq!(transaction, "UNSUPPORTED_OPERATION");
    assert_eq!(empty, "INVALID_ARGUMENT");
    assert_eq!(write, "WRITE_NOT_ALLOWED");
    assert_eq!(session, "INVALID_ARGUMENT");
}
#[tokio::test]
async fn 계획_요청은_일시_인증이_없으면_연결을_시도하기_전에_거부한다() {
    let _gate = registry::SHUTDOWN_GATE.read().unwrap();
    // given
    let connection = plan_connection();
    let mut context = plan_context(true, None);
    context.auth = None;

    // when
    let actual = plan_error("SELECT 1", &context, &connection).await;

    // then
    assert_eq!(actual, "AUTH_REQUIRED");
}

struct PlanDouble {
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    detached: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    reply: Vec<String>,
    hang_on_statement: bool,
}
impl explain::PlanConnection for PlanDouble {
    async fn batch(&mut self, sql: &str) -> Result<Vec<String>, super::ServiceError> {
        self.log.lock().unwrap().push(sql.to_owned());
        if sql == explain::SHOWPLAN_ON || sql == explain::SHOWPLAN_OFF {
            return Ok(Vec::new());
        }
        if self.hang_on_statement {
            std::future::pending::<()>().await;
        }
        Ok(self.reply.clone())
    }
    fn detach(self) {
        self.detached
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
type PlanLog = (
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
);
fn plan_double(reply: Vec<String>, hang_on_statement: bool) -> (PlanDouble, PlanLog) {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let detached = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    (
        PlanDouble {
            log: log.clone(),
            detached: detached.clone(),
            reply,
            hang_on_statement,
        },
        (log, detached),
    )
}
#[tokio::test]
async fn 계획은_켜기_문장_끄기를_각각_별도_배치로_보내고_연결을_분리한다() {
    let _gate = registry::SHUTDOWN_GATE.read().unwrap();
    // given
    let context = plan_context(true, None);
    let scope = registry::Scope::reserve(&context).unwrap();
    let (connection, (log, detached)) = plan_double(vec!["<ShowPlanXML/>".into()], false);

    // when
    let actual = explain::run_plan(connection, "SELECT * FROM orders", &scope)
        .await
        .unwrap();

    // then
    assert_eq!(
        *log.lock().unwrap(),
        [
            "SET SHOWPLAN_XML ON",
            "SELECT * FROM orders",
            "SET SHOWPLAN_XML OFF"
        ]
    );
    assert_eq!(
        actual,
        json!({"engine":"sqlserver","format":"sqlserver-showplan-xml","payload":"<ShowPlanXML/>"})
    );
    assert_eq!(detached.load(std::sync::atomic::Ordering::SeqCst), 1);
}
#[tokio::test]
async fn 계획은_통계_xml이나_분석을_쓰지_않는다() {
    let _gate = registry::SHUTDOWN_GATE.read().unwrap();
    // given
    let context = plan_context(true, None);
    let scope = registry::Scope::reserve(&context).unwrap();
    let (connection, (log, _)) = plan_double(vec!["<ShowPlanXML/>".into()], false);

    // when
    explain::run_plan(connection, "SELECT 1", &scope)
        .await
        .unwrap();

    // then
    let sent = log.lock().unwrap().join("\n").to_ascii_uppercase();
    assert!(!sent.contains("STATISTICS") && !sent.contains("ANALYZE"));
    assert!(!explain::SHOWPLAN_ON
        .to_ascii_uppercase()
        .contains("STATISTICS"));
}
#[tokio::test]
async fn 계획_문서가_없거나_둘이면_계획_실패로_답하고_연결을_분리한다() {
    let _gate = registry::SHUTDOWN_GATE.read().unwrap();
    // given
    let cases = [
        vec![],
        vec!["<ShowPlanXML/>".to_owned(), "<ShowPlanXML/>".to_owned()],
    ];

    // when
    let mut actual = Vec::new();
    for reply in cases {
        let context = plan_context(true, None);
        let scope = registry::Scope::reserve(&context).unwrap();
        let (connection, (_, detached)) = plan_double(reply, false);
        let error = explain::run_plan(connection, "SELECT 1", &scope)
            .await
            .unwrap_err();
        actual.push((
            error.code,
            detached.load(std::sync::atomic::Ordering::SeqCst),
        ));
    }

    // then
    assert_eq!(actual, [("DRIVER_EXITED", 1), ("DRIVER_EXITED", 1)]);
}
#[tokio::test]
async fn 취소되면_끄기_배치를_보내지_않고_연결을_분리한다() {
    let _gate = registry::SHUTDOWN_GATE.read().unwrap();
    // given
    let context = plan_context(true, None);
    let scope = registry::Scope::reserve(&context).unwrap();
    let (connection, (log, detached)) = plan_double(Vec::new(), true);
    let (connection_id, request_id) = (context.connection_id.clone(), context.request_id.clone());

    // when
    let (actual, _) = tokio::join!(explain::run_plan(connection, "SELECT 1", &scope), async {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        registry::cancel(&connection_id, &request_id);
    });

    // then
    assert_eq!(actual.unwrap_err().code, "CANCELLED");
    assert_eq!(*log.lock().unwrap(), ["SET SHOWPLAN_XML ON", "SELECT 1"]);
    assert_eq!(detached.load(std::sync::atomic::Ordering::SeqCst), 1);
}
#[tokio::test]
async fn 기한이_지나면_연결을_분리하고_기한초과로_답한다() {
    let _gate = registry::SHUTDOWN_GATE.read().unwrap();
    // given
    let mut context = plan_context(true, None);
    context.deadline_ms = 30;
    let scope = registry::Scope::reserve(&context).unwrap();
    let (connection, (_, detached)) = plan_double(Vec::new(), true);

    // when
    let actual = explain::run_plan(connection, "SELECT 1", &scope).await;

    // then
    assert_eq!(actual.unwrap_err().code, "DEADLINE_EXCEEDED");
    assert_eq!(detached.load(std::sync::atomic::Ordering::SeqCst), 1);
}
