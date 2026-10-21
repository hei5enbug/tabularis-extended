#[path = "support/azure_sql_harness/mod.rs"]
mod harness;

use harness::{
    config::{Config, Flags},
    credentials::{Credentials, SecretReader},
    fixture,
    report::Report,
    rpc::{AzureEnv, Response, Rpc},
    Error, Result,
};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::OsString,
    rc::Rc,
};

const _: fn() -> Report = harness::run::run_live_from_stdin;
const TENANT: &str = "11111111-1111-4111-8111-111111111111";
const OBJECT: &str = "22222222-2222-4222-8222-222222222222";
const USERNAME: &str = "synthetic-user@example.test";
const CANARY: &str = "synthetic-secret-DO-NOT-REPORT";
const SYNTHETIC_HOST: &str = "synthetic-server.database.windows.net";
fn principal_value(mode: &str) -> Value {
    if mode == "sql_password" {
        json!({"auth_mode":mode,"username":USERNAME})
    } else {
        json!({"auth_mode":mode,"username":USERNAME,"tenant_id":TENANT,"entra_object_id":OBJECT})
    }
}
fn config_value(mode: &str) -> Value {
    json!({"version":2,"nonproduction_resource":true,"host":SYNTHETIC_HOST,"database":"synthetic_db","principal":principal_value(mode)})
}
fn config(mode: &str) -> Config {
    Config::parse(&serde_json::to_vec(&config_value(mode)).unwrap()).unwrap()
}
fn secrets(mode: &str) -> Value {
    if mode == "sql_password" {
        json!({"principal":{"kind":"db_password","password":CANARY,"identity":"principal-revision"}})
    } else {
        json!({})
    }
}
struct Reader {
    bytes: Vec<u8>,
    reads: Rc<Cell<usize>>,
}
impl Reader {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            reads: Rc::new(Cell::new(0)),
        }
    }
}
impl SecretReader for Reader {
    fn read_bounded(&mut self, cap: usize) -> Result<Vec<u8>> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.bytes.iter().take(cap + 1).copied().collect())
    }
}
fn reader(mode: &str) -> Reader {
    Reader::new(serde_json::to_vec(&secrets(mode)).unwrap())
}
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    HostTokenCapability,
    ChangingSpid,
    CancelUnknown,
    ShutdownFailure,
    Timeout,
}
struct State {
    mode: Mode,
    requests: Vec<Value>,
    responses: HashMap<String, Response>,
    cancelled: bool,
    long: Option<(String, String)>,
    spid_reads: usize,
    shutdowns: usize,
}
impl State {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            requests: Vec::new(),
            responses: HashMap::new(),
            cancelled: false,
            long: None,
            spid_reads: 0,
            shutdowns: 0,
        }
    }
}
struct Fake {
    state: Rc<RefCell<State>>,
}
fn ok(result: Value) -> Response {
    Response {
        result,
        error: None,
    }
}
fn failed(code: &'static str) -> Response {
    Response {
        result: Value::Null,
        error: Some(code),
    }
}
fn rows(columns: Value, rows: Value) -> Value {
    json!({"columns":columns,"rows":rows,"truncated":false,"affected_rows":null,"warnings":["affected_rows_unavailable"],"column_metadata":[{"column_index":0},{"column_index":1}]})
}
impl Fake {
    fn respond(&self, request: &Value) -> Result<Response> {
        let mut state = self.state.borrow_mut();
        let method = request["method"].as_str().unwrap_or("");
        let input = &request["params"]["input"];
        let context = &request["params"]["driver_context"];
        let parameter = |name: &str| {
            input["parameters"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["name"] == name))
                .map(|item| item["value"].clone())
                .unwrap_or(Value::Null)
        };
        match method {
            "initialize" => Ok(ok(
                json!({"service_capabilities":{"service_protocol":1,"exact_query":true,"sessions":true,"cancel":true,"catalog":true,"rows":true,"entra_token":state.mode==Mode::HostTokenCapability,"exact_affected_rows":false}}),
            )),
            "service_test" => Ok(ok(json!({"success":true}))),
            "service_catalog" => match input["operation"].as_str() {
                Some("catalog.databases") => Ok(ok(json!({"databases":["synthetic_db"]}))),
                Some("catalog.schemas") => Ok(ok(json!({"schemas":["dbo"]}))),
                _ => Err(Error("SYNTHETIC_UNEXPECTED_OPERATION")),
            },
            "execute_query_exact" => {
                let text = input["text"].as_str().unwrap_or("");
                if text.starts_with("WAITFOR") {
                    state.long = Some((
                        context["connection_id"].as_str().unwrap_or("").into(),
                        context["request_id"].as_str().unwrap_or("").into(),
                    ));
                    return Ok(failed(if state.mode == Mode::CancelUnknown {
                        "OUTCOME_UNKNOWN"
                    } else {
                        "CANCELLED"
                    }));
                }
                if text.contains("SERVERPROPERTY") {
                    return Ok(ok(rows(
                        json!(["edition", "database"]),
                        json!([[5, "synthetic_db"]]),
                    )));
                }
                if text == "SELECT @@SPID" {
                    state.spid_reads += 1;
                    let spid = if state.mode == Mode::ChangingSpid {
                        76 + state.spid_reads
                    } else {
                        77
                    };
                    return Ok(ok(rows(json!(["spid"]), json!([[spid]]))));
                }
                if text.contains("AS duplicate") {
                    let mut value = rows(
                        json!(["duplicate", "duplicate"]),
                        json!([[parameter("@run"), parameter("@run")]]),
                    );
                    value["additional_results"] = json!([rows(
                        json!(["measure", "observed_at", "nullable"]),
                        json!([[fixture::DECIMAL, fixture::OBSERVED_AT, null]])
                    )]);
                    return Ok(ok(value));
                }
                if text.starts_with("SELECT CAST(@decimal") {
                    return Ok(ok(rows(json!(["measure"]), json!([[fixture::DECIMAL]]))));
                }
                Err(Error("SYNTHETIC_UNEXPECTED_SQL"))
            }
            "service_session_open" => {
                if state.mode == Mode::Timeout {
                    return Err(Error("RPC_TIMEOUT"));
                }
                Ok(ok(
                    json!({"session_handle":"synthetic-session","in_transaction":true,"rollback_confirmed":false}),
                ))
            }
            "service_session_finish" => Ok(ok(
                json!({"session_handle":"synthetic-session","in_transaction":false,"rollback_confirmed":true}),
            )),
            "service_invalidate_auth" => Ok(ok(json!({"invalidated":true}))),
            "cancel_request" => {
                let targeted = state.long.as_ref().is_some_and(|(conn, id)| {
                    request["params"]["connection_id"] == *conn
                        && request["params"]["request_id"] == *id
                });
                state.cancelled = targeted;
                Ok(ok(json!({"cancellation_requested":targeted})))
            }
            _ => Err(Error("SYNTHETIC_UNEXPECTED_OPERATION")),
        }
    }
}
impl Rpc for Fake {
    fn send(&mut self, request: Value) -> Result<()> {
        let id = request["id"]
            .as_str()
            .ok_or(Error("SYNTHETIC_INVALID_ID"))?
            .to_owned();
        self.state.borrow_mut().requests.push(request.clone());
        let response = self.respond(&request)?;
        self.state.borrow_mut().responses.insert(id, response);
        Ok(())
    }
    fn receive(&mut self, id: &str) -> Result<Response> {
        self.state
            .borrow_mut()
            .responses
            .remove(id)
            .ok_or(Error("SYNTHETIC_MISSING_RESPONSE"))
    }
    fn shutdown(&mut self) -> Result<()> {
        let mut state = self.state.borrow_mut();
        state.shutdowns += 1;
        if state.mode == Mode::ShutdownFailure {
            Err(Error("SYNTHETIC_SHUTDOWN_FAILED"))
        } else {
            Ok(())
        }
    }
    fn evidence_kind(&self) -> &'static str {
        "synthetic_harness"
    }
}
fn execute_config(mode: Mode, config: Config, auth_mode: &str) -> (Report, Rc<RefCell<State>>) {
    let state = Rc::new(RefCell::new(State::new(mode)));
    let rpc_state = state.clone();
    let report = harness::run::authorized(
        &config,
        Flags { live: true },
        &mut reader(auth_mode),
        move |_| Ok(Box::new(Fake { state: rpc_state })),
    );
    (report, state)
}
fn execute(mode: Mode, auth_mode: &str) -> (Report, Rc<RefCell<State>>) {
    execute_config(mode, config(auth_mode), auth_mode)
}
fn has(report: &Report, name: &str, status: &str) -> bool {
    report
        .checks
        .iter()
        .any(|check| check.name == name && check.status == status)
}
fn methods(state: &Rc<RefCell<State>>, method: &str) -> usize {
    state
        .borrow()
        .requests
        .iter()
        .filter(|request| request["method"] == method)
        .count()
}

