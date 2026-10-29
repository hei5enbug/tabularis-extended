use serde_json::{json, Value};

use super::{assemble, merge_limits, plan, ObjectKind, Plan};
use crate::handlers::spatial::types::TableRef;

fn table(name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some("app".into()),
        table: name.into(),
    }
}

fn placeholders(sql: &str) -> usize {
    (1..=9)
        .take_while(|index| sql.contains(&format!("${index}")))
        .count()
}

fn all_kinds() -> Vec<Option<ObjectKind>> {
    [
        "table",
        "view",
        "materialized_view",
        "foreign_table",
        "routine",
        "aggregate",
        "trigger",
        "sequence",
        "type",
        "collation",
        "operator",
        "operator_class",
        "operator_family",
    ]
    .iter()
    .map(|kind| Some(serde_json::from_value(json!(kind)).unwrap()))
    .chain([None])
    .collect()
}

#[test]
fn 계약이_정의한_종류_이름을_모두_객체_종류로_읽는다() {
    // given
    let names = [
        "table",
        "view",
        "materialized_view",
        "foreign_table",
        "routine",
        "aggregate",
        "trigger",
        "sequence",
        "type",
        "collation",
        "operator",
        "operator_class",
        "operator_family",
    ];

    // when
    let actual: Vec<bool> = names
        .iter()
        .map(|name| serde_json::from_value::<ObjectKind>(json!(name)).is_ok())
        .collect();

    // then
    assert!(actual.iter().all(|parsed| *parsed));
}

#[test]
fn 이_드라이버가_지원하지_않는_종류는_읽지_못한다() {
    // given
    let names = ["synonym", "rule", "security_policy", "unknown", ""];

    // when
    let actual: Vec<bool> = names
        .iter()
        .map(|name| serde_json::from_value::<ObjectKind>(json!(name)).is_ok())
        .collect();

    // then
    assert!(actual.iter().all(|parsed| !*parsed));
}

#[test]
fn 종류를_주지_않으면_관계_종류를_제한하지_않고_열과_세부를_함께_조회한다() {
    // given
    let input = table("orders");

    // when
    let Plan { columns, details } = plan(&input, None, None);

    // then
    let columns = columns.unwrap();
    assert_eq!(
        columns.values,
        json!(["app", "orders", null]).as_array().unwrap().clone()
    );
    assert_eq!(details.values, columns.values);
    assert!(details.text.contains("pg_get_viewdef"));
}

#[test]
fn 관계_종류는_pg_class_relkind로_좁혀_조회한다() {
    // given
    let input = table("orders");
    let kinds = [
        (ObjectKind::Table, "r,p"),
        (ObjectKind::View, "v"),
        (ObjectKind::MaterializedView, "m"),
        (ObjectKind::ForeignTable, "f"),
    ];

    // when
    let actual: Vec<Value> = kinds
        .iter()
        .map(|(kind, _)| plan(&input, Some(*kind), None).details.values[2].clone())
        .collect();

    // then
    let expected: Vec<Value> = kinds.iter().map(|(_, relkinds)| json!(relkinds)).collect();
    assert_eq!(actual, expected);
}

#[test]
fn 루틴과_집계는_같은_조회를_서로_다른_prokind로_나눈다() {
    // given
    let input = table("total");

    // when
    let routine = plan(&input, Some(ObjectKind::Routine), None);
    let aggregate = plan(&input, Some(ObjectKind::Aggregate), None);

    // then
    assert_eq!(routine.details.text, aggregate.details.text);
    assert_eq!(routine.details.values[2], json!("f,p,w"));
    assert_eq!(aggregate.details.values[2], json!("a"));
    assert!(routine.columns.unwrap().text.contains("generate_series"));
}

#[test]
fn 열이_없는_종류는_세부_조회_하나만_실행한다() {
    // given
    let input = table("object");
    let kinds = [
        ObjectKind::Trigger,
        ObjectKind::Sequence,
        ObjectKind::Collation,
        ObjectKind::Operator,
        ObjectKind::OperatorClass,
        ObjectKind::OperatorFamily,
    ];

    // when
    let actual: Vec<bool> = kinds
        .iter()
        .map(|kind| plan(&input, Some(*kind), None).columns.is_none())
        .collect();

    // then
    assert!(actual.iter().all(|only_details| *only_details));
}

