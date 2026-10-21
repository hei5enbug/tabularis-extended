use super::{
    super::{
        contracts::{ErrorCode, Limits},
        results::*,
    },
    support::*,
};
use serde_json::json;

#[test]
fn 저장된_결과는_원본_값이_바뀌어도_실행_시점의_값을_유지한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let mut source = snapshot(vec![json!([1]), json!([2])]);
    let id = store.insert(binding(), source.clone()).unwrap();
    source.sets[0].rows[0] = json!([99]);

    // when
    let actual = store.page(&id, &binding(), 0, 0, 10).unwrap();

    // then
    assert_eq!(actual.data["rows"], json!([[1], [2]]));
    assert_eq!(actual.data["kind"], "tabular");
}

#[test]
fn 문서와_scalar_결과를_원본_json_타입으로_보존한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let source = ResultSnapshot {
        sets: vec![
            ResultSet {
                kind: ResultKind::Documents,
                columns: vec![],
                rows: vec![
                    json!({"document":{"id":"doc","nested":[1,true,null]},"identity":{"id":"doc","partition_key":[{"type":"null"}]},"etag":"etag-1"}),
                ],
            },
            ResultSet {
                kind: ResultKind::JsonValues,
                columns: vec![],
                rows: vec![json!(null), json!(true), json!([1, 2]), json!("value")],
            },
        ],
        revision: "revision-1".into(),
        limits: Limits::default(),
        allow_truncation: false,
        warnings: vec![],
    };
    let id = store.insert(binding(), source).unwrap();

    // when
    let (documents, values) = read_result_kinds(&store, &id);

    // then
    assert_eq!(documents.data["kind"], "documents");
    assert_eq!(
        documents.data["documents"][0]["document"]["nested"],
        json!([1, true, null])
    );
    assert_eq!(documents.data["documents"][0]["etag"], "etag-1");
    assert_eq!(values.data["kind"], "json_values");
    assert_eq!(values.data["values"], json!([null, true, [1, 2], "value"]));
}

#[test]
fn 결과_행_상한을_넘으면_절단_원인을_기록한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let source = snapshot((0..10001).map(|value| json!([value])).collect());

    // when
    let actual = insert_and_read(&store, source);

    // then
    assert_eq!(actual.sets[0].rows.len(), MAX_RESULT_ROWS);
    assert!(actual.limits.truncated);
    assert_eq!(actual.limits.reasons, ["row_limit"]);
}

#[test]
fn 완성되지_않은_집계는_행_상한을_넘겨_부분_성공을_반환하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let mut source = snapshot((0..10001).map(|value| json!([value])).collect());
    source.allow_truncation = false;

    // when
    let actual = store.insert(binding(), source);

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::ResourceLimit);
}

#[test]
fn 전체_캐시_상한을_넘으면_결과를_게시하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::with_limits(directory.0.clone(), RESULT_TTL_MS, 128).unwrap();

    // when
    let actual = store.insert(binding(), snapshot(vec![json!([1])]));

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::ResourceLimit);
    assert!(!std::fs::read_dir(&directory.0).unwrap().any(|entry| entry
        .unwrap()
        .path()
        .extension()
        .is_some_and(|extension| extension == "json")));
}

#[test]
fn 만료된_결과는_읽을_수_없다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::with_limits(directory.0.clone(), 0, MAX_CACHE_BYTES).unwrap();
    let id = store.insert(binding(), snapshot(vec![json!([1])])).unwrap();

    // when
    let actual = store.page(&id, &binding(), 0, 0, 10);

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::ResultExpired);
}

#[test]
fn 결과를_해제하면_파생_페이지_토큰도_삭제된다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store
        .insert(binding(), snapshot(vec![json!([1]), json!([2])]))
        .unwrap();
    let token = store
        .page(&id, &binding(), 0, 0, 1)
        .unwrap()
        .page
        .next_token
        .unwrap();

    // when
    let (result, token) = release_and_observe(&store, &id, &token);

    // then
    assert_eq!(result.unwrap_err().code, ErrorCode::ResultExpired);
    assert_eq!(token.unwrap_err().code, ErrorCode::InvalidPageToken);
}

