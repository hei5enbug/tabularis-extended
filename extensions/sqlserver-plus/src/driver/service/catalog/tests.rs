use super::*;

#[test]
fn 객체_목록은_계약이_정의한_종류_이름으로_routine과_trigger를_포함한다() {
    // given
    let sql = objects_sql();

    // when
    let actual = ["'view'", "'routine'", "'trigger'", "'table'"].map(|kind| sql.contains(kind));

    // then
    assert_eq!(actual, [true; 4]);
}

#[test]
fn routine은_legacy_get_routines가_지원하는_형식만_열거한다() {
    // given
    let sql = objects_sql();

    // when
    let actual = sql.contains("o.type IN ('P','PC','FN','FS','FT','IF','TF')");

    // then
    assert!(actual);
    assert!(!sql.contains("'X'"));
}

#[test]
fn trigger는_table_수준만_열거하고_부모_객체를_table로_반환한다() {
    // given
    let sql = objects_sql();

    // when
    let actual = (
        sql.contains("tr.parent_class=1"),
        sql.contains("LEFT JOIN sys.objects p ON p.object_id=tr.parent_id"),
        sql.contains("AS [table]"),
        sql.contains("tr.object_id IS NOT NULL"),
    );

    // then
    assert_eq!(actual, (true, true, true, true));
}

#[test]
fn 객체_목록은_schema를_매개변수로만_받고_시스템_객체를_제외한다() {
    // given
    let sql = objects_sql();

    // when
    let parameters: Vec<&str> = sql
        .match_indices('@')
        .map(|(i, _)| &sql[i..i + 7])
        .collect();

    // then
    assert!(parameters.iter().all(|name| *name == "@schema"));
    assert!(sql.contains("o.is_ms_shipped=0"));
}

#[test]
fn 객체_목록_질의는_schema를_하나의_매개변수로_묶는다() {
    // given
    let input = Input::Objects {
        database: "app".into(),
        schema: Some("sales".into()),
    };
    let params = ConnectionParams {
        database: crate::models::DatabaseSelection::Single("app".into()),
        ..Default::default()
    };

    // when
    let actual = query(&input, &params).unwrap();

    // then
    assert_eq!(actual.key, "objects");
    assert_eq!(actual.parameters.len(), 1);
    assert_eq!(actual.parameters[0].name, "@schema");
    assert_eq!(actual.parameters[0].value, json!("sales"));
}

fn connection() -> ConnectionParams {
    ConnectionParams {
        database: crate::models::DatabaseSelection::Single("app".into()),
        ..Default::default()
    }
}

fn describe_input(value: Value) -> Result<Input, serde_json::Error> {
    let mut input =
        json!({"operation":"catalog.describe","table":{"schema":"sales","table":"orders"}});
    input
        .as_object_mut()
        .unwrap()
        .extend(value.as_object().unwrap().clone());
    serde_json::from_value(input)
}

fn set(columns: &[&str], rows: Value) -> Value {
    json!({"columns":columns,"column_metadata":[],"rows":rows})
}

fn with_additional(first: Value, rest: Vec<Value>) -> Value {
    let mut data = first;
    data["additional_results"] = json!(rest);
    data
}

fn describe_query(kind: Option<Kind>) -> Query {
    let input = Input::Describe {
        table: TableRef {
            database: None,
            schema: Some("sales".into()),
            table: "orders".into(),
        },
        kind,
    };
    query(&input, &connection()).unwrap()
}

#[test]
fn 객체_목록은_sequence_synonym_type_rule_security_policy를_계약_종류_이름으로_포함한다() {
    // given
    let sql = objects_sql();

    // when
    let actual = [
        "'sequence'",
        "'synonym'",
        "'type'",
        "'rule'",
        "'security_policy'",
    ]
    .map(|kind| sql.contains(kind));

    // then
    assert_eq!(actual, [true; 5]);
    assert!(sql.contains("o.type='SO'") && sql.contains("o.type='SN'"));
    assert!(sql.contains("o.type='R'") && sql.contains("o.type='SP'"));
    assert!(sql.contains("ty.is_user_defined=1"));
}

#[test]
fn 유형_목록도_schema_매개변수만으로_거른다() {
    // given
    let sql = objects_sql();

    // when
    let filters = sql.matches("(@schema IS NULL OR s.name=@schema)").count();

    // then
    assert_eq!(filters, 2);
}

