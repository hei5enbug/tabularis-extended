use super::{search_path_statement, ExactInput};
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