#[test]
fn sql_인증_전체_주입검사는_동일_실행함수와_select만_사용한다() {
    // given
    let mode = Mode::Normal;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert!(actual.success);
    assert_eq!(actual.writes_dispatched, 0);
    assert_eq!(actual.evidence_kind, "synthetic_harness");
    assert!(!actual.integration_complete);
    assert!(has(&actual, "driver_owned_entra_login", "not_applicable"));
    assert!(has(&actual, "reserved_session_identity", "pass"));
    assert!(has(&actual, "fresh_login_after_invalidation", "pass"));
    assert_eq!(state.borrow().shutdowns, 1);
    assert_eq!(methods(&state, "service_row"), 0);
    assert!(state.borrow().requests.iter().all(|request| {
        request["method"] == "initialize"
            || request["method"] == "cancel_request"
            || request["params"]["driver_context"]["read_only"] == true
    }));
}

#[test]
fn sql_인증은_sqlserver_plus_driver와_일회성_password_context만_보낸다() {
    // given
    let mode = Mode::Normal;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert!(actual.success);
    let state = state.borrow();
    let connected = state
        .requests
        .iter()
        .filter(|request| request["params"]["params"].is_object())
        .collect::<Vec<_>>();
    assert!(!connected.is_empty());
    assert!(connected.iter().all(|request| {
        let params = &request["params"]["params"];
        params["driver"] == "sqlserver-plus"
            && params["auth_mode"] == "sql_password"
            && params["extra"] == json!({})
            && params.get("password").is_none()
            && request["params"]["driver_context"]["auth"]["kind"] == "db_password"
    }));
}

