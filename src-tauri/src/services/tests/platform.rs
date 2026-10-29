use super::{
    super::{
        backend::*,
        contracts::*,
        dispatch::{CallerContext, ServiceDispatcher},
        error::{failure, ServiceResult},
        extensions::OperationExtension,
        jobs::JobLease,
        results::*,
        runtime::ServiceRuntime,
        security::digest,
        storage::now_ms,
    },
    support::*,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Weak,
};

fn origin() -> ResultOrigin {
    ResultOrigin {
        database: "canonical-db".into(),
        container: "canonical-container".into(),
    }
}
fn scalar_snapshot() -> ResultSnapshot {
    ResultSnapshot {
        sets: vec![ResultSet {
            kind: ResultKind::JsonValues,
            columns: vec![],
            rows: vec![json!(null), json!(17), json!({"nested":[true]})],
        }],
        revision: "revision-1".into(),
        limits: Limits::default(),
        allow_truncation: false,
        warnings: vec![],
    }
}

#[test]
fn 문서와_scalar_페이지는_호스트의_origin을_보존한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store
        .insert_reserved_with_origin(binding(), scalar_snapshot(), None, Some(origin()))
        .unwrap();
    // when
    let actual = store.page(&id, &binding(), 0, 0, 10).unwrap();
    // then
    assert_eq!(
        actual.data["origin"],
        json!({"database":"canonical-db","container":"canonical-container"})
    );
    assert_eq!(actual.data["values"], json!([null,17,{"nested":[true]}]));
}

#[test]
fn tabular_결과에는_origin을_붙이지_않고_기존_checksum을_유지한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let source = snapshot(vec![json!([1])]);
    let id = store
        .insert_reserved_with_origin(binding(), source.clone(), None, Some(origin()))
        .unwrap();
    // when
    let actual = read_page_and_record(&store, &id);
    // then
    assert!(actual.0.data.get("origin").is_none());
    assert!(actual.1["record"].get("origin").is_none());
    assert_eq!(
        actual.1["record"]["checksum"],
        digest(&serde_json::to_vec(&source).unwrap())
    );
}
fn read_page_and_record(store: &ResultStore, id: &str) -> (StoredPage, Value) {
    (
        store.page(id, &binding(), 0, 0, 10).unwrap(),
        serde_json::from_slice(&std::fs::read(store.root().join(format!("{id}.json"))).unwrap())
            .unwrap(),
    )
}

#[test]
fn 저장된_origin만_변조해도_checksum_검증으로_거부한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store
        .insert_reserved_with_origin(binding(), scalar_snapshot(), None, Some(origin()))
        .unwrap();
    let path = store.root().join(format!("{id}.json"));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["record"]["origin"]["container"] = "forged".into();
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    // when
    let actual = store.get_origin(&id, &binding());
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::ResultExpired);
}

#[test]
fn materialized_다음_페이지와_replay는_동일_origin을_반환한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store
        .insert_reserved_with_origin(binding(), scalar_snapshot(), None, Some(origin()))
        .unwrap();
    let first = store.page(&id, &binding(), 0, 0, 1).unwrap();
    let token = first.page.next_token.unwrap();
    // when
    let actual = materialized_replay(&store, &token);
    // then
    assert_eq!(
        actual.0.data["origin"],
        serde_json::to_value(origin()).unwrap()
    );
    assert_eq!(actual.0.data, actual.1.data);
    assert_eq!(actual.0.data["values"], json!([17]));
}
fn materialized_replay(store: &ResultStore, token: &str) -> (StoredPage, StoredPage) {
    let NextPage::Materialized {
        result_id,
        set_index,
        offset,
        limit,
    } = store.begin_next(token, &binding()).unwrap()
    else {
        panic!("Expected host materialization")
    };
    let page = store
        .finish_next(
            token,
            &binding(),
            store
                .page(&result_id, &binding(), set_index, offset, limit)
                .unwrap(),
        )
        .unwrap();
    let NextPage::Replay(replay) = store.begin_next(token, &binding()).unwrap() else {
        panic!("Expected same page replay")
    };
    (page, replay)
}