#[test]
fn 결과의_권한과_쿼리와_revision이_다르면_토큰을_거부한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store.insert(binding(), snapshot(vec![json!([1])])).unwrap();
    let mut altered = binding();
    altered.query_hash = "different-query".into();

    // when
    let actual = store.snapshot(&id, &altered);

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidPageToken);
}

#[test]
fn 파일_checksum이_바뀌면_결과를_사용하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store.insert(binding(), snapshot(vec![json!([1])])).unwrap();
    let path = directory.0.join(format!("{id}.json"));
    let bytes = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, bytes.replace("[[1]]", "[[2]]")).unwrap();

    // when
    let actual = store.snapshot(&id, &binding());

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::ResultExpired);
}

#[test]
fn 경로를_포함한_불투명_참조는_파일을_읽지_않는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();

    // when
    let actual = store.snapshot("../outside", &binding());

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
}

fn native_state() -> CursorState {
    let mut binding = binding();
    binding.sdk_version = Some("4.10.1".into());
    binding.container_rid = Some("rid-1".into());
    binding.endpoint_identity = Some("endpoint-1".into());
    binding.partition_hash = Some("partition-1".into());
    CursorState {
        binding,
        native_state: json!({"continuation":"synthetic-private-token"}),
        tail: vec![json!(1)],
        columns: vec![],
        page_size: 100,
        expires_at_ms: super::super::storage::now_ms() + RESULT_TTL_MS,
    }
}

#[test]
fn native_cursor의_범위와_재요청_페이지를_원자적으로_보존한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let state = native_state();
    let binding = state.binding.clone();
    let token = store.cursor_token(state).unwrap();
    let page = StoredPage {
        data: json!({"kind":"json_values","values":[1]}),
        page: Default::default(),
        result_id: None,
        limits: Default::default(),
        metrics: Default::default(),
        warnings: vec![],
    };

    // when
    let (first, duplicate, committed, replay) = continue_and_replay(&store, &token, &binding, page);

    // then
    assert!(matches!(first, NextPage::Native(_)));
    assert!(matches!(duplicate, Err(error) if error.code == ErrorCode::RateLimited));
    assert!(matches!(replay, NextPage::Replay(page) if page.data == committed.data));
    assert!(!token.contains("synthetic-private-token"));
}

#[test]
fn sdk와_container와_partition이_바뀌면_native_cursor를_거부한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let state = native_state();
    let mut altered = state.binding.clone();
    let token = store.cursor_token(state).unwrap();
    altered.sdk_version = Some("other-sdk".into());
    altered.container_rid = Some("other-rid".into());
    altered.partition_hash = Some("other-partition".into());

    // when
    let actual = store.begin_next(&token, &altered);

    // then
    assert!(matches!(actual, Err(error) if error.code == ErrorCode::InvalidPageToken));
}

#[cfg(unix)]
#[test]
fn 결과_디렉터리와_파일은_사용자만_읽을_수_있다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store.insert(binding(), snapshot(vec![json!([1])])).unwrap();

    // when
    let (directory_mode, file_mode) = permission_modes(&directory.0, &id);

    // then
    assert_eq!(directory_mode, 0o700);
    assert_eq!(file_mode, 0o600);
}

fn read_result_kinds(store: &ResultStore, id: &str) -> (StoredPage, StoredPage) {
    (
        store.page(id, &binding(), 0, 0, 10).unwrap(),
        store.page(id, &binding(), 1, 0, 10).unwrap(),
    )
}
fn insert_and_read(store: &ResultStore, source: ResultSnapshot) -> ResultSnapshot {
    let id = store.insert(binding(), source).unwrap();
    store.snapshot(&id, &binding()).unwrap()
}
fn release_and_observe(
    store: &ResultStore,
    id: &str,
    token: &str,
) -> (
    super::super::error::ServiceResult<ResultSnapshot>,
    super::super::error::ServiceResult<CursorBinding>,
) {
    store.release(id, &binding()).unwrap();
    (store.snapshot(id, &binding()), store.token_binding(token))
}
fn continue_and_replay(
    store: &ResultStore,
    token: &str,
    binding: &CursorBinding,
    page: StoredPage,
) -> (
    NextPage,
    super::super::error::ServiceResult<NextPage>,
    StoredPage,
    NextPage,
) {
    let first = store.begin_next(token, binding).unwrap();
    let duplicate = store.begin_next(token, binding);
    let committed = store.finish_next(token, binding, page).unwrap();
    (
        first,
        duplicate,
        committed,
        store.begin_next(token, binding).unwrap(),
    )
}
#[cfg(unix)]
fn permission_modes(directory: &std::path::Path, id: &str) -> (u32, u32) {
    use std::os::unix::fs::PermissionsExt;
    (
        std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
        std::fs::metadata(directory.join(format!("{id}.json")))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
    )
}