#[test]
fn entra_주입검사는_host_token_없이_azure_cli_계약_필드만_보낸다() {
    // given
    let mode = Mode::Normal;
    // when
    let (actual, state) = execute(mode, "entra_user");
    // then
    assert!(actual.success);
    assert!(has(&actual, "driver_owned_entra_login", "pass"));
    assert!(has(&actual, "reserved_session_identity", "pass"));
    assert!(has(&actual, "fresh_login_after_invalidation", "pass"));
    assert!(has(&actual, "auth_invalidation", "pass"));
    let state = state.borrow();
    let connected = state
        .requests
        .iter()
        .filter(|request| request["params"]["params"].is_object())
        .collect::<Vec<_>>();
    assert!(!connected.is_empty());
    assert!(connected.iter().all(|request| {
        let params = &request["params"]["params"];
        params["driver"] == "sqlserver-plus"
            && params["auth_mode"] == "entra_user"
            && params["username"] == USERNAME
            && params["ssl_mode"] == "verify-full"
            && params["extra"]
                == json!({"auth_source":"azure_cli","tenant_id":TENANT,"entra_object_id":OBJECT})
            && params.get("password").is_none()
            && request["params"]["driver_context"]["auth"].is_null()
    }));
    let wire = serde_json::to_string(&state.requests).unwrap();
    assert!(!wire.contains("client_id"));
    assert!(!wire.contains("entra_token"));
    assert!(!wire.contains("access_token"));
    assert!(!wire.contains("entra_service_principal"));
}

#[test]
fn entra_azure_cli_path가_지정되면_extra로만_전달된다() {
    // given
    let mut value = config_value("entra_user");
    value["principal"]["azure_cli_path"] = json!("/opt/synthetic/bin/az");
    let config = Config::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    // when
    let (actual, state) = execute_config(Mode::Normal, config, "entra_user");
    // then
    assert!(actual.success);
    assert!(state
        .borrow()
        .requests
        .iter()
        .filter(|request| request["params"]["params"].is_object())
        .all(
            |request| request["params"]["params"]["extra"]["azure_cli_path"]
                == "/opt/synthetic/bin/az"
        ));
}

#[test]
fn initialize가_entra_token_true를_알리면_검사는_실패한다() {
    // given
    let mode = Mode::HostTokenCapability;
    // when
    let (actual, state) = execute(mode, "entra_user");
    // then
    assert!(!actual.success);
    assert_eq!(actual.error, Some("REQUIRED_CHECK_UNOBSERVED"));
    assert!(has(&actual, "initialize_protocol", "fail"));
    assert_eq!(methods(&state, "service_test"), 0);
}

#[test]
fn live_동의가_없으면_보호된_stdin과_spawn에_접근하지_않는다() {
    // given
    let config = config("sql_password");
    let mut reader = reader("sql_password");
    let spawned = Rc::new(Cell::new(0));
    let observed = spawned.clone();
    // when
    let actual = harness::run::authorized(&config, Flags { live: false }, &mut reader, move |_| {
        observed.set(1);
        Err(Error("UNEXPECTED_SPAWN"))
    });
    // then
    assert!(!actual.success);
    assert_eq!(actual.error, Some("LIVE_OPT_IN_REQUIRED"));
    assert_eq!(reader.reads.get(), 0);
    assert_eq!(spawned.get(), 0);
    assert_eq!(actual.evidence_kind, "preflight_only");
}

