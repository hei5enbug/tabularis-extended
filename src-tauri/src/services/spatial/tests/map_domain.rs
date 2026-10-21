use super::support::*;
use crate::services::{contracts::*, dispatch::CallerTransport, extensions::OperationExtension};
use serde_json::json;
use std::sync::atomic::Ordering;

async fn all_operations(h: &Harness) -> Vec<ServiceResponse> {
    let created = h
        .call(&request(
            Operation::MapCreate,
            None,
            None,
            json!({"name":"fixture"}),
        ))
        .await;
    let id = created.data["map"]["map_id"].as_str().unwrap();
    let updated = h
        .call(&request(
            Operation::MapUpdate,
            Some(id),
            Some(0),
            json!({"name":"renamed"}),
        ))
        .await;
    let added = h.call(&layer(id, 1, "conn-a", table())).await;
    let layer_id = added.data["map"]["layers"][0]["layer_id"].as_str().unwrap();
    let layer_updated = h
        .call(&request(
            Operation::MapLayerUpdate,
            Some(id),
            Some(2),
            json!({"layer_id":layer_id,"visible":false}),
        ))
        .await;
    let viewport = h
        .call(&request(
            Operation::MapViewportSet,
            Some(id),
            Some(3),
            json!({"viewport":{"west":-5,"east":5,"south":-5,"north":5}}),
        ))
        .await;
    let selected = h
        .call(&request(
            Operation::MapSelectionSet,
            Some(id),
            Some(4),
            json!({"feature_refs":[{"layer_id":layer_id,"feature_id":"known-feature"}]}),
        ))
        .await;
    let get = h
        .call(&request(Operation::MapGet, Some(id), None, json!({})))
        .await;
    let list = h
        .call(&request(Operation::MapList, None, None, json!({})))
        .await;
    let opened = h
        .call(&request(
            Operation::MapOpen,
            Some(id),
            None,
            json!({"gui_instance_id":"gui-a"}),
        ))
        .await;
    let saved = h
        .call(&request(Operation::MapSave, Some(id), Some(5), json!({})))
        .await;
    let closed = h
        .call(&request(
            Operation::MapClose,
            Some(id),
            None,
            json!({"gui_instance_id":"gui-a"}),
        ))
        .await;
    let removed = h
        .call(&request(
            Operation::MapLayerRemove,
            Some(id),
            Some(5),
            json!({"layer_id":layer_id}),
        ))
        .await;
    vec![
        created,
        updated,
        added,
        layer_updated,
        viewport,
        selected,
        get,
        list,
        opened,
        saved,
        closed,
        removed,
    ]
}
#[tokio::test]
async fn 열두_지도_작업은_같은_저장소와_명시적_연결을_사용하며_쿼리를_실행하지_않는다() {
    // given
    let h = Harness::new();
    // when
    let actual = all_operations(&h).await;
    // then
    assert_eq!(actual.len(), 12);
    assert!(
        actual
            .iter()
            .all(|response| response.status == JobStatus::Succeeded),
        "{:?}",
        actual.iter().map(|value| &value.error).collect::<Vec<_>>()
    );
    assert_eq!(actual[11].data["map"]["version"], 6);
    assert_eq!(actual[11].data["map"]["selected_feature_refs"], json!([]));
    assert_eq!(actual[9].data["generation"], 6);
    assert_eq!(actual[10].data["generation"], 7);
    assert_eq!(h.gui.calls.lock().unwrap().len(), 2);
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn replay_gui(h: &Harness, input: &ServiceRequest) -> Vec<ServiceResponse> {
    h.gui.mode.store(1, Ordering::SeqCst);
    let failed = h.call(input).await;
    h.gui.mode.store(0, Ordering::SeqCst);
    let recovered = h.call(input).await;
    let changed = h
        .call(&request(
            Operation::MapUpdate,
            input.map_id.as_deref(),
            Some(0),
            json!({"name":"advanced"}),
        ))
        .await;
    let late = h.call(input).await;
    vec![failed, recovered, changed, late]
}
#[tokio::test]
async fn ack_실패_후_명시적_재생은_같은_세대를_보내고_진행된_상태는_다시_적용하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    let input = request(
        Operation::MapOpen,
        Some(&id),
        None,
        json!({"gui_instance_id":"gui-a"}),
    );
    // when
    let actual = replay_gui(&h, &input).await;
    // then
    assert_eq!(actual[0].error.as_ref().unwrap().outcome, Outcome::Applied);
    assert_eq!(actual[0].data["gui_applied"], false);
    assert_eq!(actual[1].data["gui_applied"], true);
    assert_eq!(actual[0].data["generation"], actual[1].data["generation"]);
    assert_eq!(
        actual[3].error.as_ref().unwrap().code,
        ErrorCode::VersionConflict
    );
    assert_eq!(actual[3].error.as_ref().unwrap().outcome, Outcome::Applied);
    assert_eq!(h.gui.calls.lock().unwrap().len(), 2);
    assert!(h
        .gui
        .calls
        .lock()
        .unwrap()
        .windows(2)
        .all(|calls| calls[0].1.generation == calls[1].1.generation));
}
async fn bad_ack(h: &Harness, id: &str) -> Vec<ServiceResponse> {
    let mut values = vec![];
    for mode in [2, 3] {
        h.gui.mode.store(mode, Ordering::SeqCst);
        values.push(
            h.call(&request(
                Operation::MapOpen,
                Some(id),
                None,
                json!({"gui_instance_id":"gui-a"}),
            ))
            .await,
        );
    }
    values.push(
        h.call(&request(Operation::MapGet, Some(id), None, json!({})))
            .await,
    );
    values
}
#[tokio::test]
async fn 잘못된_ack와_gui_미실행은_게시된_지도를_유지하고_applied로_보고한다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    // when
    let actual = bad_ack(&h, &id).await;
    // then
    assert_eq!(
        actual[0].error.as_ref().unwrap().code,
        ErrorCode::ApplyTimeout
    );
    assert_eq!(
        actual[1].error.as_ref().unwrap().code,
        ErrorCode::GuiUnavailable
    );
    assert!(actual[..2]
        .iter()
        .all(|value| value.error.as_ref().unwrap().outcome == Outcome::Applied));
    assert_eq!(actual[2].data["generation"], 2);
    assert_eq!(actual[2].data["map"]["version"], 0);
}
async fn query_table_viewport(h: &Harness, id: &str, result: &str) -> ServiceResponse {
    h.call(&layer(id, 0, "conn-a", query_source(result))).await;
    h.call(&layer(id, 1, "conn-b", table())).await;
    h.call(&request(
        Operation::MapViewportSet,
        Some(id),
        Some(2),
        json!({"viewport":{"west":-2,"east":2,"south":-3,"north":3}}),
    ))
    .await
}
#[tokio::test]
async fn 뷰포트는_테이블_원본의_세대만_바꾸고_저장된_결과를_재실행하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    let result = h.snapshot();
    // when
    let actual = query_table_viewport(&h, &id, &result).await;
    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(actual.data["map"]["layers"][0]["generation"], 0);
    assert_eq!(actual.data["map"]["layers"][1]["generation"], 1);
    assert_eq!(
        actual.data["map"]["layers"][0]["source"]["result_id"],
        result
    );
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn expired_metadata(h: &Harness, id: &str, result: &str) -> Vec<ServiceResponse> {
    expire_result(h, result);
    let get = h
        .call(&request(Operation::MapGet, Some(id), None, json!({})))
        .await;
    let saved = h
        .call(&request(Operation::MapSave, Some(id), Some(1), json!({})))
        .await;
    let new_layer = h.call(&layer(id, 1, "conn-a", query_source(result))).await;
    vec![get, saved, new_layer]
}
#[tokio::test]
async fn 만료된_결과_참조는_지도_메타데이터에_경고를_남기며_새_레이어는_거부한다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    let result = h.snapshot();
    h.call(&layer(&id, 0, "conn-a", query_source(&result)))
        .await;
    // when
    let actual = expired_metadata(&h, &id, &result).await;
    // then
    assert_eq!(
        actual[0].warnings,
        ["QUERY_RESULT_REFERENCE_EXPIRED"],
        "{:?}",
        actual[0]
    );
    assert_eq!(actual[1].warnings, ["QUERY_RESULT_REFERENCE_EXPIRED"]);
    assert_eq!(
        actual[2].error.as_ref().unwrap().code,
        ErrorCode::ResultExpired
    );
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn revoked_queries(h: &Harness, id: &str, result: &str) -> Vec<ServiceResponse> {
    expire_result(h, result);
    *h.policy.identity.lock().unwrap() = "changed".into();
    let policy = h
        .call(&request(Operation::MapGet, Some(id), None, json!({})))
        .await;
    *h.policy.identity.lock().unwrap() = "fixture-policy".into();
    h.connections
        .0
        .lock()
        .unwrap()
        .get_mut("conn-a")
        .unwrap()
        .auth_revision = "rotated".into();
    let auth = h
        .call(&request(Operation::MapGet, Some(id), None, json!({})))
        .await;
    let list = h
        .call(&request(Operation::MapList, None, None, json!({})))
        .await;
    vec![policy, auth, list]
}
#[tokio::test]
async fn 인증과_정책_변경은_결과_만료_경고보다_먼저_지도를_차단한다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    let result = h.snapshot();
    h.call(&layer(&id, 0, "conn-a", query_source(&result)))
        .await;
    // when
    let actual = revoked_queries(&h, &id, &result).await;
    // then
    assert_eq!(
        actual[0].error.as_ref().unwrap().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        actual[1].error.as_ref().unwrap().code,
        ErrorCode::AuthRequired
    );
    assert_eq!(actual[2].data["maps"], json!([]));
    assert!(actual[..2].iter().all(|value| value.warnings.is_empty()));
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn access_scopes(h: &Harness, id: &str) -> Vec<ServiceResponse> {
    let mut other = caller();
    other.caller_id = "other-user".into();
    let foreign = h
        .runtime
        .call_validated(
            &request(Operation::MapGet, Some(id), None, json!({})),
            &other,
        )
        .await;
    let mut mcp = caller();
    mcp.transport = CallerTransport::Mcp;
    let shared = h
        .runtime
        .call_validated(&request(Operation::MapGet, Some(id), None, json!({})), &mcp)
        .await;
    h.policy.denied.lock().unwrap().push("conn-b".into());
    let denied = h
        .call(&request(Operation::MapSave, Some(id), Some(2), json!({})))
        .await;
    vec![foreign, shared, denied]
}
#[tokio::test]
async fn 지도_소유자는_전송방식과_독립적이고_모든_레이어의_현재_권한이_필요하다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    h.call(&layer(&id, 0, "conn-a", table())).await;
    h.call(&layer(&id, 1, "conn-b", table())).await;
    // when
    let actual = access_scopes(&h, &id).await;
    // then
    assert_eq!(
        actual[0].error.as_ref().unwrap().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(actual[1].status, JobStatus::Succeeded);
    assert_eq!(
        actual[2].error.as_ref().unwrap().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn invalid_sources(h: &Harness, id: &str, result: &str) -> Vec<ServiceResponse> {
    let mut column = query_source(result);
    column["column_index"] = json!(1);
    let wrong_column = h.call(&layer(id, 0, "conn-a", column)).await;
    let mut source = table();
    source["source_srid"] = json!(1000000);
    let srid = h.call(&layer(id, 0, "conn-a", source)).await;
    h.capability.supported.store(false, Ordering::SeqCst);
    let unsupported = h.call(&layer(id, 0, "conn-a", table())).await;
    vec![wrong_column, srid, unsupported]
}
#[tokio::test]
async fn 열_인덱스와_srid와_공간_미지원은_레이어_게시_전에_거부한다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    let result = h.snapshot();
    // when
    let actual = invalid_sources(&h, &id, &result).await;
    // then
    assert_eq!(
        actual
            .iter()
            .map(|value| value.error.as_ref().unwrap().code)
            .collect::<Vec<_>>(),
        [
            ErrorCode::InvalidArgument,
            ErrorCode::InvalidArgument,
            ErrorCode::UnsupportedOperation
        ]
    );
    assert!(h
        .domain
        .store()
        .get(&id, &caller())
        .unwrap()
        .map
        .layers
        .is_empty());
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn no_implicit_gui(h: &Harness, id: &str) -> Vec<ServiceResponse> {
    let missing = h
        .call(&request(
            Operation::MapUpdate,
            Some(id),
            Some(0),
            json!({"name":"bad","await_gui":true}),
        ))
        .await;
    let explicit_false = h
        .call(&request(
            Operation::MapUpdate,
            Some(id),
            Some(0),
            json!({"name":"good","await_gui":false,"gui_instance_id":"gui-a"}),
        ))
        .await;
    vec![missing, explicit_false]
}
#[tokio::test]
async fn gui_적용은_명시적으로_요청하고_대상_인스턴스를_지정해야_한다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    // when
    let actual = no_implicit_gui(&h, &id).await;
    // then
    assert_eq!(
        actual[0].error.as_ref().unwrap().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(actual[1].status, JobStatus::Succeeded);
    assert_eq!(actual[1].data["map"]["version"], 1);
    assert!(h.gui.calls.lock().unwrap().is_empty());
}
async fn cancelled_prepare(h: &Harness, id: &str) -> ServiceResponse {
    let input = layer(id, 0, "conn-a", table());
    let lease = h
        .runtime
        .jobs
        .begin(&input, &caller(), false, false)
        .await
        .unwrap();
    let signal = lease.signal();
    h.capability
        .gate
        .lock()
        .unwrap()
        .replace(std::sync::Arc::new(tokio::sync::Barrier::new(2)));
    signal.cancel();
    super::super::MapDomain::execute(&h.domain, &input, &caller(), &lease)
        .await
        .err()
        .map(|error| ServiceResponse::failure(&input, error.code, error.message))
        .unwrap()
}
#[tokio::test]
async fn 게시_전_취소는_레이어와_db_호출을_남기지_않는다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    // when
    let actual = cancelled_prepare(&h, &id).await;
    // then
    assert_eq!(actual.error.as_ref().unwrap().code, ErrorCode::Cancelled);
    assert!(h
        .domain
        .store()
        .get(&id, &caller())
        .unwrap()
        .map
        .layers
        .is_empty());
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn cancel_published(h: &Harness, id: &str) -> ServiceResponse {
    let input = request(
        Operation::MapOpen,
        Some(id),
        None,
        json!({"gui_instance_id":"gui-a"}),
    );
    let caller = caller();
    let lease = h
        .runtime
        .jobs
        .begin(&input, &caller, false, false)
        .await
        .unwrap();
    let signal = lease.signal();
    h.gui.mode.store(4, Ordering::SeqCst);
    let execution = h.domain.execute(&input, &caller, &lease);
    let cancel = async {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while h.gui.calls.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        signal.cancel();
    };
    let (response, _) = tokio::join!(execution, cancel);
    response.unwrap()
}
#[tokio::test]
async fn 게시_후_gui_대기_취소는_이미_저장된_세대를_applied로_반환한다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    // when
    let actual = cancel_published(&h, &id).await;
    // then
    assert_eq!(actual.error.as_ref().unwrap().code, ErrorCode::Cancelled);
    assert_eq!(actual.error.as_ref().unwrap().outcome, Outcome::Applied);
    assert_eq!(actual.data["generation"], 1);
    assert_eq!(
        h.domain
            .store()
            .get(&id, &caller())
            .unwrap()
            .apply_generation,
        1
    );
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn changed_during_prepare(h: &Harness, id: &str) -> ServiceResponse {
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    *h.capability.gate.lock().unwrap() = Some(barrier.clone());
    let input = layer(id, 0, "conn-a", table());
    let execution = h.call(&input);
    let change = async {
        while h.capability.entered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        h.connections
            .0
            .lock()
            .unwrap()
            .get_mut("conn-a")
            .unwrap()
            .auth_revision = "rotated".into();
        barrier.wait().await;
    };
    let (response, _) = tokio::join!(execution, change);
    response
}
#[tokio::test]
async fn 비동기_준비_중_인증이_변경되면_마지막_검사에서_게시를_거부한다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    let before = std::fs::read(h.domain.store().root().join(format!("{id}.json"))).unwrap();
    // when
    let actual = changed_during_prepare(&h, &id).await;
    // then
    assert_eq!(actual.error.as_ref().unwrap().code, ErrorCode::AuthRequired);
    assert_eq!(
        std::fs::read(h.domain.store().root().join(format!("{id}.json"))).unwrap(),
        before
    );
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn parallel_prepare(
    h: &Harness,
    first: &str,
    second: &str,
) -> (ServiceResponse, ServiceResponse) {
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    *h.capability.gate.lock().unwrap() = Some(barrier.clone());
    let first = layer(first, 0, "conn-a", table());
    let second = layer(second, 0, "conn-b", table());
    let (first, second, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(h.call(&first), h.call(&second), barrier.wait())
    })
    .await
    .unwrap();
    (first, second)
}
#[tokio::test]
async fn 서로_다른_지도의_준비는_파일_잠금이나_전역_db_뮤텍스로_직렬화하지_않는다() {
    // given
    let h = Harness::new();
    let first = h.create().await;
    let second = h.create().await;
    // when
    let actual = parallel_prepare(&h, &first, &second).await;
    // then
    assert_eq!(actual.0.status, JobStatus::Succeeded);
    assert_eq!(actual.1.status, JobStatus::Succeeded);
    assert_eq!(h.capability.entered.load(Ordering::SeqCst), 2);
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn missing_binding(h: &Harness, id: &str) -> ServiceResponse {
    let path = h.domain.store().root().join(format!("{id}.json"));
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["source_bindings"] = json!({});
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    h.call(&request(Operation::MapGet, Some(id), None, json!({})))
        .await
}
#[tokio::test]
async fn 저장된_쿼리_레이어의_인증_바인딩이_없으면_권한을_추측하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    let result = h.snapshot();
    h.call(&layer(&id, 0, "conn-a", query_source(&result)))
        .await;
    // when
    let actual = missing_binding(&h, &id).await;
    // then
    assert_eq!(actual.error.as_ref().unwrap().code, ErrorCode::AuthRequired);
    assert!(actual.data.is_null());
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn 문서_결과는_표_기하_열처럼_해석하지_않는다() {
    use crate::services::results::{ResultKind, ResultSet, ResultSnapshot};
    // given
    let h = Harness::new();
    let id = h.create().await;
    let result = h
        .runtime
        .results
        .insert(
            binding(),
            ResultSnapshot {
                sets: vec![ResultSet {
                    kind: ResultKind::JsonValues,
                    columns: vec![json!({"name":"value"})],
                    rows: vec![json!(5)],
                }],
                revision: "fixture-result".into(),
                limits: Limits::default(),
                allow_truncation: false,
                warnings: vec![],
            },
        )
        .unwrap();
    // when
    let actual = h
        .call(&layer(&id, 0, "conn-a", query_source(&result)))
        .await;
    // then
    assert_eq!(
        actual.error.as_ref().unwrap().code,
        ErrorCode::InvalidArgument
    );
    assert!(h
        .domain
        .store()
        .get(&id, &caller())
        .unwrap()
        .map
        .layers
        .is_empty());
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn gui_ack_시간초과도_현재_지도와_미적용_렌더_상태를_반환한다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    h.gui.mode.store(4, Ordering::SeqCst);
    let input = request(
        Operation::MapOpen,
        Some(&id),
        None,
        json!({"gui_instance_id":"gui-a"}),
    );
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.as_ref().unwrap().outcome, Outcome::Applied);
    assert_eq!(actual.error.as_ref().unwrap().code, ErrorCode::ApplyTimeout);
    assert_eq!(actual.data["gui_applied"], false);
    assert_eq!(actual.data["rendered_version"], serde_json::Value::Null);
    assert_eq!(
        h.domain
            .store()
            .get(&id, &caller())
            .unwrap()
            .apply_generation,
        1
    );
    assert!(h.directory.0.is_absolute());
}
async fn malformed_bindings(h: &Harness, id: &str) -> Vec<ServiceResponse> {
    let path = h.domain.store().root().join(format!("{id}.json"));
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut values = vec![];
    for kind in 0..3 {
        let mut record = original.clone();
        match kind {
            0 => {
                record.as_object_mut().unwrap().remove("source_bindings");
            }
            1 => record["source_bindings"] = json!({"wrong":"value"}),
            _ => {
                let id = record["state"]["layers"][0]["layer_id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                record["source_bindings"][id]["permission_scope"] = json!("");
            }
        }
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        values.push(
            h.call(&request(Operation::MapGet, Some(id), None, json!({})))
                .await,
        );
    }
    values
}
#[tokio::test]
async fn 쿼리_바인딩의_누락과_형식_오류와_빈_권한은_인증_필요로_거부한다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    let result = h.snapshot();
    h.call(&layer(&id, 0, "conn-a", query_source(&result)))
        .await;
    // when
    let actual = malformed_bindings(&h, &id).await;
    // then
    assert!(actual
        .iter()
        .all(|value| value.error.as_ref().unwrap().code == ErrorCode::AuthRequired));
    assert!(actual.iter().all(|value| value.data.is_null()));
    assert_eq!(h.driver.0.load(Ordering::SeqCst), 0);
}
async fn changed_gui_replay(h: &Harness, input: &ServiceRequest) -> Vec<ServiceResponse> {
    let original = h.call(input).await;
    let mut changed = input.clone();
    changed.input["gui_instance_id"] = json!("gui-b");
    let refused = h.call(&changed).await;
    vec![original, refused]
}
#[tokio::test]
async fn 같은_요청의_gui_대상_변경은_재생으로_취급하지_않는다() {
    // given
    let h = Harness::new();
    let id = h.create().await;
    let input = request(
        Operation::MapOpen,
        Some(&id),
        None,
        json!({"gui_instance_id":"gui-a"}),
    );
    // when
    let actual = changed_gui_replay(&h, &input).await;
    // then
    assert_eq!(actual[0].status, JobStatus::Succeeded);
    assert_eq!(
        actual[1].error.as_ref().unwrap().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(h.gui.calls.lock().unwrap().len(), 1);
    assert_eq!(
        h.domain
            .store()
            .get(&id, &caller())
            .unwrap()
            .apply_generation,
        1
    );
}
