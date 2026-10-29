#![allow(non_snake_case)]
use super::super::{
    backend::{PermissionScope, SavedConnection},
    contracts::Outcome,
    credentials::SessionSecretStore,
    policy::{AccessBackend, ApprovalBackend, ApprovalGrant},
    security::value_hash,
};
use super::test_support::{
    self as support, TestConnections, TestDirectory, TestDriver, TestGate, TestOAuth,
};
use super::*;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

struct Access(AtomicBool);
#[async_trait]
impl AccessBackend for Access {
    async fn permission(
        &self,
        _connection: &SavedConnection,
        caller: &CallerContext,
    ) -> ServiceResult<PermissionScope> {
        Ok(PermissionScope {
            identity: caller.caller_id.clone(),
            allow_writes: self.0.load(Ordering::SeqCst),
        })
    }
    async fn authorize_connection_change(
        &self,
        _caller: &CallerContext,
        _request: &ServiceRequest,
    ) -> ServiceResult<()> {
        Ok(())
    }
}
struct Approvals;
#[async_trait]
impl ApprovalBackend for Approvals {
    async fn consume(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
        request: &ServiceRequest,
        hash: &str,
    ) -> ServiceResult<ApprovalGrant> {
        Ok(ApprovalGrant {
            approval_id: "one-human-grant".into(),
            connection_id: connection.id.clone(),
            caller_id: caller.caller_id.clone(),
            owner_instance_id: caller.owner_instance_id.clone(),
            request_id: request.request_id.clone(),
            request_hash: hash.into(),
        })
    }
}
struct GatedApprovals(tokio::sync::Semaphore);
#[async_trait]
impl ApprovalBackend for GatedApprovals {
    async fn consume(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
        request: &ServiceRequest,
        hash: &str,
    ) -> ServiceResult<ApprovalGrant> {
        self.0.acquire().await.unwrap().forget();
        Approvals.consume(connection, caller, request, hash).await
    }
}
struct Harness {
    _directory: TestDirectory,
    transport: Arc<ServiceTransport>,
    connections: Arc<TestConnections>,
    driver: Arc<TestDriver>,
    access: Arc<Access>,
}
impl Harness {
    fn new() -> Self {
        Self::with_approvals(Arc::new(Approvals))
    }
    fn with_approvals(approvals: Arc<dyn ApprovalBackend>) -> Self {
        let directory = TestDirectory::new();
        let connections = Arc::new(TestConnections::new());
        let driver = Arc::new(TestDriver::new());
        let access = Arc::new(Access(AtomicBool::new(false)));
        let policy = Arc::new(ApprovalPolicy::new(access.clone(), approvals));
        let runtime = ServiceRuntime::new(
            directory.0.clone(),
            "owner".into(),
            connections.clone(),
            policy.clone(),
            driver.clone(),
            Arc::new(SessionSecretStore::default()),
            Arc::new(TestOAuth::new()),
        )
        .unwrap();
        Self {
            _directory: directory,
            transport: ServiceTransport::new(runtime, policy, Arc::new(UnavailableArtifacts))
                .unwrap(),
            connections,
            driver,
            access,
        }
    }
    fn allow_write(&self) {
        self.access.0.store(true, Ordering::SeqCst);
        self.connections
            .saved
            .lock()
            .unwrap()
            .get_mut("conn-a")
            .unwrap()
            .allow_writes = true;
    }
}
fn raw_query(text: &str) -> Value {
    serde_json::to_value(support::query(text)).unwrap()
}
fn mcp_caller() -> CallerContext {
    CallerContext {
        transport: CallerTransport::Mcp,
        ..support::caller()
    }
}
fn error_code(response: &ServiceResponse) -> Option<ErrorCode> {
    response.error.as_ref().map(|error| error.code)
}

