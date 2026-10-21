use super::spatial_support::*;
use crate::services::contracts::{ErrorCode, Operation};
use serde_json::json;

#[tokio::test]
async fn 공간_열은_명시한_테이블만_조사하고_실제_메타데이터를_보존한다() {
    // given
    let h = Harness::new();
    let input = request(
        Operation::SpatialColumns,
        json!({"table":{"database":"fixture","schema":"public","table":"places"}}),
    );
    // when
    let actual = h.call(&input).await;
    // then
    assert!(actual.error.is_none());
    assert_eq!(actual.data["columns"][0]["name"], "geom");
    assert_eq!(actual.data["columns"][0]["column_index"], 1);
    assert_eq!(actual.data["columns"][0]["primary_key"], json!(["id"]));
    assert!(actual.data["columns"][0].get("column").is_none());
    assert_eq!(h.spatial.calls.lock().unwrap()[0].2, input.input);
}
#[tokio::test]
async fn 공간_export는_네_연산_등록에_섞이지_않고_성공하지_않는다() {
    // given
    let h = Harness::new();
    let input = request(Operation::SpatialExport, json!({}));
    // when
    let actual = h.call(&input).await;
    // then
    assert!(actual.error.is_some());
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 0);
    assert_eq!(
        crate::services::extensions::OperationExtension::operations(h.domain.as_ref()),
        &[
            Operation::SpatialColumns,
            Operation::SpatialQueryResult,
            Operation::SpatialTableQuery,
            Operation::SpatialFeature
        ]
    );
}
#[tokio::test]
async fn 접근이_거부되면_인증이나_드라이버를_호출하지_않는다() {
    // given
    let h = Harness::new();
    h.base.policy.denied.lock().unwrap().push("conn-a".into());
    let input = request(
        Operation::SpatialColumns,
        json!({"table":{"database":"fixture","schema":"public","table":"places"}}),
    );
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(h.spatial.calls.lock().unwrap().len(), 0);
}