#[test]
fn config가_잘못되면_stdin과_spawn_이전에_거부된다() {
    // given
    let mut config = config("sql_password");
    config.host = "http://secret-canary".into();
    let mut reader = reader("sql_password");
    let spawned = Rc::new(Cell::new(0));
    let observed = spawned.clone();
    // when
    let actual = harness::run::authorized(&config, Flags { live: true }, &mut reader, move |_| {
        observed.set(1);
        Err(Error("UNEXPECTED_SPAWN"))
    });
    // then
    assert_eq!(actual.error, Some("INVALID_HOST"));
    assert_eq!(reader.reads.get(), 0);
    assert_eq!(spawned.get(), 0);
}

#[test]
fn 폐쇄된_config는_비밀과_endpoint_별칭과_수정된_tls와_이전_fixture_필드를_거부한다() {
    // given
    let invalid = [
        ("credential_ref", json!("secret")),
        ("password", json!(CANARY)),
        ("ssl_override", json!(false)),
        ("unknown", json!(1)),
        ("schema", json!("tabularis_test_schema")),
        ("table", json!("tabularis_test_rows")),
        ("writer", principal_value("sql_password")),
        ("reader", principal_value("sql_password")),
        ("dedicated_test_resource", json!(true)),
        ("version", json!(1)),
    ];
    let endpoints = [
        "https://server.database.windows.net",
        "127.0.0.1",
        "UPPER.database.windows.net",
        "alias.example",
        "nested.server.database.windows.net",
        "server.database.windows.net:1433",
        "../server.database.windows.net",
    ];
    // when
    let actual = (
        invalid.map(|(key, value)| {
            let mut config = config_value("sql_password");
            config[key] = value;
            Config::parse(&serde_json::to_vec(&config).unwrap()).is_err()
        }),
        endpoints.map(|host| {
            let mut config = config_value("sql_password");
            config["host"] = json!(host);
            Config::parse(&serde_json::to_vec(&config).unwrap()).is_err()
        }),
    );
    // then
    assert!(actual.0.iter().all(|rejected| *rejected));
    assert!(actual.1.iter().all(|rejected| *rejected));
}

#[test]
fn config의_시스템_database는_거부된다() {
    // given
    let invalid = ["master", "MSDB", "bad[db]", "tempdb", ""];
    // when
    let actual = invalid.map(|value| {
        let mut config = config_value("sql_password");
        config["database"] = json!(value);
        Config::parse(&serde_json::to_vec(&config).unwrap()).is_err()
    });
    // then
    assert!(actual.iter().all(|rejected| *rejected));
}

#[test]
fn entra_principal은_계약_필드만_받고_service_principal과_client_id를_거부한다() {
    // given
    let mutations: [(&str, Value); 9] = [
        ("tenant_id", json!("not-uuid")),
        ("entra_object_id", json!("not-uuid")),
        ("tenant_id", Value::Null),
        ("entra_object_id", Value::Null),
        ("username", Value::Null),
        ("client_id", json!(OBJECT)),
        ("auth_mode", json!("entra_service_principal")),
        ("azure_cli_path", json!("relative/az")),
        ("azure_cli_path", json!(" /opt/az")),
    ];
    // when
    let rejected = mutations.map(|(key, value)| {
        let mut config = config_value("entra_user");
        config["principal"][key] = value;
        Config::parse(&serde_json::to_vec(&config).unwrap()).is_err()
    });
    let mut sql = config_value("sql_password");
    sql["principal"]["tenant_id"] = json!(TENANT);
    let mut accepted = config_value("entra_user");
    accepted["principal"]["azure_cli_path"] = json!("/opt/homebrew/bin/az");
    // then
    assert!(rejected.iter().all(|rejected| *rejected));
    assert!(Config::parse(&serde_json::to_vec(&sql).unwrap()).is_err());
    assert!(Config::parse(&serde_json::to_vec(&accepted).unwrap()).is_ok());
}

fn validate_payload(mode: &str, value: Value) -> Option<&'static str> {
    Credentials::read(
        &mut Reader::new(serde_json::to_vec(&value).unwrap()),
        &config(mode),
    )
    .err()
    .map(|error| error.0)
}

#[test]
fn secret_json의_알수없는_필드와_authmode_불일치는_거부된다() {
    // given
    let mut outer = secrets("sql_password");
    outer["secret_extra"] = json!(CANARY);
    let mut inner = secrets("sql_password");
    inner["principal"]["token"] = json!(CANARY);
    let wrong = secrets("entra_user");
    // when
    let actual = [outer, inner, wrong].map(|value| validate_payload("sql_password", value));
    // then
    assert!(actual.iter().all(Option::is_some));
}

