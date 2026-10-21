use super::{objects_sql, KINDS};

#[test]
fn 객체_목록은_계약이_정의한_종류_이름으로_routine과_trigger를_포함한다() {
    // given
    let sql = objects_sql();

    // when
    let actual: Vec<bool> = ["'routine'", "'trigger'", "'view'", "'materialized_view'"]
        .iter()
        .map(|kind| sql.contains(kind))
        .collect();

    // then
    assert_eq!(actual, [true, true, true, true]);
    assert!(!sql.contains("foreign_table"));
    assert_eq!(
        KINDS,
        ["table", "view", "materialized_view", "routine", "trigger"]
    );
}

#[test]
fn 트리거_목록은_내부_트리거를_제외하고_소유_테이블을_함께_반환한다() {
    // given
    let sql = objects_sql();

    // when
    let actual = (
        sql.contains("FROM pg_catalog.pg_trigger t"),
        sql.contains("NOT t.tgisinternal"),
        sql.contains("c.relname::text"),
        sql.contains("AS \"table\""),
    );

    // then
    assert_eq!(actual, (true, true, true, true));
}

#[test]
fn 루틴_목록은_prokind에_의존하지_않고_함수_프로시저_집계를_모두_포함한다() {
    // given
    let sql = objects_sql();

    // when
    let actual = (
        sql.contains("FROM pg_catalog.pg_proc p"),
        sql.contains("prokind"),
        sql.contains("proisagg"),
    );

    // then
    assert_eq!(actual, (true, false, false));
}

#[test]
fn 스키마를_지정하지_않으면_시스템_스키마와_extension_소유_루틴을_제외한다() {
    // given
    let sql = objects_sql();

    // when
    let actual = (
        sql.matches("n.nspname NOT IN ('pg_catalog','information_schema')")
            .count(),
        sql.contains("d.deptype='e'"),
    );

    // then
    assert_eq!(actual, (2, true));
}

#[test]
fn 객체_목록은_스키마를_매개변수로만_받고_값을_이어붙이지_않는다() {
    // given
    let sql = objects_sql();

    // when
    let placeholders: Vec<&str> = sql
        .match_indices('$')
        .map(|(index, _)| &sql[index..index + 2])
        .collect();

    // then
    assert!(!placeholders.is_empty());
    assert!(placeholders.iter().all(|placeholder| *placeholder == "$1"));
}

#[test]
fn 객체_목록은_관계를_먼저_정렬해_행_제한_절단이_관계를_밀어내지_않는다() {
    // given
    let sql = objects_sql();

    // when
    let actual = sql.ends_with("ORDER BY o.rank,o.schema,o.name,o.owner_table");

    // then
    assert!(actual);
    assert!(sql.contains("SELECT 0 AS rank"));
}
