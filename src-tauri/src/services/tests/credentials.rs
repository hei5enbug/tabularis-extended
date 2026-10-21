// Keep standard transport acronyms in the runner-visible Korean test titles.
#![allow(non_snake_case)]
use super::*;
use serde_json::json;

fn gui() -> CallerContext {
    CallerContext {
        transport: CallerTransport::Gui,
        caller_id: "local_user:fixture".into(),
        owner_instance_id: "gui-owner".into(),
    }
}
fn secret() -> Vec<u8> {
    b"synthetic-gui-secret-canary".to_vec()
}

fn consume_twice(
    inputs: &SecretInputs,
    channel: &Value,
) -> (ServiceResult<Secret>, ServiceResult<Secret>) {
    (
        inputs.take_for_connection(&gui(), "request", channel, "conn-a"),
        inputs.take_for_connection(&gui(), "request", channel, "conn-a"),
    )
}

fn reject_non_gui(
    inputs: &SecretInputs,
    channel: &Value,
    callers: &[CallerContext],
) -> Vec<(ServiceResult<Value>, ServiceResult<Secret>)> {
    callers
        .iter()
        .map(|caller| {
            (
                inputs.register_gui(caller, "conn-a", "other-request", secret()),
                inputs.take_for_connection(caller, "request", channel, "conn-a"),
            )
        })
        .collect()
}

fn attempt_legacy_bypass(
    inputs: &SecretInputs,
    channel: &Value,
) -> (ServiceResult<()>, ServiceResult<Secret>) {
    (
        inputs.register(&gui(), "other-request", channel, secret()),
        inputs.take(&gui(), "request", channel),
    )
}

#[test]
fn GUI_비밀은_등록된_연결과_요청에서_한번만_소비한다() {
    // Given
    let inputs = SecretInputs::default();
    let channel = inputs
        .register_gui(&gui(), "conn-a", "request", secret())
        .unwrap();
    // When
    let (first, replay) = consume_twice(&inputs, &channel);
    // Then
    assert_eq!(first.unwrap().expose(), "synthetic-gui-secret-canary");
    assert_eq!(replay.unwrap_err().code, ErrorCode::PermissionDenied);
    assert_eq!(channel.as_object().unwrap().len(), 2);
    assert!(gui_channel_id(&channel).is_ok());
    assert!(!channel.to_string().contains("synthetic-gui-secret-canary"));
}

#[test]
fn 다른_연결과_owner와_호출자와_요청은_GUI_비밀을_소비하지_못한다() {
    // Given
    let inputs = SecretInputs::default();
    let channel = inputs
        .register_gui(&gui(), "conn-a", "request", secret())
        .unwrap();
    let mut other_owner = gui();
    other_owner.owner_instance_id = "other-owner".into();
    let mut other_caller = gui();
    other_caller.caller_id = "other-caller".into();
    let cases = [
        (gui(), "request", "conn-b"),
        (other_owner, "request", "conn-a"),
        (other_caller, "request", "conn-a"),
        (gui(), "other-request", "conn-a"),
    ];
    // When
    let actual: Vec<_> = cases
        .iter()
        .map(|(caller, request, connection)| {
            inputs.take_for_connection(caller, request, &channel, connection)
        })
        .collect();
    // Then
    assert!(actual
        .into_iter()
        .all(|result| result.unwrap_err().code == ErrorCode::PermissionDenied));
}

#[test]
fn CLI와_MCP와_LocalIpc는_GUI_비밀을_등록하거나_소비하지_못한다() {
    // Given
    let inputs = SecretInputs::default();
    let channel = inputs
        .register_gui(&gui(), "conn-a", "request", secret())
        .unwrap();
    let callers: Vec<_> = [
        CallerTransport::Cli,
        CallerTransport::Mcp,
        CallerTransport::LocalIpc,
    ]
    .into_iter()
    .map(|transport| CallerContext { transport, ..gui() })
    .collect();
    // When
    let actual = reject_non_gui(&inputs, &channel, &callers);
    // Then
    assert!(actual
        .into_iter()
        .all(|(registration, consumption)| registration.unwrap_err().code
            == ErrorCode::PermissionDenied
            && consumption.unwrap_err().code == ErrorCode::PermissionDenied));
}