#[test]
fn utf8과_stdin_128kib와_secret_64kib_상한은_spawn_전에_검증된다() {
    // given
    let invalid_utf8 = vec![255, 254];
    let oversized = vec![b'a'; 128 * 1024 + 1];
    let mut big_secret = secrets("sql_password");
    big_secret["principal"]["password"] = json!("a".repeat(64 * 1024 + 1));
    // when
    let actual = [
        invalid_utf8,
        oversized,
        serde_json::to_vec(&big_secret).unwrap(),
    ]
    .map(|bytes| {
        Credentials::read(&mut Reader::new(bytes), &config("sql_password"))
            .err()
            .map(|e| e.0)
    });
    // then
    assert_eq!(
        actual,
        [
            Some("INVALID_SECRET_INPUT"),
            Some("INVALID_SECRET_INPUT"),
            Some("INVALID_SECRET")
        ]
    );
}

#[test]
fn entra는_빈_stdin과_빈_json만_받고_공급된_비밀은_거부한다() {
    // given
    let empty = Reader::new(Vec::new());
    let blank = Reader::new(b" \n".to_vec());
    let object = Reader::new(b"{}".to_vec());
    let injected = secrets("sql_password");
    // when
    let actual = (
        [empty, blank, object].map(|mut reader| {
            Credentials::read(&mut reader, &config("entra_user"))
                .map(|credentials| credentials.principal.is_none())
        }),
        validate_payload("entra_user", injected),
        validate_payload("entra_user", json!({"entra_token": CANARY})),
    );
    // then
    assert!(actual.0.iter().all(|read| matches!(read, Ok(true))));
    assert_eq!(actual.1, Some("AUTH_MODE_MISMATCH"));
    assert_eq!(actual.2, Some("INVALID_SECRET_INPUT"));
}

#[test]
fn sql_인증에_비밀이_없으면_spawn_전에_거부된다() {
    // given
    let payloads = [json!({}), json!({"principal":null})];
    // when
    let actual = payloads.map(|value| validate_payload("sql_password", value));
    // then
    assert_eq!(
        actual,
        [Some("AUTH_MODE_MISMATCH"), Some("AUTH_MODE_MISMATCH")]
    );
}

#[test]
fn 예약_session의_spid가_바뀌면_검사는_실패하고_열린_session을_rollback한다() {
    // given
    let mode = Mode::ChangingSpid;
    // when
    let (actual, state) = execute(mode, "entra_user");
    // then
    assert!(!actual.success);
    assert_eq!(actual.error, Some("REQUIRED_CHECK_UNOBSERVED"));
    assert!(has(&actual, "reserved_session_identity", "fail"));
    assert_eq!(methods(&state, "service_session_finish"), 1);
    assert_eq!(state.borrow().shutdowns, 1);
}

#[test]
fn session을_열다_시간초과가_나면_실패로_남고_shutdown은_한번_호출된다() {
    // given
    let mode = Mode::Timeout;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert!(!actual.success);
    assert_eq!(actual.error, Some("RPC_TIMEOUT"));
    assert_eq!(state.borrow().shutdowns, 1);
    assert_eq!(methods(&state, "service_session_finish"), 0);
}

#[test]
fn scoped_취소는_자기_요청만_지정하고_다른_연결을_유지한다() {
    // given
    let mode = Mode::Normal;
    // when
    let (actual, state) = execute(mode, "entra_user");
    // then
    assert!(actual.success);
    assert!(state.borrow().cancelled);
    assert_eq!(actual.cancellation_phase, "native_request");
    assert_eq!(actual.cancellation_outcome, "cancelled");
    assert!(has(&actual, "parallel_connection", "pass"));
    assert!(has(&actual, "connection_after_cancel", "pass"));
    assert!(has(&actual, "server_execution_cancel_phase", "deferred"));
}

#[test]
fn 결과가_불명확한_취소는_unknown으로_보존하고_서버_취소라고_주장하지_않는다() {
    // given
    let mode = Mode::CancelUnknown;
    // when
    let (actual, _) = execute(mode, "sql_password");
    // then
    assert!(actual.success);
    assert_eq!(actual.cancellation_outcome, "unknown");
    assert_eq!(actual.cancellation_phase, "native_request");
    assert!(has(&actual, "server_execution_cancel_phase", "deferred"));
}

#[test]
fn frame_상한과_utf8과_eof는_동일_실제_parser에서_거부된다() {
    // given
    let mut oversized = std::io::BufReader::new(std::io::Cursor::new(vec![b'a'; 65]));
    let malformed = [255, b'\n'];
    let unfinished = b"{}";
    // when
    let actual = (
        harness::rpc::read_frame(&mut oversized, 64)
            .err()
            .map(|e| e.0),
        harness::rpc::frame_json(&malformed).err().map(|e| e.0),
        harness::rpc::read_frame(&mut std::io::Cursor::new(unfinished), 64)
            .err()
            .map(|e| e.0),
    );
    // then
    assert_eq!(
        actual,
        (
            Some("FRAME_TOO_LARGE"),
            Some("INVALID_RPC_FRAME"),
            Some("INVALID_RPC_FRAME")
        )
    );
}

