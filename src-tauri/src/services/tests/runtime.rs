use super::{
    super::{
        contracts::*,
        credentials::{SecretInputs, SecretSlot},
        runtime::SERVICE_OPERATIONS,
    },
    support::*,
};
use serde_json::json;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn 쓰기는_연결과_호출자_권한과_이번_승인이_모두_있어야_실행된다() {
    // given
    let harness = Harness::new();
    let input = request(
        Operation::RowDelete,
        json!({"table":{"database":"fixture","schema":"public","table":"items"},"identity":{"columns":["id"],"values":[1]}}),
    );
    harness.policy.approved.store(true, Ordering::SeqCst);

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.policy.approvals.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 승인이_없는_쓰기는_드라이버를_호출하지_않는다() {
    // given
    let harness = Harness::new();
    harness.allow_write();
    harness.policy.approved.store(false, Ordering::SeqCst);
    let input = request(
        Operation::RowInsert,
        json!({"table":{"database":"fixture","schema":null,"table":"items"},"values":{"id":1}}),
    );

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 승인된_쓰기는_드라이버의_제한과_경고를_보존한다() {
    // given
    let harness = Harness::new();
    harness.allow_write();
    let input = request(
        Operation::RowInsert,
        json!({"table":{"database":"fixture","schema":null,"table":"items"},"values":{"id":1}}),
    );

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert!(actual.limits.truncated);
    assert_eq!(actual.limits.reasons, ["source_limit"]);
    assert_eq!(actual.warnings, ["source_warning"]);
    assert_eq!(harness.driver.contexts.lock().unwrap()[0].1, false);
}

#[tokio::test]
async fn 기본_쿼리는_sql_첫_단어와_무관하게_db_읽기_전용_문맥을_사용한다() {
    // given
    let harness = Harness::new();
    harness.allow_write();
    let input = query("WITH changed AS (DELETE FROM items RETURNING *) SELECT * FROM changed");

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(harness.driver.contexts.lock().unwrap()[0].1, true);
    assert_eq!(harness.policy.approvals.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn db_읽기_전용_보장이_없으면_쿼리를_호출하지_않는다() {
    // given
    let harness = Harness::new();
    harness.driver.readonly.store(false, Ordering::SeqCst);

    // when
    let actual = harness.call(&query("SELECT dangerous_function()")).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 저장되지_않은_연결은_드라이버를_호출하지_않는다() {
    // given
    let harness = Harness::new();
    let mut input = query("SELECT 1");
    input.connection_id = Some("absent".into());

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ConnectionNotFound);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 비밀과_알_수_없는_연결_설정은_저장되지_않는다() {
    // given
    let harness = Harness::new();
    let inputs: Vec<_> = [
        json!({"host":"localhost","extra":{"password":"canary"}}),
        json!({"host":"localhost","azure_access_token":"canary"}),
        json!({"host":5}),
    ]
    .into_iter()
    .map(|settings| {
        let mut value = request(
            Operation::ConnectionCreate,
            json!({"name":"new","driver":"postgresql","settings":settings,"credential_ref":null}),
        );
        value.connection_id = None;
        value
    })
    .collect();

    // when
    let actual = call_scenarios(&harness, inputs).await;

    // then
    assert!(actual
        .iter()
        .all(|response| response.error.as_ref().unwrap().code == ErrorCode::InvalidArgument));
    assert_eq!(harness.connections.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 연결_버전_불일치는_저장을_거부한다() {
    // given
    let harness = Harness::new();
    let mut input = request(
        Operation::ConnectionUpdate,
        json!({"patch":{"name":"renamed"}}),
    );
    input.expected_version = Some(99);

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::VersionConflict);
    assert_eq!(harness.connections.writes.load(Ordering::SeqCst), 0);
}

async fn page_replay(harness: &Harness) -> (ServiceResponse, ServiceResponse, ServiceResponse) {
    let first = harness.call(&query("SELECT 1")).await;
    let token = first.page.next_token.clone().unwrap();
    let next = harness
        .call(&request(Operation::QueryNext, json!({"next_token":token})))
        .await;
    let replay = harness
        .call(&request(Operation::QueryNext, json!({"next_token":token})))
        .await;
    (first, next, replay)
}

#[tokio::test]
async fn 다음_페이지와_토큰_재요청은_sql을_재실행하지_않는다() {
    // given
    let harness = Harness::new();

    // when
    let (first, next, replay) = page_replay(&harness).await;

    // then
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(first.data["rows"][0], json!([0]));
    assert_eq!(next.data["rows"][0], json!([10]));
    assert_eq!(next.data, replay.data);
    assert_eq!(next.page, replay.page);
    assert_eq!(next.result_id, replay.result_id);
}

#[tokio::test]
async fn 다른_연결에서_페이지_토큰을_재사용하면_거부한다() {
    // given
    let harness = Harness::new();
    let first = harness.call(&query("SELECT 1")).await;
    let mut input = request(
        Operation::QueryNext,
        json!({"next_token":first.page.next_token.unwrap()}),
    );
    input.connection_id = Some("conn-b".into());

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::InvalidPageToken);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 결과를_읽을_때_현재_연결_권한을_다시_확인한다() {
    // given
    let harness = Harness::new();
    let first = harness.call(&query("SELECT 1")).await;
    harness.policy.allowed.store(false, Ordering::SeqCst);
    let input = request(
        Operation::ResultGet,
        json!({"result_id":first.result_id.unwrap(),"result_set_index":0,"offset":0,"limit":10}),
    );

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 등록하지_않은_파일_descriptor는_열거나_가져오지_않는다() {
    // given
    let harness = Harness::new();
    let input = request(
        Operation::CredentialImport,
        json!({"slot":"db_password","input_channel":{"kind":"file_descriptor","descriptor":3},"persistence":"session"}),
    );

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn 보호된_입력은_한번만_소비하고_비밀을_응답에_넣지_않는다() {
    // given
    let harness = Harness::new();
    let input = request(
        Operation::CredentialImport,
        json!({"slot":"db_password","input_channel":{"kind":"protected_stdin"},"persistence":"session"}),
    );
    harness
        .runtime
        .secret_inputs
        .register(
            &caller(),
            &input.request_id,
            &input.input["input_channel"],
            b"canary-secret-input-958".to_vec(),
        )
        .unwrap();

    // when
    let (actual, replay, stored) = import_and_replay(&harness, &input).await;

    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("canary-secret-input-958"));
    assert_eq!(stored.unwrap().expose(), "canary-secret-input-958");
    assert_eq!(replay.error.unwrap().code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn 보호된_입력_요청이_거부되어도_등록된_비밀을_폐기한다() {
    // given
    let harness = Harness::new();
    let input = request(
        Operation::CredentialImport,
        json!({"slot":"db_password","input_channel":{"kind":"protected_stdin"},"persistence":"session"}),
    );
    harness
        .runtime
        .secret_inputs
        .register(
            &caller(),
            &input.request_id,
            &input.input["input_channel"],
            b"canary".to_vec(),
        )
        .unwrap();
    harness.policy.allowed.store(false, Ordering::SeqCst);

    // when
    let (actual, retained) = denied_import(&harness, &input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::PermissionDenied);
    assert!(retained.is_err());
}

#[test]
fn 호출자와_owner의_구분자가_겹쳐도_비밀_채널은_분리된다() {
    // given
    let inputs = SecretInputs::default();
    let mut first = caller();
    first.owner_instance_id = "a:b".into();
    first.caller_id = "c".into();
    let mut second = caller();
    second.owner_instance_id = "a".into();
    second.caller_id = "b:c".into();
    let channel = json!({"kind":"protected_stdin"});
    inputs
        .register(&first, "request", &channel, b"first-secret".to_vec())
        .unwrap();

    // when
    let actual = inputs.take(&second, "request", &channel);

    // then
    assert!(actual.is_err());
}

#[tokio::test]
async fn 드라이버_오류의_비밀과_상세_정보를_응답에서_제거한다() {
    // given
    let harness = Harness::new();

    // when
    let actual = harness.call(&query("error")).await;

    // then
    assert_eq!(actual.error.as_ref().unwrap().code, ErrorCode::DriverExited);
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("canary-password-token-593"));
    assert!(actual.error.unwrap().details.is_null());
}

#[tokio::test]
async fn 미구현_지도_operation은_성공을_가장하지_않는다() {
    // given
    let harness = Harness::new();
    let mut input = request(Operation::MapList, json!({}));
    input.connection_id = None;

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::CapabilityUnavailable);
    assert!(!SERVICE_OPERATIONS.contains(&Operation::MapList));
}

async fn call_scenarios(harness: &Harness, inputs: Vec<ServiceRequest>) -> Vec<ServiceResponse> {
    let mut responses = Vec::new();
    for input in inputs {
        responses.push(harness.call(&input).await);
    }
    responses
}
async fn import_and_replay(
    harness: &Harness,
    input: &ServiceRequest,
) -> (
    ServiceResponse,
    ServiceResponse,
    Option<super::super::credentials::Secret>,
) {
    let first = harness.call(input).await;
    let second = harness.call(input).await;
    (
        first,
        second,
        harness
            .runtime
            .auth
            .session_store()
            .get("conn-a", SecretSlot::DbPassword)
            .await
            .unwrap(),
    )
}
async fn denied_import(
    harness: &Harness,
    input: &ServiceRequest,
) -> (
    ServiceResponse,
    super::super::error::ServiceResult<super::super::credentials::Secret>,
) {
    let response = harness.call(input).await;
    (
        response,
        harness.runtime.secret_inputs.take(
            &caller(),
            &input.request_id,
            &input.input["input_channel"],
        ),
    )
}

async fn initial_and_later_affected_rows(
    harness: &Harness,
) -> (ServiceResponse, ServiceResponse, ServiceResponse) {
    let mut input = query("select 1");
    input.input["page_size"] = json!(1);

    let initial = harness.call(&input).await;
    let next = harness
        .call(&request(
            Operation::QueryNext,
            json!({"page_token":initial.page.next_token}),
        ))
        .await;
    let result = harness
        .call(&request(
            Operation::ResultGet,
            json!({"result_id":initial.result_id,"result_set_index":0,"offset":0,"limit":1}),
        ))
        .await;
    (initial, next, result)
}

#[tokio::test]
async fn affected_rows는_최초_쿼리_페이지에만_안전하게_보존한다() {
    // given
    let harness = Harness::new();
    // when
    let (initial, next, result) = initial_and_later_affected_rows(&harness).await;
    // then
    assert_eq!(initial.status, JobStatus::Succeeded);
    assert_eq!(initial.data["affected_rows"], 1);
    assert!(initial.data.get("remote").is_none());
    assert!(next.data.get("affected_rows").is_none());
    assert!(result.data.get("affected_rows").is_none());
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}