#[test]
fn trigger는_소유_테이블을_세_번째_인자로_넘겨_후보를_좁힌다() {
    // given
    let input = table("audit");

    // when
    let actual = [None, Some("orders")].map(|owner| {
        plan(&input, Some(ObjectKind::Trigger), owner)
            .details
            .values[2]
            .clone()
    });

    // then
    assert_eq!(actual, [Value::Null, json!("orders")]);
}

#[test]
fn 복합_타입은_composite_relkind의_열을_조회한다() {
    // given
    let input = table("pair");

    // when
    let actual = plan(&input, Some(ObjectKind::Type), None);

    // then
    assert_eq!(actual.columns.unwrap().values[2], json!("c"));
    assert_eq!(actual.details.values, vec![json!("app"), json!("pair")]);
}

#[test]
fn 모든_조회는_자리표시자_수와_값_수가_같고_값을_문자열에_이어붙이지_않는다() {
    // given
    let input = table("a'; DROP TABLE x; --");

    // when
    let statements: Vec<(usize, usize, bool)> = all_kinds()
        .into_iter()
        .flat_map(|kind| {
            let Plan { columns, details } = plan(&input, kind, Some("owner"));
            columns.into_iter().chain([details])
        })
        .map(|statement| {
            (
                placeholders(statement.text),
                statement.values.len(),
                statement.text.contains("DROP TABLE"),
            )
        })
        .collect();

    // then
    assert!(!statements.is_empty());
    assert!(statements
        .iter()
        .all(|(placeholders, values, injected)| placeholders == values && !injected));
}

#[test]
fn 세부_조회가_없는_키는_빈_기본값으로_채운다() {
    // given
    let columns = vec![json!({"name":"id"})];

    // when
    let actual = assemble(
        columns,
        None,
        json!({"truncated":false,"reasons":[]}),
        json!({}),
    );

    // then
    assert_eq!(actual["columns"], json!([{"name":"id"}]));
    assert_eq!(
        (
            &actual["foreign_keys"],
            &actual["indexes"],
            &actual["keys"],
            &actual["check_constraints"],
            &actual["definition"],
            &actual["properties"],
        ),
        (
            &json!([]),
            &json!([]),
            &json!([]),
            &json!([]),
            &Value::Null,
            &json!({})
        )
    );
}

#[test]
fn 세부_조회의_값은_그대로_옮기고_널은_기본값으로_둔다() {
    // given
    let detail = json!({
        "foreign_keys":[{"name":"fk","columns":["a"],"ref_schema":"app","ref_table":"p","ref_columns":["id"],"on_update":"NO ACTION","on_delete":"CASCADE"}],
        "indexes":null,
        "definition":"SELECT 1",
        "properties":{"server":"s"}
    });

    // when
    let actual = assemble(vec![], Some(detail.clone()), json!({}), json!({}));

    // then
    assert_eq!(actual["foreign_keys"], detail["foreign_keys"]);
    assert_eq!(actual["indexes"], json!([]));
    assert_eq!(actual["definition"], "SELECT 1");
    assert_eq!(actual["properties"], json!({"server":"s"}));
}

#[test]
fn 형식이_다른_세부_값은_버리고_기본값을_유지한다() {
    // given
    let detail = json!({"keys":"oops","definition":7,"properties":[1],"check_constraints":{}});

    // when
    let actual = assemble(vec![], Some(detail), json!({}), json!({}));

    // then
    assert_eq!(
        (
            &actual["keys"],
            &actual["definition"],
            &actual["properties"],
            &actual["check_constraints"]
        ),
        (&json!([]), &Value::Null, &json!({}), &json!([]))
    );
}

#[test]
fn 한도_병합은_잘림과_이유를_합치고_중복_이유를_한_번만_둔다() {
    // given
    let first = json!({"truncated":false,"reasons":["ROW_LIMIT"]});
    let second = json!({"truncated":true,"reasons":["ROW_LIMIT","BYTE_LIMIT"]});

    // when
    let actual = merge_limits(Some(first), &second);

    // then
    assert_eq!(
        actual,
        json!({"truncated":true,"reasons":["ROW_LIMIT","BYTE_LIMIT"]})
    );
}

#[test]
fn 앞선_한도가_없으면_뒤의_한도를_그대로_쓴다() {
    // given
    let second = json!({"truncated":false,"reasons":[]});

    // when
    let actual = merge_limits(None, &second);

    // then
    assert_eq!(actual, second);
}
