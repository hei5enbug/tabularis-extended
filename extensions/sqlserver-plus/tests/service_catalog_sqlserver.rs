#[path = "support/service_sqlserver/mod.rs"]
#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use support::Plugin;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct Catalog {
    plugin: Plugin,
    database: String,
    schema: String,
    requests: u32,
}

fn batch(statement: &str) -> Value {
    json!({"text":format!("EXEC(N'{}')", statement.replace('\'', "''")),"parameters":[]})
}

impl Catalog {
    fn new() -> Self {
        let mut catalog = Self {
            plugin: Plugin::new(),
            database: env_or("SQLSERVER_TEST_DATABASE", "tabularis_test"),
            schema: format!("cat_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
            requests: 0,
        };
        catalog.run(&format!("CREATE SCHEMA [{}]", catalog.schema));
        catalog
    }

    fn request(&mut self, method: &str, input: Value, read_only: bool) -> Value {
        self.requests += 1;
        let request = format!("catalog{}", self.requests);
        let user = env_or("SQLSERVER_TEST_USER", "sa");
        let password = env_or("SQLSERVER_TEST_PASSWORD", "Str0ng!Passw0rd");
        let port = env_or("SQLSERVER_TEST_PORT", "1433")
            .parse::<u16>()
            .unwrap();
        let connection = json!({"driver":"sqlserver","host":env_or("SQLSERVER_TEST_HOST","127.0.0.1"),"port":port,"database":self.database,"username":user,"ssl_mode":"require"});
        self.plugin.call(&json!({"jsonrpc":"2.0","id":request,"method":method,"params":{"params":connection,"driver_context":{"protocol_version":1,"connection_id":"catalog-live","request_id":request,"deadline_ms":20_000,"read_only":read_only,"auth":{"kind":"db_password","password":password,"identity":user}},"input":input}}))
    }

    fn run(&mut self, text: &str) {
        let actual = self.request("execute_query_exact", batch(text), false);
        assert!(
            actual.get("error").is_none(),
            "fixture SQL failed: {text}: {}",
            actual["error"]["data"]["details"]
        );
    }

    fn describe(&mut self, name: &str, kind: Option<&str>) -> Value {
        let mut input = json!({"operation":"catalog.describe","table":{"database":self.database,"schema":self.schema,"table":name}});
        if let Some(kind) = kind {
            input["kind"] = json!(kind);
        }
        let actual = self.request("service_catalog", input, true);
        assert!(actual.get("error").is_none(), "describe failed: {actual}");
        actual["result"].clone()
    }

    fn objects(&mut self) -> Vec<(String, String)> {
        let input =
            json!({"operation":"catalog.objects","database":self.database,"schema":self.schema});
        let actual = self.request("service_catalog", input, true);
        assert!(actual.get("error").is_none(), "objects failed: {actual}");
        actual["result"]["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|object| {
                (
                    object["kind"].as_str().unwrap().to_owned(),
                    object["name"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn seed(&mut self) {
        let s = self.schema.clone();
        for statement in [
            format!("CREATE TABLE [{s}].parent(id int IDENTITY(1,1) NOT NULL PRIMARY KEY, code nvarchar(20) NOT NULL, CONSTRAINT uq_parent_code UNIQUE(code))"),
            format!("CREATE TABLE [{s}].child(id int NOT NULL, parent_id int NOT NULL, qty int NOT NULL CONSTRAINT df_child_qty DEFAULT (5), total AS (qty*2), note nvarchar(50) NULL, CONSTRAINT pk_child PRIMARY KEY(id, parent_id), CONSTRAINT fk_child_parent FOREIGN KEY(parent_id) REFERENCES [{s}].parent(id) ON DELETE CASCADE ON UPDATE NO ACTION, CONSTRAINT ck_child_qty CHECK (qty>=0))"),
            format!("CREATE NONCLUSTERED INDEX ix_child_qty ON [{s}].child(qty DESC, parent_id) INCLUDE(note) WHERE qty>0"),
            format!("CREATE VIEW [{s}].child_view WITH SCHEMABINDING AS SELECT id, parent_id, qty FROM [{s}].child"),
            format!("CREATE UNIQUE CLUSTERED INDEX ix_child_view ON [{s}].child_view(id, parent_id)"),
            format!("CREATE PROCEDURE [{s}].add_child @id int, @note nvarchar(20) OUTPUT AS SET @note = N'x'"),
            format!("CREATE FUNCTION [{s}].double_it(@value decimal(10,2)) RETURNS decimal(10,2) AS BEGIN RETURN @value * 2 END"),
            format!("CREATE FUNCTION [{s}].children_of(@parent int) RETURNS TABLE AS RETURN SELECT id, qty FROM [{s}].child WHERE parent_id = @parent"),
            format!("CREATE TRIGGER [{s}].trg_child ON [{s}].child AFTER INSERT, UPDATE AS SET NOCOUNT ON"),
            format!("CREATE SEQUENCE [{s}].order_seq AS bigint START WITH 7 INCREMENT BY 3 MINVALUE 1 MAXVALUE 1000 CYCLE CACHE 10"),
            format!("CREATE SYNONYM [{s}].child_alias FOR [{s}].child"),
            format!("CREATE TYPE [{s}].short_code FROM varchar(12) NOT NULL"),
            format!("CREATE TYPE [{s}].pair AS TABLE(a int NOT NULL, b nvarchar(10) NULL, PRIMARY KEY(a))"),
            format!("CREATE RULE [{s}].positive AS @value > 0"),
            format!("CREATE FUNCTION [{s}].visible(@id int) RETURNS TABLE WITH SCHEMABINDING AS RETURN SELECT 1 AS ok WHERE @id > 0"),
            format!("CREATE TABLE [{s}].guarded(id int NOT NULL)"),
            format!("CREATE SECURITY POLICY [{s}].child_policy ADD FILTER PREDICATE [{s}].visible(id) ON [{s}].guarded, ADD BLOCK PREDICATE [{s}].visible(id) ON [{s}].guarded AFTER INSERT WITH (STATE = OFF)"),
        ] {
            self.run(&statement);
        }
    }
}

impl Drop for Catalog {
    fn drop(&mut self) {
        let s = self.schema.clone();
        for statement in [
            format!("DROP SECURITY POLICY IF EXISTS [{s}].child_policy"),
            format!("DROP TRIGGER IF EXISTS [{s}].trg_child"),
            format!("DROP SYNONYM IF EXISTS [{s}].child_alias"),
            format!("DROP VIEW IF EXISTS [{s}].child_view"),
            format!("DROP FUNCTION IF EXISTS [{s}].children_of"),
            format!("DROP FUNCTION IF EXISTS [{s}].visible"),
            format!("DROP FUNCTION IF EXISTS [{s}].double_it"),
            format!("DROP PROCEDURE IF EXISTS [{s}].add_child"),
            format!("DROP SEQUENCE IF EXISTS [{s}].order_seq"),
            format!("DROP RULE IF EXISTS [{s}].positive"),
            format!("DROP TABLE IF EXISTS [{s}].guarded"),
            format!("DROP TABLE IF EXISTS [{s}].child"),
            format!("DROP TABLE IF EXISTS [{s}].parent"),
            format!("DROP TYPE IF EXISTS [{s}].short_code"),
            format!("DROP TYPE IF EXISTS [{s}].pair"),
            format!("DROP SCHEMA IF EXISTS [{s}]"),
        ] {
            let _ = self.request("execute_query_exact", batch(&statement), false);
        }
    }
}

#[test]
fn 표를_설명하면_컬럼_기본값_계산식과_외래키_인덱스_키_검사_제약이_함께_온다() {
    // given
    let mut catalog = Catalog::new();
    catalog.seed();

    // when
    let actual = catalog.describe("child", None);

    // then
    let columns = actual["columns"].as_array().unwrap();
    let column = |name: &str| columns.iter().find(|c| c["name"] == name).unwrap();
    assert_eq!(columns.len(), 5);
    assert_eq!(column("qty")["default_value"], "((5))");
    assert_eq!(column("qty")["is_computed"], false);
    assert_eq!(column("total")["is_computed"], true);
    assert_eq!(column("total")["computed_definition"], "([qty]*(2))");
    assert_eq!(column("id")["is_pk"], true);
    assert_eq!(column("parent_id")["primary_key_position"], 2);
    assert_eq!(column("note")["default_value"], Value::Null);
    let foreign_key = &actual["foreign_keys"][0];
    assert_eq!(foreign_key["name"], "fk_child_parent");
    assert_eq!(foreign_key["columns"], json!(["parent_id"]));
    assert_eq!(foreign_key["ref_table"], "parent");
    assert_eq!(foreign_key["ref_columns"], json!(["id"]));
    assert_eq!(foreign_key["on_delete"], "CASCADE");
    assert_eq!(foreign_key["on_update"], "NO ACTION");
    let indexes = actual["indexes"].as_array().unwrap();
    let index = indexes
        .iter()
        .find(|i| i["name"] == "ix_child_qty")
        .unwrap();
    assert_eq!(index["columns"], json!(["qty", "parent_id"]));
    assert_eq!(index["included_columns"], json!(["note"]));
    assert_eq!(index["type"], "NONCLUSTERED");
    assert_eq!(index["filter"], "([qty]>(0))");
    assert_eq!(index["is_unique"], false);
    assert_eq!(index["definition"], Value::Null);
    let primary = indexes.iter().find(|i| i["name"] == "pk_child").unwrap();
    assert_eq!(primary["type"], "CLUSTERED");
    assert_eq!(primary["is_primary"], true);
    assert_eq!(
        actual["keys"],
        json!([{"name":"pk_child","kind":"primary_key","columns":["id","parent_id"],"index":"pk_child"}])
    );
    assert_eq!(
        actual["check_constraints"],
        json!([{"name":"ck_child_qty","definition":"([qty]>=(0))"}])
    );
    assert_eq!(actual["definition"], Value::Null);
    assert_eq!(actual["properties"], json!({}));
}

#[test]
fn 고유_제약이_있는_표의_keys는_기본키_다음에_고유키를_뒤따르게_한다() {
    // given
    let mut catalog = Catalog::new();
    catalog.seed();

    // when
    let actual = catalog.describe("parent", Some("table"));

    // then
    assert_eq!(actual["keys"][0]["kind"], "primary_key");
    assert_eq!(actual["keys"][0]["columns"], json!(["id"]));
    assert_eq!(
        actual["keys"][1],
        json!({"name":"uq_parent_code","kind":"unique","columns":["code"],"index":"uq_parent_code"})
    );
    assert_eq!(actual["columns"][0]["is_identity"], true);
    assert_eq!(actual["foreign_keys"], json!([]));
}

#[test]
fn 인덱스된_뷰를_설명하면_정의와_인덱스가_오고_계산식_없는_컬럼을_돌려준다() {
    // given
    let mut catalog = Catalog::new();
    catalog.seed();

    // when
    let actual = catalog.describe("child_view", Some("view"));

    // then
    assert!(actual["definition"]
        .as_str()
        .unwrap()
        .contains("CREATE VIEW"));
    let columns: Vec<_> = actual["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|column| column["name"].clone())
        .collect();
    assert_eq!(columns, [json!("id"), json!("parent_id"), json!("qty")]);
    assert_eq!(actual["indexes"][0]["name"], "ix_child_view");
    assert_eq!(actual["indexes"][0]["is_unique"], true);
    assert_eq!(actual["keys"], json!([]));
}

#[test]
fn routine을_설명하면_매개변수가_컬럼으로_오고_종류와_반환_형식이_속성으로_온다() {
    // given
    let mut catalog = Catalog::new();
    catalog.seed();

    // when
    let procedure = catalog.describe("add_child", Some("routine"));
    let scalar = catalog.describe("double_it", Some("routine"));
    let table_valued = catalog.describe("children_of", Some("routine"));

    // then
    let parameters: Vec<_> = procedure["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].clone(),
                c["data_type"].clone(),
                c["mode"].clone(),
                c["ordinal_position"].clone(),
            )
        })
        .collect();
    assert_eq!(
        parameters,
        [
            (json!("@id"), json!("int"), json!("IN"), json!(1)),
            (
                json!("@note"),
                json!("nvarchar(20)"),
                json!("INOUT"),
                json!(2)
            ),
        ]
    );
    assert_eq!(
        procedure["properties"],
        json!({"routine_type":"PROCEDURE","return_type":null})
    );
    assert!(procedure["definition"]
        .as_str()
        .unwrap()
        .contains("CREATE PROCEDURE"));
    assert_eq!(scalar["columns"][0]["mode"], "OUT");
    assert_eq!(scalar["columns"][0]["name"], "");
    assert_eq!(scalar["columns"][0]["ordinal_position"], 0);
    assert_eq!(scalar["columns"][1]["data_type"], "decimal(10,2)");
    assert_eq!(
        scalar["properties"],
        json!({"routine_type":"FUNCTION","return_type":"decimal(10,2)"})
    );
    let modes: Vec<_> = table_valued["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap().to_owned(),
                c["mode"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        modes,
        [
            ("@parent".into(), "IN".into()),
            ("id".into(), "TABLE".into()),
            ("qty".into(), "TABLE".into())
        ]
    );
    assert_eq!(table_valued["properties"]["return_type"], "TABLE");
}

#[test]
fn trigger를_설명하면_정의와_시점_이벤트_활성_여부가_온다() {
    // given
    let mut catalog = Catalog::new();
    catalog.seed();

    // when
    let actual = catalog.describe("trg_child", Some("trigger"));

    // then
    assert!(actual["definition"]
        .as_str()
        .unwrap()
        .contains("CREATE TRIGGER"));
    assert_eq!(
        actual["properties"],
        json!({"table":"child","timing":"AFTER","events":["INSERT","UPDATE"],"enabled":true})
    );
    assert_eq!(actual["columns"], json!([]));
}

#[test]
fn sequence와_synonym을_설명하면_범위_값과_대상_객체가_속성으로_온다() {
    // given
    let mut catalog = Catalog::new();
    catalog.seed();
    let schema = catalog.schema.clone();

    // when
    let sequence = catalog.describe("order_seq", Some("sequence"));
    let synonym = catalog.describe("child_alias", Some("synonym"));

    // then
    assert_eq!(
        sequence["properties"],
        json!({"data_type":"bigint","start_value":7,"increment":3,"minimum_value":1,"maximum_value":1000,"cycle":true,"cache_size":10,"current_value":7})
    );
    assert_eq!(sequence["definition"], Value::Null);
    assert_eq!(
        synonym["properties"]["base_object"],
        json!(format!("[{schema}].[child]"))
    );
}

#[test]
fn type을_설명하면_별칭은_기반_형식을_표_형식은_컬럼을_돌려준다() {
    // given
    let mut catalog = Catalog::new();
    catalog.seed();

    // when
    let alias = catalog.describe("short_code", Some("type"));
    let table = catalog.describe("pair", Some("type"));

    // then
    assert_eq!(
        alias["properties"],
        json!({"category":"alias","base_type":"varchar","max_length":12,"precision":0,"scale":0,"nullable":false})
    );
    assert_eq!(alias["columns"], json!([]));
    assert_eq!(table["properties"]["category"], "table");
    assert_eq!(table["properties"]["base_type"], Value::Null);
    assert_eq!(table["columns"][0]["name"], "a");
    assert_eq!(table["columns"][0]["is_pk"], true);
    assert_eq!(table["columns"][1]["name"], "b");
}

#[test]
fn rule과_security_policy를_설명하면_정의와_조건자가_온다() {
    // given
    let mut catalog = Catalog::new();
    catalog.seed();
    let schema = catalog.schema.clone();

    // when
    let rule = catalog.describe("positive", Some("rule"));
    let policy = catalog.describe("child_policy", Some("security_policy"));

    // then
    assert!(rule["definition"].as_str().unwrap().contains("CREATE RULE"));
    assert_eq!(rule["properties"], json!({}));
    assert_eq!(policy["properties"]["is_enabled"], false);
    assert_eq!(policy["properties"]["is_schema_bound"], true);
    let predicates = policy["properties"]["predicates"].as_array().unwrap();
    assert_eq!(predicates.len(), 2);
    let filter = predicates.iter().find(|p| p["type"] == "FILTER").unwrap();
    assert_eq!(filter["target_table"], json!(format!("{schema}.guarded")));
    assert_eq!(filter["operation"], Value::Null);
    let block = predicates.iter().find(|p| p["type"] == "BLOCK").unwrap();
    assert_eq!(block["operation"], "AFTER INSERT");
    assert!(block["definition"].as_str().unwrap().contains("visible"));
}

#[test]
fn 없는_객체를_설명하면_오류_없이_모든_키가_빈_값으로_온다() {
    // given
    let mut catalog = Catalog::new();

    // when
    let actual = [
        None,
        Some("routine"),
        Some("type"),
        Some("security_policy"),
        Some("trigger"),
    ]
    .map(|kind| catalog.describe("missing", kind));

    // then
    for response in actual {
        assert_eq!(
            response,
            json!({"columns":[],"foreign_keys":[],"indexes":[],"keys":[],"check_constraints":[],"definition":null,"properties":{}})
        );
    }
}

#[test]
fn 객체_목록은_새_종류를_모두_같은_모양으로_담고_schema로_거른다() {
    // given
    let mut catalog = Catalog::new();
    catalog.seed();

    // when
    let actual = catalog.objects();

    // then
    for expected in [
        ("table", "child"),
        ("table", "parent"),
        ("view", "child_view"),
        ("routine", "add_child"),
        ("routine", "double_it"),
        ("routine", "children_of"),
        ("trigger", "trg_child"),
        ("sequence", "order_seq"),
        ("synonym", "child_alias"),
        ("type", "short_code"),
        ("type", "pair"),
        ("rule", "positive"),
        ("security_policy", "child_policy"),
    ] {
        assert!(
            actual.contains(&(expected.0.to_owned(), expected.1.to_owned())),
            "missing {expected:?} in {actual:?}"
        );
    }
}

#[test]
fn 알_수_없는_kind로_설명하면_잘못된_인수로_거부한다() {
    // given
    let mut catalog = Catalog::new();
    let input = json!({"operation":"catalog.describe","table":{"schema":catalog.schema,"table":"child"},"kind":"materialized_view"});

    // when
    let actual = catalog.request("service_catalog", input, true);

    // then
    assert_eq!(actual["error"]["data"]["code"], "INVALID_ARGUMENT");
}
