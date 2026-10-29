#![allow(non_snake_case)]
use super::*;
use crate::plugins::driver::native_tests::{child_driver, context, params};

struct Resolver;
#[async_trait]
impl ServiceConnectionResolver for Resolver {
    async fn params(&self, connection: &SavedConnection) -> ServiceResult<ConnectionParams> {
        Ok(params(&connection.driver))
    }
}
fn saved(id: &str) -> SavedConnection {
    SavedConnection {
        id: "conn".into(),
        name: "Synthetic".into(),
        driver: id.into(),
        settings: json!({}),
        credential_ref: None,
        version: 1,
        allow_writes: false,
        auth_revision: "revision-1".into(),
    }
}
fn request(input: Value) -> ServiceRequest {
    ServiceRequest {
        protocol_version: 1,
        request_id: "request".into(),
        connection_id: Some("conn".into()),
        map_id: None,
        session_id: None,
        expected_version: None,
        deadline_ms: 2000,
        operation: Operation::QueryExecute,
        input,
    }
}
async fn registered(mode: &str, extra: Value) -> (RegistryDriverBackend, SavedConnection) {
    let id = uuid::Uuid::new_v4().to_string();
    registry::register_driver(child_driver(&id, mode, extra).await).await;
    (RegistryDriverBackend::new(Arc::new(Resolver)), saved(&id))
}

#[tokio::test]
async fn 정확한_SQL과_매개변수를_그대로_전달하고_중복열과_NULL과_빈_결과집합을_보존한다() {
    // given
    let sql = "select '$1; @fake', $1::text as duplicate, NULL as duplicate /* untouched */";
    let parameters = json!([{"name":"$1","value":"original"}]);
    let (backend, connection) = registered(
        "native",
        json!({"expected_text":sql,"expected_parameters":parameters}),
    )
    .await;
    let request =
        request(json!({"language":"sql","text":sql,"parameters":parameters,"database":"explicit"}));
    // when
    let output = backend
        .execute(&connection, &request, context("conn", "request"))
        .await
        .unwrap();
    // then
    assert_eq!(output.data["affected_rows"], Value::Null);
    assert_eq!(output.snapshot.as_ref().unwrap().sets.len(), 2);
    assert_eq!(
        output.snapshot.as_ref().unwrap().sets[0].rows,
        vec![json!([true, null])]
    );
    assert_eq!(
        output.snapshot.as_ref().unwrap().sets[0].columns[0]["name"],
        "duplicate"
    );
    assert_eq!(
        output.snapshot.as_ref().unwrap().sets[0].columns[1]["column_index"],
        1
    );
    assert_eq!(
        output.snapshot.as_ref().unwrap().sets[0].columns[0]["srid"],
        4326
    );
    assert!(output.snapshot.as_ref().unwrap().sets[1].rows.is_empty());
    assert_eq!(output.warnings, ["decimal_precision_unavailable"]);
    assert!(!serde_json::to_string(&output.data)
        .unwrap()
        .contains("SECRET_CANARY"));
}
#[tokio::test]
async fn 행이_없는_SQL도_실제_열_메타데이터를_유지한다() {
    // given
    let (backend, connection) = registered("wrapped", json!({})).await;
    let request = request(json!({"language":"sql","text":"zero_rows","parameters":[]}));
    // when
    let output = backend
        .execute(&connection, &request, context("conn", "request"))
        .await
        .unwrap();
    // then
    assert_eq!(output.snapshot.as_ref().unwrap().sets[0].columns.len(), 2);
    assert_eq!(
        output.snapshot.as_ref().unwrap().sets[0].columns[1]["precision"],
        Value::Null
    );
    assert!(output.snapshot.as_ref().unwrap().sets[0].rows.is_empty());
}

