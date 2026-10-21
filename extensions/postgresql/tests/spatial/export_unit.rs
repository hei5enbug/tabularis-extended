use serde_json::{json,Value};
use crate::handlers::spatial::{table::TableInput,types::FeatureLimits};

fn table_input() -> Value {
    json!({"table":{"database":null,"schema":null,"table":"points"},"column":"g",
        "viewport":{"west":-180,"south":-90,"east":180,"north":90,"world":true}})
}

#[test]
fn export_표시가_없으면_기존_display_기본값을_유지한다() {
    // given
    let input=table_input();
    // when
    let parsed=serde_json::from_value::<TableInput>(input).unwrap();
    // then
    assert!(parsed.export_limit_bytes.is_none());
    assert_eq!(parsed.limits,FeatureLimits::default());
    assert!(parsed.validate().is_ok());
}

#[test]
fn 명시적_export는_허용된_최대_총량을_검증한다() {
    // given
    let limit=1024*1024*1024;
    let limits=FeatureLimits {max_features:1_000_000,max_coordinates:10_000_000,max_bytes:limit};
    // when
    let result=limits.validate_export(limit);
    // then
    assert!(result.is_ok());
}

#[test]
fn export의_각_최소_총량도_허용한다() {
    // given
    let limits=FeatureLimits {max_features:1,max_coordinates:1,max_bytes:1};
    // when
    let result=limits.validate_export(1);
    // then
    assert!(result.is_ok());
}

fn invalid_export_cases() -> Vec<(FeatureLimits,usize)> {
    let ordinary=FeatureLimits::default();
    vec![(ordinary.clone(),0),(ordinary.clone(),1024*1024*1024+1),
        (FeatureLimits {max_features:0,..ordinary.clone()},1024*1024*1024),
        (FeatureLimits {max_features:1_000_001,..ordinary.clone()},1024*1024*1024),
        (FeatureLimits {max_coordinates:0,..ordinary.clone()},1024*1024*1024),
        (FeatureLimits {max_coordinates:10_000_001,..ordinary.clone()},1024*1024*1024),
        (FeatureLimits {max_bytes:0,..ordinary.clone()},1024*1024*1024),
        (ordinary,32*1024*1024-1),
        (FeatureLimits {max_features:usize::MAX,max_coordinates:usize::MAX,max_bytes:usize::MAX},usize::MAX)]
}

#[test]
fn export의_영_초과_및_정수_상한을_넘는_입력은_거부한다() {
    // given
    let cases=invalid_export_cases();
    // when
    let results=cases.iter().map(|(limits,limit)|limits.validate_export(*limit)).collect::<Vec<_>>();
    // then
    assert!(results.iter().all(|result|result.as_ref().is_err_and(|error|error.code=="INVALID_ARGUMENT")));
}

#[test]
fn export_표시_없이_큰_display_상한을_허용하지_않는다() {
    // given
    let mut input=table_input();
    input["limits"]=json!({"max_features":10001});
    let parsed=serde_json::from_value::<TableInput>(input).unwrap();
    // when
    let result=parsed.validate();
    // then
    assert_eq!(result.unwrap_err().code,"INVALID_ARGUMENT");
}

#[test]
fn 페이지_상한과_keyset_token_상호배제는_export에도_동일하다() {
    // given
    let mut cases=Vec::new();
    for page in [0,1001] {
        let mut input=table_input();input["export_limit_bytes"]=json!(1024*1024*1024);input["page_size"]=json!(page);
        cases.push(serde_json::from_value::<TableInput>(input).unwrap());
    }
    let mut input=table_input();input["next_token"]=json!("cursor");input["keyset"]=json!({"columns":["id"],"values":[1]});
    cases.push(serde_json::from_value::<TableInput>(input).unwrap());
    // when
    let results=cases.iter().map(TableInput::validate).collect::<Vec<_>>();
    // then
    assert!(results.iter().all(|result|result.as_ref().is_err_and(|error|error.code=="INVALID_ARGUMENT")));
}

#[test]
fn export_검증은_원래_요청의_총량을_변경하지_않는다() {
    // given
    let mut input=table_input();input["export_limit_bytes"]=json!(1024*1024*1024);
    input["limits"]=json!({"max_features":1_000_000,"max_coordinates":10_000_000,"max_bytes":1024*1024*1024});
    let parsed=serde_json::from_value::<TableInput>(input).unwrap();
    let before=serde_json::to_value(&parsed).unwrap();
    // when
    let result=parsed.validate();
    // then
    assert!(result.is_ok());
    assert_eq!(serde_json::to_value(&parsed).unwrap(),before);
}
