use super::*;
use crate::service_native::tests::Harness;
use std::sync::Arc;

fn adapter(h: &Harness) -> NativeLegacyMcp {
    NativeLegacyMcp::new(Arc::downgrade(&h.core))
}

#[tokio::test]
async fn 기존_다섯_도구와_공통_도구가_같은_카탈로그에_존재한다() {
    // given
    let h = Harness::new().await;
    h.core.transport.set_legacy(Arc::new(adapter(&h))).unwrap();
    // when
    let actual = h.core.transport.tools().unwrap();
    // then
    let tools = actual["tools"].as_array().unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools.len(), 54);
    assert_eq!(
        names[..5],
        [
            "list_connections",
            "list_databases",
            "list_tables",
            "describe_table",
            "run_query"
        ]
    );
    let service = &names[5..];
    assert_eq!(service.len(), 49);
    assert!(service.contains(&"table_preview") && service.contains(&"job_list"));
    assert!(!names.contains(&"credential_import") && !names.contains(&"connection_delete"));
    assert!(names.iter().all(|name| {
        (1..=64).contains(&name.len())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    }));
    let property = |tool: &str, name: &str| {
        tools
            .iter()
            .find(|candidate| candidate["name"] == tool)
            .unwrap()["inputSchema"]["properties"]
            .get(name)
            .cloned()
    };
    for tool in ["run_query", "list_tables", "describe_table"] {
        assert_eq!(
            property(tool, "database").unwrap()["type"],
            "string",
            "{tool}"
        );
    }
    assert_eq!(property("run_query", "schema").unwrap()["type"], "string");
    assert!(property("run_query", "write").is_none());
    assert!(property("run_query", "container").is_none());
    assert!(property("list_databases", "database").is_none());
}

#[tokio::test]
async fn 모든_도구_이름은_목록에서_한번씩만_나온다() {
    // given
    let h = Harness::new().await;
    h.core.transport.set_legacy(Arc::new(adapter(&h))).unwrap();
    // when
    let actual = h.core.transport.tools().unwrap();
    // then
    let mut names: Vec<&str> = actual["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count);
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