#[test]
fn 실제_wire_오류는_고정_code만_인식하고_오류원문을_노출하지_않는다() {
    // given
    let wire = json!({"jsonrpc":"2.0","id":"id","error":{"code":-32000,"message":"SQL Server denied the requested database permission","data":{"code":"OUTCOME_UNKNOWN","outcome":"unknown"}}});
    let unsafe_wire = json!({"jsonrpc":"2.0","id":"id","error":{"code":-32000,"message":CANARY,"data":{"code":CANARY,"outcome":"unknown"}}});
    // when
    let actual = (
        Response::parse(wire).unwrap(),
        Response::parse(unsafe_wire).unwrap(),
    );
    // then
    assert_eq!(actual.0.error, Some("OUTCOME_UNKNOWN"));
    assert_eq!(actual.1.error, Some("UNEXPECTED_NATIVE_ERROR"));
}

#[test]
fn report는_비밀_endpoint_identity_sql을_포함하지_않고_deferred를_유지한다() {
    // given
    let mode = Mode::Normal;
    // when
    let (actual, _) = execute(mode, "entra_user");
    // then
    assert!(actual.success);
    assert!(!actual.integration_complete);
    let report = String::from_utf8(actual.bytes().unwrap()).unwrap();
    assert!(![
        CANARY,
        SYNTHETIC_HOST,
        USERNAME,
        TENANT,
        OBJECT,
        "WAITFOR",
        "SELECT",
        "eyJ"
    ]
    .iter()
    .any(|value| report.contains(value)));
    assert!(has(&actual, "mfa", "deferred"));
    assert!(has(&actual, "azure_cli_account_change", "deferred"));
    assert!(has(&actual, "host_gui_mcp_cli_parity", "deferred"));
    assert!(has(&actual, "windows_manual_harness_owner_acl", "deferred"));
    assert!(!has(&actual, "refresh_provider", "deferred"));
}

#[test]
fn driver_shutdown_실패는_검사가_끝나도_실패로_남는다() {
    // given
    let mode = Mode::ShutdownFailure;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert!(!actual.success);
    assert_eq!(actual.error, Some("DRIVER_SHUTDOWN_FAILED"));
    assert_eq!(state.borrow().shutdowns, 1);
}

#[test]
fn jwt_모양의_driver_출력만_비밀노출로_판정한다() {
    // given
    let token = b"{\"jsonrpc\":\"2.0\",\"result\":\"eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJodHRwczovL2RhdGFiYXNlLndpbmRvd3MubmV0LyJ9.c2ln\"}";
    let short = b"eyJhYmM.eyJkZWY";
    let plain = br#"{"rows":[["eyJ"],["a.b.c"],["Entra user"]]}"#;
    // when
    let actual = (
        harness::rpc::token_shaped(token),
        harness::rpc::token_shaped(short),
        harness::rpc::token_shaped(plain),
    );
    // then
    assert_eq!(actual, (true, false, false));
}

#[test]
fn azure_cli_환경은_절대_config_dir과_home_기본값만_사용한다() {
    // given
    let vars = |entries: Vec<(&'static str, &'static str)>| {
        move |key: &str| {
            entries
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(*value))
        }
    };
    // when
    let explicit = AzureEnv::resolve(vars(vec![
        ("AZURE_CONFIG_DIR", "/work/azure"),
        ("HOME", "/Users/synthetic"),
        ("PATH", "/opt/homebrew/bin:/usr/bin"),
    ]));
    let fallback = AzureEnv::resolve(vars(vec![("HOME", "/Users/synthetic")]));
    let relative = AzureEnv::resolve(vars(vec![
        ("AZURE_CONFIG_DIR", "relative/azure"),
        ("HOME", "relative-home"),
    ]));
    // then
    let explicit = explicit.unwrap();
    assert_eq!(explicit.config_dir, std::path::PathBuf::from("/work/azure"));
    assert_eq!(explicit.path, OsString::from("/opt/homebrew/bin:/usr/bin"));
    let fallback = fallback.unwrap();
    assert_eq!(
        fallback.config_dir,
        std::path::PathBuf::from("/Users/synthetic/.azure")
    );
    assert_eq!(fallback.path, OsString::from("/usr/bin:/bin"));
    assert!(relative.is_none());
}

