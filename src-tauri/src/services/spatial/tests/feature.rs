use super::spatial_support::*;
use crate::services::contracts::{ErrorCode, Operation};
use serde_json::json;
async fn cached(h: &Harness, id: &str) -> crate::services::contracts::ServiceResponse {
    let page = h.call(&query(id, 1)).await;
    h.call(&request(
        Operation::SpatialFeature,
        json!({"result_id":page.result_id,"feature_id":page.data["features"][0]["id"]}),
    ))
    .await
}
#[tokio::test]
async fn 공간_캐시_상세는_원본_행을_돌려주고_db를_다시_읽지_않는다() {
    // given
    let h = Harness::new();
    let row = json!([value(), "original"]);
    let id = h.snapshot(columns(), vec![row.clone()]);
    // when
    let actual = cached(&h, &id).await;
    // then
    assert!(actual.error.is_none());
    assert_eq!(actual.data["row"], row);
    assert_eq!(actual.data["source"], "snapshot");
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn 일반_query_결과를_공간_페이지_캐시로_오해하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(columns(), vec![json!([value(), 1])]);
    let input = request(
        Operation::SpatialFeature,
        json!({"result_id":id,"feature_id":"unknown"}),
    );
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::InvalidArgument);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 0);
}

async fn expiry_during_final_permission(
    h: &RaceHarness,
    id: &str,
    input: crate::services::contracts::ServiceRequest,
) -> crate::services::contracts::ServiceResponse {
    let runtime = h.runtime.clone();
    let task = tokio::spawn(async move {
        runtime
            .call_validated(&input, &super::support::caller())
            .await
    });
    h.policy.gate.entered(1).await;
    expire(&h.runtime, id);
    h.policy.gate.release();
    task.await.unwrap()
}
#[tokio::test]
async fn 캐시_상세의_마지막_권한_대기_동안_원본이_만료되면_행을_반환하지_않는다() {
    // given
    let driver = Spatial::new();
    let h = RaceHarness::new(driver.clone());
    let id = h.snapshot();
    let page = h
        .runtime
        .call_validated(&query(&id, 1), &super::support::caller())
        .await;
    h.policy.count.store(0, std::sync::atomic::Ordering::SeqCst);
    h.policy
        .pause_at
        .store(5, std::sync::atomic::Ordering::SeqCst);
    let input = request(
        Operation::SpatialFeature,
        json!({"result_id":page.result_id,"feature_id":format!("{id}:0:0")}),
    );
    // when
    let actual = expiry_during_final_permission(&h, &id, input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ResultExpired);
    assert_eq!(actual.data, serde_json::Value::Null);
    assert_eq!(driver.calls.lock().unwrap().len(), 1);
}
async fn policy_changed_detail(
    h: &Harness,
    id: &str,
) -> crate::services::contracts::ServiceResponse {
    let page = h.call(&query(id, 1)).await;
    *h.base.policy.identity.lock().unwrap() = "changed-policy".into();
    h.call(&request(
        Operation::SpatialFeature,
        json!({"result_id":page.result_id,"feature_id":format!("{id}:0:0")}),
    ))
    .await
}
#[tokio::test]
async fn 캐시_상세도_현재_정책이_바뀌면_원래_행을_노출하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.snapshot(columns(), vec![json!([value(), "old"])]);
    // when
    let actual = policy_changed_detail(&h, &id).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(actual.data, serde_json::Value::Null);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 1);
}