#[tokio::test]
async fn 알수없는_호출자와_권한_입력은_드라이버에_전달되지_않는다() {
    // given
    let h = Harness::new();
    let mut raw = raw_query("select");
    raw["caller"] = json!({"permissions":{"write":true},"token":"canary-secret"});
    // when
    let actual = h.transport.submit(raw, support::caller()).await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn 프로토콜_불일치는_고정된_오류로_거부한다() {
    // given
    let h = Harness::new();
    let mut raw = raw_query("canary-secret-sql");
    raw["protocol_version"] = json!(2);
    // when
    let actual = h.transport.submit(raw, support::caller()).await;
    // then
    assert_eq!(
        actual.as_ref().unwrap_err().code,
        ErrorCode::ProtocolMismatch
    );
    assert!(!serde_json::to_string(&actual.unwrap_err())
        .unwrap()
        .contains("canary-secret"));
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
const EXCLUDED: [&str; 2] = ["credential_import", "connection_delete"];
fn catalog(h: &Harness) -> Vec<Value> {
    h.transport.tools().unwrap()["tools"]
        .as_array()
        .unwrap()
        .clone()
}
#[tokio::test]
async fn 도구_목록은_가져오기와_연결_삭제를_뺀_마흔여덟_작업과_미리보기와_작업목록과_서버_도구_둘과_개체_종류_도구이다(
) {
    // given
    let h = Harness::new();
    let pattern = |name: &str| {
        (1..=64).contains(&name.len())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    };
    // when
    let actual = catalog(&h);
    // then
    let names: Vec<&str> = actual
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 53);
    assert!(names.iter().all(|name| pattern(name)));
    assert!(names.iter().all(|name| !EXCLUDED.contains(name)));
    assert!(names.contains(&"table_preview") && names.contains(&"job_list"));
    assert!(names.contains(&"query_explain"));
    assert!(names.contains(&"server_activity") && names.contains(&"server_cancel"));
    assert!(names.contains(&"catalog_object_kinds"));
    assert!(names.contains(&"map_layer_add") && names.contains(&"connection_test"));
    assert!(!names.contains(&"query_execute_write"));
    assert_eq!(
        names.iter().collect::<std::collections::HashSet<_>>().len(),
        names.len()
    );
}
#[tokio::test]
async fn 모든_도구는_반환값과_쓰기_여부를_밝히는_실제_설명을_가진다() {
    // given
    let h = Harness::new();
    // when
    let actual = catalog(&h);
    // then
    for tool in &actual {
        let description = tool["description"].as_str().unwrap();
        assert!(description.len() > 30, "{}", tool["name"]);
        assert!(
            !description.starts_with("Common service"),
            "{}",
            tool["name"]
        );
        assert!(description.ends_with('.'), "{}", tool["name"]);
    }
    let write_tools: Vec<_> = actual
        .iter()
        .filter(|tool| tool["inputSchema"]["properties"].get("write").is_some())
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        write_tools,
        [
            "query_execute",
            "row_insert",
            "row_update",
            "row_delete",
            "document_create",
            "document_replace",
            "document_delete",
            "session_commit",
            "server_cancel"
        ]
    );
}
#[tokio::test]
async fn 도구_인자는_평평하며_봉투_필드는_도구가_쓰는_것만_노출한다() {
    // given
    let h = Harness::new();
    // when
    let actual = catalog(&h);
    let by_name = |name: &str| {
        actual
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()
            .clone()
    };
    // then
    let query = &by_name("query_execute")["inputSchema"];
    for field in [
        "connection_id",
        "text",
        "database",
        "schema",
        "wait_ms",
        "write",
        "deadline_ms",
        "session_id",
    ] {
        assert!(query["properties"].get(field).is_some(), "{field}");
    }
    assert!(query["properties"].get("input").is_none());
    assert!(query["properties"].get("operation").is_none());
    assert!(query["properties"].get("request_id").is_none());
    assert!(query["properties"].get("protocol_version").is_none());
    assert_eq!(query["required"], json!(["text", "connection_id"]));
    assert_eq!(query["properties"]["wait_ms"]["default"], 5000);
    assert_eq!(query["properties"]["wait_ms"]["maximum"], 60000);
    let update = &by_name("connection_update")["inputSchema"];
    assert!(update["properties"].get("expected_version").is_some());
    assert!(update["properties"].get("map_id").is_none());
    let map = &by_name("map_update")["inputSchema"];
    assert!(map["properties"].get("map_id").is_some());
    assert!(map["properties"].get("connection_id").is_none());
    let result = &by_name("result_get")["inputSchema"];
    assert_eq!(result["required"], json!(["result_id", "connection_id"]));
    assert_eq!(result["properties"]["limit"]["maximum"], 1000);
    let preview = &by_name("table_preview")["inputSchema"];
    assert_eq!(preview["properties"]["limit"]["default"], 10);
    assert_eq!(preview["properties"]["limit"]["maximum"], 1000);
    let list = &by_name("job_list")["inputSchema"];
    assert_eq!(list["properties"]["limit"]["maximum"], 200);
}
#[tokio::test]
async fn query_execute만_deadline_ms를_한_시간까지_알리고_다른_도구는_120000까지만_알린다() {
    // given
    let h = Harness::new();
    let actual = catalog(&h);
    let maximum = |name: &str| {
        actual.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"]["properties"]
            ["deadline_ms"]["maximum"]
            .clone()
    };

    // when
    let names = [
        "query_execute",
        "query_explain",
        "connection_test",
        "catalog_describe",
        "table_preview",
    ];
    let limits: Vec<Value> = names.iter().map(|name| maximum(name)).collect();

    // then
    assert_eq!(limits, [3_600_000, 120_000, 120_000, 120_000, 120_000]);
    let description = actual
        .iter()
        .find(|tool| tool["name"] == "query_execute")
        .unwrap()["inputSchema"]["properties"]["deadline_ms"]["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(description.contains("job_get") && description.contains("job_result"));
    assert!(description.contains("MCP server process exits"));
}
#[tokio::test]
async fn query_execute는_한_시간짜리_기한을_드라이버까지_전달하고_초과하면_거부한다() {
    // given
    let h = Harness::new();
    let with_deadline = |operation: &str, deadline_ms: u64| {
        let mut raw = raw_query("SELECT 1");
        raw["operation"] = json!(operation);
        raw["deadline_ms"] = json!(deadline_ms);
        raw
    };

    // when
    let accepted = h
        .transport
        .call(
            with_deadline("query.execute", 3_600_000),
            support::caller(),
            None,
        )
        .await
        .unwrap();
    let too_long = h
        .transport
        .call(
            with_deadline("query.execute", 3_600_001),
            support::caller(),
            None,
        )
        .await;

    // then
    assert_eq!(accepted.status, JobStatus::Succeeded);
    assert_eq!(too_long.unwrap_err().code, ErrorCode::InvalidArgument);
    let delivered = h.driver.deadlines.lock().unwrap().clone();
    assert_eq!(delivered.len(), 1);
    assert!(
        delivered[0] > 3_590_000 && delivered[0] <= 3_600_000,
        "{delivered:?}"
    );
}
#[test]
fn 포그라운드_대기_상한은_한_시간짜리_작업과_유예를_담는다() {
    // given
    let hour = 3_600_000;

    // when
    let actual = request::MAX_FOREGROUND_MS;

    // then
    assert_eq!(actual, hour + 2_000);
}
#[test]
fn stdio_요청_기한은_한_시간까지_자르지_않고_MCP_대기_시간만_더한다() {
    // given
    let hour = 3_600_000;
    let envelope = |deadline: Value, wait: Value| json!({"deadline_ms":deadline,"wait_ms":wait});

    // when
    let actual = [
        stdio::request_window_ms(&envelope(json!(hour), json!(null)), Mode::Cli),
        stdio::request_window_ms(&envelope(json!(hour + 1), json!(null)), Mode::Cli),
        stdio::request_window_ms(&envelope(json!(hour), json!(7_000)), Mode::Mcp),
        stdio::request_window_ms(&envelope(json!(null), json!(null)), Mode::Cli),
        stdio::request_window_ms(&envelope(json!(0), json!(null)), Mode::Cli),
    ];

    // then
    assert_eq!(actual, [hour, hour, hour + 7_000, 30_000, 1]);
}
#[tokio::test]
async fn 다른_작업은_120001_이상의_기한을_거부한다() {
    // given
    let h = Harness::new();
    let mut raw =
        serde_json::to_value(support::request(Operation::CatalogDatabases, json!({}))).unwrap();
    raw["deadline_ms"] = json!(120_001);

    // when
    let actual = h.transport.call(raw, support::caller(), None).await;

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn catalog_describe는_trigger의_소유_테이블을_받아_드라이버로_넘긴다() {
    // given
    let h = Harness::new();
    let schema = catalog(&h)
        .into_iter()
        .find(|tool| tool["name"] == "catalog_describe")
        .unwrap()["inputSchema"]["properties"]["owner_table"]
        .clone();
    let call = |owner_table: &str| {
        h.transport.call_tool(
            "catalog_describe",
            json!({"connection_id":"conn-a","table":"audit","kind":"trigger","owner_table":owner_table}),
            mcp_caller(),
            None,
        )
    };
    // when
    let accepted = call("orders").await;
    let empty = call("").await;
    // then
    assert_eq!(schema["type"], "string");
    assert_eq!(schema["minLength"], 1);
    // The fixture driver cannot describe triggers; the argument itself was accepted.
    assert!(accepted
        .unwrap()
        .to_string()
        .contains("CAPABILITY_UNAVAILABLE"));
    assert_eq!(empty.unwrap_err().code, ErrorCode::InvalidArgument);
}
#[tokio::test]
async fn 알수없는_인자가_있으면_실행하지_않는다() {
    // given
    let h = Harness::new();
    let arguments = json!({"connection_id":"conn-a","unexpected":true});
    // when
    let actual = h
        .transport
        .call_tool("connection_test", arguments, mcp_caller(), None)
        .await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn 제외된_두_도구는_도구_이름과_점_이름_모두에서_거부한다() {
    // given
    let h = Harness::new();
    let arguments = json!({"connection_id":"conn-a","expected_version":1});
    // when
    let mut actual = Vec::new();
    for name in [
        "credential_import",
        "credential.import",
        "connection_delete",
        "connection.delete",
    ] {
        actual.push(
            h.transport
                .call_tool(name, arguments.clone(), mcp_caller(), None)
                .await,
        );
    }
    let submitted = {
        let mut request = support::request(Operation::ConnectionDelete, json!({}));
        request.expected_version = Some(1);
        h.transport
            .submit(serde_json::to_value(request).unwrap(), mcp_caller())
            .await
    };
    // then
    assert!(actual
        .iter()
        .all(|result| result.as_ref().unwrap_err().code == ErrorCode::OperationNotAllowed));
    assert_eq!(submitted.unwrap_err().code, ErrorCode::OperationNotAllowed);
    assert_eq!(h.connections.writes.load(Ordering::SeqCst), 0);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn 쓰기_의도가_있어도_연결의_쓰기_승인을_우회하지_않는다() {
    // given
    let h = Harness::new();
    let arguments =
        json!({"connection_id":"conn-a","text":"UPDATE fixture SET value = 1","write":true});
    // when
    let actual = h
        .transport
        .call_tool("query_execute", arguments, mcp_caller(), None)
        .await
        .unwrap();
    // then
    assert_eq!(actual["isError"], true);
    assert_eq!(
        actual["structuredContent"]["error"]["code"],
        "WRITE_NOT_ALLOWED"
    );
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn 쓰기_의도_없는_쓰기_문장은_드라이버에_닿기_전에_거부한다() {
    // given
    let h = Harness::new();
    h.allow_write();
    let arguments = json!({"connection_id":"conn-a","text":"UPDATE fixture SET value = 1"});
    // when
    let actual = h
        .transport
        .call_tool("query_execute", arguments, mcp_caller(), None)
        .await
        .unwrap();
    // then
    assert_eq!(
        actual["structuredContent"]["error"]["code"],
        "WRITE_INTENT_REQUIRED"
    );
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
async fn approved_twice(h: &Harness) -> (ServiceResponse, ServiceResponse) {
    let first = h
        .transport
        .run_one(raw_query("write"), support::caller(), true, None)
        .await
        .unwrap();
    let second = h
        .transport
        .run_one(raw_query("write"), support::caller(), true, None)
        .await
        .unwrap();
    (first, second)
}
#[tokio::test]
async fn 명시적_쓰기_의도와_사람의_승인은_한번만_소비된다() {
    // given
    let h = Harness::new();
    h.allow_write();
    // when
    let actual = approved_twice(&h).await;
    // then
    assert_eq!(actual.0.status, JobStatus::Succeeded);
    assert_eq!(error_code(&actual.1), Some(ErrorCode::WriteNotAllowed));
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 1);
    assert!(!h.driver.contexts.lock().unwrap()[0].1);
}
#[tokio::test]
async fn 승인_대기_시간은_호출자_기한을_소모하지_않아_승인_뒤_남은_시간으로_실행을_끝낸다() {
    // given
    let gate = Arc::new(GatedApprovals(tokio::sync::Semaphore::new(0)));
    let h = Harness::with_approvals(gate.clone());
    h.allow_write();
    let held = TestGate::new();
    h.driver
        .gates
        .lock()
        .unwrap()
        .insert("held".into(), held.clone());
    let mut raw = serde_json::to_value(support::write_query("held")).unwrap();
    raw["deadline_ms"] = json!(500);
    let transport = h.transport.clone();
    let task = tokio::spawn(async move {
        transport
            .run_one(raw, support::caller(), true, None)
            .await
            .unwrap()
    });

    // when
    tokio::time::sleep(Duration::from_millis(3_000)).await;
    let waiting = !task.is_finished();
    gate.0.add_permits(1);
    held.wait_entered().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    held.release();
    let actual = task.await.unwrap();

    // then
    assert!(waiting);
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn 의도가_없는_쿼리는_읽기전용_물리_경로로_전달된다() {
    // given
    let h = Harness::new();
    h.allow_write();
    // when
    let actual = h
        .transport
        .call(raw_query("arbitrary-sql"), support::caller(), None)
        .await
        .unwrap();
    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert!(h.driver.contexts.lock().unwrap()[0].1);
}
#[tokio::test]
async fn 사전_취소는_의도등록과_드라이버_실행을_막는다() {
    // given
    let h = Harness::new();
    let signal = CancellationSignal::default();
    signal.cancel();
    // when
    let actual = h
        .transport
        .run_one(raw_query("select"), support::caller(), true, Some(signal))
        .await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::Cancelled);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn 인증_결과의_중첩된_디바이스_코드는_MCP에_표시하지_않는다() {
    // given
    let request = support::request(Operation::AuthBegin, json!({"auth_mode":"entra_user"}));
    let mut response = ServiceResponse::failure(&request, ErrorCode::AuthRequired, "fixed");
    response.error = None;
    response.status = JobStatus::Succeeded;
    response.data = json!({"auth_job_id":"auth-job","state":"interaction_required","user_action_required":true,"challenge":{"verification_uri":"canary-uri","user_code":"canary-code","nested":{"user_code":"canary-nested"}}});
    // when
    let actual = present::tool_result(Operation::AuthBegin, &response, None);
    // then
    assert!(!actual.to_string().contains("canary"));
    assert_eq!(
        actual["structuredContent"]["data"]["auth_job_id"],
        "auth-job"
    );
    assert_eq!(response.data["challenge"]["user_code"], "canary-code");
}
#[test]
fn 일반_문서의_challenge_필드는_인증정보로_오판하지_않는다() {
    // given
    let request = support::query("select");
    let mut response = ServiceResponse::failure(&request, ErrorCode::InvalidArgument, "fixed");
    response.error = None;
    response.status = JobStatus::Succeeded;
    response.data = json!({"values":[{"challenge":"ordinary-document-value"}]});
    // when
    let actual = present::tool_result(Operation::QueryExecute, &response, None);
    // then
    assert_eq!(
        actual["structuredContent"]["data"]["values"][0]["challenge"],
        "ordinary-document-value"
    );
}
#[test]
fn 화면_예산을_넘는_실행_계획은_자르지_않고_통째로_빼고_생략을_알린다() {
    // given
    let request = support::query("select");
    let mut response = ServiceResponse::failure(&request, ErrorCode::InvalidArgument, "fixed");
    response.error = None;
    response.status = JobStatus::Succeeded;
    let plan = format!("<ShowPlanXML>{}</ShowPlanXML>", "x".repeat(32_700));
    response.data = json!({"engine":"sqlserver","format":"sqlserver-showplan-xml","executed":false,
        "summary":{"total_cost":1.5,"nodes":[]},"plan":plan});
    // when
    let actual = present::tool_result(Operation::QueryExplain, &response, None);
    // then
    let data = &actual["structuredContent"]["data"];
    assert!(data.get("plan").is_none(), "{data}");
    assert_eq!(data["plan_omitted"], true);
    assert_eq!(data["summary"]["total_cost"], 1.5);
    assert!(serde_json::to_vec(&actual).unwrap().len() <= 64 * 1024);
}
#[test]
fn 일반_목록은_응답이_맞으면_전부_싣고_진단_배열만_다섯개로_줄인다() {
    // given
    let request = support::query("select");
    let mut response = ServiceResponse::failure(&request, ErrorCode::InvalidArgument, "fixed");
    response.error = None;
    response.status = JobStatus::Succeeded;
    response.data = json!({
        "maps": (0..40).map(|id| json!({"id":format!("map-{id}"),"name":"synthetic"})).collect::<Vec<_>>(),
        "warnings": (0..12).map(|id| json!(format!("warning_{id}"))).collect::<Vec<_>>(),
    });

    // when
    let actual = present::tool_result(Operation::MapList, &response, None);

    // then
    let data = &actual["structuredContent"]["data"];
    assert_eq!(data["maps"].as_array().unwrap().len(), 40);
    assert_eq!(data["warnings"].as_array().unwrap().len(), 5);
    assert_eq!(actual["structuredContent"]["presentation_truncated"], true);
}
fn describe_response(data: Value) -> ServiceResponse {
    let request = support::request(
        Operation::CatalogDescribe,
        json!({"table":{"database":null,"schema":null,"table":"orders"}}),
    );
    let mut response = ServiceResponse::failure(&request, ErrorCode::InvalidArgument, "fixed");
    response.error = None;
    response.status = JobStatus::Succeeded;
    response.data = data;
    response
}
fn foreign_keys(count: usize) -> Vec<Value> {
    (0..count)
        .map(|id| json!({"name":format!("fk_{id}"),"columns":["a"],"ref_schema":"public","ref_table":"parent","ref_columns":["id"],"on_update":"NO ACTION","on_delete":"CASCADE"}))
        .collect()
}
#[test]
fn describe의_키_정보는_다섯개로_줄이지_않고_긴_정의도_통째로_싣는다() {
    // given
    let definition = "select 1 ".repeat(400);
    let response = describe_response(json!({
        "columns":[{"name":"id","native_type":"int4"}],
        "foreign_keys":foreign_keys(12),
        "indexes":(0..9).map(|id| json!({"name":format!("ix_{id}"),"columns":["a"],"included_columns":[],"is_unique":false,"is_primary":false,"type":"btree","filter":null,"definition":"c".repeat(1500)})).collect::<Vec<_>>(),
        "keys":[{"name":"pk","kind":"primary_key","columns":["id"],"index":"pk"}],
        "check_constraints":[{"name":"ck","definition":"d".repeat(1800)}],
        "definition":definition,
        "properties":{"labels":(0..8).map(|id| json!(format!("label_{id}"))).collect::<Vec<_>>()},
    }));
    // when
    let actual = present::tool_result(Operation::CatalogDescribe, &response, None);
    // then
    let data = &actual["structuredContent"]["data"];
    assert_eq!(data["foreign_keys"].as_array().unwrap().len(), 12);
    assert_eq!(data["indexes"].as_array().unwrap().len(), 9);
    assert_eq!(data["indexes"][8]["definition"].as_str().unwrap().len(), 1500);
    assert_eq!(data["check_constraints"][0]["definition"].as_str().unwrap().len(), 1800);
    assert_eq!(data["properties"]["labels"].as_array().unwrap().len(), 8);
    assert_eq!(data["definition"], definition);
    assert!(data.get("truncation_reason").is_none());
    assert_eq!(actual["structuredContent"]["presentation_truncated"], false);
    assert!(serde_json::to_vec(&actual).unwrap().len() <= 64 * 1024);
}
#[test]
fn describe의_정의가_응답에_맞지_않으면_정의만_줄이고_사유를_밝힌다() {
    // given
    let definition = "가".repeat(40_000);
    let response = describe_response(json!({
        "columns":[{"name":"id","native_type":"int4"},{"name":"qty","native_type":"int4"}],
        "foreign_keys":foreign_keys(12),
        "definition":definition,
    }));
    // when
    let actual = present::tool_result(Operation::CatalogDescribe, &response, None);
    // then
    let data = &actual["structuredContent"]["data"];
    let shown = data["definition"].as_str().unwrap();
    assert!(shown.len() < definition.len() && shown.len() > 1024);
    assert!(definition.starts_with(shown));
    assert_eq!(data["truncated"], true);
    assert_eq!(data["truncation_reason"], "definition_shortened_to_fit_response");
    assert_eq!(data["definition_bytes"], definition.len());
    assert_eq!(data["foreign_keys"].as_array().unwrap().len(), 12);
    assert_eq!(data["columns"].as_array().unwrap().len(), 2);
    assert_eq!(data["has_more"], false);
    assert_eq!(actual["structuredContent"]["presentation_truncated"], true);
    assert!(serde_json::to_vec(&actual).unwrap().len() <= 64 * 1024);
}
#[test]
fn describe가_아닌_카탈로그_응답의_나머지_필드는_기존처럼_다섯개로_줄인다() {
    // given
    let request = support::request(
        Operation::CatalogObjects,
        json!({"database":"fixture","schema":null}),
    );
    let mut response = ServiceResponse::failure(&request, ErrorCode::InvalidArgument, "fixed");
    response.error = None;
    response.status = JobStatus::Succeeded;
    response.data = json!({
        "objects":[{"name":"orders","kind":"table"}],
        "foreign_keys":foreign_keys(12),
    });
    // when
    let actual = present::tool_result(Operation::CatalogObjects, &response, None);
    // then
    let data = &actual["structuredContent"]["data"];
    assert_eq!(data["foreign_keys"].as_array().unwrap().len(), 5);
    assert_eq!(actual["structuredContent"]["presentation_truncated"], true);
}
#[test]
fn 공간_표시는_다섯_샘플과_참조만_보존하고_좌표는_제외한다() {
    // given
    let request = support::query("select");
    let mut response = ServiceResponse::failure(&request, ErrorCode::InvalidArgument, "fixed");
    response.error = None;
    response.status = JobStatus::Succeeded;
    response.result_id = Some("result-reference".into());
    response.data = json!({"artifact_id":"artifact-reference","features":(0..20).map(|id|json!({"id":id,"geometry":{"type":"Polygon","coordinates":["coordinate-canary".repeat(5000)]},"properties":{"label":"가".repeat(20000)}})).collect::<Vec<_>>()});
    // when
    let actual = present::tool_result(Operation::SpatialQueryResult, &response, None);
    // then
    assert_eq!(actual["structuredContent"]["data"]["features_count"], 20);
    assert_eq!(
        actual["structuredContent"]["data"]["features_sample"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(actual["structuredContent"]["result_id"], "result-reference");
    assert_eq!(
        actual["structuredContent"]["data"]["artifact_id"],
        "artifact-reference"
    );
    assert!(!actual.to_string().contains("coordinate-canary"));
    assert!(serde_json::to_vec(&actual).unwrap().len() <= 65536);
    assert_eq!(actual["structuredContent"]["presentation_truncated"], true);
    assert_eq!(
        actual["structuredContent"]["limits"],
        serde_json::to_value(&response.limits).unwrap()
    );
}
#[test]
fn artifact_URI는_정규_UUID와_고정_청크_한계만_허용한다() {
    // given
    let id = format!("artifact-{}", uuid::Uuid::new_v4());
    let valid = format!("tabularis://artifact/{id}?offset=0&limit=65536");
    let invalid = [
        format!("tabularis://artifact/{id}?offset=0&offset=1&limit=2"),
        format!("tabularis://artifact/{id}?offset=18446744073709551616&limit=1"),
        format!("tabularis://artifact/{id}?offset=0&limit=65537"),
        format!("tabularis://artifact/{id}%2f../x"),
        format!("tabularis://artifact/{id}#fragment"),
        format!("tabularis://artifact/{id}?offset=0&limit=1&path=x"),
    ];
    // when
    let actual = (
        ArtifactResource::parse(&valid),
        invalid
            .iter()
            .map(|uri| ArtifactResource::parse(uri))
            .collect::<Vec<_>>(),
        ArtifactResource::parse("tabularis://connection/fixture"),
    );
    // then
    assert_eq!(
        actual.0.unwrap(),
        Some(ArtifactResource::Chunk {
            id,
            offset: 0,
            limit: 65536
        })
    );
    assert!(actual.1.iter().all(Result::is_err));
    assert_eq!(actual.2.unwrap(), None);
}
struct Artifacts {
    mismatched: bool,
}
#[async_trait]
impl TransportArtifacts for Artifacts {
    async fn metadata(&self, id: &str, _caller: &CallerContext) -> ServiceResult<Value> {
        Ok(json!({"artifact_id":id,"size":3,"checksum":format!("{:x}",Sha256::digest(b"abc"))}))
    }
    async fn read_chunk(
        &self,
        id: &str,
        _caller: &CallerContext,
        offset: u64,
        _limit: usize,
    ) -> ServiceResult<Value> {
        Ok(
            json!({"artifact_id":id,"offset":if self.mismatched{offset+1}else{offset},"bytes_base64":STANDARD.encode(b"abc"),"eof":true,"checksum":format!("{:x}",Sha256::digest(b"abc")),"chunk_checksum":format!("{:x}",Sha256::digest(b"abc"))}),
        )
    }
}
async fn artifact_read(mismatch: bool) -> ServiceResult<ResourceRead> {
    let h = Harness::new();
    let transport = ServiceTransport::new(
        h.transport.runtime.clone(),
        h.transport.policy.clone(),
        Arc::new(Artifacts {
            mismatched: mismatch,
        }),
    )
    .unwrap();
    transport
        .read_resource(
            &format!(
                "tabularis://artifact/artifact-{}?offset=7&limit=3",
                uuid::Uuid::new_v4()
            ),
            &support::caller(),
        )
        .await
}
#[tokio::test]
async fn 청크는_권한_어댑터의_바이트를_blob으로만_표시한다() {
    // given
    let mismatch = false;
    // when
    let actual = artifact_read(mismatch).await.unwrap();
    // then
    assert!(
        matches!(actual,ResourceRead::Content(ref value) if value["contents"][0]["blob"]=="YWJj" && value["offset"]==7 && value.get("bytes_base64").is_none())
    );
}
#[tokio::test]
async fn 청크의_offset이_요청과_다르면_거부한다() {
    // given
    let mismatch = true;
    // when
    let actual = artifact_read(mismatch).await;
    // then
    assert!(matches!(actual,Err(ref error) if error.code==ErrorCode::InvalidArgument));
}

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);
impl tokio::io::AsyncWrite for Output {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        std::task::Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
async fn stream(h: &Harness, frames: Vec<Value>, mode: Mode) -> (i32, Vec<Value>) {
    let mut input = Vec::new();
    for frame in frames {
        input.extend_from_slice(frame.to_string().as_bytes());
        input.push(b'\n');
    }
    let output = Output::default();
    let code = h
        .transport
        .run_stdio(
            tokio::io::BufReader::new(std::io::Cursor::new(input)),
            output.clone(),
            support::caller(),
            mode,
            false,
            Default::default(),
        )
        .await
        .unwrap();
    let values = output
        .0
        .lock()
        .unwrap()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    (code, values)
}
#[tokio::test]
async fn MCP_초기화는_지원버전을_보존하고_알림에는_응답하지_않는다() {
    // given
    let h = Harness::new();
    let frames = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"other"}}),
    ];
    // when
    let actual = stream(&h, frames, Mode::Mcp).await;
    // then
    assert_eq!(actual.0, 0);
    assert_eq!(actual.1.len(), 2);
    assert!(actual
        .1
        .iter()
        .any(|value| value["id"] == 1 && value["result"]["protocolVersion"] == "2024-11-05"));
    assert!(actual
        .1
        .iter()
        .any(|value| value["id"] == 2 && value["result"]["protocolVersion"] == "2025-06-18"));
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn MCP_params의_meta_객체는_무시하고_요청을_처리한다() {
    // given
    let h = Harness::new();
    let frames = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"progressToken":1}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"query_execute","arguments":{"connection_id":"conn-a","text":"select 1"},"_meta":{"claudecode/toolUseId":"toolu_1"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"query_execute","arguments":{"connection_id":"conn-a","text":"select 1"},"_meta":"not-an-object"}}),
    ];
    // when
    let actual = stream(&h, frames, Mode::Mcp).await;
    // then
    let by_id = |id: i64| actual.1.iter().find(|value| value["id"] == id).unwrap();
    assert!(by_id(1)["result"]["tools"].is_array());
    assert!(by_id(2).get("result").is_some());
    assert_eq!(by_id(3)["error"]["data"]["code"], "INVALID_ARGUMENT");
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn MCP_안전하지_않은_정수_ID는_실행_전에_거부한다() {
    // given
    let h = Harness::new();
    let frames = vec![
        json!({"jsonrpc":"2.0","id":9007199254740992_u64,"method":"tools/call","params":{"name":"query_execute","arguments":{"connection_id":"conn-a","text":"select 1"}}}),
    ];
    // when
    let actual = stream(&h, frames, Mode::Mcp).await;
    // then
    assert_eq!(actual.0, 2);
    assert_eq!(actual.1[0]["id"], Value::Null);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn EOF는_접수한_쿼리를_완료하고_한줄_JSON만_쓴다() {
    // given
    let h = Harness::new();
    let frames = vec![raw_query("slow"), raw_query("select")];
    // when
    let actual = stream(&h, frames, Mode::Cli).await;
    // then
    assert_eq!(actual.0, 0);
    assert_eq!(actual.1.len(), 2);
    assert!(actual.1.iter().all(|value| value["status"] == "succeeded"));
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 2);
}
async fn overlapping(h: &Harness) -> (ServiceResponse, ServiceResponse, usize) {
    let gate = TestGate::new();
    h.driver
        .gates
        .lock()
        .unwrap()
        .insert("blocked".into(), gate.clone());
    let first = h.transport.clone();
    let task = tokio::spawn(async move {
        first
            .call(raw_query("blocked"), support::caller(), None)
            .await
            .unwrap()
    });
    gate.wait_entered().await;
    let second = h
        .transport
        .call(raw_query("select"), support::caller(), None)
        .await
        .unwrap();
    let peak = h.driver.peak.load(Ordering::SeqCst);
    gate.release();
    (task.await.unwrap(), second, peak)
}
#[tokio::test]
async fn 막힌_첫_쿼리와_독립_쿼리는_동시에_진행한다() {
    // given
    let h = Harness::new();
    // when
    let actual = overlapping(&h).await;
    // then
    assert_eq!(actual.0.status, JobStatus::Succeeded);
    assert_eq!(actual.1.status, JobStatus::Succeeded);
    assert!(actual.2 >= 2);
}
async fn cancelled(h: &Harness) -> ServiceResponse {
    let signal = CancellationSignal::default();
    let transport = h.transport.clone();
    let cancellation = signal.clone();
    let task = tokio::spawn(async move {
        transport
            .call(raw_query("wait"), support::caller(), Some(cancellation))
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while h.driver.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    signal.cancel();
    task.await.unwrap()
}
#[tokio::test]
async fn 취소_신호는_한번_전달하고_확인된_최종결과를_기다린다() {
    // given
    let h = Harness::new();
    // when
    let actual = cancelled(&h).await;
    // then
    assert_eq!(actual.status, JobStatus::Cancelled);
    assert_eq!(h.driver.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn 불명확한_결과는_롤백으로_표시하지_않고_실행오류로_종료한다() {
    // given
    let request = support::query("write");
    // when
    let actual = request::unknown_response(&request, Some("job-ref".into()));
    // then
    assert_eq!(actual.request_id, request.request_id);
    assert_eq!(actual.status, JobStatus::OutcomeUnknown);
    assert_eq!(actual.error.as_ref().unwrap().outcome, Outcome::Unknown);
    assert_eq!(exit_code(&actual), 4);
}
#[test]
fn 승인_요청의_해시는_원본_SQL_매개변수를_그대로_포함한다() {
    // given
    let mut query = support::query("select @value");
    query.input["parameters"] = json!([{"name":"@value","value":{"nested":[null,true,3]}}]);
    let original = query.input.clone();
    // when
    let actual = request::decode(
        &SchemaValidator::new().unwrap(),
        &serde_json::to_value(&query).unwrap(),
    )
    .unwrap();
    // then
    assert_eq!(actual.input, original);
    assert_eq!(value_hash(&actual.input), value_hash(&original));
}

struct PendingOAuth;
#[async_trait]
impl super::super::oauth::OAuthProvider for PendingOAuth {
    async fn device_begin(
        &self,
        _config: &super::super::auth::AuthConfig,
    ) -> ServiceResult<oauth2::StandardDeviceAuthorizationResponse> {
        serde_json::from_value(json!({"device_code":"private-device-canary","user_code":"TEST-CODE","verification_uri":"https://microsoft.com/devicelogin","expires_in":900,"interval":1})).map_err(|_|failure(ErrorCode::AuthRequired))
    }
    async fn device_finish(
        &self,
        _config: &super::super::auth::AuthConfig,
        _device: oauth2::StandardDeviceAuthorizationResponse,
        signal: CancellationSignal,
    ) -> ServiceResult<super::super::auth::OAuthGrant> {
        signal.cancelled().await;
        Err(failure(ErrorCode::Cancelled))
    }
    async fn client_credentials(
        &self,
        _config: &super::super::auth::AuthConfig,
        _secret: &super::super::credentials::Secret,
    ) -> ServiceResult<super::super::auth::OAuthGrant> {
        Err(failure(ErrorCode::CapabilityUnavailable))
    }
    async fn refresh(
        &self,
        _config: &super::super::auth::AuthConfig,
        _refresh: &super::super::credentials::Secret,
        _previous: Option<&str>,
    ) -> ServiceResult<super::super::auth::OAuthGrant> {
        Err(failure(ErrorCode::CapabilityUnavailable))
    }
}
async fn auth_challenge(transport: CallerTransport) -> (ServiceResponse, Value) {
    let directory = TestDirectory::new();
    let connections = Arc::new(TestConnections::new());
    connections
        .saved
        .lock()
        .unwrap()
        .insert("conn-a".into(), support::entra_connection("entra_user"));
    let policy = Arc::new(ApprovalPolicy::new(
        Arc::new(Access(AtomicBool::new(false))),
        Arc::new(Approvals),
    ));
    let runtime = ServiceRuntime::new(
        directory.0.clone(),
        "owner".into(),
        connections,
        policy.clone(),
        Arc::new(TestDriver::new()),
        Arc::new(SessionSecretStore::default()),
        Arc::new(PendingOAuth),
    )
    .unwrap();
    let facade =
        ServiceTransport::new(runtime.clone(), policy, Arc::new(UnavailableArtifacts)).unwrap();
    let request = support::request(
        Operation::AuthBegin,
        json!({"auth_mode":"entra_user","persistence":"session"}),
    );
    let caller = CallerContext {
        transport,
        ..support::caller()
    };
    let response = facade
        .call(serde_json::to_value(request).unwrap(), caller.clone(), None)
        .await
        .unwrap();
    let record = runtime
        .jobs
        .get(response.job_id.as_ref().unwrap(), "owner", &caller)
        .unwrap();
    runtime.shutdown().await.unwrap();
    (response, serde_json::to_value(record.response).unwrap())
}
#[tokio::test]
async fn CLI는_인증_challenge를_일시적으로_받지만_job에는_저장하지_않는다() {
    // given
    let transport = CallerTransport::Cli;
    // when
    let actual = auth_challenge(transport).await;
    // then
    assert_eq!(actual.0.status, JobStatus::Succeeded);
    assert_eq!(actual.0.data["challenge"]["user_code"], "TEST-CODE");
    assert!(actual.1["data"].get("challenge").is_none());
    assert!(!actual.0.data.to_string().contains("private-device-canary"));
}
#[tokio::test]
async fn MCP는_인증_begin_이후에도_challenge를_가져오지_않는다() {
    // given
    let transport = CallerTransport::Mcp;
    // when
    let actual = auth_challenge(transport).await;
    // then
    assert_eq!(actual.0.status, JobStatus::Succeeded);
    assert!(actual.0.data.get("challenge").is_none());
    assert!(actual.1["data"].get("challenge").is_none());
}
async fn isolated_ids(h: &Harness) -> (ServiceResponse, ServiceResponse) {
    let first = raw_query("slow");
    let mut second = first.clone();
    second["connection_id"] = json!("conn-b");
    let (first, second) = tokio::join!(
        h.transport.call(first, support::caller(), None),
        h.transport.call(second, support::caller(), None)
    );
    (first.unwrap(), second.unwrap())
}
#[tokio::test]
async fn 같은_요청_ID도_연결이_다르면_독립적으로_완료한다() {
    // given
    let h = Harness::new();
    // when
    let actual = isolated_ids(&h).await;
    // then
    assert_eq!(actual.0.request_id, actual.1.request_id);
    assert_ne!(actual.0.connection_id, actual.1.connection_id);
    assert_eq!(actual.0.status, JobStatus::Succeeded);
    assert_eq!(actual.1.status, JobStatus::Succeeded);
    assert_ne!(actual.0.job_id, actual.1.job_id);
}
async fn control_while_blocked(h: &Harness) -> (i32, Vec<Value>, usize) {
    let queued = h
        .transport
        .submit(raw_query("wait"), support::caller())
        .await
        .unwrap();
    let gate = TestGate::new();
    h.driver
        .gates
        .lock()
        .unwrap()
        .insert("blocked-stream".into(), gate.clone());
    let control = support::request(
        Operation::JobCancel,
        json!({"job_id":queued.job_id,"owner_instance_id":"owner"}),
    );
    let frames = vec![
        raw_query("blocked-stream"),
        serde_json::to_value(control).unwrap(),
    ];
    let output = Output::default();
    let observed = output.clone();
    let transport = h.transport.clone();
    let mut input = Vec::new();
    for frame in frames {
        input.extend_from_slice(frame.to_string().as_bytes());
        input.push(b'\n');
    }
    let task = tokio::spawn(async move {
        transport
            .run_stdio(
                tokio::io::BufReader::new(std::io::Cursor::new(input)),
                output,
                support::caller(),
                Mode::Cli,
                false,
                Default::default(),
            )
            .await
            .unwrap()
    });
    gate.wait_entered().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if observed.0.lock().unwrap().contains(&b'\n') {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let cancellations = h.driver.cancels.load(Ordering::SeqCst);
    gate.release();
    let code = task.await.unwrap();
    let lines = observed
        .0
        .lock()
        .unwrap()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    (code, lines, cancellations)
}
#[tokio::test]
async fn NDJSON은_막힌_일반_요청을_기다리지_않고_취소_control을_읽는다() {
    // given
    let h = Harness::new();
    // when
    let actual = control_while_blocked(&h).await;
    // then
    assert_eq!(actual.0, 0);
    assert_eq!(actual.1.len(), 2);
    assert!(actual.2 >= 1);
    assert!(actual
        .1
        .iter()
        .any(|value| value["data"]["cancel_requested"] == true));
}
#[tokio::test]
async fn MCP는_막힌_도구_호출을_기다리지_않고_제어_호출에_먼저_답한다() {
    // given
    let h = Harness::new();
    let gate = TestGate::new();
    h.driver
        .gates
        .lock()
        .unwrap()
        .insert("blocked-mcp".into(), gate.clone());
    let frames = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"query_execute","arguments":{"connection_id":"conn-a","text":"SELECT 'blocked-mcp'","wait_ms":20000}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"job_get","arguments":{"job_id":"j-none","owner_instance_id":"owner"}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"result_release","arguments":{"connection_id":"conn-a","result_id":"r-none"}}}),
    ];
    let mut input = Vec::new();
    for frame in frames {
        input.extend_from_slice(frame.to_string().as_bytes());
        input.push(b'\n');
    }
    let output = Output::default();
    let observed = output.clone();
    let transport = h.transport.clone();
    let task = tokio::spawn(async move {
        transport
            .run_stdio(
                tokio::io::BufReader::new(std::io::Cursor::new(input)),
                output,
                support::caller(),
                Mode::Mcp,
                false,
                Default::default(),
            )
            .await
            .unwrap()
    });
    gate.wait_entered().await;
    // when
    let answered = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let ids: Vec<Value> = observed
                .0
                .lock()
                .unwrap()
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice::<Value>(line).unwrap()["id"].clone())
                .collect();
            if ids.len() >= 4 {
                return ids;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    gate.release();
    task.await.unwrap();
    // then
    assert!(!answered.contains(&json!(1)));
    for id in [2, 3, 4, 5] {
        assert!(answered.contains(&json!(id)), "{id}");
    }
}
#[tokio::test]
async fn 입력을_닫은_stdio_요청도_승인을_기다리는_동안은_drain_기한에_끊기지_않는다() {
    // given
    let gate = Arc::new(GatedApprovals(tokio::sync::Semaphore::new(0)));
    let h = Harness::with_approvals(gate.clone());
    h.allow_write();
    let held = TestGate::new();
    h.driver
        .gates
        .lock()
        .unwrap()
        .insert("held".into(), held.clone());
    let mut raw = serde_json::to_value(support::write_query("held")).unwrap();
    raw["deadline_ms"] = json!(500);
    let mut input = serde_json::to_vec(&raw).unwrap();
    input.push(b'\n');
    let output = Output::default();
    let transport = h.transport.clone();
    let written = output.clone();
    let task = tokio::spawn(async move {
        transport
            .run_stdio(
                tokio::io::BufReader::new(std::io::Cursor::new(input)),
                written,
                support::caller(),
                Mode::Cli,
                true,
                Default::default(),
            )
            .await
            .unwrap()
    });

    // when
    tokio::time::sleep(Duration::from_millis(3_500)).await;
    let waiting = !task.is_finished();
    gate.0.add_permits(1);
    held.wait_entered().await;
    held.release();
    let actual = task.await.unwrap();

    // then
    assert!(waiting);
    assert_eq!(actual, 0);
    assert!(String::from_utf8(output.0.lock().unwrap().clone())
        .unwrap()
        .contains("\"succeeded\""));
}
#[tokio::test]
async fn 초과_프레임은_입력을_중단하고_내용을_오류에_노출하지_않는다() {
    // given
    let h = Harness::new();
    let mut input = vec![b'x'; MAX_FRAME_BYTES + 1];
    input.extend_from_slice(b"secret-canary\n");
    let output = Output::default();
    // when
    let actual = h
        .transport
        .run_stdio(
            tokio::io::BufReader::new(std::io::Cursor::new(input)),
            output.clone(),
            support::caller(),
            Mode::Cli,
            false,
            Default::default(),
        )
        .await
        .unwrap();
    // then
    assert_eq!(actual, 2);
    assert!(!String::from_utf8(output.0.lock().unwrap().clone())
        .unwrap()
        .contains("secret-canary"));
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
struct BrokenOutput;
impl tokio::io::AsyncWrite for BrokenOutput {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "private-io-canary",
        )))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
#[tokio::test]
async fn 끊긴_stdout은_고정_오류로_종료하고_실행을_재전송하지_않는다() {
    // given
    let h = Harness::new();
    let input = format!("{}\n", raw_query("select"));
    // when
    let actual = h
        .transport
        .run_stdio(
            tokio::io::BufReader::new(std::io::Cursor::new(input.into_bytes())),
            BrokenOutput,
            support::caller(),
            Mode::Cli,
            false,
            Default::default(),
        )
        .await;
    // then
    assert_eq!(actual.as_ref().unwrap_err().code, ErrorCode::DriverExited);
    assert!(!serde_json::to_string(&actual.unwrap_err())
        .unwrap()
        .contains("private-io-canary"));
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 1);
}
struct DuplicateLegacy;
#[async_trait]
impl LegacyMcpBackend for DuplicateLegacy {
    fn tools(&self) -> Value {
        json!({"tools":[{"name":"query_execute"}]})
    }
    async fn call(
        &self,
        _name: &str,
        _arguments: Value,
        _caller: &CallerContext,
    ) -> Option<Result<Value, LegacyFailure>> {
        None
    }
    async fn read(&self, _uri: &str, _caller: &CallerContext) -> Option<Value> {
        None
    }
}
#[tokio::test]
async fn legacy_도구의_중복_이름은_등록_후_목록에서_거부한다() {
    // given
    let h = Harness::new();
    h.transport.set_legacy(Arc::new(DuplicateLegacy)).unwrap();
    // when
    let actual = h.transport.tools();
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
}

async fn nonquery_write_flag(h: &Harness) -> (ServiceResponse, bool) {
    let request = support::request(Operation::CatalogDatabases, json!({}));
    let response = h
        .transport
        .run_one(
            serde_json::to_value(&request).unwrap(),
            support::caller(),
            true,
            None,
        )
        .await
        .unwrap();
    let intent = super::super::backend::PolicyBackend::query_is_write(
        &*h.transport.policy,
        &support::caller(),
        &request,
    )
    .await
    .unwrap();
    (response, intent)
}
#[tokio::test]
async fn CLI_쓰기옵션은_카탈로그에_쿼리_쓰기의도를_등록하지_않는다() {
    // given
    let h = Harness::new();
    // when
    let actual = nonquery_write_flag(&h).await;
    // then
    assert_eq!(actual.0.status, JobStatus::Succeeded);
    assert!(!actual.1);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 1);
    assert!(h.driver.contexts.lock().unwrap()[0].1);
}

