use super::contracts::*;
use super::dispatch::*;
use serde::Deserialize;
use serde_json::{json, Value};
mod azure_cli;
mod driver_owned_entra;
mod lifecycle;
mod newguiinput;
mod oauth;
mod platform;
mod platform_mutations;
mod policy;
mod regressions;
mod results;
mod runtime;
mod spatial_tokens;
mod support;

#[derive(Deserialize)]
struct Fixture {
    name: String,
    schema: String,
    valid: bool,
    value: Value,
}

#[test]
fn 모든_스키마_fixture를_정해진_결과로_검증한다() {
    // given
    let validator = SchemaValidator::new().unwrap();
    let fixtures: Vec<Fixture> = serde_json::from_str(VALIDATION_FIXTURES).unwrap();

    // when
    let actual: Vec<bool> = fixtures
        .iter()
        .map(|fixture| match fixture.schema.as_str() {
            "request" => validator.validate_request(&fixture.value).is_ok(),
            "response" => validator.validate_response(&fixture.value).is_ok(),
            _ => false,
        })
        .collect();

    // then
    for (fixture, valid) in fixtures.iter().zip(actual) {
        assert_eq!(valid, fixture.valid, "{}", fixture.name);
    }
}

#[test]
fn 허용된_요청과_응답은_dto를_왕복해도_계약을_유지한다() {
    // given
    let validator = SchemaValidator::new().unwrap();
    let fixtures: Vec<Fixture> = serde_json::from_str(VALIDATION_FIXTURES).unwrap();
    let valid: Vec<&Fixture> = fixtures.iter().filter(|fixture| fixture.valid).collect();

    // when
    let actual: Vec<(Value, Value, bool)> = valid
        .iter()
        .map(|fixture| {
            if fixture.schema == "request" {
                let dto: ServiceRequest = serde_json::from_value(fixture.value.clone()).unwrap();
                let encoded = serde_json::to_value(&dto).unwrap();
                let decoded: ServiceRequest = serde_json::from_value(encoded.clone()).unwrap();
                (
                    encoded.clone(),
                    serde_json::to_value(decoded).unwrap(),
                    validator.validate_request(&encoded).is_ok(),
                )
            } else {
                let dto: ServiceResponse = serde_json::from_value(fixture.value.clone()).unwrap();
                let encoded = serde_json::to_value(&dto).unwrap();
                let decoded: ServiceResponse = serde_json::from_value(encoded.clone()).unwrap();
                (
                    encoded.clone(),
                    serde_json::to_value(decoded).unwrap(),
                    validator.validate_response(&encoded).is_ok(),
                )
            }
        })
        .collect();

    // then
    for (fixture, (value, decoded, valid)) in valid.iter().zip(actual) {
        assert_eq!(value, decoded);
        assert!(valid, "{}", fixture.name);
        assert_eq!(value["input"], fixture.value["input"]);
        assert_eq!(value["data"], fixture.value["data"]);
    }
}

#[test]
fn 제한_시간이_생략되면_30초를_사용한다() {
    // given
    let input = json!({"protocol_version":1,"operation":"connection.list","request_id":"default","input":{}});
    let validator = SchemaValidator::new().unwrap();

    // when
    let actual = validator.decode_request(&input).unwrap();

    // then
    assert_eq!(actual.deadline_ms, DEFAULT_DEADLINE_MS);
}

#[test]
fn 모든_operation의_입력_스키마를_레지스트리에서_얻는다() {
    // given
    let validator = SchemaValidator::new().unwrap();
    let names = validator.operation_names();

    // when
    let actual: Vec<Value> = names
        .iter()
        .map(|name| validator.input_schema(serde_json::from_value(json!(name)).unwrap()))
        .collect();

    // then
    assert_eq!(names.len(), 49);
    assert!(actual
        .iter()
        .all(|schema| schema["additionalProperties"] == false));
}

#[tokio::test]
async fn 연결되지_않은_핸들러는_미지원_오류를_반환한다() {
    // given
    let dispatcher = ServiceDispatcher::new().unwrap();
    let request = dispatcher.validator.decode_request(&json!({"protocol_version":1,"operation":"connection.list","request_id":"unavailable","input":{}})).unwrap();
    let caller = CallerContext {
        transport: CallerTransport::Cli,
        caller_id: "test".into(),
        owner_instance_id: "owner".into(),
    };

    // when
    let actual = dispatcher.dispatch(&request, &caller).await;

    // then
    assert_eq!(actual.status, JobStatus::Failed);
    assert_eq!(
        actual.error.as_ref().unwrap().code,
        ErrorCode::CapabilityUnavailable
    );
    assert_eq!(actual.error.as_ref().unwrap().outcome, Outcome::NotStarted);
    assert_eq!(actual.data, Value::Null);
    assert!(dispatcher
        .validator
        .validate_response(&serde_json::to_value(actual).unwrap())
        .is_ok());
}
