use super::{
    super::{contracts::*, error::redact_error, results::*, security::digest, storage::now_ms},
    support::*,
};
use serde_json::{json, Value};
fn state() -> SpatialCursorState {
    SpatialCursorState {
        binding: binding(),
        state: json!({"ordinal":4,"column_index":0}),
        page_size: 10,
        expires_at_ms: now_ms() + RESULT_TTL_MS,
    }
}
fn page() -> StoredPage {
    StoredPage {
        data: json!({"features":[],"partial":false}),
        page: Page::default(),
        result_id: None,
        limits: Limits::default(),
        metrics: Metrics::default(),
        warnings: vec![],
    }
}
fn replay_scenario(store: &ResultStore, token: &str) -> (bool, ErrorCode, Value, Value) {
    let first = matches!(
        store.begin_spatial_next(token, &binding()).unwrap(),
        SpatialNextPage::State(_)
    );
    let concurrent = store
        .begin_spatial_next(token, &binding())
        .err()
        .unwrap()
        .code;
    let published = store
        .finish_spatial_next(token, &binding(), page())
        .unwrap();
    let repeated = store
        .finish_spatial_next(
            token,
            &binding(),
            StoredPage {
                data: json!({"wrong":true}),
                ..page()
            },
        )
        .unwrap();
    let replay = match store.begin_spatial_next(token, &binding()).unwrap() {
        SpatialNextPage::Replay(page) => page,
        _ => panic!("missing replay"),
    };
    (
        first,
        concurrent,
        json!([published.data, repeated.data]),
        replay.data,
    )
}
#[test]
fn 공간_페이지는_한번만_계산하고_동시_실행을_거부하며_동일한_결과를_재생한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let token = store.spatial_token(state(), None).unwrap();
    // when
    let actual = replay_scenario(&store, &token);
    // then
    assert!(actual.0);
    assert_eq!(actual.1, ErrorCode::RateLimited);
    assert_eq!(actual.2, json!([page().data, page().data]));
    assert_eq!(actual.3, page().data);
}
fn mismatch_scenario(store: &ResultStore, token: &str) -> Vec<ErrorCode> {
    let mut bindings = vec![];
    for field in 0..9 {
        let mut value = binding();
        match field {
            0 => value.connection_id = "other".into(),
            1 => value.permission_scope = "other".into(),
            2 => value.auth_revision = "other".into(),
            3 => value.query_hash = "other".into(),
            4 => value.result_revision = "other".into(),
            5 => value.sdk_version = Some("other".into()),
            6 => value.container_rid = Some("other".into()),
            7 => value.endpoint_identity = Some("other".into()),
            _ => value.partition_hash = Some("other".into()),
        }
        bindings.push(value);
    }
    bindings
        .iter()
        .map(|binding| store.begin_spatial_next(token, binding).err().unwrap().code)
        .collect()
}
#[test]
fn 공간_토큰은_연결_권한_인증_쿼리_결과와_나머지_바인딩을_모두_검사한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let token = store.spatial_token(state(), None).unwrap();
    // when
    let actual = mismatch_scenario(&store, &token);
    // then
    assert_eq!(actual, vec![ErrorCode::InvalidPageToken; 9]);
}
fn marker(store: &ResultStore, revision: &str) {
    let file = store.root().join(format!(
        "a-{}.json",
        digest(binding().connection_id.as_bytes())
    ));
    std::fs::write(
        file,
        serde_json::to_vec(
            &json!({"kind":"AuthRevision","record":{"connection_id":"conn-a","revision":revision}}),
        )
        .unwrap(),
    )
    .unwrap();
}
fn revoked_scenario(store: &ResultStore, token: &str) -> Vec<ErrorCode> {
    marker(store, "rotated");
    vec![
        store.spatial_token(state(), None).err().unwrap().code,
        store.token_binding(token).err().unwrap().code,
        store
            .begin_spatial_next(token, &binding())
            .err()
            .unwrap()
            .code,
        store
            .finish_spatial_next(token, &binding(), page())
            .err()
            .unwrap()
            .code,
    ]
}
#[test]
fn 인증_세대가_바뀌면_동일한_바인딩으로도_토큰_생성_조회_시작_완료를_거부한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let token = store.spatial_token(state(), None).unwrap();
    store.begin_spatial_next(&token, &binding()).unwrap();
    // when
    let actual = revoked_scenario(&store, &token);
    // then
    assert_eq!(actual, vec![ErrorCode::AuthRequired; 4]);
}
fn released_scenario(store: &ResultStore, id: &str, token: &str) -> ErrorCode {
    store.release(id, &binding()).unwrap();
    store
        .begin_spatial_next(token, &binding())
        .err()
        .unwrap()
        .code
}
#[test]
fn 원본_결과를_해제하면_그_결과에_묶인_공간_토큰도_해제한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store.insert(binding(), snapshot(vec![json!([1])])).unwrap();
    let token = store.spatial_token(state(), Some(&id)).unwrap();
    // when
    let actual = released_scenario(&store, &id, &token);
    // then
    assert_eq!(actual, ErrorCode::InvalidPageToken);
}
fn expired_source_scenario(store: &ResultStore, id: &str, token: &str) -> ErrorCode {
    let file = store.root().join(format!("{id}.json"));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    record["record"]["expires_at_ms"] = json!(0);
    std::fs::write(file, serde_json::to_vec(&record).unwrap()).unwrap();
    store
        .begin_spatial_next(token, &binding())
        .err()
        .unwrap()
        .code
}
#[test]
fn 원본_결과가_만료되면_아직_유효한_공간_토큰도_진행하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store.insert(binding(), snapshot(vec![])).unwrap();
    let token = store.spatial_token(state(), Some(&id)).unwrap();
    // when
    let actual = expired_source_scenario(&store, &id, &token);
    // then
    assert_eq!(actual, ErrorCode::ResultExpired);
}
fn corrupt_scenario(store: &ResultStore, token: &str) -> ErrorCode {
    let file = store.root().join(format!("{token}.json"));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    record["record"]["state"]["state"]["ordinal"] = json!(999);
    std::fs::write(file, serde_json::to_vec(&record).unwrap()).unwrap();
    store
        .begin_spatial_next(token, &binding())
        .err()
        .unwrap()
        .code
}
#[test]
fn 저장된_공간_상태의_변조는_체크섬으로_거부한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let token = store.spatial_token(state(), None).unwrap();
    // when
    let actual = corrupt_scenario(&store, &token);
    // then
    assert_eq!(actual, ErrorCode::InvalidPageToken);
}
fn fail_scenario(store: &ResultStore, token: &str) -> ErrorCode {
    store.begin_spatial_next(token, &binding()).unwrap();
    store.fail_spatial_next(token, &binding()).unwrap();
    store
        .begin_spatial_next(token, &binding())
        .err()
        .unwrap()
        .code
}
#[test]
fn 실패한_공간_진행은_토큰을_폐기하여_불확실한_상태를_재사용하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let token = store.spatial_token(state(), None).unwrap();
    // when
    let actual = fail_scenario(&store, &token);
    // then
    assert_eq!(actual, ErrorCode::InvalidPageToken);
}
#[test]
fn 공간_토큰은_결과와_같은_전체_캐시_용량을_소비한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::with_limits(directory.0.clone(), RESULT_TTL_MS, 1600).unwrap();
    store
        .insert(binding(), snapshot(vec![json!(["x".repeat(400)])]))
        .unwrap();
    let mut input = state();
    input.state = json!({"payload":"x".repeat(1000)});
    // when
    let actual = store.spatial_token(input, None);
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::ResourceLimit);
}
#[test]
fn 공간_응답은_공통_응답을_위한_바이트_여유를_남긴다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let token = store.spatial_token(state(), None).unwrap();
    store.begin_spatial_next(&token, &binding()).unwrap();
    let mut input = page();
    input.data = json!("x".repeat(MAX_PAGE_BYTES - 64 * 1024));
    // when
    let actual = store.finish_spatial_next(&token, &binding(), input);
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::ResourceLimit);
}
#[test]
fn 공간_토큰이_추가되어도_공식_네이티브_커서의_필수_바인딩은_완화되지_않는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let cursor = CursorState {
        binding: binding(),
        native_state: json!({}),
        tail: vec![],
        columns: vec![],
        page_size: 10,
        expires_at_ms: now_ms() + RESULT_TTL_MS,
    };
    // when
    let actual = store.cursor_token(cursor);
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::InvalidArgument);
}
#[test]
fn gui_미실행_오류는_검증된_인스턴스_메타데이터만_보존한다() {
    // given
    let mut error = super::super::error::failure(ErrorCode::GuiUnavailable);
    error.details = json!({"instances":[{"instance_id":"gui-a","process_id":42,"kind":"gui","nonce":"canary","path":"canary"},{"instance_id":"../bad","process_id":1,"kind":"gui"},{"instance_id":"cli","process_id":1,"kind":"cli"}],"password":"canary"});
    // when
    let actual = redact_error(error);
    // then
    assert_eq!(
        actual.details,
        json!({"instances":[{"instance_id":"gui-a","process_id":42,"kind":"gui"}]})
    );
    assert!(!serde_json::to_string(&actual).unwrap().contains("canary"));
}
#[test]
fn 만료된_공간_상태는_커서_만료_오류로_거부한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let mut input = state();
    input.expires_at_ms = 0;
    // when
    let actual = store.spatial_token(input, None);
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::CursorExpired);
}
fn revoked_replay(store: &ResultStore, token: &str) -> ErrorCode {
    marker(store, "rotated");
    store
        .begin_spatial_next(token, &binding())
        .err()
        .unwrap()
        .code
}
#[test]
fn 완료된_공간_페이지도_인증_세대_변경_후에는_재생하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let token = store.spatial_token(state(), None).unwrap();
    store.begin_spatial_next(&token, &binding()).unwrap();
    store
        .finish_spatial_next(&token, &binding(), page())
        .unwrap();
    // when
    let actual = revoked_replay(&store, &token);
    // then
    assert_eq!(actual, ErrorCode::AuthRequired);
}
fn invalidated_tokens(store: &ResultStore, token: &str) -> ErrorCode {
    store
        .invalidate_connection_revision("conn-a", "rotated")
        .unwrap();
    store
        .begin_spatial_next(token, &binding())
        .err()
        .unwrap()
        .code
}
#[test]
fn 연결_인증_무효화는_공간_토큰도_같은_캐시에서_삭제한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let token = store.spatial_token(state(), None).unwrap();
    // when
    let actual = invalidated_tokens(&store, &token);
    // then
    assert_eq!(actual, ErrorCode::InvalidPageToken);
}
#[test]
fn 공간_진행_상태는_삼십이_mebibyte를_넘어_저장하지_않는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let mut input = state();
    input.state = json!("x".repeat(MAX_RESULT_BYTES));
    // when
    let actual = store.spatial_token(input, None);
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::ResourceLimit);
}
fn source_changed_after_begin(store: &ResultStore, id: &str, token: &str) -> ErrorCode {
    let path = store.root().join(format!("{id}.json"));
    let mut source: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    source["record"]["snapshot"]["sets"][0]["rows"] = json!([[999]]);
    std::fs::write(path, serde_json::to_vec(&source).unwrap()).unwrap();
    store
        .finish_spatial_next(token, &binding(), page())
        .err()
        .unwrap()
        .code
}
#[test]
fn 원본_결과의_체크섬은_공간_페이지_완료_직전에도_다시_검사한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let id = store.insert(binding(), snapshot(vec![])).unwrap();
    let token = store.spatial_token(state(), Some(&id)).unwrap();
    store.begin_spatial_next(&token, &binding()).unwrap();
    // when
    let actual = source_changed_after_begin(&store, &id, &token);
    // then
    assert_eq!(actual, ErrorCode::ResultExpired);
}
fn reservation_scenario(store: &ResultStore) -> (String, usize) {
    let reservation = store.reserve_query("conn-a", 30000).unwrap();
    let token = store
        .spatial_token_reserved(state(), None, Some(&reservation))
        .unwrap();
    let reservations = std::fs::read_dir(store.root())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("q-"))
        .count();
    drop(reservation);
    (token, reservations)
}
#[test]
fn 공간_토큰은_공통_용량_예약을_소비한_후에도_같은_저장소에_남는다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    // when
    let actual = reservation_scenario(&store);
    // then
    assert_eq!(actual.1, 0);
    assert!(store.token_binding(&actual.0).is_ok());
}
fn corrupt_replay(store: &ResultStore, token: &str) -> ErrorCode {
    let path = store.root().join(format!("{token}.json"));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["record"]["replay"]["data"]["features"] = json!([{"wrong":true}]);
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    store
        .begin_spatial_next(token, &binding())
        .err()
        .unwrap()
        .code
}
#[test]
fn 이미_완료된_공간_페이지의_변조도_체크섬으로_거부한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let token = store.spatial_token(state(), None).unwrap();
    store.begin_spatial_next(&token, &binding()).unwrap();
    store
        .finish_spatial_next(&token, &binding(), page())
        .unwrap();
    // when
    let actual = corrupt_replay(&store, &token);
    // then
    assert_eq!(actual, ErrorCode::InvalidPageToken);
}
fn bound_mismatch_scenario(store: &ResultStore, token: &str, hash: &str) -> (ErrorCode, bool) {
    let rejected = store
        .check_spatial_request(token, &binding(), &"b".repeat(64))
        .unwrap_err()
        .code;
    store
        .check_spatial_request(token, &binding(), hash)
        .unwrap();
    (
        rejected,
        matches!(
            store.begin_spatial_next(token, &binding()).unwrap(),
            SpatialNextPage::State(_)
        ),
    )
}
#[test]
fn 요청_해시가_다르면_토큰을_소비하지_않고_원래_요청은_시작한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let hash = "a".repeat(64);
    let token = store
        .spatial_token_bound_reserved(state(), None, &hash, None)
        .unwrap();
    // when
    let actual = bound_mismatch_scenario(&store, &token, &hash);
    // then
    assert_eq!(actual.0, ErrorCode::InvalidPageToken);
    assert!(actual.1);
}
fn bound_replay_scenario(store: &ResultStore, token: &str, hash: &str) -> (ErrorCode, bool) {
    store.begin_spatial_next(token, &binding()).unwrap();
    store
        .finish_spatial_next(token, &binding(), page())
        .unwrap();
    let rejected = store
        .check_spatial_request(token, &binding(), &"b".repeat(64))
        .unwrap_err()
        .code;
    store
        .check_spatial_request(token, &binding(), hash)
        .unwrap();
    (
        rejected,
        matches!(
            store.begin_spatial_next(token, &binding()).unwrap(),
            SpatialNextPage::Replay(_)
        ),
    )
}
#[test]
fn 완료된_공간_페이지도_원래_요청_해시로만_재생한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let hash = "a".repeat(64);
    let token = store
        .spatial_token_bound_reserved(state(), None, &hash, None)
        .unwrap();
    // when
    let actual = bound_replay_scenario(&store, &token, &hash);
    // then
    assert_eq!(actual.0, ErrorCode::InvalidPageToken);
    assert!(actual.1);
}
fn bound_rejection_scenario(store: &ResultStore, token: &str, hash: &str) -> Vec<ErrorCode> {
    let malformed = store
        .spatial_token_bound_reserved(state(), None, "invalid", None)
        .err()
        .unwrap()
        .code;
    marker(store, "rotated");
    let revoked = store
        .check_spatial_request(token, &binding(), hash)
        .unwrap_err()
        .code;
    marker(store, &binding().auth_revision);
    let path = store.root().join(format!("{token}.json"));
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["record"]["expires_at_ms"] = json!(0);
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    vec![
        malformed,
        revoked,
        store
            .check_spatial_request(token, &binding(), hash)
            .unwrap_err()
            .code,
    ]
}
#[test]
fn 요청_해시_확인은_잘못된_해시와_인증_변경과_만료도_거부한다() {
    // given
    let directory = TestDirectory::new();
    let store = ResultStore::new(directory.0.clone()).unwrap();
    let hash = "a".repeat(64);
    let token = store
        .spatial_token_bound_reserved(state(), None, &hash, None)
        .unwrap();
    // when
    let actual = bound_rejection_scenario(&store, &token, &hash);
    // then
    assert_eq!(
        actual,
        vec![
            ErrorCode::InvalidArgument,
            ErrorCode::AuthRequired,
            ErrorCode::CursorExpired
        ]
    );
}
