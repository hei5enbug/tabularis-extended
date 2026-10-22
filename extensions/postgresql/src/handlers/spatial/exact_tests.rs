use super::{search_path_statement, validate_settings, ExactInput, Setting};
use serde_json::json;

#[test]
fn 스키마가_없으면_search_path를_바꾸지_않는다() {
    // given
    let schema = None;

    // when
    let actual = search_path_statement(schema).unwrap();

    // then
    assert_eq!(actual, None);
}

#[test]
fn 스키마가_있으면_트랜잭션_범위의_search_path만_설정한다() {
    // given
    let schema = Some("sales");

    // when
    let actual = search_path_statement(schema).unwrap().unwrap();

    // then
    assert_eq!(actual, "SET LOCAL search_path TO \"sales\"");
    assert!(actual.starts_with("SET LOCAL "));
}

#[test]
fn 스키마에_들어_있는_큰따옴표는_두_번_써서_식별자_밖으로_빠져나가지_못한다() {
    // given
    let schema = Some("a\"; DROP TABLE t; --");

    // when
    let actual = search_path_statement(schema).unwrap().unwrap();

    // then
    assert_eq!(
        actual,
        "SET LOCAL search_path TO \"a\"\"; DROP TABLE t; --\""
    );
}

#[test]
fn 빈_스키마는_잘못된_인자_오류로_거부한다() {
    // given
    let schema = Some("");

    // when
    let actual = search_path_statement(schema).unwrap_err();

    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
}

#[test]
fn 널_문자를_포함한_스키마는_잘못된_인자_오류로_거부한다() {
    // given
    let schema = Some("sa\0les");

    // when
    let actual = search_path_statement(schema).unwrap_err();

    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
}

#[test]
fn 정확_쿼리_입력은_선택적_schema_필드를_받는다() {
    // given
    let with_schema = json!({"text":"SELECT 1","schema":"sales"});
    let without_schema = json!({"text":"SELECT 1"});

    // when
    let with_schema: ExactInput = serde_json::from_value(with_schema).unwrap();
    let without_schema: ExactInput = serde_json::from_value(without_schema).unwrap();

    // then
    assert_eq!(with_schema.schema.as_deref(), Some("sales"));
    assert_eq!(without_schema.schema, None);
}

#[test]
fn 정확_쿼리_입력은_알_수_없는_필드를_계속_거부한다() {
    // given
    let value = json!({"text":"SELECT 1","schema":"sales","search_path":"x"});

    // when
    let actual = serde_json::from_value::<ExactInput>(value);

    // then
    assert!(actual.is_err());
}

fn setting(name: &str, value: &str) -> Setting {
    Setting {
        name: name.into(),
        value: value.into(),
    }
}

#[test]
fn 정확_쿼리_입력은_선택적_settings_배열을_받는다() {
    // given
    let value = json!({"text":"SELECT 1","settings":[{"name":"statement_timeout","value":"30s"},{"name":"lock_timeout","value":"1s"}]});

    // when
    let actual: ExactInput = serde_json::from_value(value).unwrap();

    // then
    assert_eq!(actual.settings.len(), 2);
    assert_eq!(actual.settings[0].name, "statement_timeout");
    assert_eq!(actual.settings[0].value, "30s");
    assert_eq!(actual.settings[1].name, "lock_timeout");
}

#[test]
fn settings가_없으면_빈_목록으로_읽는다() {
    // given
    let value = json!({"text":"SELECT 1"});

    // when
    let actual: ExactInput = serde_json::from_value(value).unwrap();

    // then
    assert!(actual.settings.is_empty());
}

#[test]
fn 빈_settings는_쓰기_작업과_예약_세션에서도_통과한다() {
    // given
    let settings: Vec<Setting> = Vec::new();

    // when
    let actual = validate_settings(&settings, false, true);

    // then
    assert!(actual.is_ok());
}

#[test]
fn settings_항목의_알_수_없는_필드는_거부한다() {
    // given
    let value = json!({"text":"SELECT 1","settings":[{"name":"jit","value":"off","local":false}]});

    // when
    let actual = serde_json::from_value::<ExactInput>(value);

    // then
    assert!(actual.is_err());
}

#[test]
fn 읽기_작업의_허용된_settings는_대소문자와_무관하게_통과한다() {
    // given
    let settings = vec![
        setting("statement_timeout", "1500ms"),
        setting("LOCK_TIMEOUT", "1s"),
        setting("search_path", "a,b"),
        setting("enable_seqscan", "off"),
    ];

    // when
    let actual = validate_settings(&settings, true, false);

    // then
    assert!(actual.is_ok());
}

#[test]
fn 허용_목록에_없는_setting_이름은_거부한다() {
    // given
    let settings = vec![
        setting("statement_timeout", "1s"),
        setting("role", "postgres"),
    ];

    // when
    let actual = validate_settings(&settings, true, false).unwrap_err();

    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
}

#[test]
fn transaction_read_only_는_거부한다() {
    // given
    let settings = vec![setting("transaction_read_only", "off")];

    // when
    let actual = validate_settings(&settings, true, false).unwrap_err();

    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
}

#[test]
fn default_transaction_read_only_도_거부한다() {
    // given
    let settings = vec![setting("Default_Transaction_Read_Only", "off")];

    // when
    let actual = validate_settings(&settings, true, false).unwrap_err();

    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
}

#[test]
fn 쓰기_작업의_settings는_거부한다() {
    // given
    let settings = vec![setting("statement_timeout", "1s")];

    // when
    let actual = validate_settings(&settings, false, false).unwrap_err();

    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
    assert_eq!(
        actual.message,
        "Settings are accepted only for read jobs outside a session"
    );
}

#[test]
fn 예약_세션의_settings는_읽기_전용이어도_거부한다() {
    // given
    let settings = vec![setting("statement_timeout", "1s")];

    // when
    let actual = validate_settings(&settings, true, true).unwrap_err();

    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
}

#[test]
fn 널_문자를_포함한_setting_값은_거부한다() {
    // given
    let settings = vec![setting("search_path", "a\0b")];

    // when
    let actual = validate_settings(&settings, true, false).unwrap_err();

    // then
    assert_eq!(actual.code, "INVALID_ARGUMENT");
}