fn derived_fixture_parity(h: &Harness) -> (Vec<String>, usize, usize) {
    let tools = h.transport.tools().unwrap();
    let bytes = serde_json::to_vec(&tools).unwrap().len();
    let fixtures: Value =
        serde_json::from_str(super::super::contracts::VALIDATION_FIXTURES).unwrap();
    let mut validators = std::collections::HashMap::new();
    for tool in tools["tools"].as_array().unwrap() {
        validators.insert(
            tool["name"].as_str().unwrap().to_owned(),
            jsonschema::JSONSchema::options()
                .with_draft(jsonschema::Draft::Draft7)
                .compile(&tool["inputSchema"])
                .unwrap(),
        );
    }
    let mut differences = Vec::new();
    let mut cases = 0;
    let mut rejected = 0;
    for fixture in fixtures
        .as_array()
        .unwrap()
        .iter()
        .filter(|fixture| fixture["schema"] == "request")
    {
        let value = &fixture["value"];
        let Some(operation) = value["operation"].as_str() else {
            continue;
        };
        let Some(validator) = validators.get(&operation.replace('.', "_")) else {
            continue;
        };
        // The same call written flat: input fields beside the identifiers.
        let mut flat = value["input"].as_object().cloned().unwrap_or_default();
        for key in ["connection_id", "session_id", "map_id", "expected_version"] {
            if !value[key].is_null() {
                flat.insert(key.into(), value[key].clone());
            }
        }
        if let Some(deadline) = value
            .get("deadline_ms")
            .filter(|deadline| !deadline.is_null())
        {
            flat.insert("deadline_ms".into(), deadline.clone());
        }
        let canonical = h.transport.validator.validate_request(value).is_ok();
        let actual = validator.is_valid(&Value::Object(flat));
        cases += 1;
        if canonical && !actual {
            differences.push(format!("{operation}:{}", fixture["name"]));
        }
        if !canonical && !actual {
            rejected += 1;
        }
    }
    println!("derived_tools_bytes={bytes} fixture_comparisons={cases} rejected={rejected}");
    (differences, bytes, cases)
}
#[tokio::test]
async fn 모든_도구의_평평한_스키마는_공유_fixture의_허용_입력을_모두_허용한다() {
    // given
    let h = Harness::new();
    // when
    let actual = derived_fixture_parity(&h);
    // then
    assert!(actual.0.is_empty(), "{:?}", actual.0);
    assert!(actual.1 < 512 * 1024);
    assert!(actual.2 > 300);
}