#[test]
fn describe_입력은_kind_없이도_받고_알_수_없는_kind나_필드는_거부한다() {
    // given
    let without = describe_input(json!({}));
    let with = describe_input(json!({"kind":"security_policy"}));

    // when
    let unknown_kind = describe_input(json!({"kind":"materialized_view"}));
    let unknown_field = describe_input(json!({"limit":1}));

    // then
    assert!(matches!(without, Ok(Input::Describe { kind: None, .. })));
    assert!(matches!(
        with,
        Ok(Input::Describe {
            kind: Some(Kind::SecurityPolicy),
            ..
        })
    ));
    assert!(unknown_kind.is_err());
    assert!(unknown_field.is_err());
}

#[test]
fn kind가_없으면_table과_같은_질의로_table_매개변수_하나만_묶는다() {
    // given
    let without = describe_query(None);

    // when
    let table = describe_query(Some(Kind::Table));
    let view = describe_query(Some(Kind::View));

    // then
    assert_eq!(without.text, table.text);
    assert_eq!(without.text, view.text);
    assert_eq!(without.parameters.len(), 1);
    assert_eq!(without.parameters[0].name, "@table");
    assert_eq!(without.parameters[0].value, json!("[sales].[orders]"));
    assert_eq!(without.kind, Some(Kind::Table));
}

#[test]
fn 객체_종류별_질의는_schema와_이름을_각각_매개변수로만_받는다() {
    // given
    let kinds = [
        Kind::Routine,
        Kind::Trigger,
        Kind::Sequence,
        Kind::Synonym,
        Kind::Type,
        Kind::Rule,
        Kind::SecurityPolicy,
    ];

    // when
    let actual = kinds.map(|kind| describe_query(Some(kind)));

    // then
    for query in &actual {
        let names: Vec<_> = query.parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["@schema", "@name"]);
        assert_eq!(query.parameters[0].value, json!("sales"));
        assert_eq!(query.parameters[1].value, json!("orders"));
        assert!(!query.text.contains("orders") && !query.text.contains("sales"));
    }
}

#[test]
fn 질의는_schema를_생략하면_dbo로_묶는다() {
    // given
    let input = Input::Describe {
        table: TableRef {
            database: None,
            schema: None,
            table: "seq".into(),
        },
        kind: Some(Kind::Sequence),
    };

    // when
    let actual = query(&input, &connection()).unwrap();

    // then
    assert_eq!(actual.parameters[0].value, json!("dbo"));
}

#[test]
fn 종류별_질의는_결과_집합_순서대로_필요한_원본만_조회한다() {
    // given
    let table = describe_query(None).text;

    // when
    let positions = [
        "FROM sys.columns c",
        "FROM sys.foreign_keys fk",
        "FROM sys.indexes i",
        "FROM sys.check_constraints",
        "FROM sys.sql_modules m",
    ]
    .map(|source| table.find(source).unwrap());

    // then
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(table.contains("default_value") && table.contains("computed_definition"));
    assert!(table.contains("filter_definition") && table.contains("is_included_column"));
}

