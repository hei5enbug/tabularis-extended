//! The five legacy tools against what the legacy server answered before the
//! service transport replaced its loop. `golden/legacy_head.jsonl` holds the
//! wire answers the previous server wrote for `golden/legacy_requests.jsonl`
//! over a fixed SQLite fixture; `{DB}` stands for the fixture's path.

use super::*;
use crate::drivers::sqlite;
use sqlx::{ConnectOptions, Executor};

const HEAD: &str = include_str!("golden/legacy_head.jsonl");
const REQUESTS: &str = include_str!("golden/legacy_requests.jsonl");

struct Fixture {
    directory: std::path::PathBuf,
    database: String,
}
impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("g-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&directory).unwrap();
        let database = directory
            .join("fixture.sqlite")
            .to_string_lossy()
            .into_owned();
        let mut connection = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        connection
            .execute(
                "create table users(id integer primary key, name text not null, email text);\
                 create table orders(id integer primary key, user_id integer references users(id), total real);\
                 create index idx_orders_user on orders(user_id);\
                 insert into users values (1,'ann','a@x.io'),(2,'bob',null),(3,'cy','c@x.io');\
                 insert into orders values (1,1,9.5),(2,1,3.25),(3,2,7);",
            )
            .await
            .unwrap();
        drop(connection);
        std::fs::write(
            directory.join("connections.json"),
            json!([{"id":"fx-sqlite","name":"Fixture","params":{"driver":"sqlite","database":database}}])
                .to_string(),
        )
        .unwrap();
        *CONFIG_DIR_OVERRIDE.lock().unwrap() = Some(directory.clone());
        Self {
            directory,
            database,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        *CONFIG_DIR_OVERRIDE.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn wire(id: Value, result: Value) -> String {
    serde_json::to_string(&JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: Some(id),
        result: Some(result),
        error: None,
    })
    .unwrap()
}

#[test]
fn 구형_도구_스키마는_새_선택_인자를_뺀_나머지가_이전_서버와_바이트가_같다() {
    // given
    let expected = HEAD.lines().next().unwrap();
    let mut tools = handle_list_tools(&AppConfig::default()).unwrap();
    // when
    let names: Vec<String> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    for tool in tools["tools"].as_array_mut().unwrap() {
        let name = tool["name"].as_str().unwrap().to_owned();
        let properties = tool["inputSchema"]["properties"].as_object_mut().unwrap();
        if matches!(
            name.as_str(),
            "list_tables" | "describe_table" | "run_query"
        ) {
            assert!(properties.remove("database").is_some(), "{name}");
        }
        if name == "run_query" {
            assert!(properties.remove("schema").is_some());
        }
    }
    // then
    assert_eq!(
        names,
        [
            "list_connections",
            "list_databases",
            "list_tables",
            "describe_table",
            "run_query"
        ]
    );
    assert_eq!(wire(json!(1), tools), expected);
}

#[test]
fn 새_선택_인자는_모두_선택이며_필수_목록을_바꾸지_않는다() {
    // given
    let tools = handle_list_tools(&AppConfig::default()).unwrap();
    // when
    let required: Vec<Value> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["inputSchema"]["required"].clone())
        .collect();
    // then
    assert_eq!(
        required,
        [
            Value::Null,
            json!(["connection_id"]),
            json!(["connection_id"]),
            json!(["connection_id", "table_name"]),
            json!(["connection_id", "query"])
        ]
    );
}

#[tokio::test]
async fn 구형_도구_출력은_인자가_없으면_이전_서버와_바이트가_같다() {
    // given
    let _guard = crate::drivers::registry::REGISTRY_TEST_LOCK.lock().await;
    crate::drivers::registry::register_driver(sqlite::SqliteDriver::new()).await;
    let fixture = Fixture::new().await;
    let config = AppConfig::default();
    // when
    let mut actual = Vec::new();
    for line in REQUESTS.lines().skip(1) {
        let request: Value = serde_json::from_str(line).unwrap();
        let params = &request["params"];
        let mut audit = CallAudit::for_tool(params["name"].as_str().unwrap());
        let result = dispatch_tool(
            params["name"].as_str().unwrap(),
            params["arguments"].as_object(),
            &config,
            "golden-session",
            &mut audit,
        )
        .await
        .unwrap();
        actual.push(wire(request["id"].clone(), result));
    }
    // then
    let expected: Vec<String> = HEAD
        .lines()
        .skip(1)
        .map(|line| line.replace("{DB}", &fixture.database))
        .collect();
    assert_eq!(actual.len(), 7);
    assert_eq!(actual, expected);
}

async fn call_legacy(config: &AppConfig, name: &str, args: Value) -> Result<Value, JsonRpcError> {
    let mut audit = CallAudit::for_tool(name);
    dispatch_tool(name, args.as_object(), config, "golden-session", &mut audit).await
}

