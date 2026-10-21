// Keep standard transport acronyms in the runner-visible Korean test titles.
#![allow(non_snake_case)]
use super::{
    super::{
        contracts::*,
        credentials::SecretSlot,
        dispatch::{CallerContext, CallerTransport},
    },
    support::*,
};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

fn gui() -> CallerContext {
    CallerContext {
        transport: CallerTransport::Gui,
        ..caller()
    }
}
fn registered_import(harness: &Harness) -> ServiceRequest {
    let mut input = request(
        Operation::CredentialImport,
        json!({"slot":"db_password","input_channel":null,"persistence":"session"}),
    );
    input.input["input_channel"] = harness
        .runtime
        .secret_inputs
        .register_gui(
            &gui(),
            "conn-a",
            &input.request_id,
            b"synthetic-gui-runtime-canary".to_vec(),
        )
        .unwrap();
    input
}
async fn import_and_replay(
    harness: &Harness,
    input: &ServiceRequest,
) -> (ServiceResponse, ServiceResponse, String) {
    let first = harness.dispatcher.dispatch(input, &gui()).await;
    let replay = harness.dispatcher.dispatch(input, &gui()).await;
    let stored = harness
        .runtime
        .auth
        .session_store()
        .get("conn-a", SecretSlot::DbPassword)
        .await
        .unwrap()
        .unwrap()
        .expose()
        .to_owned();
    (first, replay, stored)
}

#[tokio::test]
async fn GUI_입력은_공통_credential_import로_저장하고_비밀을_요청과_응답에_넣지_않는다() {
    // Given
    let harness = Harness::new();
    let input = registered_import(&harness);
    // When
    let (first, replay, stored) = import_and_replay(&harness, &input).await;
    // Then
    assert_eq!(first.status, JobStatus::Succeeded);
    assert_eq!(replay.error.unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(stored, "synthetic-gui-runtime-canary");
    assert!(!serde_json::to_string(&input)
        .unwrap()
        .contains("synthetic-gui-runtime-canary"));
    assert!(!serde_json::to_string(&first)
        .unwrap()
        .contains("synthetic-gui-runtime-canary"));
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

async fn rejected_import(
    harness: &Harness,
    input: &ServiceRequest,
    caller: &CallerContext,
) -> (ServiceResponse, bool, bool) {
    let response = harness.dispatcher.dispatch(input, caller).await;
    let stored = harness
        .runtime
        .auth
        .session_store()
        .get("conn-b", SecretSlot::DbPassword)
        .await
        .unwrap()
        .is_some();
    let retained = harness
        .runtime
        .secret_inputs
        .take_for_connection(
            &gui(),
            &input.request_id,
            &input.input["input_channel"],
            "conn-a",
        )
        .is_ok();
    (response, stored, retained)
}

async fn reject_transport_cases(
    cases: &[(Harness, ServiceRequest, CallerContext)],
) -> Vec<(ServiceResponse, bool, bool)> {
    let mut actual = Vec::new();
    for (harness, input, caller) in cases {
        actual.push(rejected_import(harness, input, caller).await);
    }
    actual
}

#[tokio::test]
async fn 다른_연결에_제출된_GUI_비밀은_저장하지_않고_폐기한다() {
    // Given
    let harness = Harness::new();
    let mut input = registered_import(&harness);
    input.connection_id = Some("conn-b".into());
    // When
    let (response, stored, retained) = rejected_import(&harness, &input, &gui()).await;
    // Then
    assert_eq!(response.error.unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(response.data, Value::Null);
    assert!(!stored);
    assert!(!retained);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 비GUI_transport는_등록된_GUI_채널을_credential_import에_사용하지_못한다() {
    // Given
    let cases: Vec<_> = [
        CallerTransport::Cli,
        CallerTransport::Mcp,
        CallerTransport::LocalIpc,
    ]
    .into_iter()
    .map(|transport| {
        let harness = Harness::new();
        let input = registered_import(&harness);
        (harness, input, CallerContext { transport, ..gui() })
    })
    .collect();
    let expected = [
        ErrorCode::PermissionDenied,
        ErrorCode::OperationNotAllowed,
        ErrorCode::PermissionDenied,
    ];
    // When
    let actual = reject_transport_cases(&cases).await;
    // Then
    assert!(actual
        .into_iter()
        .zip(expected)
        .all(
            |((response, stored, retained), code)| response.error.unwrap().code == code
                && !stored
                && !retained
        ));
}
