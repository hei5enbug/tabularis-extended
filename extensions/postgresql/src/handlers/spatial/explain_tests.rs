use super::{statement, ExplainInput};
use serde_json::json;

#[test]
fn 계획_문장은_분석_옵션_없이_json_형식만_요청한다() {
    // given
    let sql = "SELECT * FROM orders WHERE id = 1";

    // when
    let actual = statement(sql).unwrap();

    // then
    assert_eq!(
        actual,
        "EXPLAIN (FORMAT JSON) SELECT * FROM orders WHERE id = 1"
    );
    assert!(!actual.to_ascii_uppercase().contains("ANALYZE"));
    assert!(!actual.to_ascii_uppercase().contains("BUFFERS"));
}

#[test]
fn 트랜잭션과_세션_제어는_계획_대상이_아니다() {
    // given
    let statements = [
        "BEGIN",
        "commit",
        "SET search_path = x",
        "  /* c */ ROLLBACK",
        "",
    ];

    // when
    let actual: Vec<String> = statements
        .iter()
        .map(|sql| statement(sql).unwrap_err().code)
        .collect();

    // then
    assert_eq!(
        actual,
        [
            "UNSUPPORTED_OPERATION",
            "UNSUPPORTED_OPERATION",
            "UNSUPPORTED_OPERATION",
            "UNSUPPORTED_OPERATION",
            "INVALID_ARGUMENT"
        ]
    );
}

#[test]
fn 입력은_sql과_스키마만_받는다() {
    // given
    let accepted = json!({"sql":"SELECT 1","schema":"sales"});
    let without_schema = json!({"sql":"SELECT 1"});
    let analyze = json!({"sql":"SELECT 1","analyze":true});
    let database = json!({"sql":"SELECT 1","database":"other"});
    let missing = json!({"schema":"sales"});

    // when
    let actual = [accepted, without_schema, analyze, database, missing]
        .map(|input| serde_json::from_value::<ExplainInput>(input).is_ok());

    // then
    assert_eq!(actual, [true, true, false, false, false]);
}