fn payload(result: &Value) -> Value {
    assert!(
        result.get("structuredContent").is_none(),
        "a structuredContent without the payload hides it from clients"
    );
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn 스키마를_주면_run_query는_행과_유효_데이터베이스와_스키마를_같은_본문에_담는다() {
    // given
    let _guard = crate::drivers::registry::REGISTRY_TEST_LOCK.lock().await;
    crate::drivers::registry::register_driver(sqlite::SqliteDriver::new()).await;
    let _fixture = Fixture::new().await;
    let config = AppConfig::default();
    let plain = json!({"connection_id":"fx-sqlite","query":"select id from users order by id"});
    let chosen = json!({"connection_id":"fx-sqlite","query":"select id from users order by id","schema":"main"});
    // when
    let baseline = call_legacy(&config, "run_query", plain).await.unwrap();
    let with_schema = call_legacy(&config, "run_query", chosen).await.unwrap();
    // then
    let body = payload(&with_schema);
    assert_eq!(body["rows"], json!([[1], [2], [3]]));
    assert_eq!(body["columns"], json!(["id"]));
    assert_eq!(body["effective"]["schema"], "main");
    assert!(body["effective"]["database"].is_string());
    assert!(baseline.get("structuredContent").is_none());
    assert!(payload(&baseline).get("effective").is_none());
}

#[tokio::test]
async fn 스키마를_주면_describe_table은_열과_유효_데이터베이스와_스키마를_같은_본문에_담는다() {
    // given
    let _guard = crate::drivers::registry::REGISTRY_TEST_LOCK.lock().await;
    crate::drivers::registry::register_driver(sqlite::SqliteDriver::new()).await;
    let _fixture = Fixture::new().await;
    let config = AppConfig::default();
    let plain = json!({"connection_id":"fx-sqlite","table_name":"users"});
    let chosen = json!({"connection_id":"fx-sqlite","table_name":"users","schema":"main"});
    // when
    let baseline = payload(&call_legacy(&config, "describe_table", plain).await.unwrap());
    let with_schema = payload(
        &call_legacy(&config, "describe_table", chosen)
            .await
            .unwrap(),
    );
    // then
    assert_eq!(with_schema["table"], "users");
    let names: Vec<&str> = with_schema["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|column| column["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["id", "name", "email"]);
    assert_eq!(with_schema["effective"]["schema"], "main");
    assert!(baseline.get("effective").is_none());
}

#[tokio::test]
async fn 스키마를_주면_list_tables는_테이블과_유효_데이터베이스와_스키마를_같은_본문에_담는다() {
    // given
    let _guard = crate::drivers::registry::REGISTRY_TEST_LOCK.lock().await;
    crate::drivers::registry::register_driver(sqlite::SqliteDriver::new()).await;
    let _fixture = Fixture::new().await;
    let config = AppConfig::default();
    let plain = json!({"connection_id":"fx-sqlite"});
    let chosen = json!({"connection_id":"fx-sqlite","schema":"main"});
    // when
    let baseline = payload(&call_legacy(&config, "list_tables", plain).await.unwrap());
    let with_schema = payload(&call_legacy(&config, "list_tables", chosen).await.unwrap());
    // then
    assert!(baseline.is_array());
    assert_eq!(with_schema["tables"], baseline);
    assert_eq!(with_schema["effective"]["schema"], "main");
}

#[tokio::test]
async fn toon으로_받아도_스키마를_주면_본문에_행과_유효_스키마가_함께_있다() {
    // given
    let _guard = crate::drivers::registry::REGISTRY_TEST_LOCK.lock().await;
    crate::drivers::registry::register_driver(sqlite::SqliteDriver::new()).await;
    let _fixture = Fixture::new().await;
    let config = AppConfig::default();
    let chosen = json!({"connection_id":"fx-sqlite","query":"select id from users order by id","schema":"main","output_format":"toon"});
    // when
    let result = call_legacy(&config, "run_query", chosen).await.unwrap();
    // then
    assert!(result.get("structuredContent").is_none());
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("rows"), "{text}");
    assert!(text.contains("effective"), "{text}");
    assert!(text.contains("main"), "{text}");
}

#[tokio::test]
async fn 알_수_없는_데이터베이스는_계속_잘못된_인자로_거부한다() {
    // given
    let _guard = crate::drivers::registry::REGISTRY_TEST_LOCK.lock().await;
    crate::drivers::registry::register_driver(sqlite::SqliteDriver::new()).await;
    let _fixture = Fixture::new().await;
    let config = AppConfig::default();
    // when
    let unknown_database = call_legacy(
        &config,
        "run_query",
        json!({"connection_id":"fx-sqlite","query":"select 1","database":"missing"}),
    )
    .await
    .unwrap_err();
    // then
    assert_eq!(unknown_database.code, -32602);
    assert!(unknown_database.message.starts_with("INVALID_ARGUMENT"));
}

#[test]
fn 구형_서버의_오류는_같은_바이트로_전달한다() {
    // given
    let error = JsonRpcError {
        code: -32000,
        message: "Connection not found: x".to_string(),
        data: None,
    };
    let expected = serde_json::to_string(&JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(7)),
        result: None,
        error: Some(error),
    })
    .unwrap();
    // when
    let actual = crate::services::transport::legacy_error_frame(
        json!(7),
        crate::services::transport::LegacyFailure {
            code: -32000,
            message: "Connection not found: x".to_string(),
            data: None,
        },
    )
    .to_string();
    // then
    assert_eq!(actual, expected);
}
