use super::spatial_support::*;
use crate::services::contracts::ErrorCode;
use serde_json::{json, Value};
async fn roundtrip(
    h: &Harness,
    id: &str,
) -> (
    crate::services::contracts::ServiceResponse,
    crate::services::contracts::ServiceResponse,
    Value,
) {
    let first = h.call(&query(id, 2)).await;
    let mut next = query(id, 2);
    next.input["next_token"] = first.page.next_token.clone().into();
    let second = h.call(&next).await;
    let binding = h.base.runtime.results.result_binding(id).unwrap();
    let original =
        serde_json::to_value(h.base.runtime.results.snapshot(id, &binding).unwrap()).unwrap();
    (first, second, original)
}
#[tokio::test]
async fn 원본_행_순서와_중복열과_null을_유지하며_sql을_다시_실행하지_않는다() {
    // given
    let h = Harness::new();
    let rows = vec![
        json!([value(), null]),
        json!([null, "two"]),
        json!([value(), "three"]),
    ];
    let id = h.snapshot(columns(), rows.clone());
    // when
    let actual = roundtrip(&h, &id).await;
    // then
    assert!(actual.0.error.is_none());
    assert!(actual.1.error.is_none());
    assert_eq!(actual.0.data["features"][0]["id"], format!("{id}:0:0"));
    assert_eq!(actual.0.data["features"][1]["geometry"], Value::Null);
    assert_eq!(actual.1.data["row_references"][0]["row_ordinal"], 2);
    assert_eq!(
        actual.0.data["features"][0]["properties"]["columns"][1],
        json!({"name":"duplicate","index":1,"value":null})
    );
    assert_eq!(actual.2["sets"][0]["rows"], json!(rows));
    assert_eq!(h.base.driver.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}
async fn mismatch_replay(
    h: &Harness,
    id: &str,
) -> Vec<crate::services::contracts::ServiceResponse> {
    let first = h.call(&query(id, 1)).await;
    let mut next = query(id, 1);
    next.input["next_token"] = first.page.next_token.into();
    let mut changed = next.clone();
    changed.input["longitude_mode"] = json!("shortest");
    let rejected = h.call(&changed).await;
    let second = h.call(&next).await;
    next.request_id = uuid::Uuid::new_v4().to_string();
    let replay = h.call(&next).await;
    vec![rejected, second, replay]
}
#[tokio::test]
async fn 바뀐_본문은_소비_전에_거부하고_올바른_다음_페이지는_그대로_재생한다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(columns(), vec![json!([value(), 1]), json!([value(), 2])]);
    // when
    let actual = mismatch_replay(&h, &id).await;
    // then
    assert_eq!(
        actual[0].error.as_ref().unwrap().code,
        ErrorCode::InvalidPageToken
    );
    assert!(actual[1].error.is_none());
    assert!(actual[2].error.is_none());
    assert_eq!(actual[1].data, actual[2].data);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn 일반_wkt_문자열은_공간_타입으로_추측하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(
        vec![json!({"name":"plain","native_type":"text"})],
        vec![json!(["POINT(1 2)"])],
    );
    let input = query(&id, 1);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::UnsupportedType);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 0);
}
#[tokio::test]
async fn srid가_없으면_skip을_켜도_추측하지_않는다() {
    // given
    let h = Harness::new();
    let mut raw = value();
    raw["srid"] = Value::Null;
    let id = h.snapshot(
        vec![json!({"name":"geom","native_type":"geometry"})],
        vec![json!([raw])],
    );
    let mut input = query(&id, 1);
    input.input["skip_invalid"] = json!(true);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::SridRequired);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn 배치_자원_한계만_이분하고_각_원본_행은_한번만_게시한다() {
    // given
    let h = Harness::new();
    h.spatial.mode.store(1, std::sync::atomic::Ordering::SeqCst);
    let id = h.snapshot(columns(), vec![json!([value(), 1]), json!([value(), 2])]);
    let input = query(&id, 2);
    // when
    let actual = h.call(&input).await;
    // then
    assert!(actual.error.is_none());
    assert_eq!(actual.data["features"].as_array().unwrap().len(), 2);
    assert_eq!(
        h.spatial
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.2["values"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [2, 1, 1]
    );
    assert_eq!(h.base.driver.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}
async fn all_skipped_then_next(
    h: &Harness,
    id: &str,
) -> Vec<crate::services::contracts::ServiceResponse> {
    let mut input = query(id, 2);
    input.input["skip_invalid"] = json!(true);
    let first = h.call(&input).await;
    input.request_id = uuid::Uuid::new_v4().to_string();
    input.input["next_token"] = json!(first.page.next_token);
    vec![first, h.call(&input).await]
}
#[tokio::test]
async fn 전부_생략한_페이지도_다음_원본_순서를_보존하며_원격_메시지를_숨긴다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(
        columns(),
        vec![
            json!([value(), 1]),
            json!([value(), 2]),
            json!([value(), 3]),
        ],
    );
    h.spatial.responses.lock().unwrap().push_back(Ok(json!({"geometries":[null,null],"feature_errors":[{"index":0,"code":"UNSUPPORTED_TYPE","message":"secret-canary-native"},{"index":1,"code":"INVALID_GEOMETRY","message":"secret-canary-native"}],"warnings":["DIMENSIONS_REDUCED_TO_XY"],"limits":{"truncated":true,"reasons":["SKIPPED_FEATURES"]}})));
    // when
    let actual = all_skipped_then_next(&h, &id).await;
    // then
    assert!(actual.iter().all(|response| response.error.is_none()));
    assert_eq!(actual[0].data["features"], json!([]));
    assert!(actual[0].page.has_more);
    assert_eq!(actual[0].data["feature_errors"][0]["index"], 0);
    assert_eq!(actual[0].data["feature_errors"][1]["index"], 1);
    assert_eq!(actual[1].data["row_references"][0]["row_ordinal"], 2);
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("secret-canary-native"));
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn 잘못된_변환_배열은_캐시와_예약을_남기지_않고_재호출하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(columns(), vec![json!([value(), 1]), json!([value(), 2])]);
    let input = query(&id, 2);
    h.spatial.responses.lock().unwrap().push_back(Ok(json!({"geometries":[null],"feature_errors":[],"warnings":[],"limits":{"truncated":false,"reasons":[]}})));
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::InvalidArgument);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
    assert_eq!(cache_count(&h.base.runtime, "r-"), 1);
    assert_eq!(cache_count(&h.base.runtime, "s-"), 0);
    assert_eq!(cache_count(&h.base.runtime, "q-"), 0);
}
#[tokio::test]
async fn 권한_오류는_skip이나_배치_분할로_재시도하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(columns(), vec![json!([value(), 1]), json!([value(), 2])]);
    let mut input = query(&id, 2);
    input.input["skip_invalid"] = json!(true);
    h.spatial
        .responses
        .lock()
        .unwrap()
        .push_back(Err(crate::services::error::failure(
            ErrorCode::PermissionDenied,
        )));
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
    assert_eq!(cache_count(&h.base.runtime, "q-"), 0);
}
#[tokio::test]
async fn 공유_캐시_예약이_가득_차면_새_공간_쿼리는_드라이버_호출_전에_거부한다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(columns(), vec![json!([value(), 1])]);
    let input = query(&id, 1);
    let reservations = (0..12)
        .map(|_| {
            h.base
                .runtime
                .results
                .reserve_query("conn-a", 30000)
                .unwrap()
        })
        .collect::<Vec<_>>();
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ResourceLimit);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 0);
    assert_eq!(reservations.len(), 12);
    assert_eq!(cache_count(&h.base.runtime, "q-"), 12);
}
fn near_layer_token(
    h: &Harness,
    id: &str,
    budget: super::super::paging::Budget,
) -> crate::services::contracts::ServiceRequest {
    let mut input = query(id, 2);
    let source = super::super::paging::source_from_input(&input.input, "query_result").unwrap();
    let binding = h.base.runtime.results.result_binding(id).unwrap();
    let hash = super::super::paging::request_hash(&source, 2, input.operation).unwrap();
    let cursor = super::super::query_result::QueryCursor {
        source: source.clone(),
        source_hash: super::super::paging::source_hash(&source).unwrap(),
        page_size: 2,
        next_row_offset: 0,
        budget,
    };
    let state =
        super::super::paging::cursor_state(&binding, serde_json::to_value(cursor).unwrap(), 2);
    let token = h
        .base
        .runtime
        .results
        .spatial_token_bound_reserved(state, Some(id), &hash, None)
        .unwrap();
    input.input["next_token"] = json!(token);
    input
}
async fn layer_caps(h: &Harness, id: &str) -> Vec<crate::services::contracts::ServiceResponse> {
    let mut responses = vec![];
    for budget in [
        super::super::paging::Budget {
            features: 9999,
            coordinates: 9999,
            bytes: 0,
        },
        super::super::paging::Budget {
            features: 0,
            coordinates: 249999,
            bytes: 0,
        },
        byte_cap_budget(id),
    ] {
        responses.push(h.call(&near_layer_token(h, id, budget)).await);
    }
    responses
}
fn byte_cap_budget(id: &str) -> super::super::paging::Budget {
    use tabularis_spatial_core::{Feature, Geometry, LayerLimits, RowReference};
    let empty =
        super::super::paging::builder(2, Default::default(), Default::default(), vec![], vec![])
            .unwrap()
            .page()
            .clone();
    let mut final_empty = empty.clone();
    final_empty.page = Default::default();
    final_empty.limits.truncate("LAYER_LIMIT");
    let fid = format!("{id}:0:0");
    let properties=json!({"columns":[{"name":"duplicate","index":0,"value":value()},{"name":"duplicate","index":1,"value":1}]}).as_object().unwrap().clone();
    let mut first = empty.clone();
    first.features.push(Feature::new(
        &fid,
        Geometry::from_json(&json!({"type":"Point","coordinates":[1,2]})).unwrap(),
        properties,
    ));
    first.row_references.push(RowReference {
        feature_id: fid,
        identity: None,
        snapshot_id: Some(id.into()),
        row_ordinal: 0,
    });
    let delta = first.serialized_bytes().unwrap() - empty.serialized_bytes().unwrap();
    assert!(delta > 1);
    let remaining = empty
        .serialized_bytes()
        .unwrap()
        .max(final_empty.serialized_bytes().unwrap())
        + delta / 2;
    super::super::paging::Budget {
        features: 0,
        coordinates: 0,
        bytes: LayerLimits::default().max_bytes - remaining,
    }
}
#[tokio::test]
async fn 레이어의_누적_객체와_좌표와_바이트_한계는_다음_토큰을_닫는다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(columns(), vec![json!([value(), 1]), json!([value(), 2])]);
    // when
    let actual = layer_caps(&h, &id).await;
    // then
    assert!(actual.iter().all(|response| response.error.is_none()
        && response.limits.truncated
        && response
            .limits
            .reasons
            .iter()
            .any(|reason| reason == "LAYER_LIMIT")
        && !response.page.has_more
        && response.page.next_token.is_none()));
    assert_eq!(actual[0].data["features"].as_array().unwrap().len(), 1);
    assert_eq!(actual[1].data["features"].as_array().unwrap().len(), 1);
    assert_eq!(actual[2].data["features"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn 왼쪽_배치가_페이지를_채우면_불필요한_오른쪽_변환을_호출하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(
        columns(),
        (0..4)
            .map(|_| json!([value(), "x".repeat(4_170_000)]))
            .collect(),
    );
    let input = query(&id, 4);
    h.spatial.responses.lock().unwrap().extend([Err(crate::services::error::failure(ErrorCode::ResourceLimit)),Ok(json!({"geometries":[{"type":"Point","coordinates":[1,2]},{"type":"Point","coordinates":[1,2]}],"feature_errors":[],"warnings":[],"limits":{"truncated":false,"reasons":[]}}))]);
    // when
    let actual = h.call(&input).await;
    // then
    assert!(actual.error.is_none());
    assert_eq!(actual.data["features"].as_array().unwrap().len(), 1);
    assert!(actual.page.has_more);
    assert_eq!(
        h.spatial
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.2["values"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [4, 2]
    );
    assert_eq!(cache_count(&h.base.runtime, "q-"), 0);
}
fn quota_publication(
    store: &crate::services::results::ResultStore,
) -> (ErrorCode, usize, usize, usize) {
    use crate::services::results::{ResultKind, ResultSet, ResultSnapshot};
    let binding = super::support::binding();
    let reservation = store.reserve_query("conn-a", 30000).unwrap();
    let mut publication = super::super::paging::Publication::new(store, &binding);
    publication
        .cache(
            ResultSnapshot {
                sets: vec![ResultSet {
                    kind: ResultKind::JsonValues,
                    columns: vec![],
                    rows: vec![json!("x".repeat(12 * 1024 * 1024))],
                }],
                revision: binding.result_revision.clone(),
                limits: Default::default(),
                warnings: vec![],
                allow_truncation: false,
            },
            &reservation,
        )
        .unwrap();
    let state =
        super::super::paging::cursor_state(&binding, json!("x".repeat(32 * 1024 * 1024 - 2048)), 1);
    let code = publication
        .token(state, None, &"a".repeat(64))
        .unwrap_err()
        .code;
    drop(publication);
    drop(reservation);
    let count = |prefix: &str| {
        std::fs::read_dir(store.root())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
            .count()
    };
    (code, count("r-"), count("s-"), count("q-"))
}
#[test]
fn 토큰의_공유_quota_실패는_이번_페이지_캐시와_예약을_정리한다() {
    // given
    let directory = super::support::Directory::new();
    let store = crate::services::results::ResultStore::with_limits(
        directory.0.clone(),
        900000,
        42 * 1024 * 1024,
    )
    .unwrap();
    // when
    let actual = quota_publication(&store);
    // then
    assert_eq!(actual, (ErrorCode::ResourceLimit, 0, 0, 0));
}
async fn thousand_rows(h: &Harness, id: &str) -> Vec<crate::services::contracts::ServiceResponse> {
    let first = h.call(&query(id, 1000)).await;
    let mut next = query(id, 1000);
    next.input["next_token"] = json!(first.page.next_token);
    vec![first, h.call(&next).await]
}
#[tokio::test]
async fn 한_페이지의_천개_한도_뒤에도_다음_원본_행을_빠뜨리지_않는다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(
        columns(),
        (0..1001).map(|ordinal| json!([value(), ordinal])).collect(),
    );
    // when
    let actual = thousand_rows(&h, &id).await;
    // then
    assert!(actual.iter().all(|page| page.error.is_none()));
    assert_eq!(actual[0].data["features"].as_array().unwrap().len(), 1000);
    assert_eq!(actual[1].data["row_references"][0]["row_ordinal"], 1000);
    assert!(!actual[1].page.has_more);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 9);
    assert_eq!(h.base.driver.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}
async fn expire_next_source(h: &Harness, id: &str) -> crate::services::contracts::ServiceResponse {
    let first = h.call(&query(id, 1)).await;
    expire(&h.base.runtime, id);
    let mut next = query(id, 1);
    next.input["next_token"] = json!(first.page.next_token);
    h.call(&next).await
}
#[tokio::test]
async fn 원본_결과가_만료되면_새로운_공간_토큰도_쿼리를_재실행하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(columns(), vec![json!([value(), 1]), json!([value(), 2])]);
    // when
    let actual = expire_next_source(&h, &id).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ResultExpired);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
    assert_eq!(h.base.driver.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}