#[test]
fn 열_메타데이터와_결과집합_개수를_포함한_페이지도_응답_여유분을_지킨다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let mut source = snapshot(vec![json!(["x".repeat(1024 * 1024)]); 8]);
    source.sets[0].columns =
        vec![json!({"name":"wide","type":"text","metadata":"m".repeat(1024*1024)})];
    source.sets.push(ResultSet {
        kind: ResultKind::Tabular,
        columns: vec![json!({"name":"empty"})],
        rows: vec![],
    });
    let id = store.insert(binding(), source).unwrap();
    // when
    let actual = store.page(&id, &binding(), 0, 0, 1000).unwrap();
    // then
    assert!(serde_json::to_vec(&actual.data).unwrap().len() <= MAX_PAGE_BYTES - 64 * 1024);
    assert_eq!(actual.data["result_set_count"], 2);
    assert!(actual.page.has_more);
    assert_eq!(actual.data["rows"].as_array().unwrap().len(), 6);
}

#[test]
fn 열_메타데이터만으로_페이지_예산을_넘으면_원본결과를_부분성공으로_공개하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let mut source = snapshot(vec![]);
    source.sets[0].columns = vec![json!({"name":"wide","metadata":"m".repeat(MAX_PAGE_BYTES)})];
    let id = store.insert(binding(), source).unwrap();
    // when
    let actual = store.page(&id, &binding(), 0, 0, 1000);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::ResourceLimit);
}

fn json_files(directory: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect();
    names.sort();
    names
}

#[test]
fn 해제한_결과는_캐시_파일까지_지운다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store.insert(binding(), snapshot(vec![json!([1])])).unwrap();
    let before = json_files(&directory.0);

    // when
    store.release(&id, &binding()).unwrap();

    // then
    assert_eq!(before, [format!("{id}.json")]);
    assert!(json_files(&directory.0).is_empty());
}

#[test]
fn 만료된_결과_파일은_정리_때_지워진다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::with_limits(directory.0.clone(), 0, MAX_CACHE_BYTES).unwrap();
    store.insert(binding(), snapshot(vec![json!([1])])).unwrap();
    let before = json_files(&directory.0).len();

    // when
    store.cleanup().unwrap();

    // then
    assert_eq!(before, 1);
    assert!(json_files(&directory.0).is_empty());
}

#[test]
fn 단독으로_시작한_프로세스는_이전_프로세스가_남긴_결과와_임시_파일을_지운다() {
    // given
    let directory = TestDirectory::new();
    let previous = ResultStore::new(directory.0.clone()).unwrap();
    let id = previous
        .insert(binding(), snapshot(vec![json!([1]), json!([2])]))
        .unwrap();
    previous.page(&id, &binding(), 0, 0, 1).unwrap();
    std::fs::write(directory.0.join("interrupted.tmp"), b"partial").unwrap();
    drop(previous);

    // when
    let current = ResultStore::new(directory.0.clone()).unwrap();

    // then
    assert!(json_files(&directory.0).is_empty());
    assert!(!directory.0.join("interrupted.tmp").exists());
    assert_eq!(
        current.snapshot(&id, &binding()).unwrap_err().code,
        ErrorCode::ResultExpired
    );
}

#[test]
fn 다른_프로세스가_같은_디렉터리를_쓰는_동안에는_그_결과를_지우지_않는다() {
    // given
    let directory = TestDirectory::new();
    let running = ResultStore::new(directory.0.clone()).unwrap();
    let id = running
        .insert(binding(), snapshot(vec![json!([1])]))
        .unwrap();

    // when
    let starting = ResultStore::new(directory.0.clone()).unwrap();

    // then
    assert!(starting.snapshot(&id, &binding()).is_ok());
    assert_eq!(json_files(&directory.0), [format!("{id}.json")]);
}
