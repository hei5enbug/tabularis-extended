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
struct Harness {
    _directory: TestDirectory,
    transport: Arc<ServiceTransport>,
    connections: Arc<TestConnections>,
    driver: Arc<TestDriver>,
    access: Arc<Access>,
}
impl Harness {
    fn new() -> Self {
        let directory = TestDirectory::new();
        let connections = Arc::new(TestConnections::new());
        let driver = Arc::new(TestDriver::new());
        let access = Arc::new(Access(AtomicBool::new(false)));
        let policy = Arc::new(ApprovalPolicy::new(access.clone(), Arc::new(Approvals)));
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
#[tokio::test]
async fn 도구_목록은_공유_스키마의_작업에서_가져오기와_연결_삭제를_뺀_마흔일곱_작업과_쓰기_별칭을_사용한다(
) {
    // given
    let h = Harness::new();
    let source: Value = serde_json::from_str(super::super::contracts::REQUEST_SCHEMA).unwrap();
    // when
    let actual = checked_catalog(&h);
    // then
    assert_eq!(actual.0["tools"].as_array().unwrap().len(), 48);
    assert!(actual.0["tools"].as_array().unwrap().iter().all(|tool| {
        !matches!(
            tool["name"].as_str(),
            Some("credential.import" | "connection.delete")
        )
    }));
    assert_eq!(
        actual.0["tools"][0]["inputSchema"]["definitions"]["input.connection.list"],
        source["definitions"]["input.connection.list"]
    );
    assert!(actual.0["tools"][0]["inputSchema"]["definitions"]
        .get("input.query.execute")
        .is_none());
    assert_eq!(
        actual.0["tools"][0]["inputSchema"]["additionalProperties"],
        false
    );
    assert_eq!(
        actual.0["tools"][0]["inputSchema"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(actual.0["tools"][47]["name"], "query.execute_write");
    assert_eq!(
        actual.0["tools"][47]["inputSchema"]["properties"]["operation"]["const"],
        "query.execute"
    );
    assert!(actual.1);
}
#[tokio::test]
async fn 도구_이름과_작업이_다르면_실행하지_않는다() {
    // given
    let h = Harness::new();
    let raw = raw_query("select");
    // when
    let actual = h
        .transport
        .call_tool("connection.test", raw, mcp_caller(), None)
        .await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn MCP_자격증명_가져오기는_보호된_입력채널을_읽지_않는다() {
    // given
    let h = Harness::new();
    let raw=serde_json::to_value(support::request(Operation::CredentialImport,json!({"slot":"db_password","input_channel":{"kind":"file_descriptor","descriptor":7},"persistence":"session"}))).unwrap();
    // when
    let actual = h
        .transport
        .call_tool("credential.import", raw, mcp_caller(), None)
        .await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::OperationNotAllowed);
    assert_eq!(h.driver.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn MCP는_연결_삭제를_도구와_직접_제출_모두에서_거부한다() {
    // given
    let h = Harness::new();
    let mut request = support::request(Operation::ConnectionDelete, json!({}));
    request.expected_version = Some(1);
    let raw = serde_json::to_value(request).unwrap();
    // when
    let tool = h
        .transport
        .call_tool("connection.delete", raw.clone(), mcp_caller(), None)
        .await;
    let submitted = h.transport.submit(raw, mcp_caller()).await;
    // then
    assert_eq!(tool.unwrap_err().code, ErrorCode::OperationNotAllowed);
    assert_eq!(submitted.unwrap_err().code, ErrorCode::OperationNotAllowed);
    assert_eq!(h.connections.writes.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn 쓰기_별칭도_연결의_쓰기_승인을_우회하지_않는다() {
    // given
    let h = Harness::new();
    let raw = raw_query("write");
    // when
    let actual = h
        .transport
        .call_tool("query.execute_write", raw, mcp_caller(), None)
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
    let actual = mcp::tool_result(Operation::AuthBegin, &response);
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
    let actual = mcp::tool_result(Operation::QueryExecute, &response);
    // then
    assert_eq!(
        actual["structuredContent"]["data"]["values_sample"][0]["challenge"],
        "ordinary-document-value"
    );
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
    let actual = mcp::tool_result(Operation::SpatialQueryResult, &response);
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
async fn MCP_안전하지_않은_정수_ID는_실행_전에_거부한다() {
    // given
    let h = Harness::new();
    let frames = vec![
        json!({"jsonrpc":"2.0","id":9007199254740992_u64,"method":"tools/call","params":{"name":"query.execute","arguments":raw_query("select")}}),
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
        json!({"tools":[{"name":"query.execute"}]})
    }
    async fn call(&self, _name: &str, _arguments: Value, _caller: &CallerContext) -> Option<Value> {
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

fn checked_catalog(h: &Harness) -> (Value, bool) {
    let tools = h.transport.tools().unwrap();
    let validator = jsonschema::JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .compile(&tools["tools"][0]["inputSchema"])
        .unwrap();
    let mut request = support::request(Operation::ConnectionList, json!({}));
    request.connection_id = None;
    let mut unknown = serde_json::to_value(request).unwrap();
    unknown["unknown_field"] = json!(true);
    let rejected = !validator.is_valid(&unknown);
    (tools, rejected)
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

fn derived_fixture_parity(h: &Harness) -> (Vec<String>, usize, usize, usize) {
    let tools = h.transport.tools().unwrap();
    let bytes = serde_json::to_vec(&tools).unwrap().len();
    let fixtures: Value =
        serde_json::from_str(super::super::contracts::VALIDATION_FIXTURES).unwrap();
    let mut differences = Vec::new();
    let mut cases = 0;
    let mut other_operations = 0;
    for tool in tools["tools"].as_array().unwrap() {
        let operation = tool["inputSchema"]["properties"]["operation"]["const"]
            .as_str()
            .unwrap();
        let validator = jsonschema::JSONSchema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .compile(&tool["inputSchema"])
            .unwrap();
        for fixture in fixtures
            .as_array()
            .unwrap()
            .iter()
            .filter(|fixture| fixture["schema"] == "request")
        {
            let value = &fixture["value"];
            let canonical = h.transport.validator.validate_request(value).is_ok();
            let matches = value["operation"] == operation;
            let expected = canonical && matches;
            let actual = validator.is_valid(value);
            cases += 1;
            if actual != expected || canonical != fixture["valid"].as_bool().unwrap() {
                differences.push(format!("{}:{}", tool["name"], fixture["name"]));
            }
            if canonical && !matches && !actual {
                other_operations += 1;
            }
        }
    }
    println!("derived_tools_bytes={bytes} fixture_comparisons={cases} other_operation_rejections={other_operations}");
    (differences, bytes, cases, other_operations)
}
#[tokio::test]
async fn 모든_도구의_참조_폐쇄_스키마는_공유_fixture_허용거부와_일치한다() {
    // given
    let h = Harness::new();
    // when
    let actual = derived_fixture_parity(&h);
    // then
    assert!(actual.0.is_empty(), "{:?}", actual.0);
    assert!(actual.1 < 512 * 1024);
    assert!(actual.2 > 10000);
    assert!(actual.3 > 1000);
}

struct LegacyResources;
#[async_trait]
impl LegacyMcpBackend for LegacyResources {
    fn tools(&self) -> Value {
        json!({"tools":[]})
    }
    async fn call(&self, _name: &str, _arguments: Value, _caller: &CallerContext) -> Option<Value> {
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