async fn parallel_cancel(
    h: &RaceHarness,
    driver: &BlockingSpatial,
    first: crate::services::contracts::ServiceRequest,
    second: crate::services::contracts::ServiceRequest,
) -> (
    crate::services::contracts::ServiceResponse,
    crate::services::contracts::ServiceResponse,
    crate::services::contracts::ServiceResponse,
    crate::services::contracts::JobStatus,
) {
    let a = h.runtime.submit(first, super::support::caller()).await;
    let b = h.runtime.submit(second, super::support::caller()).await;
    tokio::join!(
        driver.gates["conn-a"].entered(1),
        driver.gates["conn-b"].entered(1)
    );
    let before = h
        .runtime
        .jobs
        .get(
            b.job_id.as_deref().unwrap(),
            "map-owner",
            &super::support::caller(),
        )
        .unwrap()
        .status;
    let cancelled = h
        .runtime
        .call_validated(
            &request(
                Operation::JobCancel,
                json!({"job_id":a.job_id,"owner_instance_id":"map-owner"}),
            ),
            &super::support::caller(),
        )
        .await;
    driver.gates["conn-b"].release();
    (
        cancelled,
        terminal(&h.runtime, a.job_id.as_deref().unwrap()).await,
        terminal(&h.runtime, b.job_id.as_deref().unwrap()).await,
        before,
    )
}
#[tokio::test]
async fn 서로_다른_연결의_같은_요청은_동시에_진입하고_한쪽만_취소한다() {
    // given
    let driver = std::sync::Arc::new(BlockingSpatial {
        gates: [
            ("conn-a".into(), SignalGate::new()),
            ("conn-b".into(), SignalGate::new()),
        ]
        .into(),
        cancelled: Default::default(),
        calls: Default::default(),
    });
    let h = RaceHarness::new(driver.clone());
    let first = request(
        Operation::SpatialColumns,
        json!({"table":{"database":"fixture","schema":"public","table":"places"}}),
    );
    let mut second = first.clone();
    second.connection_id = Some("conn-b".into());
    // when
    let actual = parallel_cancel(&h, &driver, first, second).await;
    // then
    assert!(actual.0.error.is_none());
    assert_eq!(
        actual.1.status,
        crate::services::contracts::JobStatus::Cancelled
    );
    assert!(actual.2.error.is_none());
    assert_eq!(actual.3, crate::services::contracts::JobStatus::Running);
    assert_eq!(driver.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(driver.cancelled.lock().unwrap().len(), 1);
    assert_eq!(driver.cancelled.lock().unwrap()[0].0, "conn-a");
}
async fn cancel_before_cache(
    h: &RaceHarness,
    input: crate::services::contracts::ServiceRequest,
) -> crate::services::contracts::ServiceResponse {
    let submitted = h.runtime.submit(input, super::support::caller()).await;
    h.policy.gate.entered(1).await;
    h.runtime
        .call_validated(
            &request(
                Operation::JobCancel,
                json!({"job_id":submitted.job_id,"owner_instance_id":"map-owner"}),
            ),
            &super::support::caller(),
        )
        .await;
    h.policy.gate.release();
    terminal(&h.runtime, submitted.job_id.as_deref().unwrap()).await
}
#[tokio::test]
async fn 변환_완료_후_권한_대기_중_취소되면_새_캐시와_토큰을_게시하지_않는다() {
    // given
    let driver = Spatial::new();
    let h = RaceHarness::new(driver.clone());
    let id = h.snapshot();
    h.policy
        .pause_at
        .store(5, std::sync::atomic::Ordering::SeqCst);
    let input = query(&id, 1);
    // when
    let actual = cancel_before_cache(&h, input).await;
    // then
    assert_eq!(
        actual.status,
        crate::services::contracts::JobStatus::Cancelled
    );
    assert!(actual.result_id.is_none());
    assert!(actual.page.next_token.is_none());
    assert_eq!(driver.calls.lock().unwrap().len(), 1);
    assert_eq!(cache_count(&h.runtime, "r-"), 1);
    assert_eq!(cache_count(&h.runtime, "q-"), 0);
    assert_eq!(cache_count(&h.runtime, "s-"), 0);
}
async fn revoke_during_transform(
    h: &RaceHarness,
    driver: &BlockingSpatial,
    input: crate::services::contracts::ServiceRequest,
) -> crate::services::contracts::ServiceResponse {
    let runtime = h.runtime.clone();
    let task = tokio::spawn(async move {
        runtime
            .call_validated(&input, &super::support::caller())
            .await
    });
    driver.gates["conn-a"].entered(1).await;
    h.base
        .connections
        .0
        .lock()
        .unwrap()
        .get_mut("conn-a")
        .unwrap()
        .auth_revision = "auth-revoked".into();
    h.runtime
        .results
        .invalidate_connection_revision("conn-a", "auth-revoked")
        .unwrap();
    driver.gates["conn-a"].release();
    task.await.unwrap()
}
#[tokio::test]
async fn 변환_중_인증이_바뀌면_이전_주체의_공간_결과를_게시하지_않는다() {
    // given
    let driver = std::sync::Arc::new(BlockingSpatial {
        gates: [("conn-a".into(), SignalGate::new())].into(),
        cancelled: Default::default(),
        calls: Default::default(),
    });
    let h = RaceHarness::new(driver.clone());
    let id = h.snapshot();
    let input = query(&id, 1);
    // when
    let actual = revoke_during_transform(&h, &driver, input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::AuthRequired);
    assert!(actual.result_id.is_none());
    assert!(actual.page.next_token.is_none());
    assert_eq!(cache_count(&h.runtime, "r-"), 0);
    assert_eq!(cache_count(&h.runtime, "q-"), 0);
    assert_eq!(driver.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
async fn change_map_during_transform(
    h: &RaceHarness,
    driver: &BlockingSpatial,
    input: crate::services::contracts::ServiceRequest,
    map: &str,
) -> crate::services::contracts::ServiceResponse {
    let runtime = h.runtime.clone();
    let task = tokio::spawn(async move {
        runtime
            .call_validated(&input, &super::support::caller())
            .await
    });
    driver.gates["conn-a"].entered(1).await;
    h.base
        .call(&super::support::request(
            Operation::MapUpdate,
            Some(map),
            Some(1),
            json!({"name":"new map"}),
        ))
        .await;
    driver.gates["conn-a"].release();
    task.await.unwrap()
}
#[tokio::test]
async fn 명시한_지도가_변환_중_변경되면_이전_세대의_캐시를_게시하지_않는다() {
    // given
    let driver = std::sync::Arc::new(BlockingSpatial {
        gates: [("conn-a".into(), SignalGate::new())].into(),
        cancelled: Default::default(),
        calls: Default::default(),
    });
    let h = RaceHarness::new(driver.clone());
    let id = h.snapshot();
    let map = h.base.create().await;
    super::super::MapDomain::attach(
        &h.runtime,
        h.base.domain.store().clone(),
        h.base.gui.clone(),
        h.base.capability.clone(),
    )
    .unwrap();
    h.runtime
        .call_validated(
            &super::support::layer(&map, 0, "conn-a", super::support::query_source(&id)),
            &super::support::caller(),
        )
        .await;
    let mut input = query(&id, 1);
    input.input["source_srid"] = json!(4326);
    input.map_id = Some(map.clone());
    input.expected_version = Some(1);
    // when
    let actual = change_map_during_transform(&h, &driver, input, &map).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::VersionConflict);
    assert!(actual.result_id.is_none());
    assert_eq!(cache_count(&h.runtime, "r-"), 1);
    assert_eq!(cache_count(&h.runtime, "q-"), 0);
    assert_eq!(driver.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
