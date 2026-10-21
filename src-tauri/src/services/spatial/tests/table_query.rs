use super::spatial_support::*;
use crate::services::contracts::ResumeMode;
use serde_json::json;
async fn drain(
    h: &Harness,
    input: crate::services::contracts::ServiceRequest,
) -> Vec<crate::services::contracts::ServiceResponse> {
    let mut request = input;
    let mut pages = vec![];
    for _ in 0..4 {
        request.request_id = uuid::Uuid::new_v4().to_string();
        let page = h.call(&request).await;
        let next = page.page.next_token.clone();
        pages.push(page);
        match next {
            Some(next) => request.input["next_token"] = json!(next),
            None => break,
        }
    }
    pages
}
#[tokio::test]
async fn 네이티브_커서가_없어도_호스트_바이트_여유로_남은_행을_모두_보존한다() {
    // given
    let h = Harness::new();
    h.spatial
        .responses
        .lock()
        .unwrap()
        .push_back(Ok(feature_page(2, 4_170_000, None)));
    let input = table(2);
    // when
    let actual = drain(&h, input).await;
    // then
    assert_eq!(actual.len(), 2);
    assert!(actual.iter().all(|page| page.error.is_none()));
    assert_eq!(actual[0].page.resume_mode, ResumeMode::Materialized);
    assert_eq!(
        actual
            .iter()
            .map(|page| page.data["features"].as_array().unwrap().len())
            .sum::<usize>(),
        2
    );
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn 네이티브의_마지막_잘림_표시가_있어도_호스트_잔여_행을_버리지_않는다() {
    // given
    let h = Harness::new();
    let mut page = feature_page(2, 4_170_000, None);
    page["limits"] = json!({"truncated":true,"reasons":["LAYER_LIMIT"]});
    h.spatial.responses.lock().unwrap().push_back(Ok(page));
    let input = table(2);
    // when
    let actual = drain(&h, input).await;
    // then
    assert_eq!(actual.len(), 2);
    assert!(actual
        .iter()
        .all(|response| response.error.is_none() && response.limits.truncated));
    assert_eq!(
        actual
            .iter()
            .map(|response| response.data["features"].as_array().unwrap().len())
            .sum::<usize>(),
        2
    );
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
    assert!(!actual[1].page.has_more);
}
async fn table_changed_then_replay(
    h: &Harness,
) -> Vec<crate::services::contracts::ServiceResponse> {
    let first = h.call(&table(2)).await;
    let mut next = table(2);
    next.input["next_token"] = json!(first.page.next_token);
    let mut changed = next.clone();
    changed.input["page_size"] = json!(1);
    let rejected = h.call(&changed).await;
    let second = h.call(&next).await;
    next.request_id = uuid::Uuid::new_v4().to_string();
    let replay = h.call(&next).await;
    vec![first, rejected, second, replay]
}
#[tokio::test]
async fn 테이블_본문_변경을_거부한_뒤에도_올바른_잔여_페이지를_재생한다() {
    // given
    let h = Harness::new();
    h.spatial
        .responses
        .lock()
        .unwrap()
        .push_back(Ok(feature_page(2, 4_170_000, None)));
    // when
    let actual = table_changed_then_replay(&h).await;
    // then
    assert_eq!(
        actual[1].error.as_ref().unwrap().code,
        crate::services::contracts::ErrorCode::InvalidPageToken
    );
    assert!(actual[2].error.is_none());
    assert_eq!(actual[2].data, actual[3].data);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
    assert!(serde_json::to_vec(&actual[0]).unwrap().len() < 8 * 1024 * 1024);
    assert!(serde_json::to_vec(&actual[2]).unwrap().len() < 8 * 1024 * 1024);
}
#[tokio::test]
async fn 잘못된_키_인코딩과_원격_메타데이터는_공개_페이지로_게시하지_않는다() {
    // given
    let h = Harness::new();
    let mut page = feature_page(1, 0, None);
    page["features"][0]["id"] = json!("pk:not-canonical!");
    page["row_references"][0]["feature_id"] = page["features"][0]["id"].clone();
    h.spatial.responses.lock().unwrap().push_back(Ok(page));
    let input = table(1);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(
        actual.error.unwrap().code,
        crate::services::contracts::ErrorCode::InvalidArgument
    );
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
    assert_eq!(cache_count(&h.base.runtime, "r-"), 0);
    assert_eq!(cache_count(&h.base.runtime, "q-"), 0);
}

#[tokio::test]
async fn 단일_객체가_호스트_페이지_바이트를_넘으면_누락_성공을_반환하지_않는다() {
    // given
    let h = Harness::new();
    h.spatial
        .responses
        .lock()
        .unwrap()
        .push_back(Ok(feature_page(1, 8 * 1024 * 1024 - 32768, None)));
    let input = table(1);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(
        actual.error.unwrap().code,
        crate::services::contracts::ErrorCode::FeatureTooLarge
    );
    assert!(actual.result_id.is_none());
    assert_eq!(cache_count(&h.base.runtime, "r-"), 0);
    assert_eq!(cache_count(&h.base.runtime, "q-"), 0);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn 명시한_skip만_과대한_객체를_안전한_오류와_함께_생략한다() {
    // given
    let h = Harness::new();
    h.spatial
        .responses
        .lock()
        .unwrap()
        .push_back(Ok(feature_page(1, 8 * 1024 * 1024 - 32768, None)));
    let mut input = table(1);
    input.input["skip_invalid"] = json!(true);
    // when
    let actual = h.call(&input).await;
    // then
    assert!(actual.error.is_none());
    assert_eq!(actual.data["features"], json!([]));
    assert_eq!(
        actual.data["feature_errors"][0]["code"],
        "FEATURE_TOO_LARGE"
    );
    assert_eq!(actual.data["feature_errors"][0]["index"], 0);
    assert!(actual.limits.truncated);
    assert!(!actual.page.has_more);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn 단일_공간_객체의_십만_좌표_초과는_캐시를_게시하지_않는다() {
    // given
    let h = Harness::new();
    let mut page = feature_page(1, 0, None);
    page["features"][0]["geometry"] = json!({"type":"LineString","coordinates":vec![[1,2];100001]});
    h.spatial.responses.lock().unwrap().push_back(Ok(page));
    let input = table(1);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(
        actual.error.unwrap().code,
        crate::services::contracts::ErrorCode::FeatureTooLarge
    );
    assert!(actual.result_id.is_none());
    assert_eq!(cache_count(&h.base.runtime, "r-"), 0);
    assert_eq!(cache_count(&h.base.runtime, "q-"), 0);
}
