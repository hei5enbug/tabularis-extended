use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    process::{Command, Stdio},
};
use tokio_postgres::{Client, NoTls};

const DISPLAY_BYTES: usize = 32 * 1024 * 1024;
const EXPORT_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXPORT_BYTES: usize = 1024 * 1024 * 1024;
const WARNING: &str = "PRIMARY_KEYLESS_EXPORT_USES_DISPLAY_LIMITS";

struct ExportFixture {
    port: u16,
    schema: String,
    client: Client,
}
impl ExportFixture {
    async fn new(port: u16, primary_key: bool, count: i32, label_bytes: i32) -> Self {
        let config = format!(
            "host=127.0.0.1 port={port} user=postgres dbname=spatial_fixture sslmode=disable"
        );
        let (client, connection) = tokio_postgres::connect(&config, NoTls).await.unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let schema = format!("p2_{}", uuid::Uuid::new_v4().simple());
        let primary = if primary_key { "PRIMARY KEY" } else { "" };
        client.batch_execute(&format!("CREATE SCHEMA {schema}; CREATE TABLE {schema}.points(id integer {primary},label text,g geometry(Point,4326))")).await.unwrap();
        client.execute(&format!("INSERT INTO {schema}.points SELECT i,repeat('a',$2),ST_SetSRID(ST_MakePoint((i%300)-150,0),4326) FROM generate_series(1,$1) i"),&[&count,&label_bytes]).await.unwrap();
        Self {
            port,
            schema,
            client,
        }
    }
    fn input(&self) -> Value {
        json!({"table":{"database":"spatial_fixture","schema":self.schema,"table":"points"},"column":"g",
            "viewport":{"west":-180,"south":-90,"east":180,"north":90,"world":true},"page_size":1000})
    }
    fn export_input(&self, bytes: usize) -> Value {
        let mut input = self.input();
        input["export_limit_bytes"] = json!(bytes);
        input["limits"] =
            json!({"max_features":1_000_000,"max_coordinates":10_000_000,"max_bytes":bytes});
        input
    }
    async fn call(&self, input: Value) -> Value {
        let request = json!({"jsonrpc":"2.0","id":uuid::Uuid::new_v4().to_string(),"method":"spatial_query_table","params":{
            "params":{"driver":"postgresql-plus","host":"127.0.0.1","port":self.port,"database":"spatial_fixture","username":"postgres","ssl_mode":"disable"},
            "driver_context":{"protocol_version":1,"connection_id":self.schema,"request_id":uuid::Uuid::new_v4().to_string(),"deadline_ms":120000,"read_only":true},"input":input}});
        postgresql_plugin::rpc::handle_line(&request.to_string())
            .await
            .unwrap()
    }
    async fn pages(&self, mut input: Value) -> PageSummary {
        let mut summary = PageSummary::default();
        for _ in 0..1500 {
            let response = self.call(input.clone()).await;
            let result = response
                .get("result")
                .unwrap_or_else(|| panic!("spatial export returned {}", response["error"]));
            summary.record(result);
            if result["page"]["has_more"] == false {
                return summary;
            }
            input["next_token"] = result["page"]["next_token"].clone();
        }
        panic!("spatial export exceeded the bounded page count");
    }
    async fn compared_pages(&self, export: Value) -> (PageSummary, PageSummary, i64) {
        let displayed = self.pages(self.input()).await;
        let exported = self.pages(export).await;
        let cleaned = self.cleanup().await;
        (displayed, exported, cleaned)
    }
    async fn pages_and_cleanup(&self, input: Value) -> (PageSummary, i64) {
        let result = self.pages(input).await;
        let cleaned = self.cleanup().await;
        (result, cleaned)
    }
    async fn mismatches_then_resume(&self, mut original: Value) -> (Vec<Value>, Value, Value, i64) {
        original["page_size"] = json!(1);
        let first = self.call(original.clone()).await;
        original["next_token"] = first["result"]["page"]["next_token"].clone();
        let mut display = original.clone();
        display
            .as_object_mut()
            .unwrap()
            .remove("export_limit_bytes");
        display.as_object_mut().unwrap().remove("limits");
        let mut changed_limit = original.clone();
        changed_limit["export_limit_bytes"] = json!(MAX_EXPORT_BYTES - 1);
        let mut changed_features = original.clone();
        changed_features["limits"]["max_features"] = json!(999_999);
        let mut changed_coordinates = original.clone();
        changed_coordinates["limits"]["max_coordinates"] = json!(9_999_999);
        let mut changed_bytes = original.clone();
        changed_bytes["limits"]["max_bytes"] = json!(EXPORT_BYTES - 1);
        let mut rejected = Vec::new();
        for input in [
            display,
            changed_limit,
            changed_features,
            changed_coordinates,
            changed_bytes,
        ] {
            rejected.push(self.call(input).await);
        }
        let resumed = self.call(original).await;
        let cleaned = self.cleanup().await;
        (rejected, first, resumed, cleaned)
    }
    async fn cleanup(&self) -> i64 {
        postgresql_plugin::handlers::spatial::table::invalidate_connection(&self.schema);
        self.client
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {} CASCADE", self.schema))
            .await
            .unwrap();
        self.client
            .query_one(
                "SELECT count(*) FROM pg_namespace WHERE nspname=$1",
                &[&self.schema],
            )
            .await
            .unwrap()
            .get(0)
    }
}
impl Drop for ExportFixture {
    fn drop(&mut self) {
        postgresql_plugin::handlers::spatial::table::invalidate_connection(&self.schema);
        let _ = Command::new("psql")
            .args([
                "-X",
                "-w",
                "-h",
                "127.0.0.1",
                "-p",
                &self.port.to_string(),
                "-U",
                "postgres",
                "-d",
                "spatial_fixture",
                "-c",
                &format!("DROP SCHEMA IF EXISTS {} CASCADE", self.schema),
            ])
            .env("PGPASSFILE", "/dev/null")
            .env("PGSERVICEFILE", "/dev/null")
            .env("PGSSLMODE", "disable")
            .env("PGGSSENCMODE", "disable")
            .env_remove("PGSERVICE")
            .env_remove("PGPASSWORD")
            .env_remove("PGOPTIONS")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct PageSummary {
    rows: usize,
    ids: BTreeSet<i64>,
    feature_ids: BTreeSet<String>,
    page_sizes: Vec<usize>,
    max_page_bytes: usize,
    total_bytes: usize,
    max_page_coordinates: usize,
    truncated: bool,
    layer_limit: bool,
    all_warned: bool,
    any_warned: bool,
    null_identities: bool,
    snapshot_ids: BTreeSet<String>,
    resumes: BTreeSet<String>,
    errors: usize,
    label_lengths: BTreeSet<usize>,
    reference_integrity: bool,
}
impl Default for PageSummary {
    fn default() -> Self {
        Self {
            rows: 0,
            ids: BTreeSet::new(),
            feature_ids: BTreeSet::new(),
            page_sizes: Vec::new(),
            max_page_bytes: 0,
            total_bytes: 0,
            max_page_coordinates: 0,
            truncated: false,
            layer_limit: false,
            all_warned: true,
            any_warned: false,
            null_identities: true,
            snapshot_ids: BTreeSet::new(),
            resumes: BTreeSet::new(),
            errors: 0,
            label_lengths: BTreeSet::new(),
            reference_integrity: true,
        }
    }
}
impl PageSummary {
    fn record(&mut self, page: &Value) {
        let features = page["features"].as_array().unwrap();
        self.rows += features.len();
        self.page_sizes.push(features.len());
        let bytes = serde_json::to_vec(page).unwrap().len();
        self.max_page_bytes = self.max_page_bytes.max(bytes);
        self.total_bytes += bytes;
        let coordinates = features
            .iter()
            .map(|feature| {
                postgresql_plugin::handlers::spatial::types::positions(&feature["geometry"])
                    .unwrap()
            })
            .sum();
        self.max_page_coordinates = self.max_page_coordinates.max(coordinates);
        for feature in features {
            self.ids
                .insert(feature["properties"]["id"].as_i64().unwrap());
            self.feature_ids
                .insert(feature["id"].as_str().unwrap().to_string());
            self.label_lengths
                .insert(feature["properties"]["label"].as_str().unwrap().len());
        }
        let references = page["row_references"].as_array().unwrap();
        self.reference_integrity &= references.len() == features.len();
        for (index, reference) in references.iter().enumerate() {
            self.reference_integrity &= features
                .get(index)
                .is_some_and(|feature| feature["id"] == reference["feature_id"]);
            self.null_identities &= reference["identity"].is_null();
            if let Some(snapshot) = reference["snapshot_id"].as_str() {
                self.snapshot_ids.insert(snapshot.to_string());
            }
        }
        self.truncated |= page["limits"]["truncated"] == true;
        self.layer_limit |= page["limits"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "LAYER_LIMIT");
        let warned = page["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning == WARNING);
        self.all_warned &= warned;
        self.any_warned |= warned;
        self.resumes
            .insert(page["page"]["resume_mode"].as_str().unwrap().to_string());
        self.errors += page["feature_errors"].as_array().unwrap().len();
    }
}

#[tokio::test]
async fn pk_export는_표시_상한을_넘는_10001개_점을_빠짐없이_읽는다() {
    for port in super::ports() {
        // given
        let f = ExportFixture::new(port, true, 10001, 0).await;
        let input = f.export_input(EXPORT_BYTES);
        // when
        let (displayed, exported, cleaned) = f.compared_pages(input).await;
        // then
        assert_eq!(displayed.rows, 10000);
        assert!(displayed.truncated && displayed.layer_limit);
        assert_eq!(exported.rows, 10001);
        assert_eq!(exported.ids, (1..=10001).collect());
        assert_eq!(exported.feature_ids.len(), 10001);
        assert!(exported.reference_integrity && !exported.null_identities);
        assert_eq!(
            exported.page_sizes,
            vec![1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000, 1]
        );
        assert!(
            exported.max_page_bytes <= 8 * 1024 * 1024 && exported.max_page_coordinates <= 100_000
        );
        assert!(!exported.truncated && !exported.any_warned);
        assert_eq!(exported.errors, 0);
        assert_eq!(cleaned, 0);
    }
}

#[tokio::test]
async fn pk_export는_32mib를_넘어도_8mib_페이지로_원본_속성을_읽는다() {
    for port in super::ports() {
        // given
        let f = ExportFixture::new(port, true, 120, 300 * 1024).await;
        let input = f.export_input(EXPORT_BYTES);
        // when
        let (displayed, exported, cleaned) = f.compared_pages(input).await;
        // then
        assert!(displayed.rows < 120 && displayed.truncated && displayed.layer_limit);
        assert!(displayed.total_bytes <= DISPLAY_BYTES);
        assert_eq!(exported.rows, 120);
        assert_eq!(exported.ids, (1..=120).collect());
        assert_eq!(exported.feature_ids.len(), 120);
        assert_eq!(exported.label_lengths, BTreeSet::from([300 * 1024]));
        assert!(exported.reference_integrity && !exported.null_identities);
        assert!(exported.total_bytes > DISPLAY_BYTES && exported.total_bytes <= EXPORT_BYTES);
        assert!(exported.max_page_bytes <= 8 * 1024 * 1024);
        assert!(exported.page_sizes.iter().all(|size| *size <= 1000));
        assert!(!exported.truncated && !exported.any_warned);
        assert_eq!(exported.errors, 0);
        assert_eq!(cleaned, 0);
    }
}

#[tokio::test]
async fn pk없는_export는_1gib_요청에도_10000개_상한과_모든_페이지_경고를_유지한다() {
    for port in super::ports() {
        // given
        let f = ExportFixture::new(port, false, 10001, 0).await;
        let input = f.export_input(MAX_EXPORT_BYTES);
        // when
        let (exported, cleaned) = f.pages_and_cleanup(input).await;
        // then
        assert_eq!(exported.rows, 10000);
        assert_eq!(exported.feature_ids.len(), 10000);
        assert!(exported.truncated && exported.layer_limit && exported.all_warned);
        assert!(exported.null_identities);
        assert!(exported.reference_integrity);
        assert_eq!(exported.snapshot_ids.len(), 1);
        assert!(exported.resumes.contains("materialized"));
        assert!(
            exported.total_bytes <= DISPLAY_BYTES && exported.max_page_bytes <= 8 * 1024 * 1024
        );
        assert_eq!(exported.errors, 0);
        assert_eq!(cleaned, 0);
    }
}

#[tokio::test]
async fn pk없는_export는_큰_속성의_snapshot을_32mib로_제한한다() {
    for port in super::ports() {
        // given
        let f = ExportFixture::new(port, false, 120, 300 * 1024).await;
        let input = f.export_input(MAX_EXPORT_BYTES);
        // when
        let (exported, cleaned) = f.pages_and_cleanup(input).await;
        // then
        assert!(exported.rows > 0 && exported.rows < 120);
        assert_eq!(exported.feature_ids.len(), exported.rows);
        assert!(
            exported.total_bytes <= DISPLAY_BYTES && exported.max_page_bytes <= 8 * 1024 * 1024
        );
        assert!(exported.truncated && exported.layer_limit && exported.all_warned);
        assert_eq!(exported.snapshot_ids.len(), 1);
        assert_eq!(exported.errors, 0);
        assert_eq!(cleaned, 0);
    }
}

#[tokio::test]
async fn pk없는_export가_상한_안에_들면_경고만_있고_잘리지_않는다() {
    for port in super::ports() {
        // given
        let f = ExportFixture::new(port, false, 3, 0).await;
        let mut input = f.export_input(MAX_EXPORT_BYTES);
        input["page_size"] = json!(1);
        // when
        let (exported, cleaned) = f.pages_and_cleanup(input).await;
        // then
        assert_eq!(exported.rows, 3);
        assert_eq!(exported.ids, (1..=3).collect());
        assert_eq!(exported.page_sizes, vec![1, 1, 1]);
        assert!(exported.all_warned && !exported.truncated && !exported.layer_limit);
        assert_eq!(exported.snapshot_ids.len(), 1);
        assert_eq!(exported.errors, 0);
        assert_eq!(cleaned, 0);
    }
}

async fn token_scenario(primary_key: bool) -> Vec<(Vec<Value>, Value, Value, i64)> {
    let mut results = Vec::new();
    for port in super::ports() {
        let f = ExportFixture::new(port, primary_key, 3, 0).await;
        let input = f.export_input(EXPORT_BYTES);
        results.push(f.mismatches_then_resume(input).await);
    }
    results
}

#[tokio::test]
async fn pk_export_token은_변경된_모드와_총량을_거부해도_원래_재개에_쓸_수_있다() {
    // given
    let primary_key = true;
    // when
    let results = token_scenario(primary_key).await;
    // then
    assert!(
        results.iter().all(|(rejected, _, _, _)| rejected
            .iter()
            .all(|response| response["error"]["data"]["code"] == "INVALID_PAGE_TOKEN")),
        "{results:?}"
    );
    assert!(results
        .iter()
        .all(
            |(_, first, resumed, _)| first["result"]["features"][0]["properties"]["id"] == 1
                && resumed["result"]["features"][0]["properties"]["id"] == 2
        ));
    assert!(results
        .iter()
        .all(|(_, _, resumed, _)| resumed["result"]["limits"]["truncated"] == false));
    assert!(results.iter().all(|(_, _, _, cleaned)| *cleaned == 0));
}

#[tokio::test]
async fn pk없는_export_token은_clamp된_총량이_같아도_원래_요청만_허용한다() {
    // given
    let primary_key = false;
    // when
    let results = token_scenario(primary_key).await;
    // then
    assert!(
        results.iter().all(|(rejected, _, _, _)| rejected
            .iter()
            .all(|response| response["error"]["data"]["code"] == "INVALID_PAGE_TOKEN")),
        "{results:?}"
    );
    assert!(results
        .iter()
        .all(
            |(_, first, resumed, _)| first["result"]["features"][0]["properties"]["id"] == 1
                && resumed["result"]["features"][0]["properties"]["id"] == 2
        ));
    assert!(results
        .iter()
        .all(
            |(_, _, resumed, _)| resumed["result"]["warnings"] == json!([WARNING])
                && resumed["result"]["limits"]["truncated"] == false
        ));
    assert!(results.iter().all(|(_, _, _, cleaned)| *cleaned == 0));
}

async fn rejected_before_connection(inputs: Vec<Value>) -> Vec<Value> {
    let mut results = Vec::new();
    for input in inputs {
        let request = json!({"jsonrpc":"2.0","id":uuid::Uuid::new_v4().to_string(),"method":"spatial_query_table","params":{
            "params":{"driver":"postgresql-plus","host":"127.0.0.1","port":1,"database":"spatial_fixture","username":"postgres","ssl_mode":"disable"},
            "driver_context":{"protocol_version":1,"connection_id":format!("p2_{}",uuid::Uuid::new_v4().simple()),"request_id":uuid::Uuid::new_v4().to_string(),"deadline_ms":5000,"read_only":true},"input":input}});
        results.push(
            postgresql_plugin::rpc::handle_line(&request.to_string())
                .await
                .unwrap(),
        );
    }
    results
}

#[tokio::test]
async fn 잘못된_export와_display_상한은_연결을_시도하기_전에_거부한다() {
    // given
    let base = json!({"table":{"database":"spatial_fixture","schema":null,"table":"points"},"column":"g",
        "viewport":{"west":-180,"south":-90,"east":180,"north":90,"world":true},"export_limit_bytes":MAX_EXPORT_BYTES,
        "limits":{"max_features":1_000_000,"max_coordinates":10_000_000,"max_bytes":EXPORT_BYTES}});
    let mut inputs = Vec::new();
    for limit in [0, MAX_EXPORT_BYTES + 1] {
        let mut input = base.clone();
        input["export_limit_bytes"] = json!(limit);
        inputs.push(input);
    }
    for (field, value) in [
        ("max_features", 1_000_001),
        ("max_coordinates", 10_000_001),
        ("max_bytes", MAX_EXPORT_BYTES + 1),
    ] {
        let mut input = base.clone();
        input["limits"][field] = json!(value);
        inputs.push(input);
    }
    let mut too_many = base.clone();
    too_many
        .as_object_mut()
        .unwrap()
        .remove("export_limit_bytes");
    inputs.push(too_many);
    let mut too_many_coordinates = base.clone();
    too_many_coordinates
        .as_object_mut()
        .unwrap()
        .remove("export_limit_bytes");
    too_many_coordinates["limits"] = json!({"max_coordinates":250_001});
    inputs.push(too_many_coordinates);
    let mut too_many_bytes = base.clone();
    too_many_bytes
        .as_object_mut()
        .unwrap()
        .remove("export_limit_bytes");
    too_many_bytes["limits"] = json!({"max_bytes":DISPLAY_BYTES+1});
    inputs.push(too_many_bytes);
    for page in [0, 1001] {
        let mut input = base.clone();
        input["page_size"] = json!(page);
        inputs.push(input);
    }
    let mut exclusive = base;
    exclusive["keyset"] = json!({"columns":["id"],"values":[1]});
    exclusive["next_token"] = json!("cursor");
    inputs.push(exclusive);
    // when
    let responses = rejected_before_connection(inputs).await;
    // then
    assert_eq!(responses.len(), 11);
    assert!(
        responses
            .iter()
            .all(|response| response["error"]["data"]["code"] == "INVALID_ARGUMENT"),
        "{responses:?}"
    );
    assert!(responses
        .iter()
        .all(|response| response["error"]["data"]["outcome"] == "not_applied"));
    assert!(responses
        .iter()
        .all(|response| response.get("result").is_none()));
}