#[test]
fn native_token의_origin은_연결된_호스트_snapshot에서만_읽는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let mut scope = binding();
    scope.sdk_version = Some("synthetic-sdk".into());
    scope.container_rid = Some("synthetic-container-rid".into());
    scope.endpoint_identity = Some("synthetic-endpoint".into());
    scope.partition_hash = Some("synthetic-partition".into());
    let id = store
        .insert_reserved_with_origin(scope.clone(), scalar_snapshot(), None, Some(origin()))
        .unwrap();
    let token = store
        .cursor_token(CursorState {
            binding: scope.clone(),
            native_state: json!({"query":{"database":"forged-db","container":"forged-container"}}),
            tail: vec![],
            columns: vec![],
            page_size: 2,
            expires_at_ms: now_ms() + RESULT_TTL_MS,
        })
        .unwrap();
    store.attach_cursor_result(&token, &id, &scope).unwrap();
    // when
    let actual = store.token_origin(&token, &scope).unwrap();
    // then
    assert_eq!(actual, Some(origin()));
}

#[test]
fn origin_조회도_권한_바인딩과_auth_revision을_검증한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store
        .insert_reserved_with_origin(binding(), scalar_snapshot(), None, Some(origin()))
        .unwrap();
    let mut other = binding();
    other.permission_scope = "other".into();
    // when
    let actual = reject_origin_bindings(&store, &id, &other);
    // then
    assert_eq!(actual.0.unwrap_err().code, ErrorCode::InvalidPageToken);
    assert!(actual.1.is_err());
}
fn reject_origin_bindings(
    store: &ResultStore,
    id: &str,
    other: &CursorBinding,
) -> (
    ServiceResult<Option<ResultOrigin>>,
    ServiceResult<Option<ResultOrigin>>,
) {
    let wrong = store.get_origin(id, other);
    store
        .invalidate_connection_revision("conn-a", "new-auth")
        .unwrap();
    (wrong, store.get_origin(id, &binding()))
}

struct OriginDriver(Arc<TestDriver>);
#[async_trait]
impl DriverBackend for OriginDriver {
    async fn supports(
        &self,
        connection: &SavedConnection,
        operation: Operation,
    ) -> ServiceResult<bool> {
        self.0.supports(connection, operation).await
    }
    async fn read_only_guarantee(
        &self,
        connection: &SavedConnection,
    ) -> ServiceResult<ReadOnlyGuarantee> {
        self.0.read_only_guarantee(connection).await
    }
    async fn execute(
        &self,
        connection: &SavedConnection,
        request: &ServiceRequest,
        context: DriverContext,
    ) -> ServiceResult<DriverOutput> {
        let mut output = self.0.execute(connection, request, context).await?;
        output.data = json!({"origin":{"database":"forged","container":"forged"}});
        if let Some(cursor) = &mut output.cursor {
            cursor.native_state = json!({"query":{"database":"forged","container":"forged"}});
        }
        Ok(output)
    }
    async fn continue_cursor(
        &self,
        connection: &SavedConnection,
        state: &CursorState,
        context: DriverContext,
    ) -> ServiceResult<DriverOutput> {
        let mut output = self.0.continue_cursor(connection, state, context).await?;
        output.cursor = None;
        output.data = json!({"origin":{"database":"forged","container":"forged"}});
        Ok(output)
    }
    async fn cancel(&self, connection: &str, request: &str) -> ServiceResult<()> {
        self.0.cancel(connection, request).await
    }
    async fn reserve_session(
        &self,
        connection: &SavedConnection,
        database: &str,
        context: DriverContext,
    ) -> ServiceResult<String> {
        self.0.reserve_session(connection, database, context).await
    }
    async fn finish_session(
        &self,
        connection: &SavedConnection,
        handle: &str,
        finish: SessionFinish,
        context: DriverContext,
    ) -> ServiceResult<()> {
        self.0
            .finish_session(connection, handle, finish, context)
            .await
    }
    async fn discard_session(&self, connection: &str, handle: &str) {
        self.0.discard_session(connection, handle).await;
    }
    async fn invalidate_auth(&self, connection: &str, principal: Option<&str>) {
        self.0.invalidate_auth(connection, principal).await;
    }
}