struct LegacyResources;
#[async_trait]
impl LegacyMcpBackend for LegacyResources {
    fn tools(&self) -> Value {
        json!({"tools":[]})
    }
    async fn call(
        &self,
        _name: &str,
        _arguments: Value,
        _caller: &CallerContext,
    ) -> Option<Result<Value, LegacyFailure>> {
        None
    }
    async fn read(&self, _uri: &str, _caller: &CallerContext) -> Option<Value> {
        None
    }
    async fn list_resources(&self, caller: &CallerContext) -> Option<Value> {
        Some(
            json!({"resources":[{"uri":"tabularis://connection/fixture","name":"saved-connection"},{"uri":"tabularis://schema/fixture","name":"saved-schema"}],"caller_id":caller.caller_id}),
        )
    }
}
#[tokio::test]
async fn resources_list는_주입된_기존_연결과_스키마_목록을_전달한다() {
    // given
    let h = Harness::new();
    h.transport.set_legacy(Arc::new(LegacyResources)).unwrap();
    let frames = vec![json!({"jsonrpc":"2.0","id":9,"method":"resources/list","params":{}})];
    // when
    let actual = stream(&h, frames, Mode::Mcp).await;
    // then
    assert_eq!(actual.0, 0);
    assert_eq!(actual.1[0]["id"], 9);
    assert_eq!(
        actual.1[0]["result"]["resources"][0]["uri"],
        "tabularis://connection/fixture"
    );
    assert_eq!(
        actual.1[0]["result"]["resources"][1]["uri"],
        "tabularis://schema/fixture"
    );
    assert_eq!(actual.1[0]["result"]["caller_id"], "caller");
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn 미리보기_문장은_방언마다_식별자를_인용하고_한도를_문법에_맞게_넣는다() {
    // given
    let cases = [
        (
            SqlFlavor::Postgres,
            Some("public"),
            "orders",
            "SELECT * FROM \"public\".\"orders\" LIMIT 3",
        ),
        (
            SqlFlavor::Mysql,
            None,
            "orders",
            "SELECT * FROM `orders` LIMIT 3",
        ),
        (
            SqlFlavor::Sqlite,
            None,
            "orders",
            "SELECT * FROM \"orders\" LIMIT 3",
        ),
        (
            SqlFlavor::Mssql,
            Some("dbo"),
            "orders",
            "SELECT TOP (3) * FROM [dbo].[orders]",
        ),
        (
            SqlFlavor::Postgres,
            None,
            "a\"b",
            "SELECT * FROM \"a\"\"b\" LIMIT 3",
        ),
        (
            SqlFlavor::Mysql,
            None,
            "a`b",
            "SELECT * FROM `a``b` LIMIT 3",
        ),
        (
            SqlFlavor::Mssql,
            None,
            "a]b",
            "SELECT TOP (3) * FROM [a]]b]",
        ),
    ];
    // when
    let actual: Vec<String> = cases
        .iter()
        .map(|(flavor, schema, table, _)| {
            tool_call::preview_sql(*flavor, *schema, table, 3).unwrap()
        })
        .collect();
    // then
    assert_eq!(
        actual,
        cases
            .iter()
            .map(|case| case.3.to_owned())
            .collect::<Vec<_>>()
    );
}

#[test]
fn 미리보기_문장은_빈_이름과_제어문자와_너무_긴_이름을_만들지_않는다() {
    // given
    let long = "x".repeat(129);
    let names = ["", "a\nb", "a\0b", long.as_str()];
    // when
    let actual: Vec<_> = names
        .iter()
        .map(|name| tool_call::preview_sql(SqlFlavor::Postgres, None, name, 1))
        .collect();
    let schema = tool_call::preview_sql(SqlFlavor::Postgres, Some("a\nb"), "t", 1);
    // then
    assert!(actual.iter().all(Result::is_err));
    assert_eq!(schema.unwrap_err().code, ErrorCode::InvalidArgument);
}

#[test]
fn 도구_이름_규칙은_점_이름을_밑줄로_바꾸고_64자_패턴만_허용한다() {
    // given
    let valid = ["map_layer_add", "a-b", "x"];
    let invalid = ["", "map.layer", "has space", &"x".repeat(65)];
    // when
    let actual = (
        valid.map(tool_catalog::valid_tool_name),
        invalid.map(tool_catalog::valid_tool_name),
        tool_catalog::tool_name("map.layer.add"),
    );
    // then
    assert!(actual.0.iter().all(|valid| *valid));
    assert!(actual.1.iter().all(|valid| !*valid));
    assert_eq!(actual.2, "map_layer_add");
}

#[test]
fn 제어_도구는_결과와_작업을_다루는_도구만이고_쿼리_실행은_아니다() {
    // given
    let names = [
        "job_get",
        "job_cancel",
        "job_list",
        "result_get",
        "result_release",
        "query_execute",
        "table_preview",
        "catalog_databases",
    ];
    // when
    let actual = names.map(tool_catalog::is_control_tool);
    // then
    assert_eq!(actual, [true, true, true, true, true, false, false, false]);
}

struct SlowLegacy {
    delay: Duration,
    finished: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl LegacyMcpBackend for SlowLegacy {
    fn tools(&self) -> Value {
        json!({"tools":[{"name":"slow_tool"}]})
    }
    async fn call(
        &self,
        name: &str,
        _arguments: Value,
        _caller: &CallerContext,
    ) -> Option<Result<Value, LegacyFailure>> {
        if name != "slow_tool" {
            return None;
        }
        tokio::time::sleep(self.delay).await;
        self.finished.fetch_add(1, Ordering::SeqCst);
        Some(Ok(json!({"content":[{"type":"text","text":"done"}]})))
    }
    async fn read(&self, _uri: &str, _caller: &CallerContext) -> Option<Value> {
        None
    }
}

/// Reports the first write of every frame as not ready, so a write races the
/// deadline timer instead of finishing on its first poll.
#[derive(Clone, Default)]
struct StallingOutput {
    inner: Output,
    stalled: Arc<AtomicBool>,
}
impl tokio::io::AsyncWrite for StallingOutput {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if !self.stalled.swap(true, Ordering::SeqCst) {
            cx.waker().wake_by_ref();
            return std::task::Poll::Pending;
        }
        self.stalled.store(false, Ordering::SeqCst);
        let inner = self.inner.clone();
        std::pin::Pin::new(&mut { inner }).poll_write(cx, bytes)
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

fn written_ids(output: &Output) -> Vec<Value> {
    output
        .0
        .lock()
        .unwrap()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap()["id"].clone())
        .collect()
}

async fn send(client: &mut tokio::io::DuplexStream, frame: Value) {
    use tokio::io::AsyncWriteExt;
    client
        .write_all(format!("{frame}\n").as_bytes())
        .await
        .unwrap();
}

async fn until_written(output: &Output, count: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while written_ids(output).len() < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the frames were never written");
}

fn open_stream(
    h: &Harness,
    output: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
) -> (
    tokio::io::DuplexStream,
    tokio::task::JoinHandle<ServiceResult<i32>>,
) {
    let (client, server) = tokio::io::duplex(1 << 16);
    let transport = h.transport.clone();
    let task = tokio::spawn(async move {
        transport
            .run_stdio(
                tokio::io::BufReader::new(server),
                output,
                support::caller(),
                Mode::Mcp,
                false,
                Default::default(),
            )
            .await
    });
    (client, task)
}

#[tokio::test]
async fn 요청_기한이_지난_뒤에_끝난_응답도_쓰고_같은_스트림의_다음_요청에_답한다() {
    // given
    let h = Harness::new();
    let finished = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    h.transport
        .set_legacy(Arc::new(SlowLegacy {
            delay: Duration::from_millis(2500),
            finished: finished.clone(),
        }))
        .unwrap();
    let output = StallingOutput::default();
    let observed = output.inner.clone();
    let (mut client, task) = open_stream(&h, output);
    send(
        &mut client,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"slow_tool","arguments":{"deadline_ms":1,"wait_ms":0}}}),
    )
    .await;

    // when
    until_written(&observed, 1).await;
    send(&mut client, json!({"jsonrpc":"2.0","id":2,"method":"ping"})).await;
    until_written(&observed, 2).await;
    drop(client);
    let code = task.await.unwrap().unwrap();

    // then
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    assert_eq!(written_ids(&observed), vec![json!(1), json!(2)]);
    assert_eq!(code, 0);
}