struct InjectedClock {
    base: std::time::Instant,
    elapsed_ms: std::sync::atomic::AtomicU64,
}
impl InjectedClock {
    fn new() -> Self {
        Self {
            base: std::time::Instant::now(),
            elapsed_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }
    fn advance(&self, duration: std::time::Duration) {
        self.elapsed_ms.fetch_add(
            duration.as_millis().try_into().unwrap(),
            std::sync::atomic::Ordering::SeqCst,
        );
    }
}
impl harness::rpc::Clock for InjectedClock {
    fn now(&self) -> std::time::Instant {
        self.base
            + std::time::Duration::from_millis(
                self.elapsed_ms.load(std::sync::atomic::Ordering::SeqCst),
            )
    }
    fn pause(&self, duration: std::time::Duration) {
        self.advance(duration);
    }
}
fn shared_request_deadline() -> (Vec<u64>, Option<&'static str>) {
    use harness::rpc::{Clock, Deadlines};
    use std::time::Duration;
    let clock = InjectedClock::new();
    let mut deadlines = Deadlines::new(clock.now());
    deadlines.begin("request", clock.now()).unwrap();
    clock.advance(Duration::from_secs(90));
    let after_ack = deadlines
        .remaining("request", clock.now())
        .unwrap()
        .as_secs();
    clock.advance(Duration::from_secs(20));
    let after_receive = deadlines
        .remaining("request", clock.now())
        .unwrap()
        .as_secs();
    clock.advance(Duration::from_secs(10));
    (
        vec![after_ack, after_receive],
        deadlines
            .remaining("request", clock.now())
            .err()
            .map(|error| error.0),
    )
}
#[test]
fn 전송_ack와_receive는_전송시작의_동일한_120초_deadline을_사용한다() {
    // given
    let scenario = shared_request_deadline;
    // when
    let actual = scenario();
    // then
    assert_eq!(actual.0, vec![30, 10]);
    assert_eq!(actual.1, Some("RPC_TIMEOUT"));
}
fn whole_run_deadline() -> (u64, Option<&'static str>) {
    use harness::rpc::{Clock, Deadlines};
    use std::time::Duration;
    let clock = InjectedClock::new();
    let mut deadlines = Deadlines::new(clock.now());
    clock.advance(Duration::from_secs(595));
    deadlines.begin("late", clock.now()).unwrap();
    let remaining = deadlines.remaining("late", clock.now()).unwrap().as_secs();
    clock.advance(Duration::from_secs(5));
    (
        remaining,
        deadlines
            .begin("expired", clock.now())
            .err()
            .map(|error| error.0),
    )
}
#[test]
fn 마지막_요청도_전체_10분_deadline을_연장하지_않는다() {
    // given
    let scenario = whole_run_deadline;
    // when
    let actual = scenario();
    // then
    assert_eq!(actual.0, 5);
    assert_eq!(actual.1, Some("WHOLE_RUN_TIMEOUT"));
}
fn shared_shutdown_deadline() -> (Vec<u64>, Option<&'static str>, u64) {
    use harness::rpc::{Clock, Deadlines};
    use std::time::Duration;
    let clock = InjectedClock::new();
    let mut deadlines = Deadlines::new(clock.now());
    let end = deadlines.shutdown(clock.now());
    clock.advance(Duration::from_secs(2));
    deadlines.begin("shutdown", clock.now()).unwrap();
    let after_send = deadlines
        .remaining("shutdown", clock.now())
        .unwrap()
        .as_secs();
    clock.advance(Duration::from_secs(2));
    let after_ack = deadlines
        .remaining("shutdown", clock.now())
        .unwrap()
        .as_secs();
    let exit_remaining = end.saturating_duration_since(clock.now()).as_secs();
    clock.advance(Duration::from_secs(1));
    (
        vec![after_send, after_ack],
        deadlines
            .remaining("shutdown", clock.now())
            .err()
            .map(|error| error.0),
        exit_remaining,
    )
}
#[test]
fn shutdown의_전송_ack와_child_exit는_하나의_5초_한도를_공유한다() {
    // given
    let scenario = shared_shutdown_deadline;
    // when
    let actual = scenario();
    // then
    assert_eq!(actual.0, vec![3, 1]);
    assert_eq!(actual.1, Some("RPC_TIMEOUT"));
    assert_eq!(actual.2, 1);
}

#[cfg(unix)]
struct RealClock;
#[cfg(unix)]
impl harness::rpc::Clock for RealClock {
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
    fn pause(&self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }
}
#[cfg(unix)]
fn synthetic_shutdown(
    script: &str,
    clock: std::sync::Arc<dyn harness::rpc::Clock>,
) -> (Option<&'static str>, std::time::Duration, &'static str) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let root = Temp::new();
    let entry = root.0.join("synthetic.py");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&entry)
        .unwrap();
    file.write_all(script.as_bytes()).unwrap();
    drop(file);
    let credentials =
        Credentials::read(&mut reader("sql_password"), &config("sql_password")).unwrap();
    let mut rpc = harness::rpc::LiveRpc::spawn_process(
        std::path::Path::new("/usr/bin/python3"),
        &[
            std::ffi::OsStr::new("-I"),
            std::ffi::OsStr::new("-S"),
            entry.as_os_str(),
        ],
        &credentials,
        None,
        clock,
        "synthetic_harness",
    )
    .unwrap();
    let evidence = rpc.evidence_kind();
    let started = std::time::Instant::now();
    let result = rpc.shutdown().err().map(|error| error.0);
    drop(rpc);
    (result, started.elapsed(), evidence)
}
#[cfg(unix)]
#[test]
fn synthetic_child의_ack_뒤_정지에도_총_5초에_자기_child만_종료한다() {
    // given
    let script="import json,sys,threading\nr=json.loads(sys.stdin.readline())\nprint(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':{}}),flush=True)\nthreading.Event().wait()\n";
    let clock = std::sync::Arc::new(InjectedClock::new());
    let observed = clock.clone();
    // when
    let actual = synthetic_shutdown(script, clock);
    // then
    assert_eq!(actual.0, Some("DRIVER_SHUTDOWN_TIMEOUT"));
    assert!(actual.1 < std::time::Duration::from_secs(5));
    assert_eq!(
        observed
            .elapsed_ms
            .load(std::sync::atomic::Ordering::SeqCst),
        5000
    );
    assert_eq!(actual.2, "synthetic_harness");
}
#[cfg(unix)]
#[test]
fn synthetic_child가_응답하지_않으면_전송부터_종료까지_5초만_기다린다() {
    // given
    let script = "import sys,threading\nsys.stdin.readline()\nthreading.Event().wait()\n";
    let clock = std::sync::Arc::new(RealClock);
    // when
    let actual = synthetic_shutdown(script, clock);
    // then
    assert_eq!(actual.0, Some("DRIVER_SHUTDOWN_TIMEOUT"));
    assert!(actual.1 >= std::time::Duration::from_secs(5));
    assert!(actual.1 < std::time::Duration::from_secs(7));
    assert_eq!(actual.2, "synthetic_harness");
}
#[cfg(unix)]
#[test]
fn synthetic_child의_64개_초과_응답도_receiver를_drop한_뒤_join한다() {
    // given
    let script="import json,sys\nr=json.loads(sys.stdin.readline())\nprint(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':{}}),flush=True)\nfor i in range(100):\n print(json.dumps({'jsonrpc':'2.0','id':'extra'+str(i),'result':{}}))\nsys.stdout.flush()\n";
    let clock = std::sync::Arc::new(RealClock);
    // when
    let actual = synthetic_shutdown(script, clock);
    // then
    assert_eq!(actual.0, None);
    assert!(actual.1 < std::time::Duration::from_secs(5));
    assert_eq!(actual.2, "synthetic_harness");
}