#[tokio::test]
async fn cosmos_처음_마지막_replay_result_get은_원래_요청_origin을_보존한다() {
    // given
    let harness = Harness::new();
    harness
        .connections
        .saved
        .lock()
        .unwrap()
        .get_mut("conn-a")
        .unwrap()
        .driver = "cosmos-nosql".into();
    let runtime = ServiceRuntime::new(
        harness.directory.0.join("origin-runtime"),
        "owner".into(),
        harness.connections.clone(),
        harness.policy.clone(),
        Arc::new(OriginDriver(harness.driver.clone())),
        harness.secrets.clone(),
        Arc::new(TestOAuth::new()),
    )
    .unwrap();
    let request = request(
        Operation::QueryExecute,
        json!({"language":"cosmos_sql","text":"SELECT 'native'","parameters":[],"page_size":2,"database":"canonical-db","container":"canonical-container"}),
    );
    // when
    let actual = native_origin_pages(&runtime, request).await;
    // then
    assert!(actual
        .iter()
        .all(|page| page.status == JobStatus::Succeeded));
    assert!(actual
        .iter()
        .all(|page| page.data["origin"] == serde_json::to_value(origin()).unwrap()));
    assert!(!actual[1].page.has_more);
    assert_eq!(actual[1].data["values"], json!([3, 4]));
    assert_eq!(actual[1].data, actual[2].data);
    assert_eq!(actual[1].data, actual[3].data);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 2);
}
async fn native_origin_pages(
    runtime: &ServiceRuntime,
    original: ServiceRequest,
) -> Vec<ServiceResponse> {
    let first = runtime.call_validated(&original, &caller()).await;
    let token = first.page.next_token.clone().unwrap();
    let next = runtime
        .call_validated(
            &request(Operation::QueryNext, json!({"next_token":token})),
            &caller(),
        )
        .await;
    let replay = runtime
        .call_validated(
            &request(Operation::QueryNext, json!({"next_token":token})),
            &caller(),
        )
        .await;
    let get = runtime
        .call_validated(
            &request(
                Operation::ResultGet,
                json!({"result_id":next.result_id,"result_set_index":0,"offset":0,"limit":100}),
            ),
            &caller(),
        )
        .await;
    vec![first, next, replay, get]
}

struct MockExtension {
    operations: Vec<Operation>,
    runtime: Weak<ServiceRuntime>,
    calls: AtomicUsize,
    wait: bool,
}
#[async_trait]
impl OperationExtension for MockExtension {
    fn operations(&self) -> &[Operation] {
        &self.operations
    }
    async fn execute(
        &self,
        request: &ServiceRequest,
        _: &CallerContext,
        lease: &JobLease,
    ) -> ServiceResult<ServiceResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.wait {
            lease.signal().cancelled().await;
            return Err(failure(ErrorCode::Cancelled));
        }
        if self.runtime.upgrade().is_none() {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
        Ok(ServiceResponse {
            protocol_version: 1,
            request_id: request.request_id.clone(),
            connection_id: request.connection_id.clone(),
            status: JobStatus::Succeeded,
            job_id: None,
            result_id: None,
            data: json!({"maps":[]}),
            page: Page::default(),
            limits: Limits::default(),
            metrics: Metrics::default(),
            warnings: vec![],
            error: None,
        })
    }
}
fn extension(
    runtime: &Arc<ServiceRuntime>,
    operations: Vec<Operation>,
    wait: bool,
) -> Arc<MockExtension> {
    Arc::new(MockExtension {
        operations,
        runtime: Arc::downgrade(runtime),
        calls: AtomicUsize::new(0),
        wait,
    })
}