async fn session_isolation(
    backend: &RegistryDriverBackend,
    connection: &SavedConnection,
) -> (
    ServiceResult<DriverOutput>,
    ServiceResult<DriverOutput>,
    ServiceResult<()>,
) {
    let handle = backend
        .reserve_session(connection, "explicit", context("conn", "open"))
        .await
        .unwrap();
    let mut context = context("conn", "query");
    context.session_handle = Some(handle.clone());
    let mismatched = backend
        .execute(
            connection,
            &request(
                json!({"language":"sql","text":"SELECT 1","parameters":[],"database":"other"}),
            ),
            context.clone(),
        )
        .await;
    let mut rotated = connection.clone();
    rotated.auth_revision = "revision-2".into();
    let revised = backend
        .execute(
            &rotated,
            &request(json!({"language":"sql","text":"SELECT 1","parameters":[]})),
            context.clone(),
        )
        .await;
    context.request_id = "rollback".into();
    let rollback = backend
        .finish_session(connection, &handle, SessionFinish::Rollback, context)
        .await;
    (mismatched, revised, rollback)
}
#[tokio::test]
async fn 예약된_물리_세션은_DB와_auth_revision을_고정하고_확인된_rollback만_성공한다() {
    // given
    let (backend, connection) = registered("native", json!({})).await;
    // when
    let (database, revision, rollback) = session_isolation(&backend, &connection).await;
    // then
    assert_eq!(database.err().unwrap().code, ErrorCode::InvalidArgument);
    assert_eq!(revision.err().unwrap().code, ErrorCode::InvalidArgument);
    assert!(rollback.is_ok());
    assert!(backend.sessions.lock().unwrap().is_empty());
}
async fn rollback_unconfirmed(
    backend: &RegistryDriverBackend,
    connection: &SavedConnection,
) -> ServiceResult<()> {
    let handle = backend
        .reserve_session(connection, "db", context("conn", "open"))
        .await
        .unwrap();
    backend
        .finish_session(
            connection,
            &handle,
            SessionFinish::Rollback,
            context("conn", "rollback"),
        )
        .await
}
#[tokio::test]
async fn 세션_종료_응답만으로_rollback_완료를_꾸미지_않는다() {
    // given
    let (backend, connection) = registered("native", json!({"unconfirmed_rollback":true})).await;
    // when
    let actual = rollback_unconfirmed(&backend, &connection).await;
    // then
    assert_eq!(actual.unwrap_err().outcome, Outcome::Unknown);
    assert_eq!(backend.sessions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn Cosmos의_보호된_응답은_scalar와_private_cursor를_서로_분리한다() {
    // given
    registry::register_driver(child_driver("cosmos-nosql", "cosmos", json!({})).await).await;
    let connection = saved("cosmos-nosql");
    let backend = RegistryDriverBackend::new(Arc::new(Resolver));
    let request = request(
        json!({"language":"cosmos_sql","database":"db","container":"container","text":"SELECT VALUE c.value FROM c","parameters":[],"page_size":2}),
    );
    // when
    let output = backend
        .execute(&connection, &request, context("conn", "request"))
        .await
        .unwrap();
    // then
    assert_eq!(
        output.snapshot.as_ref().unwrap().sets[0].kind,
        ResultKind::JsonValues
    );
    assert_eq!(
        output.snapshot.as_ref().unwrap().sets[0].rows,
        vec![Value::Null, json!(true)]
    );
    assert_eq!(
        output.cursor.as_ref().unwrap().native_state["continuation_token"],
        "PRIVATE_SDK_CURSOR"
    );
    assert_eq!(output.metrics.request_charge, Some(2.5));
    assert!(!serde_json::to_string(&output.data)
        .unwrap()
        .contains("PRIVATE_SDK_CURSOR"));
    assert!(output
        .warnings
        .contains(&"cosmos_read_only_database_permissions_unverified".into()));
}

async fn spatial_rejections(
    backend: &RegistryDriverBackend,
    connection: &SavedConnection,
) -> Vec<ErrorCode> {
    let mut write = context("conn", "write");
    write.read_only = false;
    let mut session = context("conn", "session");
    session.session_handle = Some("untrusted".into());
    let mut codes = vec![];
    for (method, context) in [
        (ServiceRpcMethod::SpatialColumns, write),
        (ServiceRpcMethod::SpatialColumns, session),
        (
            ServiceRpcMethod::ExactQuery,
            context("conn", "wrong-method"),
        ),
    ] {
        codes.push(
            backend
                .call_spatial(connection, method, json!({}), context)
                .await
                .unwrap_err()
                .code,
        );
    }
    codes
}
#[tokio::test]
async fn 공간_서비스는_읽기전용_독립요청과_고정된_네_method만_받는다() {
    // given
    let (backend, connection) = registered("native", json!({})).await;
    // when
    let actual = spatial_rejections(&backend, &connection).await;
    // then
    assert_eq!(
        actual,
        vec![
            ErrorCode::WriteNotAllowed,
            ErrorCode::WriteNotAllowed,
            ErrorCode::UnsupportedOperation
        ]
    );
}
#[tokio::test]
async fn 공간_capability가_없는_내장_드라이버는_서비스_호출을_거부한다() {
    // given
    registry::register_driver(crate::drivers::sqlite::SqliteDriver::new()).await;
    let connection = saved("sqlite");
    let backend = RegistryDriverBackend::new(Arc::new(Resolver));
    // when
    let actual = backend
        .call_spatial(
            &connection,
            ServiceRpcMethod::SpatialColumns,
            json!({}),
            context("conn", "spatial"),
        )
        .await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::CapabilityUnavailable);
}

async fn delayed_session_invalidated(
    backend: Arc<RegistryDriverBackend>,
    connection: SavedConnection,
) -> (ServiceResult<String>, usize, Value) {
    let task = tokio::spawn({
        let backend = backend.clone();
        let connection = connection.clone();
        async move {
            backend
                .reserve_session(&connection, "db", context("conn", "open"))
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if backend
                .active
                .lock()
                .unwrap()
                .contains_key(&("conn".into(), "open".into()))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    backend.invalidate_auth("conn", None).await;
    let result = task.await.unwrap();
    let count = backend.sessions.lock().unwrap().len();
    let driver = registry::get_driver(&connection.driver).await.unwrap();
    let stats = driver
        .service_call(
            ServiceRpcMethod::Test,
            &params(&connection.driver),
            &context("conn", "stats"),
            json!({}),
        )
        .await
        .unwrap();
    (result, count, stats)
}
#[tokio::test]
async fn 인증_무효화_중_늦게_열린_물리_세션은_게시하지_않고_폐기한다() {
    // given
    let (backend, connection) = registered("native", json!({"slow_session":true})).await;
    // when
    let (result, count, stats) = delayed_session_invalidated(Arc::new(backend), connection).await;
    // then
    assert_eq!(result.unwrap_err().code, ErrorCode::AuthRequired);
    assert_eq!(count, 0);
    assert_eq!(stats["sessions"], 0);
}

#[test]
fn Cosmos_catalog의_구형_열배열과_새_파티션_정보를_명시적으로_보존한다() {
    // given
    let legacy = json!([{"name":"id","type":"string"}]);
    let current = json!({"columns":[],"partition_key_paths":["/region","/id"],"partition_key_kind":"MultiHash","partition_key_version":2,"system_key":false});
    // when
    let actual = [legacy, current.clone()].map(cosmos_columns);
    // then
    assert_eq!(
        actual[0].as_ref().unwrap()["columns"],
        json!([{"name":"id","type":"string"}])
    );
    assert_eq!(actual[1].as_ref().unwrap(), &current);
}

async fn native_cursor_continued(
    backend: &RegistryDriverBackend,
    connection: &SavedConnection,
) -> DriverOutput {
    let request = request(
        json!({"language":"cosmos_sql","database":"db","container":"container","text":"SELECT VALUE c.value FROM c","parameters":[],"page_size":2}),
    );
    let initial = backend
        .execute(connection, &request, context("conn", "initial"))
        .await
        .unwrap();
    backend
        .continue_cursor(
            connection,
            &initial.cursor.unwrap(),
            context("conn", "next"),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn Cosmos의_다음_페이지는_저장된_native_cursor를_그대로_재사용한다() {
    // given
    let id = uuid::Uuid::new_v4().to_string();
    registry::register_driver(child_driver(&id, "cosmos_optin", json!({})).await).await;
    let connection = saved(&id);
    let backend = RegistryDriverBackend::new(Arc::new(Resolver));
    // when
    let output = native_cursor_continued(&backend, &connection).await;
    // then
    assert_eq!(
        output.snapshot.unwrap().sets[0].rows,
        vec![Value::Null, json!(true)]
    );
    assert!(output.cursor.is_none());
    assert!(!serde_json::to_string(&output.data)
        .unwrap()
        .contains("PRIVATE_SDK_CURSOR"));
}

async fn wait_native_active(connection: &SavedConnection, count: usize) {
    let driver = registry::get_driver(&connection.driver).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let stats = driver
                .service_call(
                    ServiceRpcMethod::Test,
                    &params(&connection.driver),
                    &context("stats", &uuid::Uuid::new_v4().to_string()),
                    json!({}),
                )
                .await
                .unwrap();
            if stats["active"].as_array().unwrap().len() == count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn backend_cancelled_pair(
    backend: Arc<RegistryDriverBackend>,
    mut connection: SavedConnection,
) -> (
    ServiceResult<DriverOutput>,
    ServiceResult<DriverOutput>,
    Value,
) {
    connection.id = "conn-a".into();
    let mut other = connection.clone();
    other.id = "conn-b".into();
    let first = tokio::spawn({
        let backend = backend.clone();
        let connection = connection.clone();
        async move {
            backend
                .execute(
                    &connection,
                    &request(json!({"language":"sql","text":"wait_pair","parameters":[]})),
                    context("conn-a", "same-request"),
                )
                .await
        }
    });
    let second = tokio::spawn({
        let backend = backend.clone();
        async move {
            backend
                .execute(
                    &other,
                    &request(json!({"language":"sql","text":"wait_pair","parameters":[]})),
                    context("conn-b", "same-request"),
                )
                .await
        }
    });
    wait_native_active(&connection, 2).await;
    backend.cancel("conn-a", "same-request").await.unwrap();
    let results = (first.await.unwrap(), second.await.unwrap());
    let driver = registry::get_driver(&connection.driver).await.unwrap();
    let stats = driver
        .service_call(
            ServiceRpcMethod::Test,
            &params(&connection.driver),
            &context("conn-a", "stats"),
            json!({}),
        )
        .await
        .unwrap();
    (results.0, results.1, stats)
}
#[tokio::test]
async fn Registry의_두_연결은_같은_request_ID를_병렬_실행하고_하나만_취소한다() {
    // given
    let (backend, connection) = registered("native", json!({})).await;
    // when
    let (first, second, stats) = backend_cancelled_pair(Arc::new(backend), connection).await;
    // then
    assert_eq!(first.err().unwrap().outcome, Outcome::NotApplied);
    assert!(second.is_ok());
    assert_eq!(stats["queries"], 2);
}
async fn spatial_cancelled(
    backend: Arc<RegistryDriverBackend>,
    connection: SavedConnection,
) -> ServiceResult<Value> {
    let task = tokio::spawn({
        let backend = backend.clone();
        let connection = connection.clone();
        async move {
            backend
                .call_spatial(
                    &connection,
                    ServiceRpcMethod::SpatialQueryTable,
                    json!({"database":"explicit","probe":"wait"}),
                    context("conn", "spatial"),
                )
                .await
        }
    });
    wait_native_active(&connection, 1).await;
    backend.cancel("conn", "spatial").await.unwrap();
    task.await.unwrap()
}
#[tokio::test]
async fn 공간_취소도_Registry의_명시적인_연결과_request_경로를_재사용한다() {
    // given
    let (backend, connection) = registered("native", json!({})).await;
    // when
    let actual = spatial_cancelled(Arc::new(backend), connection).await;
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::Cancelled);
}
struct SlowResolver;
#[async_trait]
impl ServiceConnectionResolver for SlowResolver {
    async fn params(&self, connection: &SavedConnection) -> ServiceResult<ConnectionParams> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(params(&connection.driver))
    }
}
#[tokio::test]
async fn resolver_대기도_전체_deadline에_포함되어_DB송신전_종료된다() {
    // given
    let (_, connection) = registered("native", json!({})).await;
    let driver = registry::get_driver(&connection.driver).await.unwrap();
    driver.service_capabilities().await.unwrap();
    let backend = RegistryDriverBackend::new(Arc::new(SlowResolver));
    let mut context = context("conn", "deadline");
    context.deadline_ms = 20;
    // when
    let actual = backend
        .execute(
            &connection,
            &request(json!({"language":"sql","text":"SELECT 1","parameters":[]})),
            context,
        )
        .await;
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::DeadlineExceeded);
    assert!(backend.active.lock().unwrap().is_empty());
}
#[test]
fn 추가_결과집합의_절단과_경고도_모든_결과에_보존한다() {
    // given
    let source = json!({"columns":[],"rows":[],"affected_rows":0,"additional_results":[{"columns":["empty"],"rows":[],"limits":{"truncated":true,"reasons":["source_limit"]},"warnings":["column_precision_unavailable"]}]});
    // when
    let actual = sql_output(source).unwrap();
    // then
    assert!(actual.limits.truncated);
    assert_eq!(actual.limits.reasons, ["source_limit"]);
    assert_eq!(actual.warnings, ["column_precision_unavailable"]);
    assert_eq!(actual.snapshot.unwrap().sets.len(), 2);
}

struct DeletedResolver {
    deleted: std::sync::atomic::AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl ServiceConnectionResolver for DeletedResolver {
    async fn params(&self, connection: &SavedConnection) -> ServiceResult<ConnectionParams> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.deleted.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(failure(ErrorCode::ConnectionNotFound));
        }
        Ok(params(&connection.driver))
    }
}
async fn invalidate_after_delete(
    backend: &RegistryDriverBackend,
    resolver: &DeletedResolver,
    connection: &SavedConnection,
) -> Value {
    for id in ["conn-a", "conn-b"] {
        let mut connection = connection.clone();
        connection.id = id.into();
        let mut request = request(json!({}));
        request.operation = Operation::ConnectionTest;
        request.connection_id = Some(id.into());
        backend
            .execute(&connection, &request, context(id, "test"))
            .await
            .unwrap();
    }
    resolver
        .deleted
        .store(true, std::sync::atomic::Ordering::SeqCst);
    backend.invalidate_auth("conn-a", None).await;
    let driver = registry::get_driver(&connection.driver).await.unwrap();
    driver
        .service_call(
            ServiceRpcMethod::Test,
            &params(&connection.driver),
            &context("conn-b", "stats"),
            json!({}),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn 삭제후_resolver가_실패해도_비밀없는_제어호출로_해당_연결만_무효화한다() {
    // given
    let (_, connection) = registered("native", json!({})).await;
    let resolver = Arc::new(DeletedResolver {
        deleted: std::sync::atomic::AtomicBool::new(false),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let backend = RegistryDriverBackend::new(resolver.clone());
    // when
    let actual = invalidate_after_delete(&backend, &resolver, &connection).await;
    // then
    assert_eq!(resolver.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        actual["invalidations"],
        json!([{"connection_id":"conn-a","minimal":true}])
    );
    assert!(backend.connections.lock().unwrap().contains_key("conn-b"));
}

struct WrongDriverResolver;
#[async_trait]
impl ServiceConnectionResolver for WrongDriverResolver {
    async fn params(&self, connection: &SavedConnection) -> ServiceResult<ConnectionParams> {
        let mut params = params(&connection.driver);
        params.driver = "different-driver".into();
        Ok(params)
    }
}
#[tokio::test]
async fn resolver는_저장된_driver_ID를_다른_드라이버로_바꿀_수_없다() {
    // given
    let (_, connection) = registered("native", json!({})).await;
    let backend = RegistryDriverBackend::new(Arc::new(WrongDriverResolver));
    // when
    let actual = backend
        .execute(
            &connection,
            &request(json!({"language":"sql","text":"SELECT 1","parameters":[]})),
            context("conn", "request"),
        )
        .await;
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::InvalidArgument);
    assert!(backend.active.lock().unwrap().is_empty());
}

async fn applied_metadata_limit(
    backend: &RegistryDriverBackend,
    connection: &SavedConnection,
) -> (ServiceResult<DriverOutput>, Value) {
    let mut context = context("conn", "write");
    context.read_only = false;
    let result = backend
        .execute(
            connection,
            &request(json!({"language":"sql","text":"SELECT 1","parameters":[]})),
            context,
        )
        .await;
    let driver = registry::get_driver(&connection.driver).await.unwrap();
    let stats = driver
        .service_call(
            ServiceRpcMethod::Test,
            &params(&connection.driver),
            &crate::plugins::driver::native_tests::context("conn", "stats"),
            json!({}),
        )
        .await
        .unwrap();
    (result, stats)
}
#[tokio::test]
async fn DB쓰기_성공후_응답_metadata_상한은_Applied로_실패하고_재실행하지_않는다() {
    // given
    let (backend, connection) = registered("native", json!({"metadata_count":10000})).await;
    // when
    let (actual, stats) = applied_metadata_limit(&backend, &connection).await;
    // then
    assert_eq!(
        actual.as_ref().err().unwrap().code,
        ErrorCode::ResourceLimit
    );
    assert_eq!(actual.err().unwrap().outcome, Outcome::Applied);
    assert_eq!(stats["queries"], 1);
    assert!(backend.active.lock().unwrap().is_empty());
}
#[test]
fn 큰_열_메타데이터는_envelope_32KiB_제한에_포함하지_않는다() {
    // given
    let source = json!({"columns":["value"],"column_metadata":[{"type":"text","metadata":"m".repeat(64*1024)}],"rows":[],"affected_rows":null});
    // when
    let actual = sql_output(source);
    // then
    assert_eq!(
        actual.unwrap().snapshot.unwrap().sets[0].columns[0]["metadata"]
            .as_str()
            .unwrap()
            .len(),
        64 * 1024
    );
}

fn continuation_warning_boundary() -> Vec<String> {
    let metadata_bytes = |warnings: &[String]| {
        serde_json::to_vec(&json!({"metrics":Metrics { elapsed_ms:0, request_charge:Some(2.5), retry_count:0 },"limits":Limits::default(),"warnings":warnings,"snapshot_metadata":{"limits":Limits::default(),"warnings":[]}})).unwrap().len()
    };
    let mut warnings = vec![];
    while metadata_bytes(&warnings) + 131 <= 32 * 1024 {
        warnings.push("w".repeat(128));
    }
    let remaining = 32 * 1024 - metadata_bytes(&warnings);
    if remaining > 3 {
        warnings.push("w".repeat(remaining - 3));
    }
    warnings
}
async fn bounded_continuation(
    backend: &RegistryDriverBackend,
    connection: &SavedConnection,
) -> ServiceResult<DriverOutput> {
    let query = request(
        json!({"language":"cosmos_sql","database":"db","container":"container","text":"SELECT VALUE c.value FROM c","parameters":[],"page_size":2}),
    );
    let initial = backend
        .execute(connection, &query, context("conn", "initial"))
        .await
        .unwrap();
    backend
        .continue_cursor(
            connection,
            &initial.cursor.unwrap(),
            context("conn", "continuation"),
        )
        .await
}
#[tokio::test]
async fn continuation도_권한_경고를_추가한_뒤_32KiB_상한을_검사한다() {
    // given
    let (backend, connection) = registered(
        "cosmos_optin",
        json!({"continuation_warnings":continuation_warning_boundary()}),
    )
    .await;
    // when
    let actual = bounded_continuation(&backend, &connection).await;
    // then
    assert_eq!(
        actual.as_ref().err().unwrap().code,
        ErrorCode::ResourceLimit
    );
    assert_eq!(actual.err().unwrap().outcome, Outcome::NotStarted);
    assert!(backend.active.lock().unwrap().is_empty());
}

async fn truncated_reserved_query(
    backend: &RegistryDriverBackend,
    connection: &SavedConnection,
) -> (ServiceResult<DriverOutput>, Value) {
    let mut open = context("conn", "open");
    open.read_only = false;
    let handle = backend
        .reserve_session(connection, "db", open)
        .await
        .unwrap();
    let mut query = context("conn", "write");
    query.read_only = false;
    query.session_handle = Some(handle);
    let result = backend
        .execute(
            connection,
            &request(json!({"language":"sql","text":"SELECT 1","parameters":[]})),
            query,
        )
        .await;
    let driver = registry::get_driver(&connection.driver).await.unwrap();
    let stats = driver
        .service_call(
            ServiceRpcMethod::Test,
            &params(&connection.driver),
            &context("conn", "stats"),
            json!({}),
        )
        .await
        .unwrap();
    (result, stats)
}
#[tokio::test]
async fn 예약_쓰기_세션의_절단은_결과를_게시하지_않고_물리_세션을_폐기한다() {
    // given
    let (backend, connection) = registered("native", json!({"truncated":true})).await;
    // when
    let (actual, stats) = truncated_reserved_query(&backend, &connection).await;
    // then
    assert_eq!(
        actual.as_ref().err().unwrap().code,
        ErrorCode::ResourceLimit
    );
    assert_eq!(actual.as_ref().err().unwrap().outcome, Outcome::Unknown);
    assert_eq!(
        actual.err().unwrap().details,
        json!({"session_closed":true})
    );
    assert_eq!(stats["queries"], 1);
    assert_eq!(stats["sessions"], 0);
    assert!(backend.sessions.lock().unwrap().is_empty());
}
async fn invalid_open_response(
    backend: &RegistryDriverBackend,
    connection: &SavedConnection,
) -> (ServiceResult<String>, Value) {
    let result = backend
        .reserve_session(connection, "db", context("conn", "open"))
        .await;
    let driver = registry::get_driver(&connection.driver).await.unwrap();
    let stats = driver
        .service_call(
            ServiceRpcMethod::Test,
            &params(&connection.driver),
            &context("conn", "stats"),
            json!({}),
        )
        .await
        .unwrap();
    (result, stats)
}
#[tokio::test]
async fn 모순된_세션_open_응답은_예약하지_않고_알려진_handle을_폐기한다() {
    // given
    let (backend, connection) =
        registered("native", json!({"invalid_session_response":true})).await;
    // when
    let (actual, stats) = invalid_open_response(&backend, &connection).await;
    // then
    assert_eq!(actual.err().unwrap().outcome, Outcome::Unknown);
    assert_eq!(stats["sessions"], 0);
    assert!(backend.sessions.lock().unwrap().is_empty());
}

fn entra_context(request: &str, token: &str, expires_at_ms: u64) -> DriverContext {
    let mut refresh = context("conn", request);
    refresh.transient_auth = Some(TransientAuth::EntraToken {
        access_token: crate::services::credentials::Secret::new(token.as_bytes().to_vec()).unwrap(),
        expires_at_ms,
        tenant_id: "tenant".into(),
        client_id: "client".into(),
        principal_id: "principal".into(),
        scope: "scope".into(),
    });
    refresh
}
async fn driver_stats(connection: &SavedConnection) -> Value {
    registry::get_driver(&connection.driver)
        .await
        .unwrap()
        .service_call(
            ServiceRpcMethod::Test,
            &params(&connection.driver),
            &context("conn", "stats"),
            json!({}),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn 토큰_갱신은_드라이버에_새_토큰을_전달하고_드라이버의_채택_여부를_반환한다() {
    // given
    let (backend, connection) = registered("native", json!({"auth_refresh":true})).await;
    // when
    let actual = backend
        .refresh_auth(&connection, entra_context("refresh", "new-token", 4242))
        .await;
    // then
    let stats = driver_stats(&connection).await;
    assert!(actual.unwrap());
    assert_eq!(stats["refreshes"].as_array().unwrap().len(), 1);
    assert_eq!(stats["refreshes"][0]["expires_at_ms"], 4242);
    assert_eq!(stats["refreshes"][0]["input"], json!({}));
    assert!(backend.active.lock().unwrap().is_empty());
}
#[tokio::test]
async fn 드라이버가_토큰을_채택하지_않으면_false를_반환한다() {
    // given
    let (backend, connection) =
        registered("native", json!({"auth_refresh":true,"refreshed":false})).await;
    // when
    let actual = backend
        .refresh_auth(&connection, entra_context("refresh", "new-token", 4242))
        .await;
    // then
    assert!(!actual.unwrap());
}
#[tokio::test]
async fn 토큰_갱신을_지원하지_않는_드라이버에는_호출을_보내지_않고_false를_반환한다() {
    // given
    let (backend, connection) = registered("native", json!({})).await;
    // when
    let actual = backend
        .refresh_auth(&connection, entra_context("refresh", "new-token", 4242))
        .await;
    // then
    let stats = driver_stats(&connection).await;
    assert!(!actual.unwrap());
    assert_eq!(stats["refreshes"], json!([]));
}
#[tokio::test]
async fn 토큰이_없거나_다른_연결의_갱신_컨텍스트는_거부한다() {
    // given
    let (backend, connection) = registered("native", json!({"auth_refresh":true})).await;
    let missing = context("conn", "refresh");
    let mut mismatched = entra_context("refresh", "new-token", 4242);
    mismatched.connection_id = "other".into();
    // when
    let actual = [
        backend.refresh_auth(&connection, missing).await,
        backend.refresh_auth(&connection, mismatched).await,
    ];
    // then
    let stats = driver_stats(&connection).await;
    assert!(actual
        .iter()
        .all(|result| result.as_ref().err().unwrap().code == ErrorCode::InvalidArgument));
    assert_eq!(stats["refreshes"], json!([]));
}
#[tokio::test]
async fn 진행중인_쿼리가_있어도_토큰_갱신은_취소_없이_완료된다() {
    // given
    let (backend, connection) = registered("native", json!({"auth_refresh":true})).await;
    let backend = Arc::new(backend);
    let running = tokio::spawn({
        let backend = backend.clone();
        let connection = connection.clone();
        async move {
            backend
                .execute(
                    &connection,
                    &request(json!({"language":"sql","text":"wait_pair","parameters":[]})),
                    context("conn", "running"),
                )
                .await
        }
    });
    wait_native_active(&connection, 1).await;
    // when
    let refreshed = backend
        .refresh_auth(&connection, entra_context("refresh", "new-token", 4242))
        .await;
    backend.cancel("conn", "running").await.unwrap();
    let query = running.await.unwrap();
    // then
    assert!(refreshed.unwrap());
    assert_eq!(query.err().unwrap().code, ErrorCode::Cancelled);
}