#[test]
fn 기존_register와_take로_GUI_연결_바인딩을_우회하지_못한다() {
    // Given
    let inputs = SecretInputs::default();
    let channel = inputs
        .register_gui(&gui(), "conn-a", "request", secret())
        .unwrap();
    // When
    let actual = attempt_legacy_bypass(&inputs, &channel);
    // Then
    assert_eq!(actual.0.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(actual.1.unwrap_err().code, ErrorCode::InvalidArgument);
}

#[test]
fn 채널_JSON만_제출해서는_GUI_비밀을_등록할_수_없다() {
    // Given
    let inputs = SecretInputs::default();
    let channel = json!({"kind":"gui_buffer","channel_id":uuid::Uuid::new_v4().to_string()});
    // When
    let actual = inputs.take_for_connection(&gui(), "request", &channel, "conn-a");
    // Then
    assert_eq!(actual.unwrap_err().code, ErrorCode::PermissionDenied);
}

#[test]
fn 같은_GUI_요청의_중복_등록은_이전_비밀을_덮어쓰지_않는다() {
    // Given
    let inputs = SecretInputs::default();
    inputs
        .register_gui(&gui(), "conn-a", "request", secret())
        .unwrap();
    // When
    let actual = inputs.register_gui(&gui(), "conn-b", "request", b"replacement".to_vec());
    // Then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(inputs.channels.lock().unwrap().len(), 1);
}

#[test]
fn 만료된_GUI_채널은_비밀을_반환하지_않고_폐기한다() {
    // Given
    let inputs = SecretInputs::default();
    let channel = inputs
        .register_gui(&gui(), "conn-a", "request", secret())
        .unwrap();
    inputs
        .channels
        .lock()
        .unwrap()
        .values_mut()
        .for_each(|entry| entry.expires_at_ms = super::super::storage::now_ms());
    // When
    let actual = consume_twice(&inputs, &channel);
    // Then
    assert_eq!(actual.0.unwrap_err().code, ErrorCode::AuthExpired);
    assert_eq!(actual.1.unwrap_err().code, ErrorCode::PermissionDenied);
    assert!(inputs.channels.lock().unwrap().is_empty());
}

#[test]
fn GUI와_기존_보호_입력은_64개_채널_상한을_공유한다() {
    // Given
    let inputs = SecretInputs::default();
    for index in 0..32 {
        inputs
            .register_gui(&gui(), "conn-a", &format!("gui-{index}"), secret())
            .unwrap();
        inputs
            .register(
                &gui(),
                &format!("stdin-{index}"),
                &json!({"kind":"protected_stdin"}),
                secret(),
            )
            .unwrap();
    }
    // When
    let actual = inputs.register_gui(&gui(), "conn-a", "overflow", secret());
    // Then
    assert_eq!(actual.unwrap_err().code, ErrorCode::ResourceLimit);
    assert_eq!(inputs.channels.lock().unwrap().len(), 64);
}

#[test]
fn 만료된_채널은_새_GUI_등록의_용량을_막지_않는다() {
    // Given
    let inputs = SecretInputs::default();
    for index in 0..64 {
        inputs
            .register_gui(&gui(), "conn-a", &format!("request-{index}"), secret())
            .unwrap();
    }
    inputs
        .channels
        .lock()
        .unwrap()
        .values_mut()
        .for_each(|entry| entry.expires_at_ms = 0);
    // When
    let actual = inputs.register_gui(&gui(), "conn-a", "fresh", secret());
    // Then
    assert!(actual.is_ok());
    assert_eq!(inputs.channels.lock().unwrap().len(), 1);
}

#[test]
fn 빈_ID와_너무_긴_ID와_빈_비밀은_GUI_등록에서_거부한다() {
    // Given
    let inputs = SecretInputs::default();
    let too_long = "x".repeat(129);
    let cases = [
        ("", "request", secret()),
        ("conn-a", "", secret()),
        (too_long.as_str(), "request", secret()),
        ("conn-a", too_long.as_str(), secret()),
        ("conn-a", "request", vec![]),
    ];
    // When
    let actual: Vec<_> = cases
        .into_iter()
        .map(|(connection, request, bytes)| inputs.register_gui(&gui(), connection, request, bytes))
        .collect();
    // Then
    assert!(actual
        .into_iter()
        .all(|result| result.unwrap_err().code == ErrorCode::InvalidArgument));
    assert!(inputs.channels.lock().unwrap().is_empty());
}

#[test]
fn 연결_소비_API는_기존_stdin과_등록된_descriptor를_한번만_소비한다() {
    // Given
    let inputs = SecretInputs::default();
    let channels = [
        json!({"kind":"protected_stdin"}),
        json!({"kind":"file_descriptor","descriptor":3}),
    ];
    for channel in &channels {
        inputs
            .register(&gui(), "request", channel, secret())
            .unwrap();
    }
    // When
    let actual: Vec<_> = channels
        .iter()
        .map(|channel| consume_twice(&inputs, channel))
        .collect();
    // Then
    assert!(actual
        .into_iter()
        .all(
            |(first, replay)| first.unwrap().expose() == "synthetic-gui-secret-canary"
                && replay.unwrap_err().code == ErrorCode::PermissionDenied
        ));
}

#[test]
fn 등록되지_않은_descriptor와_비밀이_섞인_GUI_채널은_거부한다() {
    // Given
    let inputs = SecretInputs::default();
    let channels = [
        json!({"kind":"file_descriptor","descriptor":255}),
        json!({"kind":"gui_buffer","channel_id":uuid::Uuid::new_v4().to_string(),"secret":"synthetic"}),
    ];
    // When
    let actual: Vec<_> = channels
        .iter()
        .map(|channel| inputs.take_for_connection(&gui(), "request", channel, "conn-a"))
        .collect();
    // Then
    assert_eq!(
        actual[0].as_ref().unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        actual[1].as_ref().unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}
