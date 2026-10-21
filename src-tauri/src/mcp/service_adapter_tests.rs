use super::*;
use crate::{
    service_native::tests::Harness,
    services::{dispatch::CallerTransport, transport::LegacyMcpBackend},
};
use std::sync::{atomic::Ordering, Arc};

fn adapter(h: &Harness) -> NativeLegacyMcp {
    NativeLegacyMcp::new(Arc::downgrade(&h.core))
}
async fn query(h: &Harness, format: &str) -> Value {
    adapter(h).call("run_query",json!({"connection_id":h.id,"query":"select '$1; original' /* unchanged */","limit":3,"output_format":format}),&h.core.caller(CallerTransport::Mcp).unwrap()).await.unwrap()
}

#[tokio::test]
async fn 기존_다섯_도구와_공통_오십_도구가_같은_카탈로그에_존재한다() {
    // given
    let h = Harness::new().await;
    h.core.transport.set_legacy(Arc::new(adapter(&h))).unwrap();
    // when
    let actual = h.core.transport.tools().unwrap();
    // then
    assert_eq!(actual["tools"].as_array().unwrap().len(), 55);
    assert!(actual["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "run_query"
            && tool["inputSchema"]["properties"]["write"]["type"] == "boolean"));
    assert!(actual["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "run_query"
            && tool["inputSchema"]["properties"].get("container").is_some()));
}
#[tokio::test]
async fn 구형_쿼리_도구도_원본_문장과_불변_참조를_보존한다() {
    // given
    let h = Harness::new().await;
    // when
    let actual = query(&h, "json").await;
    // then
    assert_eq!(actual["structuredContent"]["status"], "succeeded");
    assert!(actual["structuredContent"]["result_id"].is_string());
    assert_eq!(
        actual["structuredContent"]["data"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        h.state.calls.lock().unwrap()[0].1["text"],
        "select '$1; original' /* unchanged */"
    );
    assert_eq!(h.state.legacy.load(Ordering::SeqCst), 0);
    assert!(serde_json::to_vec(&actual).unwrap().len() <= 64 * 1024);
}
#[tokio::test]
async fn 기존_문자_출력_포맷은_공통_응답에서도_유지한다() {
    // given
    let h = Harness::new().await;
    // when
    let actual = query(&h, "toon").await;
    // then
    assert_eq!(actual["structuredContent"]["status"], "succeeded");
    assert!(actual["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("result_id"));
    assert_ne!(
        actual["content"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .next(),
        Some('{')
    );
}
#[tokio::test]
async fn 계약에_없는_인덱스와_외래키는_없다고_꾸미지_않는다() {
    // given
    let h = Harness::new().await;
    let caller = h.core.caller(CallerTransport::Mcp).unwrap();
    // when
    let actual = adapter(&h)
        .call(
            "describe_table",
            json!({"connection_id":h.id,"table_name":"records","schema":"public"}),
            &caller,
        )
        .await
        .unwrap();
    // then
    assert_eq!(actual["structuredContent"]["data"]["indexes"], Value::Null);
    assert_eq!(
        actual["structuredContent"]["data"]["foreign_keys"],
        Value::Null
    );
    assert_eq!(
        actual["structuredContent"]["data"]["unsupported_fields"],
        json!(["indexes", "foreign_keys"])
    );
    assert_eq!(
        actual["structuredContent"]["data"]["columns"][0]["primary_key_position"],
        2
    );
}
#[tokio::test]
async fn 인증_오류는_고정된_오류로_반환하고_구형_인증_경로를_사용하지_않는다() {
    // given
    let h = Harness::new().await;
    *h.state.capability_error.lock().unwrap() = Some(ErrorCode::AuthRequired);
    // when
    let actual = query(&h, "json").await;
    // then
    assert_eq!(actual["isError"], true);
    assert_eq!(
        actual["structuredContent"]["error"]["code"],
        "AUTH_REQUIRED"
    );
    assert_eq!(h.state.legacy.load(Ordering::SeqCst), 0);
    assert!(h.state.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn 관계형_쿼리에_문서_컨테이너를_지정하면_추측하지_않고_거부한다() {
    // given
    let h = Harness::new().await;
    let caller = h.core.caller(CallerTransport::Mcp).unwrap();
    // when
    let actual = adapter(&h)
        .call(
            "run_query",
            json!({"connection_id":h.id,"query":"select 1","container":"unexpected"}),
            &caller,
        )
        .await
        .unwrap();
    // then
    assert_eq!(
        actual["structuredContent"]["error"]["code"],
        "INVALID_ARGUMENT"
    );
    assert!(h.state.calls.lock().unwrap().is_empty());
}
#[test]
fn 큰_공간_응답은_다섯_샘플과_참조만_남기고_좌표와_내부_상태를_제외한다() {
    // given
    let request = catalog::request(Operation::QueryExecute, None, json!({}));
    let mut response = ServiceResponse::failure(&request, ErrorCode::InvalidArgument, "fixed");
    response.data = json!({"features":(0..10).map(|n|json!({"id":n,"geometry":{"coordinates":["COORDINATE_CANARY"]},"properties":{"label":"x".repeat(20000)}})).collect::<Vec<_>>(),"native_state":"PRIVATE_CANARY"});
    response.result_id = Some("result-reference".into());
    // when
    let actual = summary(&response);
    // then
    assert_eq!(actual["data"]["features"].as_array().unwrap().len(), 5);
    assert_eq!(actual["result_id"], "result-reference");
    assert_eq!(actual["presentation_truncated"], true);
    assert!(!actual.to_string().contains("CANARY"));
    assert!(serde_json::to_vec(&actual).unwrap().len() <= 64 * 1024);
}