#[test]
fn 표_설명은_외래키와_인덱스와_키를_열_단위_행에서_묶어_만든다() {
    // given
    let columns = set(
        &["name", "data_type"],
        json!([["id", "int"], ["parent_id", "int"]]),
    );
    let foreign_keys = set(
        &[
            "name",
            "column_name",
            "ref_schema",
            "ref_table",
            "ref_column",
            "on_update",
            "on_delete",
        ],
        json!([
            ["fk_a", "a", "dbo", "parent", "x", "NO ACTION", "CASCADE"],
            ["fk_a", "b", "dbo", "parent", "y", "NO ACTION", "CASCADE"]
        ]),
    );
    let index_columns = [
        "name",
        "type_desc",
        "is_unique",
        "is_primary",
        "filter",
        "constraint_name",
        "constraint_type",
        "column_name",
        "key_ordinal",
        "is_included",
    ];
    let indexes = set(
        &index_columns,
        json!([
            [
                "pk_t",
                "CLUSTERED",
                true,
                true,
                null,
                "pk_t",
                "PK",
                "id",
                1,
                false
            ],
            [
                "uq_t",
                "NONCLUSTERED",
                true,
                false,
                null,
                "uq_t",
                "UQ",
                "code",
                1,
                false
            ],
            [
                "ix_t",
                "NONCLUSTERED",
                false,
                false,
                "([qty]>(0))",
                null,
                null,
                "b",
                1,
                false
            ],
            [
                "ix_t",
                "NONCLUSTERED",
                false,
                false,
                "([qty]>(0))",
                null,
                null,
                "a",
                2,
                false
            ],
            [
                "ix_t",
                "NONCLUSTERED",
                false,
                false,
                "([qty]>(0))",
                null,
                null,
                "note",
                0,
                true
            ]
        ]),
    );
    let checks = set(&["name", "definition"], json!([["ck_qty", "([qty]>=(0))"]]));
    let definition = set(&["definition"], json!([]));
    let data = with_additional(columns, vec![foreign_keys, indexes, checks, definition]);

    // when
    let actual = result(&describe_query(None), &data);

    // then
    assert_eq!(actual["columns"][1]["name"], "parent_id");
    assert_eq!(
        actual["foreign_keys"],
        json!([{"name":"fk_a","columns":["a","b"],"ref_schema":"dbo","ref_table":"parent","ref_columns":["x","y"],"on_update":"NO ACTION","on_delete":"CASCADE"}])
    );
    assert_eq!(
        actual["indexes"][2],
        json!({"name":"ix_t","columns":["b","a"],"included_columns":["note"],"is_unique":false,"is_primary":false,"type":"NONCLUSTERED","filter":"([qty]>(0))","definition":null})
    );
    assert_eq!(
        actual["keys"],
        json!([
            {"name":"pk_t","kind":"primary_key","columns":["id"],"index":"pk_t"},
            {"name":"uq_t","kind":"unique","columns":["code"],"index":"uq_t"}
        ])
    );
    assert_eq!(
        actual["check_constraints"],
        json!([{"name":"ck_qty","definition":"([qty]>=(0))"}])
    );
    assert_eq!(actual["definition"], Value::Null);
    assert_eq!(actual["properties"], json!({}));
}

#[test]
fn 없는_객체의_설명은_오류_없이_모든_키가_빈_값으로_온다() {
    // given
    let data = with_additional(
        set(&["name"], json!([])),
        vec![
            set(&["name"], json!([])),
            set(&["name"], json!([])),
            set(&["name"], json!([])),
            set(&["definition"], json!([])),
        ],
    );

    // when
    let actual = [
        None,
        Some(Kind::Routine),
        Some(Kind::SecurityPolicy),
        Some(Kind::Sequence),
        Some(Kind::Trigger),
    ]
    .map(|kind| result(&describe_query(kind), &data));

    // then
    for response in actual {
        assert_eq!(
            response,
            json!({"columns":[],"foreign_keys":[],"indexes":[],"keys":[],"check_constraints":[],"definition":null,"properties":{}})
        );
    }
}

#[test]
fn 뷰_정의는_definition으로_오고_암호화된_정의는_null이다() {
    // given
    let view = with_additional(
        set(&["name"], json!([["id"]])),
        vec![
            set(&["name"], json!([])),
            set(&["name"], json!([])),
            set(&["name"], json!([])),
            set(
                &["definition"],
                json!([["CREATE VIEW v AS SELECT 1 AS id"]]),
            ),
        ],
    );
    let encrypted = with_additional(
        set(&["name"], json!([])),
        vec![
            set(&["name"], json!([])),
            set(&["name"], json!([])),
            set(&["name"], json!([])),
            set(&["definition"], json!([[null]])),
        ],
    );

    // when
    let actual =
        [&view, &encrypted].map(|data| result(&describe_query(None), data)["definition"].clone());

    // then
    assert_eq!(
        actual,
        [json!("CREATE VIEW v AS SELECT 1 AS id"), Value::Null]
    );
}

#[test]
fn routine_설명은_매개변수를_컬럼으로_두고_종류와_반환_형식을_속성으로_둔다() {
    // given
    let parameters = set(
        &["name", "data_type", "mode", "ordinal_position"],
        json!([
            ["", "int", "OUT", 0],
            ["@x", "int", "IN", 1],
            ["@y", "varchar(10)", "INOUT", 2]
        ]),
    );
    let meta = set(
        &["definition", "p_routine_type", "p_return_type"],
        json!([["CREATE FUNCTION f()", "FUNCTION", "int"]]),
    );
    let data = with_additional(parameters, vec![meta]);

    // when
    let actual = result(&describe_query(Some(Kind::Routine)), &data);

    // then
    assert_eq!(actual["columns"][2]["mode"], "INOUT");
    assert_eq!(actual["definition"], "CREATE FUNCTION f()");
    assert_eq!(
        actual["properties"],
        json!({"routine_type":"FUNCTION","return_type":"int"})
    );
    assert_eq!(actual["foreign_keys"], json!([]));
}

