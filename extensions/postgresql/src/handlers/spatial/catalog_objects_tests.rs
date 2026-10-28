use super::{objects_sql, KINDS};

#[test]
fn 객체_목록은_계약이_정의한_종류_이름을_모두_포함한다() {
    // given
    let sql = objects_sql();

    // when
    let missing: Vec<&str> = KINDS
        .iter()
        .copied()
        .filter(|kind| !sql.contains(&format!("'{kind}'")))
        .collect();

    // then
    assert_eq!(missing, Vec::<&str>::new());
    assert_eq!(
        KINDS,
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
            "operator_family"
        ]
    );
}

#[test]
fn 외부_테이블은_테이블이_아니라_foreign_table_종류로_나온다() {
    // given
    let sql = objects_sql();

    // when
    let actual = sql.contains("WHEN 'f' THEN 'foreign_table'");

    // then
    assert!(actual);
    assert!(!sql.contains("WHEN 'f' THEN 'table'"));
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
fn 집계는_루틴에서_빠지고_aggregate_종류로_따로_나온다() {
    // given
    let sql = objects_sql();

    // when
    let actual = (
        sql.contains("FROM pg_catalog.pg_proc p"),
        sql.contains("CASE WHEN p.prokind='a' THEN 'aggregate' ELSE 'routine' END"),
    );

    // then
    assert_eq!(actual, (true, true));
}

#[test]
fn 새_종류는_각자의_시스템_카탈로그에서_읽는다() {
    // given
    let sql = objects_sql();

    // when
    let actual: Vec<bool> = [
        "FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE c.relkind='S'",
        "FROM pg_catalog.pg_type t",
        "FROM pg_catalog.pg_collation co",
        "FROM pg_catalog.pg_operator o",
        "FROM pg_catalog.pg_opclass oc",
        "FROM pg_catalog.pg_opfamily f",
    ]
    .iter()
    .map(|fragment| sql.contains(fragment))
    .collect();

    // then
    assert_eq!(actual, [true; 6]);
}

#[test]
fn 타입_목록은_배열_타입과_테이블_행_타입을_제외한다() {
    // given
    let sql = objects_sql();

    // when
    let actual = (
        sql.contains("t.typcategory<>'A'"),
        sql.contains("t.typtype IN ('d','e','c','r','b')"),
        sql.contains("c.oid=t.typrelid AND c.relkind='c'"),
    );

    // then
    assert_eq!(actual, (true, true, true));
}

#[test]
fn 스키마를_지정하지_않으면_시스템_스키마와_extension_소유_객체를_제외한다() {
    // given
    let sql = objects_sql();

    // when
    let actual = (
        sql.matches("n.nspname NOT IN ('pg_catalog','information_schema')")
            .count(),
        sql.matches("d.deptype='e'").count() + sql.matches("d.deptype IN ('e','i')").count(),
    );

    // then
    assert_eq!(actual, (8, 7));
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