#[tokio::test]
async fn 등록된_map_확장은_connection_null에서도_같은_dispatcher를_사용한다() {
    // given
    let harness = Harness::new();
    let extension = extension(&harness.runtime, vec![Operation::MapList], false);
    harness
        .runtime
        .register_extension(extension.clone())
        .unwrap();
    let mut dispatcher = ServiceDispatcher::new().unwrap();
    harness.runtime.register(&mut dispatcher);
    let mut request = request(Operation::MapList, json!({}));
    request.connection_id = None;
    // when
    let actual = dispatcher.dispatch(&request, &caller()).await;
    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(actual.data, json!({"maps":[]}));
    assert_eq!(extension.calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 확장_등록은_base_operation과_중복을_거부한다() {
    // given
    let harness = Harness::new();
    // when
    let actual = register_invalid_extensions(&harness.runtime);
    // then
    assert!(actual.iter().all(Result::is_err));
}
fn register_invalid_extensions(runtime: &Arc<ServiceRuntime>) -> Vec<ServiceResult<()>> {
    let mut results = vec![
        runtime.register_extension(extension(runtime, vec![Operation::QueryExecute], false)),
        runtime.register_extension(extension(runtime, vec![], false)),
        runtime.register_extension(extension(
            runtime,
            vec![Operation::MapList, Operation::MapList],
            false,
        )),
    ];
    runtime
        .register_extension(extension(runtime, vec![Operation::MapList], false))
        .unwrap();
    results.push(runtime.register_extension(extension(runtime, vec![Operation::MapList], false)));
    results
}

#[tokio::test]
async fn 등록되지_않은_map_operation은_성공을_꾸미지_않는다() {
    // given
    let harness = Harness::new();
    let mut request = request(Operation::MapList, json!({}));
    request.connection_id = None;
    // when
    let actual = harness.runtime.call_validated(&request, &caller()).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::CapabilityUnavailable);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 확장_job도_같은_cancel과_terminal_응답을_사용한다() {
    // given
    let harness = Harness::new();
    let extension = extension(&harness.runtime, vec![Operation::MapList], true);
    harness.runtime.register_extension(extension).unwrap();
    let mut request = request(Operation::MapList, json!({}));
    request.connection_id = None;
    // when
    let actual = cancel_extension(&harness.runtime, request).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::Cancelled);
    assert_eq!(actual.status, JobStatus::Cancelled);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}
async fn cancel_extension(
    runtime: &Arc<ServiceRuntime>,
    original: ServiceRequest,
) -> ServiceResponse {
    let queued = runtime.submit(original, caller()).await;
    let id = queued.job_id.unwrap();
    runtime.jobs.cancel(&id, "owner", &caller()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(response) = runtime.jobs.get(&id, "owner", &caller()).unwrap().response {
                return response;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn 취소되거나_기한이_지난_lease는_쓰기_게시_표시를_거부한다() {
    // given
    let directory = TestDirectory::new();
    let manager = super::super::jobs::JobManager::new("owner".into(), directory.0.clone()).unwrap();
    let first = request(Operation::ConnectionDelete, json!({}));
    let mut cancelled = manager
        .begin(&first, &caller(), false, false)
        .await
        .unwrap();
    cancelled.signal().cancel();
    let mut second = request(Operation::ConnectionDelete, json!({}));
    second.connection_id = Some("conn-b".into());
    let mut expired = manager
        .begin(&second, &caller(), false, false)
        .await
        .unwrap();
    expired.set_deadline_at_ms(0);
    // when
    let actual = dispatch_guards(&mut cancelled, &mut expired);
    // then
    assert_eq!(actual.0.unwrap_err().code, ErrorCode::Cancelled);
    assert_eq!(actual.1.unwrap_err().code, ErrorCode::DeadlineExceeded);
    assert!(!cancelled.write_dispatched());
    assert!(!expired.write_dispatched());
}
fn dispatch_guards(
    cancelled: &mut JobLease,
    expired: &mut JobLease,
) -> (ServiceResult<()>, ServiceResult<()>) {
    (
        cancelled.mark_write_dispatched(),
        expired.mark_write_dispatched(),
    )
}

#[tokio::test]
async fn 긴_유효_cosmos_이름도_처음_next_replay_get의_origin을_보존한다() {
    // given
    let harness = Harness::new();
    harness
        .connections
        .saved
        .lock()
        .unwrap()
        .get_mut("conn-a")
        .unwrap()
        .driver = "cosmos-nosql".into();
    let runtime = ServiceRuntime::new(
        harness.directory.0.join("long-origin-runtime"),
        "owner".into(),
        harness.connections.clone(),
        harness.policy.clone(),
        Arc::new(OriginDriver(harness.driver.clone())),
        harness.secrets.clone(),
        Arc::new(TestOAuth::new()),
    )
    .unwrap();
    // when
    let actual = long_origin_pages(&runtime).await;
    // then
    assert!(actual.iter().all(|(expected, pages)| pages
        .iter()
        .all(|page| page.status == JobStatus::Succeeded && page.data["origin"] == *expected)));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 4);
}
async fn long_origin_pages(runtime: &ServiceRuntime) -> Vec<(Value, Vec<ServiceResponse>)> {
    let mut output = vec![];
    for length in [129, 255] {
        let origin = json!({"database":"d".repeat(length),"container":"c".repeat(length)});
        let request = request(
            Operation::QueryExecute,
            json!({"language":"cosmos_sql","text":"SELECT 'native'","parameters":[],"page_size":2,"database":origin["database"],"container":origin["container"]}),
        );
        output.push((origin, native_origin_pages(runtime, request).await));
    }
    output
}

#[test]
fn origin의_바이트도_page_전체_상한에_포함한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store
        .insert_reserved_with_origin(
            binding(),
            scalar_snapshot(),
            None,
            Some(ResultOrigin {
                database: "d".repeat(MAX_PAGE_BYTES),
                container: "c".into(),
            }),
        )
        .unwrap();
    // when
    let actual = store.page(&id, &binding(), 0, 0, 10);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::ResourceLimit);
}
