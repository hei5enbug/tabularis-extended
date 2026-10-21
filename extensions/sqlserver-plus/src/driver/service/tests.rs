use super::{
    exact, pool, registry,
    types::{self, DriverContext},
};
use crate::{
    connection::resolve_connection_params,
    models::{ConnectionParams, DatabaseSelection},
    settings::PluginSettings,
};
use serde_json::json;
fn entra() -> (ConnectionParams, DriverContext) {
    let params = ConnectionParams {
        host: Some("synthetic.database.windows.net".into()),
        database: DatabaseSelection::Single("fixture".into()),
        auth_mode: Some("entra_user".into()),
        extra: [
            ("tenant_id".into(), "tenant".into()),
            ("client_id".into(), "client".into()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let context = serde_json::from_value(json!({"protocol_version":1,"connection_id":uuid::Uuid::new_v4().to_string(),"request_id":"request","auth":{"kind":"entra_token","access_token":"synthetic-token","expires_at_ms":types::unix_ms()+1_000_000,"identity":"principal","tenant_id":"tenant","client_id":"client","principal_id":"principal","scope":types::SQL_SCOPE}})).unwrap();
    (params, context)
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
    params.username = Some("sql-user".into());
    // when
    let actual = resolve_connection_params(&params);
    // then
    assert!(actual.is_err());
}
#[test]
fn entra_token은_공개_aad_token과_검증하는_tls로_전달한다() {
    // given
    let (params, context) = entra();
    let params = resolve_connection_params(&params).unwrap();
    // when
    let actual = pool::config(&params, &PluginSettings::default(), &context, 1000)
        .unwrap()
        .to_client_context();
    // then
    assert_eq!(actual.access_token.as_deref(), Some("synthetic-token"));
    assert!(!actual.encryption_options.trust_server_certificate);
    assert!(actual.password.is_empty());
}
#[test]
fn deadline과_여유_시간보다_짧은_token을_거부한다() {
    // given
    let (params, mut context) = entra();
    if let Some(types::TransientAuth::EntraToken { expires_at_ms, .. }) = &mut context.auth {
        *expires_at_ms = types::unix_ms() + 300_000;
    }
    // when
    let actual = context.validate_auth(&params, 1000);
    // then
    assert_eq!(actual.unwrap_err().code, "AUTH_EXPIRED");
}
#[test]
fn 다른_연결의_같은_request_id는_별도로_취소한다() {
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
