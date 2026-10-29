#![allow(non_snake_case)]
use super::*;
use crate::config::AppConfig;
use crate::mcp::{dispatch_tool, CallAudit, CONFIG_DIR_OVERRIDE};
use crate::service_native::tests::Harness;
use crate::services::{
    contracts::{Metrics, Outcome},
    dispatch::CallerTransport,
};
use std::sync::atomic::Ordering;

fn args(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

fn read_only() -> AppConfig {
    AppConfig {
        mcp_readonly_default: Some(true),
        ..AppConfig::default()
    }
}

fn outcome(
    error: Option<ServiceError>,
    status: JobStatus,
    cancel_state: Option<&str>,
) -> ReadOutcome {
    ReadOutcome {
        response: ServiceResponse {
            protocol_version: 1,
            request_id: "request".into(),
            connection_id: Some("conn".into()),
            status,
            job_id: Some("job".into()),
            result_id: None,
            data: json!({"columns":[{"name":"n"}],"rows":[[1]]}),
            page: Default::default(),
            limits: Default::default(),
            metrics: Metrics::default(),
            warnings: vec![],
            error,
        },
        cancel_state: cancel_state.map(str::to_owned),
    }
}

fn error(code: ErrorCode) -> ServiceError {
    ServiceError {
        outcome: Outcome::NotStarted,
        ..crate::services::error::failure(code)
    }
}

/// The service and the legacy tools share one profile, the way the app runs.
struct Served {
    harness: Harness,
    _registry: tokio::sync::MutexGuard<'static, ()>,
}
impl Served {
    async fn new() -> Self {
        let registry = crate::drivers::registry::REGISTRY_TEST_LOCK.lock().await;
        let harness = Harness::new().await;
        *CONFIG_DIR_OVERRIDE.lock().unwrap() = Some(harness.directory.0.clone());
        Self {
            harness,
            _registry: registry,
        }
    }
    async fn run(
        &self,
        config: &AppConfig,
        arguments: Value,
    ) -> (Result<Value, JsonRpcError>, CallAudit) {
        let route = Route::new(
            self.harness.core.clone(),
            self.harness.core.caller(CallerTransport::Mcp).unwrap(),
        );
        let mut arguments = args(arguments);
        arguments.insert("connection_id".into(), json!(self.harness.id));
        let mut audit = CallAudit::for_tool("run_query");
        let result = scope(
            route,
            dispatch_tool("run_query", Some(&arguments), config, "session", &mut audit),
        )
        .await;
        (result, audit)
    }
}
impl Drop for Served {
    fn drop(&mut self) {
        *CONFIG_DIR_OVERRIDE.lock().unwrap() = None;
    }
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

#[test]
fn timeout_ms는_없거나_1부터_120000_사이의_정수만_받는다() {
    // given
    let accepted = [
        (json!({}), None),
        (json!({"timeout_ms":null}), None),
        (json!({"timeout_ms":1}), Some(1)),
        (json!({"timeout_ms":30000}), Some(30000)),
        (json!({"timeout_ms":120000}), Some(120000)),
    ];
    let rejected = [
        json!({"timeout_ms":0}),
        json!({"timeout_ms":120001}),
        json!({"timeout_ms":-5}),
        json!({"timeout_ms":1.5}),
        json!({"timeout_ms":"1000"}),
        json!({"timeout_ms":true}),
    ];
    // when
    let actual: Vec<_> = accepted
        .iter()
        .map(|(arguments, _)| timeout_ms(&args(arguments.clone())))
        .collect();
    let errors: Vec<_> = rejected
        .iter()
        .map(|arguments| timeout_ms(&args(arguments.clone())).unwrap_err())
        .collect();
    // then
    for (actual, (_, expected)) in actual.into_iter().zip(accepted) {
        assert_eq!(actual.unwrap(), expected);
    }
    for error in errors {
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("timeout_ms"), "{}", error.message);
    }
}

#[test]
fn 도구_목록의_run_query에만_timeout_ms가_추가된다() {
    // given
    let tools = crate::mcp::handle_list_tools(&AppConfig::default()).unwrap();
    // when
    let actual = advertise_timeout(tools.clone());
    // then
    let property = |tools: &Value, name: &str| {
        tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()["inputSchema"]["properties"]
            .get("timeout_ms")
            .cloned()
    };
    let timeout = property(&actual, "run_query").unwrap();
    assert_eq!(timeout["type"], "integer");
    assert_eq!(timeout["minimum"], 1);
    assert_eq!(timeout["maximum"], 120000);
    assert!(timeout["description"].as_str().unwrap().contains("110000"));
    for name in [
        "list_connections",
        "list_databases",
        "list_tables",
        "describe_table",
    ] {
        assert!(property(&actual, name).is_none(), "{name}");
    }
    assert!(property(&tools, "run_query").is_none());
}

#[test]
fn 서비스_결과는_이전_서버와_같은_열_행_페이지_모양이_된다() {
    // given
    let data = json!({
        "kind":"tabular",
        "columns":[{"name":"id","type":"integer"},{"name":"name","type":"text"}],
        "rows":[[1,"ann"],[2,null]],
        "affected_rows":null,
    });
    // when
    let actual = serde_json::to_value(legacy_result(&data, true, 2)).unwrap();
    // then
    assert_eq!(
        actual,
        json!({
            "columns":["id","name"],
            "rows":[[1,"ann"],[2,null]],
            "affected_rows":0,
            "truncated":true,
            "pagination":{"page":1,"page_size":2,"total_rows":null,"has_more":true},
        })
    );
}

#[test]
fn 드라이버가_읽은_행_수를_보고해도_affected_rows는_이전_서버처럼_0이다() {
    // given
    let data = json!({"columns":[{"name":"a"}],"rows":[[1],[2]],"affected_rows":2});
    // when
    let actual = legacy_result(&data, false, 100);
    // then
    assert_eq!(actual.affected_rows, 0);
}

#[test]
fn 서비스가_실행_전에_거부한_문장은_기존_관문으로_넘긴다() {
    // given
    let codes = [
        ErrorCode::WriteNotAllowed,
        ErrorCode::WriteIntentRequired,
        ErrorCode::UnsupportedOperation,
        ErrorCode::CapabilityUnavailable,
        ErrorCode::OperationNotAllowed,
    ];
    // when
    let actual: Vec<_> = codes
        .iter()
        .map(|code| judge(outcome(Some(error(*code)), JobStatus::Failed, None), 1000))
        .collect();
    // then
    assert!(actual
        .iter()
        .all(|judged| matches!(judged, Judged::Fallback)));
}

#[test]
fn 쿼리_오류와_권한_오류는_넘기지_않고_코드_이름과_함께_돌려준다() {
    // given
    let cases = [
        (ErrorCode::DatabaseError, "DATABASE_ERROR"),
        (ErrorCode::DriverExited, "DRIVER_EXITED"),
        (ErrorCode::PermissionDenied, "PERMISSION_DENIED"),
        (ErrorCode::InvalidArgument, "INVALID_ARGUMENT"),
    ];
    // when
    let actual: Vec<_> = cases
        .iter()
        .map(|(code, _)| judge(outcome(Some(error(*code)), JobStatus::Failed, None), 1000))
        .collect();
    // then
    for (judged, (code, name)) in actual.into_iter().zip(cases) {
        let Judged::Failed(failed) = judged else {
            panic!("{name} was not returned as an error");
        };
        assert_eq!(failed.error.code, -32000);
        assert_eq!(
            failed.error.message,
            format!("{name}: {}", error(code).message)
        );
        assert_eq!(failed.status, "error");
    }
}

fn with_database_error(code: ErrorCode) -> ServiceError {
    ServiceError {
        message: "SQL Server error 241: Conversion failed [statement failure; severity 16; state 1; line 1]".into(),
        details: json!({"database_error":{"engine":"sqlserver","message":"SQL Server error 241: Conversion failed [statement failure; severity 16; state 1; line 1]"}}),
        ..error(code)
    }
}

#[test]
fn 데이터베이스가_알려준_문구는_코드_접두사_없이_그대로_돌려준다() {
    // given
    let error = with_database_error(ErrorCode::DatabaseError);

    // when
    let actual = judge(outcome(Some(error), JobStatus::Failed, None), 1000);

    // then
    let Judged::Failed(failed) = actual else {
        panic!("the database error was not returned as an error");
    };
    assert_eq!(failed.error.code, -32000);
    assert_eq!(
        failed.error.message,
        "SQL Server error 241: Conversion failed [statement failure; severity 16; state 1; line 1]"
    );
    assert_eq!(failed.error.data, None);
    assert_eq!(failed.status, "error");
}

#[test]
fn 쓰기_거부는_database_error가_있어도_기존_관문으로_넘긴다() {
    // given
    let refused = with_database_error(ErrorCode::WriteNotAllowed);

    // when
    let actual = judge(outcome(Some(refused), JobStatus::Failed, None), 1000);

    // then
    assert!(matches!(actual, Judged::Fallback));
}

#[test]
fn 마감이_지난_쿼리는_database_error가_있어도_QUERY_TIMEOUT으로_알린다() {
    // given
    let error = with_database_error(ErrorCode::DeadlineExceeded);

    // when
    let actual = judge(
        outcome(Some(error), JobStatus::Failed, Some("confirmed")),
        30000,
    );

    // then
    let Judged::Failed(failed) = actual else {
        panic!("the deadline was not returned as an error");
    };
    assert!(failed.error.message.starts_with("QUERY_TIMEOUT: "));
    assert_eq!(failed.status, "timeout");
}

#[test]
fn database_error가_없으면_계속_코드_이름과_고정_문구를_돌려준다() {
    // given
    let error = ServiceError {
        details: json!({"database_error":{"engine":"sqlserver"}}),
        ..error(ErrorCode::DriverExited)
    };

    // when
    let actual = service_failure(&error);

    // then
    assert_eq!(
        actual.error.message,
        format!("DRIVER_EXITED: {}", error.message)
    );
}

#[test]
fn 마감이_지난_쿼리는_취소_상태에_따라_QUERY_TIMEOUT으로_알린다() {
    // given
    let cases = [
        (
            Some("confirmed"),
            "QUERY_TIMEOUT: the query ran longer than timeout_ms=30000 and was cancelled on the database server",
        ),
        (
            Some("not_confirmed"),
            "QUERY_TIMEOUT: the query ran longer than timeout_ms=30000; the cancellation was asked of the database server but is not confirmed (cancel_state=not_confirmed), so the statement may still be running",
        ),
        (
            None,
            "QUERY_TIMEOUT: the query ran longer than timeout_ms=30000 and was stopped before it finished on the database server",
        ),
    ];
    // when
    let actual: Vec<_> = cases
        .iter()
        .map(|(state, _)| {
            judge(
                outcome(
                    Some(error(ErrorCode::DeadlineExceeded)),
                    JobStatus::Failed,
                    *state,
                ),
                30000,
            )
        })
        .collect();
    // then
    for (judged, (_, message)) in actual.into_iter().zip(cases) {
        let Judged::Failed(failed) = judged else {
            panic!("the deadline was not returned as an error");
        };
        assert_eq!(
            failed.error.message,
            format!("{message}. {LONGER_READS_HINT}")
        );
        assert!(failed.error.message.contains("query_execute"));
        assert!(failed.error.message.contains("deadline_ms up to 3600000"));
        assert!(failed.error.message.contains("job_get and job_result"));
        assert_eq!(failed.error.code, -32000);
        assert_eq!(failed.status, "timeout");
    }
}

#[tokio::test]
async fn 서비스를_거친_결과는_json_모양으로_돌아오고_감사_행수가_채워진다() {
    // given
    let served = Served::new().await;
    served.harness.state.rows.store(5, Ordering::SeqCst);
    // when
    let (result, audit) = served
        .run(
            &AppConfig::default(),
            json!({"query":"SELECT n FROM t","limit":2}),
        )
        .await;
    // then
    let result = result.unwrap();
    let body: Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(body["columns"], json!(["duplicate", "duplicate"]));
    assert_eq!(body["rows"].as_array().unwrap().len(), 2);
    assert_eq!(body["affected_rows"], 0);
    assert_eq!(body["truncated"], true);
    assert_eq!(
        body["pagination"],
        json!({"page":1,"page_size":2,"total_rows":null,"has_more":true})
    );
    assert_eq!(audit.rows, Some(2));
    assert_eq!(audit.status, "success");
    assert_eq!(served.harness.state.executions.load(Ordering::SeqCst), 1);
    assert_eq!(served.harness.state.legacy.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 서비스를_거친_결과도_toon_출력_형식을_따른다() {
    // given
    let served = Served::new().await;
    served.harness.state.rows.store(3, Ordering::SeqCst);
    let query = json!({"query":"SELECT n FROM t","limit":10});
    // when
    let (as_json, _) = served.run(&AppConfig::default(), query.clone()).await;
    let mut toon_query = query;
    toon_query["output_format"] = json!("toon");
    let (as_toon, _) = served.run(&AppConfig::default(), toon_query).await;
    // then
    let decoded: crate::models::QueryResult =
        serde_json::from_str(text(&as_json.unwrap())).unwrap();
    let expected = toon_format::encode_default(&decoded).unwrap();
    assert_eq!(text(&as_toon.unwrap()), expected);
}

#[tokio::test]
async fn database와_schema를_쓰면_서비스를_거쳐도_실제_값을_알린다() {
    // given
    let served = Served::new().await;
    // A caller can only pick a database the saved connection already selects.
    // when
    let (result, audit) = served
        .run(
            &AppConfig::default(),
            json!({"query":"SELECT 1","database":"fixture","schema":"app"}),
        )
        .await;
    // then
    result.unwrap();
    assert_eq!(audit.effective_database.as_deref(), Some("fixture"));
    assert_eq!(audit.effective_schema.as_deref(), Some("app"));
    assert_eq!(
        served
            .harness
            .state
            .params_databases
            .lock()
            .unwrap()
            .last()
            .unwrap(),
        "fixture"
    );
}

#[tokio::test]
async fn 서비스가_읽기로_받지_않는_쓰기_문장은_읽기_전용_관문이_막는다() {
    // given
    let served = Served::new().await;
    // when
    let (result, audit) = served
        .run(&read_only(), json!({"query":"DELETE FROM t"}))
        .await;
    // then
    let error = result.unwrap_err();
    assert!(error
        .message
        .starts_with("Query blocked by Tabularis read-only mode"));
    assert_eq!(audit.status, "blocked_readonly");
    assert_eq!(served.harness.state.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 한도가_1부터_1000_밖이면_서비스를_거치지_않고_기존_경로로_간다() {
    // given
    let served = Served::new().await;
    // when
    let (result, _) = served
        .run(
            &AppConfig::default(),
            json!({"query":"SELECT 1","limit":5000}),
        )
        .await;
    // then
    assert_eq!(result.unwrap_err().message, "Legacy path was not expected");
    assert_eq!(served.harness.state.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 서비스를_지원하지_않는_드라이버는_기존_경로로_실행한다() {
    // given
    let served = Served::new().await;
    *served.harness.state.capability_error.lock().unwrap() = Some(ErrorCode::CapabilityUnavailable);
    // when
    let (result, _) = served
        .run(&AppConfig::default(), json!({"query":"SELECT 1"}))
        .await;
    // then
    assert_eq!(result.unwrap_err().message, "Legacy path was not expected");
    assert_eq!(served.harness.state.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 서비스를_거친_쿼리가_timeout_ms를_넘기면_서버에서_취소하고_QUERY_TIMEOUT을_돌려준다() {
    // given
    let served = Served::new().await;
    served.harness.state.hold.store(true, Ordering::SeqCst);
    // when
    let (result, audit) = served
        .run(
            &AppConfig::default(),
            json!({"query":"SELECT 1","timeout_ms":150}),
        )
        .await;
    // then
    let error = result.unwrap_err();
    assert_eq!(error.code, -32000);
    assert_eq!(
        error.message,
        format!("QUERY_TIMEOUT: the query ran longer than timeout_ms=150 and was cancelled on the database server. {LONGER_READS_HINT}")
    );
    assert_eq!(audit.status, "timeout");
    assert_eq!(served.harness.state.cancels.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 잘못된_timeout_ms는_연결을_찾기_전에_거부한다() {
    // given
    let served = Served::new().await;
    // when
    let (result, audit) = served
        .run(
            &AppConfig::default(),
            json!({"query":"SELECT 1","timeout_ms":0}),
        )
        .await;
    // then
    let error = result.unwrap_err();
    assert_eq!(error.code, -32602);
    assert!(error.message.contains("timeout_ms"));
    assert_eq!(audit.connection_name, None);
    assert_eq!(served.harness.state.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 경로가_없으면_서비스를_시도하지_않는다() {
    // given
    let served = Served::new().await;
    let request = Request {
        connection: &served.harness.id,
        query: "SELECT 1",
        limit: 10,
        timeout_ms: None,
        args: &Map::new(),
    };
    // when
    let actual = run(&request).await.ok().flatten();
    // then
    assert!(actual.is_none());
    assert_eq!(served.harness.state.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 서비스를_쓰지_않는_경로는_timeout_ms가_지나면_기다리기를_멈추고_QUERY_TIMEOUT을_돌려준다()
{
    // given
    let _registry = crate::drivers::registry::REGISTRY_TEST_LOCK.lock().await;
    let directory = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("t-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&directory).unwrap();
    let driver_id = "test-mcp-legacy-timeout";
    std::fs::write(
        directory.join("connections.json"),
        json!([{"id":"slow-conn","name":"Slow","params":{"driver":driver_id,"host":"localhost","database":"db"}}])
            .to_string(),
    )
    .unwrap();
    *CONFIG_DIR_OVERRIDE.lock().unwrap() = Some(directory.clone());
    crate::drivers::registry::register_driver(crate::plugins::driver::with_test_driver_id(
        crate::plugins::driver::test_driver_result(|_request| {
            std::thread::sleep(std::time::Duration::from_millis(600));
            Ok(json!({"columns":["n"],"rows":[[1]],"affected_rows":0}))
        }),
        driver_id,
    ))
    .await;
    let mut audit = CallAudit::for_tool("run_query");
    let arguments = args(json!({"connection_id":"slow-conn","query":"SELECT 1","timeout_ms":100}));
    // when
    let started = std::time::Instant::now();
    let result = dispatch_tool(
        "run_query",
        Some(&arguments),
        &AppConfig::default(),
        "session",
        &mut audit,
    )
    .await;
    *CONFIG_DIR_OVERRIDE.lock().unwrap() = None;
    let _ = std::fs::remove_dir_all(&directory);
    // then
    let error = result.unwrap_err();
    assert_eq!(error.code, -32000);
    assert_eq!(
        error.message,
        format!("QUERY_TIMEOUT: the query ran longer than timeout_ms=100; the host stopped waiting and asked the driver to cancel, but this driver cannot confirm that the server stopped the statement. {LONGER_READS_HINT}")
    );
    assert_eq!(audit.status, "timeout");
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
}

#[tokio::test]
async fn 경로_범위_밖에서는_현재_경로가_없다() {
    // given
    let served = Served::new().await;
    let route = Route::new(
        served.harness.core.clone(),
        served.harness.core.caller(CallerTransport::Mcp).unwrap(),
    );
    // when
    let outside = current().is_some();
    let inside = scope(route, async { current().is_some() }).await;
    // then
    assert!(!outside);
    assert!(inside);
}