#[test]
fn trigger_설명은_이벤트를_배열로_나누고_활성_여부를_속성으로_둔다() {
    // given
    let data = set(
        &["definition", "p_table", "p_timing", "p_events", "p_enabled"],
        json!([["CREATE TRIGGER t", "orders", "AFTER", "INSERT,UPDATE", true]]),
    );

    // when
    let actual = result(&describe_query(Some(Kind::Trigger)), &data);

    // then
    assert_eq!(
        actual["properties"],
        json!({"table":"orders","timing":"AFTER","events":["INSERT","UPDATE"],"enabled":true})
    );
    assert_eq!(actual["definition"], "CREATE TRIGGER t");
}

#[test]
fn sequence_설명은_정수_범위는_숫자로_넘치는_값은_문자열로_둔다() {
    // given
    let data = set(
        &[
            "definition",
            "p_data_type",
            "p_start_value",
            "p_increment",
            "p_minimum_value",
            "p_maximum_value",
            "p_cycle",
            "p_cache_size",
            "p_current_value",
        ],
        json!([[
            null,
            "decimal",
            "7",
            "-3",
            "-99999999999999999999",
            "99999999999999999999",
            true,
            null,
            "7"
        ]]),
    );

    // when
    let actual = result(&describe_query(Some(Kind::Sequence)), &data);

    // then
    assert_eq!(
        actual["properties"],
        json!({"data_type":"decimal","start_value":7,"increment":-3,"minimum_value":"-99999999999999999999","maximum_value":"99999999999999999999","cycle":true,"cache_size":null,"current_value":7})
    );
}

#[test]
fn sequence_설명은_javascript_안전_정수를_넘는_값도_문자열로_둔다() {
    // given
    let data = set(
        &[
            "definition",
            "p_data_type",
            "p_start_value",
            "p_increment",
            "p_minimum_value",
            "p_maximum_value",
            "p_cycle",
            "p_cache_size",
            "p_current_value",
        ],
        json!([[
            null,
            "bigint",
            "9007199254740991",
            "1",
            "-9007199254740993",
            "9007199254740993",
            false,
            null,
            "-9007199254740991"
        ]]),
    );

    // when
    let actual = result(&describe_query(Some(Kind::Sequence)), &data);

    // then
    assert_eq!(
        actual["properties"]["start_value"],
        json!(9007199254740991_i64)
    );
    assert_eq!(
        actual["properties"]["current_value"],
        json!(-9007199254740991_i64)
    );
    assert_eq!(actual["properties"]["minimum_value"], "-9007199254740993");
    assert_eq!(actual["properties"]["maximum_value"], "9007199254740993");
}

#[test]
fn type_설명은_표_형식의_컬럼과_별칭_속성을_함께_돌려준다() {
    // given
    let columns = set(&["name", "data_type"], json!([["id", "int"]]));
    let meta = set(
        &[
            "definition",
            "p_category",
            "p_base_type",
            "p_max_length",
            "p_precision",
            "p_scale",
            "p_nullable",
        ],
        json!([[null, "table", null, null, null, null, null]]),
    );
    let data = with_additional(columns, vec![meta]);

    // when
    let actual = result(&describe_query(Some(Kind::Type)), &data);

    // then
    assert_eq!(actual["columns"][0]["name"], "id");
    assert_eq!(actual["properties"]["category"], "table");
    assert_eq!(actual["properties"]["base_type"], Value::Null);
}

#[test]
fn security_policy_설명은_조건자를_속성에_담는다() {
    // given
    let predicates = set(
        &["type", "operation", "target_table", "definition"],
        json!([
            ["FILTER", null, "dbo.orders", "([dbo].[f]([id]))"],
            ["BLOCK", "AFTER INSERT", "dbo.orders", "([dbo].[f]([id]))"]
        ]),
    );
    let meta = set(
        &["definition", "p_is_enabled", "p_is_schema_bound"],
        json!([[null, false, true]]),
    );
    let data = with_additional(predicates, vec![meta]);

    // when
    let actual = result(&describe_query(Some(Kind::SecurityPolicy)), &data);

    // then
    assert_eq!(actual["properties"]["is_enabled"], false);
    assert_eq!(actual["properties"]["is_schema_bound"], true);
    assert_eq!(
        actual["properties"]["predicates"][1],
        json!({"type":"BLOCK","operation":"AFTER INSERT","target_table":"dbo.orders","definition":"([dbo].[f]([id]))"})
    );
}