#[cfg(unix)]
struct Temp(std::path::PathBuf);
#[cfg(unix)]
impl Temp {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::path::PathBuf::from(format!("/tmp/tabularis-a2-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn config(&self) -> std::path::PathBuf {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.0.join("config.json");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(&serde_json::to_vec(&config_value("sql_password")).unwrap())
            .unwrap();
        path
    }
}
#[cfg(unix)]
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
#[test]
fn private_config는_자기_600_regular_file만_읽는다() {
    // given
    let root = Temp::new();
    let path = root.config();
    // when
    let actual = harness::config::load(&path);
    // then
    assert!(actual.is_ok());
}

#[cfg(unix)]
#[test]
fn config_symlink와_공개_permission은_거부된다() {
    // given
    use std::os::unix::fs::PermissionsExt;
    let root = Temp::new();
    let path = root.config();
    let link = root.0.join("linked.json");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    // when
    let actual = [
        harness::config::load(&path).err().map(|e| e.0),
        harness::config::load(&link).err().map(|e| e.0),
    ];
    // then
    assert_eq!(
        actual,
        [Some("CONFIG_NOT_PRIVATE"), Some("CONFIG_NOT_PRIVATE")]
    );
}

#[cfg(unix)]
#[test]
fn private_report는_600으로_게시되고_기존_원본을_덮어쓰지_않는다() {
    // given
    use std::os::unix::fs::PermissionsExt;
    let root = Temp::new();
    let path = root.0.join("report.json");
    let (report, _) = execute(Mode::Normal, "sql_password");
    // when
    let actual = (
        harness::report::publish(&report, &path),
        harness::report::publish(&report, &path),
    );
    // then
    assert!(actual.0.is_ok());
    assert_eq!(actual.1.err().map(|e| e.0), Some("REPORT_EXISTS"));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!std::fs::read_to_string(&path).unwrap().contains(CANARY));
    assert_eq!(std::fs::read(&path).unwrap(), report.bytes().unwrap());
}
