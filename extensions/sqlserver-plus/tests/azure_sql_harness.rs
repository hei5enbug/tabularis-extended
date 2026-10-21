#[path = "support/azure_sql_harness/mod.rs"]
mod harness;

use harness::{
    config::{Config, Flags},
    credentials::{Credentials, SecretReader},
    fixture,
    report::Report,
    rpc::{Response, Rpc},
    Error, Result,
};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    rc::Rc,
};

const _: fn() -> Report = harness::run::run_live_from_stdin;
const TENANT: &str = "11111111-1111-4111-8111-111111111111";
const CLIENT: &str = "22222222-2222-4222-8222-222222222222";
const PRINCIPAL: &str = "33333333-3333-4333-8333-333333333333";
const CANARY: &str = "synthetic-secret-DO-NOT-REPORT";
fn config_value(mode: &str) -> Value {
    let principal = |username| {
        if mode == "sql_password" {
            json!({"auth_mode":mode,"username":username})
        } else {
            json!({"auth_mode":mode,"tenant_id":TENANT,"client_id":CLIENT})
        }
    };
    json!({"version":1,"dedicated_test_resource":true,"host":"synthetic-server.database.windows.net","database":"synthetic_db","schema":"tabularis_test_schema","table":"tabularis_test_rows","writer":principal("writer"),"reader":principal("reader")})
}
fn config(mode: &str) -> Config {
    Config::parse(&serde_json::to_vec(&config_value(mode)).unwrap()).unwrap()
}
fn secrets(mode: &str) -> Value {
    let auth = |identity, token: &str, extra: u64| {
        if mode == "sql_password" {
            json!({"kind":"db_password","password":token,"identity":identity})
        } else {
            json!({"kind":"entra_token","access_token":token,"expires_at_ms":harness::now_ms()+3_600_000+extra,"identity":identity,"tenant_id":TENANT,"client_id":CLIENT,"principal_id":PRINCIPAL,"scope":harness::credentials::SQL_SCOPE})
        }
    };
    let mut value = json!({"writer":auth("writer-revision",CANARY,0),"reader":auth("reader-revision","synthetic-reader-secret",0)});
    if mode != "sql_password" {
        value["refresh"] = auth("writer-revision", "synthetic-refreshed-secret", 1000);
    }
    value
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
    BadMetadata,
    ReaderAllowed,
    WrongPermissionError,
    UnknownInsert,
    UnknownAppliedInsert,
    CleanupForeign,
    CleanupFailure,
    MissingRow,
    WrongCount,
    CancelQueued,
    ShutdownFailure,
    Timeout,
}
struct State {
    mode: Mode,
    rows: BTreeMap<i32, Value>,
    requests: Vec<Value>,
    responses: HashMap<String, Response>,
    cleaning: bool,
    cancelled: bool,
    long: Option<(String, String)>,
    shutdowns: usize,
}
impl State {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            rows: BTreeMap::new(),
            requests: Vec::new(),
            responses: HashMap::new(),
            cleaning: false,
            cancelled: false,
            long: None,
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
        unknown: false,
        permission_denial: false,
    }
}
fn denied(code: &'static str, permission: bool) -> Response {
    Response {
        result: Value::Null,
        error: Some(code),
        unknown: code == "OUTCOME_UNKNOWN",
        permission_denial: permission,
    }
}
fn rows(columns: Value, rows: Value) -> Value {
    json!({"columns":columns,"rows":rows,"truncated":false,"affected_rows":null,"warnings":["affected_rows_unavailable"],"column_metadata":[{"column_index":0},{"column_index":1}]})
}
fn metadata() -> Value {
    json!({"columns":[
        {"name":"run_id","data_type":"nvarchar","max_length":72,"precision":0,"scale":0,"is_nullable":false,"is_identity":false,"is_pk":true,"primary_key_position":1},
        {"name":"id","data_type":"int","max_length":4,"precision":10,"scale":0,"is_nullable":false,"is_identity":false,"is_pk":true,"primary_key_position":2},
        {"name":"value","data_type":"nvarchar","max_length":200,"precision":0,"scale":0,"is_nullable":true,"is_identity":false,"is_pk":false,"primary_key_position":null},
        {"name":"measure","data_type":"decimal","max_length":17,"precision":38,"scale":18,"is_nullable":true,"is_identity":false,"is_pk":false,"primary_key_position":null},
        {"name":"observed_at","data_type":"datetimeoffset","max_length":10,"precision":34,"scale":7,"is_nullable":true,"is_identity":false,"is_pk":false,"primary_key_position":null}
    ]})
}
impl Fake {
    fn respond(&self, request: &Value) -> Result<Response> {
        let mut state = self.state.borrow_mut();
        let method = request["method"].as_str().unwrap_or("");
        let input = &request["params"]["input"];
        let context = &request["params"]["driver_context"];
        let is_reader = context["auth"]["identity"] == "reader-revision";
        let read_only = context["read_only"] == true;
        let parameter = |name: &str| {
            input["parameters"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["name"] == name))
                .map(|item| item["value"].clone())
                .unwrap_or(Value::Null)
        };
        match method {
            "initialize" => Ok(ok(
                json!({"service_capabilities":{"service_protocol":1,"exact_query":true,"sessions":true,"cancel":true,"catalog":true,"rows":true,"entra_token":true,"exact_affected_rows":false,"readonly_guarantee":"restricted_database_principal"}}),
            )),
            "service_test" => {
                if state.cancelled {
                    state.cleaning = true;
                }
                Ok(ok(json!({"success":true})))
            }
            "service_catalog" => match input["operation"].as_str() {
                Some("catalog.databases") => Ok(ok(json!({"databases":["synthetic_db"]}))),
                Some("catalog.schemas") => Ok(ok(json!({"schemas":["tabularis_test_schema"]}))),
                Some("catalog.objects") => Ok(ok(
                    json!({"objects":[{"schema_name":"tabularis_test_schema","name":"tabularis_test_rows","kind":"table"}]}),
                )),
                Some("catalog.describe") => {
                    let mut value = metadata();
                    if state.mode == Mode::BadMetadata {
                        value["columns"][1]["primary_key_position"] = json!(1);
                    }
                    Ok(ok(value))
                }
                _ => Err(Error("SYNTHETIC_UNEXPECTED_OPERATION")),
            },
            "service_row" => {
                let operation = input["operation"].as_str().unwrap_or("");
                let id = if operation == "row.insert" {
                    input["values"]["id"].as_i64()
                } else {
                    input["identity"]["values"][1].as_i64()
                }
                .unwrap_or(0) as i32;
                if read_only {
                    return Ok(denied("WRITE_NOT_ALLOWED", false));
                }
                if is_reader && state.mode != Mode::ReaderAllowed {
                    return Ok(denied(
                        "OUTCOME_UNKNOWN",
                        state.mode != Mode::WrongPermissionError,
                    ));
                }
                if operation != "row.insert"
                    && input["identity"]["columns"] != json!(["run_id", "id"])
                {
                    return Ok(denied("INVALID_ARGUMENT", false));
                }
                if operation == "row.insert" {
                    if state.rows.contains_key(&id) {
                        return Ok(denied("OUTCOME_UNKNOWN", false));
                    }
                    if id == 2
                        && matches!(
                            state.mode,
                            Mode::UnknownInsert | Mode::UnknownAppliedInsert | Mode::Timeout
                        )
                    {
                        if state.mode == Mode::UnknownAppliedInsert {
                            state.rows.insert(id, input["values"].clone());
                        }
                        state.cleaning = true;
                        if state.mode == Mode::Timeout {
                            return Err(Error("RPC_TIMEOUT"));
                        }
                        return Ok(denied("OUTCOME_UNKNOWN", false));
                    }
                    state.rows.insert(id, input["values"].clone());
                } else if operation == "row.update" {
                    if let Some(row) = state.rows.get_mut(&id) {
                        row["value"] = input["changes"]["value"].clone();
                    }
                } else if operation == "row.delete" {
                    if state.cleaning && id == 1 && state.mode == Mode::CleanupFailure {
                        return Ok(denied("OUTCOME_UNKNOWN", false));
                    }
                    state.rows.remove(&id);
                } else {
                    return Err(Error("SYNTHETIC_UNEXPECTED_OPERATION"));
                }
                Ok(ok(
                    json!({"affected_rows":if state.mode==Mode::WrongCount {0}else{1},"truncated":false}),
                ))
            }
            "execute_query_exact" => {
                let text = input["text"].as_str().unwrap_or("");
                if text.starts_with("WAITFOR") {
                    state.long = Some((
                        context["connection_id"].as_str().unwrap_or("").into(),
                        context["request_id"].as_str().unwrap_or("").into(),
                    ));
                    return Ok(denied(
                        if state.mode == Mode::CancelQueued {
                            "CANCELLED"
                        } else {
                            "OUTCOME_UNKNOWN"
                        },
                        false,
                    ));
                }
                if text.contains("SERVERPROPERTY") {
                    return Ok(ok(rows(
                        json!(["edition", "database"]),
                        json!([[5, "synthetic_db"]]),
                    )));
                }
                if text == "SELECT @@SPID" {
                    return Ok(ok(rows(json!(["spid"]), json!([[77]]))));
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
                if text.contains("blocked") || text.starts_with("EXEC sys.sp_executesql") {
                    return Ok(denied("WRITE_NOT_ALLOWED", true));
                }
                if text.starts_with("SELECT CAST(@decimal") {
                    return Ok(ok(rows(json!(["measure"]), json!([[fixture::DECIMAL]]))));
                }
                if text.starts_with("SELECT id,value") {
                    if state.mode == Mode::MissingRow {
                        state.rows.remove(&2);
                        state.cleaning = true;
                    }
                    let values = state
                        .rows
                        .values()
                        .map(|row| {
                            json!([row["id"], row["value"], row["measure"], row["observed_at"]])
                        })
                        .collect::<Vec<_>>();
                    return Ok(ok(rows(
                        json!(["id", "value", "measure", "observed_at"]),
                        json!(values),
                    )));
                }
                if text.starts_with("SELECT run_id,id,value") {
                    let id = parameter("@id").as_i64().unwrap_or(0) as i32;
                    let values = state
                        .rows
                        .get(&id)
                        .map(|row| {
                            json!([
                                if state.cleaning && state.mode == Mode::CleanupForeign && id == 1 {
                                    json!("foreign-run-private-canary")
                                } else {
                                    row["run_id"].clone()
                                },
                                row["id"],
                                row["value"]
                            ])
                        })
                        .into_iter()
                        .collect::<Vec<_>>();
                    return Ok(ok(rows(json!(["run_id", "id", "value"]), json!(values))));
                }
                Err(Error("SYNTHETIC_UNEXPECTED_SQL"))
            }
            "service_session_open" => Ok(ok(
                json!({"session_handle":"synthetic-session","in_transaction":true,"rollback_confirmed":false}),
            )),
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
fn execute(mode: Mode, auth_mode: &str) -> (Report, Rc<RefCell<State>>) {
    let config = config(auth_mode);
    let state = Rc::new(RefCell::new(State::new(mode)));
    let rpc_state = state.clone();
    let report = harness::run::authorized(
        &config,
        Flags {
            live: true,
            fixture_writes: true,
        },
        &mut reader(auth_mode),
        move |_| Ok(Box::new(Fake { state: rpc_state })),
    );
    (report, state)
}
fn has(report: &Report, name: &str, status: &str) -> bool {
    report
        .checks
        .iter()
        .any(|check| check.name == name && check.status == status)
}

#[test]
fn sql_인증_전체_주입검사는_동일_실행함수와_정확한_정리_결과를_사용한다() {
    // given
    let mode = Mode::Normal;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert!(actual.success);
    assert_eq!(actual.cleanup_remaining, 0);
    assert_eq!(actual.evidence_kind, "synthetic_harness");
    assert!(!actual.integration_complete);
    assert!(state.borrow().rows.is_empty());
    assert!(has(&actual, "supplied_token_rotation", "not_applicable"));
    assert!(has(&actual, "exact_affected_rows_unavailable", "pass"));
    assert!(has(&actual, "reader_database_permission_denial", "pass"));
    assert!(has(&actual, "reader_probe_absent", "pass"));
    assert_eq!(state.borrow().shutdowns, 1);
}

#[test]
fn entra_주입검사는_공급된_토큰_교체와_같은_예약_session을_확인한다() {
    // given
    let modes = ["entra_user", "entra_service_principal"];
    // when
    let actual = modes.map(|mode| execute(Mode::Normal, mode));
    // then
    assert!(actual.iter().all(|(report, _)| report.success));
    assert!(actual
        .iter()
        .all(|(report, _)| has(report, "supplied_token_rotation", "pass")));
    assert!(actual.iter().all(|(report, _)| has(
        report,
        "reserved_session_token_identity",
        "pass"
    )));
    assert!(actual.iter().all(|(report, _)| has(
        report,
        "token_local_contract_rejections",
        "pass"
    )));
    assert!(actual
        .iter()
        .all(|(report, _)| has(report, "refresh_provider", "deferred")));
    assert!(actual
        .iter()
        .all(|(_, state)| state.borrow().rows.is_empty()));
}

#[test]
fn 두_live_동의가_없으면_보호된_stdin과_spawn에_접근하지_않는다() {
    // given
    let config = config("sql_password");
    let mut reader = reader("sql_password");
    let spawned = Rc::new(Cell::new(0));
    let observed = spawned.clone();
    // when
    let actual = harness::run::authorized(
        &config,
        Flags {
            live: true,
            fixture_writes: false,
        },
        &mut reader,
        move |_| {
            observed.set(1);
            Err(Error("UNEXPECTED_SPAWN"))
        },
    );
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
    let actual = harness::run::authorized(
        &config,
        Flags {
            live: true,
            fixture_writes: true,
        },
        &mut reader,
        move |_| {
            observed.set(1);
            Err(Error("UNEXPECTED_SPAWN"))
        },
    );
    // then
    assert_eq!(actual.error, Some("INVALID_HOST"));
    assert_eq!(reader.reads.get(), 0);
    assert_eq!(spawned.get(), 0);
}

#[test]
fn 폐쇄된_config는_비밀과_endpoint_별칭과_수정된_tls를_거부한다() {
    // given
    let invalid = [
        ("credential_ref", json!("secret")),
        ("password", json!(CANARY)),
        ("ssl_override", json!(false)),
        ("unknown", json!(1)),
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
fn config의_시스템_database와_fixture_밖_식별자는_거부된다() {
    // given
    let invalid = [
        ("database", "master"),
        ("database", "MSDB"),
        ("database", "bad[db]"),
        ("schema", "dbo"),
        ("table", "normal_table"),
        ("table", "tabularis_test_bad-name"),
    ];
    // when
    let actual = invalid.map(|(key, value)| {
        let mut config = config_value("sql_password");
        config[key] = json!(value);
        Config::parse(&serde_json::to_vec(&config).unwrap()).is_err()
    });
    // then
    assert!(actual.iter().all(|rejected| *rejected));
}

fn validate_payload(mode: &str, value: Value) -> Option<&'static str> {
    Credentials::read(
        &mut Reader::new(serde_json::to_vec(&value).unwrap()),
        &config(mode),
        harness::now_ms(),
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
    inner["writer"]["token"] = json!(CANARY);
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
    big_secret["writer"]["password"] = json!("a".repeat(64 * 1024 + 1));
    // when
    let actual = [
        invalid_utf8,
        oversized,
        serde_json::to_vec(&big_secret).unwrap(),
    ]
    .map(|bytes| {
        Credentials::read(
            &mut Reader::new(bytes),
            &config("sql_password"),
            harness::now_ms(),
        )
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
fn entra_만료_scope_uuid와_refresh_identity_변경은_계약에서_거부된다() {
    // given
    let mutations = [
        ("expires_at_ms", json!(0)),
        ("scope", json!("wrong")),
        ("tenant_id", json!("not-uuid")),
        (
            "principal_id",
            json!("44444444-4444-4444-8444-444444444444"),
        ),
        ("identity", json!("changed")),
        ("access_token", json!(CANARY)),
    ];
    // when
    let actual = mutations.map(|(key, value)| {
        let mut payload = secrets("entra_user");
        payload["refresh"][key] = value;
        validate_payload("entra_user", payload)
    });
    // then
    assert!(actual.iter().all(Option::is_some));
}

#[test]
fn entra_refresh가_누락되면_토큰_acquisition을_추정하지_않는다() {
    // given
    let mut payload = secrets("entra_user");
    payload.as_object_mut().unwrap().remove("refresh");
    // when
    let actual = validate_payload("entra_user", payload);
    // then
    assert_eq!(actual, Some("REFRESH_REQUIRED"));
}

#[test]
fn pk_metadata가_틀리면_첫_write_이전에_실패한다() {
    // given
    let mode = Mode::BadMetadata;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert_eq!(actual.error, Some("FIXTURE_METADATA_MISMATCH"));
    assert_eq!(actual.writes_dispatched, 0);
    assert!(state.borrow().rows.is_empty());
    assert_eq!(
        state
            .borrow()
            .requests
            .iter()
            .filter(|request| request["method"] == "service_row")
            .count(),
        0
    );
}

#[test]
fn reader_probe가_예상과_달리_성공하면_검사는_실패하고_자기행을_정리한다() {
    // given
    let mode = Mode::ReaderAllowed;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert!(!actual.success);
    assert!(has(&actual, "reader_database_permission_denial", "fail"));
    assert!(state.borrow().rows.is_empty());
    assert_eq!(actual.cleanup_remaining, 0);
}

#[test]
fn unknown_오류만으로는_실제_db_permission_denial을_주장하지_않는다() {
    // given
    let mode = Mode::WrongPermissionError;
    // when
    let (actual, _) = execute(mode, "sql_password");
    // then
    assert!(!actual.success);
    assert!(has(&actual, "reader_database_permission_denial", "fail"));
    assert!(actual.unknown_write_outcomes > 0);
}

#[test]
fn 불명확한_write는_재전송하지_않고_조회된_자기행만_정리한다() {
    // given
    let modes = [
        Mode::UnknownInsert,
        Mode::UnknownAppliedInsert,
        Mode::Timeout,
    ];
    // when
    let actual = modes.map(|mode| execute(mode, "sql_password"));
    // then
    assert!(actual
        .iter()
        .all(|(report, _)| !report.success && report.unknown_write_outcomes > 0));
    assert!(actual
        .iter()
        .all(|(_, state)| state.borrow().rows.is_empty()));
    assert!(actual.iter().all(|(_, state)| state
        .borrow()
        .requests
        .iter()
        .filter(|request| request["method"] == "service_row"
            && request["params"]["input"]["operation"] == "row.insert"
            && request["params"]["input"]["values"]["id"] == 2)
        .count()
        == 1));
}

#[test]
fn cleanup의_다른_run_id는_삭제하지_않고_남은_id_sha만_보고한다() {
    // given
    let mode = Mode::CleanupForeign;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert_eq!(actual.error, Some("CLEANUP_FOREIGN_RUN"));
    assert_eq!(actual.cleanup_remaining, 1);
    assert!(actual
        .remaining_id_sha256
        .as_ref()
        .is_some_and(|hash| hash.len() == 64));
    assert!(state.borrow().rows.contains_key(&1));
    assert_eq!(
        state
            .borrow()
            .requests
            .iter()
            .filter(|request| request["method"] == "service_row"
                && request["params"]["input"]["operation"] == "row.delete"
                && request["params"]["input"]["identity"]["values"][1] == 1)
            .count(),
        0
    );
    assert!(!String::from_utf8(actual.bytes().unwrap())
        .unwrap()
        .contains("foreign-run-private-canary"));
}

#[test]
fn cleanup_삭제가_불명확하면_한번만_시도하고_남은_행을_유지한다() {
    // given
    let mode = Mode::CleanupFailure;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert!(!actual.success);
    assert_eq!(actual.cleanup_remaining, 1);
    assert_eq!(
        state
            .borrow()
            .requests
            .iter()
            .filter(|request| request["method"] == "service_row"
                && request["params"]["input"]["operation"] == "row.delete"
                && request["params"]["input"]["identity"]["values"][1] == 1)
            .count(),
        1
    );
}

#[test]
fn 이미_없는_행은_cleanup에서_삭제를_재전송하지_않는다() {
    // given
    let mode = Mode::MissingRow;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert!(!actual.success);
    assert_eq!(actual.cleanup_remaining, 0);
    assert!(state.borrow().rows.is_empty());
    assert_eq!(
        state
            .borrow()
            .requests
            .iter()
            .filter(|request| request["method"] == "service_row"
                && request["params"]["input"]["operation"] == "row.delete"
                && request["params"]["input"]["identity"]["values"][1] == 2)
            .count(),
        0
    );
}

#[test]
fn affected_rows가_실제_한행을_확인하지_못하면_성공을_만들지_않는다() {
    // given
    let mode = Mode::WrongCount;
    // when
    let (actual, _) = execute(mode, "sql_password");
    // then
    assert!(!actual.success);
    assert!(has(&actual, "insert_affected_rows", "fail"));
}

#[test]
fn scoped_취소는_자기_요청만_지정하고_다른_연결과_unknown을_보존한다() {
    // given
    let mode = Mode::Normal;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert!(actual.success);
    assert!(state.borrow().cancelled);
    assert_eq!(actual.cancellation_phase, "native_request");
    assert_eq!(actual.cancellation_write_outcome, "unknown");
    assert!(has(&actual, "parallel_reader_connection", "pass"));
    assert!(has(&actual, "reader_after_cancel", "pass"));
    assert!(has(&actual, "server_execution_cancel_phase", "deferred"));
}

#[test]
fn dispatch_이전_cancelled도_서버_취소라고_주장하지_않는다() {
    // given
    let mode = Mode::CancelQueued;
    // when
    let (actual, _) = execute(mode, "sql_password");
    // then
    assert!(actual.success);
    assert_eq!(actual.cancellation_write_outcome, "not_applied");
    assert_eq!(actual.cancellation_phase, "native_request");
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
fn 실제_wire_permission_고정문구만_인식하고_오류원문을_노출하지_않는다() {
    // given
    let wire = json!({"jsonrpc":"2.0","id":"id","error":{"code":-32000,"message":"SQL Server denied the requested database permission","data":{"code":"OUTCOME_UNKNOWN","outcome":"unknown"}}});
    let unsafe_wire = json!({"jsonrpc":"2.0","id":"id","error":{"code":-32000,"message":CANARY,"data":{"code":CANARY,"outcome":"unknown"}}});
    // when
    let actual = (
        Response::parse(wire).unwrap(),
        Response::parse(unsafe_wire).unwrap(),
    );
    // then
    assert!(actual.0.permission_denial && actual.0.unknown);
    assert!(!actual.1.permission_denial);
    assert_eq!(actual.1.error, Some("UNEXPECTED_NATIVE_ERROR"));
}

#[test]
fn report는_비밀_endpoint_principal_sql을_포함하지_않고_deferred를_유지한다() {
    // given
    let mode = Mode::Normal;
    // when
    let (actual, _) = execute(mode, "entra_service_principal");
    // then
    assert!(actual.success);
    assert!(!actual.integration_complete);
    assert!(![
        CANARY,
        "synthetic-reader-secret",
        "synthetic-refreshed-secret",
        "synthetic-server.database.windows.net",
        TENANT,
        CLIENT,
        PRINCIPAL,
        "WAITFOR",
        "SELECT"
    ]
    .iter()
    .any(|value| String::from_utf8(actual.bytes().unwrap())
        .unwrap()
        .contains(value)));
    assert!(has(&actual, "mfa", "deferred"));
    assert!(has(&actual, "host_gui_mcp_cli_parity", "deferred"));
    assert!(has(&actual, "windows_manual_harness_owner_acl", "deferred"));
}

#[test]
fn driver_shutdown_실패는_fixture가_정리되어도_실패로_남는다() {
    // given
    let mode = Mode::ShutdownFailure;
    // when
    let (actual, state) = execute(mode, "sql_password");
    // then
    assert!(!actual.success);
    assert_eq!(actual.error, Some("DRIVER_SHUTDOWN_FAILED"));
    assert!(state.borrow().rows.is_empty());
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
    let credentials = Credentials::read(
        &mut reader("sql_password"),
        &config("sql_password"),
        harness::now_ms(),
    )
    .unwrap();
    let mut rpc = harness::rpc::LiveRpc::spawn_process(
        std::path::Path::new("/usr/bin/python3"),
        &[
            std::ffi::OsStr::new("-I"),
            std::ffi::OsStr::new("-S"),
            entry.as_os_str(),
        ],
        &credentials,
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