#[tokio::test]
async fn 취소_알림을_받은_요청은_중단하고_응답을_쓰지_않으며_다른_요청은_계속_처리한다() {
    // given
    let h = Harness::new();
    let finished = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    h.transport
        .set_legacy(Arc::new(SlowLegacy {
            delay: Duration::from_secs(1),
            finished: finished.clone(),
        }))
        .unwrap();
    let output = Output::default();
    let (mut client, task) = open_stream(&h, output.clone());
    let call = |id: Value| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"slow_tool","arguments":{"deadline_ms":1,"wait_ms":0}}});
    send(&mut client, call(json!(1))).await;
    send(&mut client, call(json!("abc"))).await;
    send(&mut client, call(json!(3))).await;

    // when
    send(
        &mut client,
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1,"reason":"user","_meta":{}}}),
    )
    .await;
    send(
        &mut client,
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"abc"}}),
    )
    .await;
    send(
        &mut client,
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"unknown"}}),
    )
    .await;
    send(&mut client, json!({"jsonrpc":"2.0","id":4,"method":"ping"})).await;
    until_written(&output, 2).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(client);
    let code = task.await.unwrap().unwrap();

    // then
    let mut ids = written_ids(&output);
    ids.sort_by_key(Value::to_string);
    assert_eq!(ids, vec![json!(3), json!(4)]);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    assert_eq!(code, 0);
}

#[tokio::test]
async fn 서비스_오류_응답의_메시지는_코드와_사유를_담고_data는_그대로_둔다() {
    // given
    let h = Harness::new();
    let frames = vec![json!({"jsonrpc":"2.0","id":1,"method":"unknown/method"})];

    // when
    let actual = stream(&h, frames, Mode::Mcp).await;

    // then
    let error = &actual.1[0]["error"];
    assert_eq!(
        error["message"],
        "Service request failed: UNSUPPORTED_OPERATION: The connection does not support this operation."
    );
    assert_eq!(error["data"]["code"], "UNSUPPORTED_OPERATION");
}
