use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
use tokio_postgres::{Client, NoTls};

#[path = "service_live.rs"]
mod service;

#[path = "export_live.rs"]
mod export;

fn ports() -> [u16; 2] {
    let configured = std::env::var("TABULARIS_P1_POSTGIS_PORTS")
        .expect("두 필수 PostGIS 포트를 TABULARIS_P1_POSTGIS_PORTS에 지정해야 한다");
    let values = configured
        .split(',')
        .map(|value| {
            value
                .trim()
                .parse::<u16>()
                .expect("PostGIS 포트는 정수여야 한다")
        })
        .collect::<Vec<_>>();
    let ports: [u16; 2] = values
        .try_into()
        .expect("서로 다른 두 PostGIS 포트가 필요하다");
    assert!(
        ports[0] != 0 && ports[1] != 0 && ports[0] != ports[1],
        "서로 다른 유효한 두 포트가 필요하다"
    );
    ports
}

struct Fixture {
    port: u16,
    schema: String,
    client: Client,
    params: Value,
}
impl Fixture {
    async fn new(port: u16) -> Self {
        Self::new_in_database(port, "spatial_fixture", "public").await
    }
    async fn new_in_database(port: u16, database: &str, extension_schema: &str) -> Self {
        let config =
            format!("host=127.0.0.1 port={port} user=postgres dbname={database} sslmode=disable");
        let (client, connection) = tokio_postgres::connect(&config, NoTls)
            .await
            .expect("필수 PostGIS fixture에 연결해야 한다");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .batch_execute(&format!(
                "SET search_path TO {}, public",
                postgresql_plugin::utils::identifiers::quote_identifier(extension_schema)
            ))
            .await
            .unwrap();
        let schema = format!("p1_{}", uuid::Uuid::new_v4().simple());
        let sql = format!(
            r#"
            CREATE SCHEMA {schema};
            CREATE TABLE {schema}.projected(tenant integer,id integer,label text,g geometry(PointZ,3857),PRIMARY KEY(tenant,id));
            INSERT INTO {schema}.projected VALUES (1,1,'첫째',ST_GeomFromText('POINT Z(1113194.9079327357 0 7)',3857)),
                (1,2,'둘째',ST_GeomFromText('POINT Z(2226389.8158654715 0 8)',3857)),
                (2,1,'셋째',ST_GeomFromText('POINT Z(3339584.723798207 0 9)',3857));
            CREATE TABLE {schema}.no_pk(label text,g geometry(Point,4326));
            INSERT INTO {schema}.no_pk SELECT '원본'||i,ST_SetSRID(ST_MakePoint(i,0),4326) FROM generate_series(1,3) i;
            CREATE TABLE {schema}.dateline(id integer PRIMARY KEY,g geometry(Point,4326));
            INSERT INTO {schema}.dateline VALUES (1,ST_SetSRID(ST_MakePoint(179,0),4326)),(2,ST_SetSRID(ST_MakePoint(-179,0),4326)),
                (3,ST_SetSRID(ST_MakePoint(0,0),4326)),(4,ST_SetSRID(ST_MakePoint(180,0),4326)),(5,ST_SetSRID(ST_MakePoint(-180,0),4326));
            CREATE TABLE {schema}.mixed(id integer PRIMARY KEY,g geometry);
            INSERT INTO {schema}.mixed VALUES (1,ST_MakePoint(1,0)),(2,ST_SetSRID(ST_MakePoint(1,0),4326)),
                (3,ST_SetSRID(ST_MakePoint(111319.49079327357,0),3857));
            CREATE TABLE {schema}.geographic(id integer PRIMARY KEY,g geography(Point,4326));
            INSERT INTO {schema}.geographic VALUES (1,ST_SetSRID(ST_MakePoint(179,0),4326)::geography),
                (2,ST_SetSRID(ST_MakePoint(-179,0),4326)::geography),(3,ST_SetSRID(ST_MakePoint(0,0),4326)::geography);
            CREATE INDEX ON {schema}.geographic USING gist(g);
            CREATE DOMAIN {schema}.geom_domain AS geometry(PointZ,3857);
            CREATE TABLE {schema}.domain_table(id integer PRIMARY KEY,g {schema}.geom_domain);
            INSERT INTO {schema}.domain_table VALUES(1,NULL),(2,ST_GeomFromText('POINT Z(0 0 4)',3857));
            CREATE TABLE {schema}.writes(id integer PRIMARY KEY,counter integer NOT NULL);
            INSERT INTO {schema}.writes VALUES(1,0);
            CREATE FUNCTION {schema}.counter_shape() RETURNS geometry LANGUAGE plpgsql AS $fn$
                BEGIN UPDATE {schema}.writes SET counter=counter+1 WHERE id=1;
                RETURN ST_SetSRID(ST_MakePoint(10,0),4326); END $fn$;
            CREATE FUNCTION {schema}.delayed_write() RETURNS integer LANGUAGE plpgsql AS $fn$
                BEGIN UPDATE {schema}.writes SET counter=counter+1 WHERE id=1;
                PERFORM pg_sleep(20); RETURN 1; END $fn$;
        "#
        );
        client.batch_execute(&sql).await.unwrap();
        let params = json!({"driver":"postgresql-plus","host":"127.0.0.1","port":port,"database":database,"username":"postgres","ssl_mode":"disable"});
        Self {
            port,
            schema,
            client,
            params,
        }
    }
    fn request(&self, method: &str, input: Value) -> Value {
        json!({"jsonrpc":"2.0","id":uuid::Uuid::new_v4().to_string(),"method":method,"params":{
            "params":self.params,"driver_context":{"protocol_version":1,"connection_id":self.schema,
            "request_id":uuid::Uuid::new_v4().to_string(),"deadline_ms":5000,"read_only":true},"input":input}})
    }
    async fn send(&self, request: Value) -> Value {
        postgresql_plugin::rpc::handle_line(&request.to_string())
            .await
            .unwrap()
    }
    async fn send_all(&self, requests: Vec<Value>) -> Vec<Value> {
        let mut responses = Vec::new();
        for request in requests {
            responses.push(self.send(request).await);
        }
        responses
    }
    async fn scan_count(&self, tables: &[String]) -> i64 {
        self.client
            .query_one("SELECT pg_stat_force_next_flush()", &[])
            .await
            .unwrap();
        self.client
            .query_one("SELECT pg_stat_clear_snapshot()", &[])
            .await
            .unwrap();
        self.client.query_one("SELECT COALESCE(sum(seq_scan+COALESCE(idx_scan,0)),0)::bigint FROM pg_stat_user_tables WHERE schemaname=$1 AND relname=ANY($2)",&[&self.schema,&tables]).await.unwrap().get(0)
    }
    async fn reject_and_measure_scans(
        &self,
        requests: Vec<Value>,
        tables: &[String],
    ) -> (Vec<Value>, i64) {
        let responses = self.send_all(requests).await;
        let scans = self.scan_count(tables).await;
        (responses, scans)
    }
    async fn call(&self, method: &str, input: Value) -> Value {
        self.send(self.request(method, input)).await
    }
    async fn exact(&self, text: String) -> Value {
        self.call("execute_query_exact", json!({"text":text,"parameters":[]}))
            .await
    }
    fn table(&self, name: &str) -> Value {
        json!({"database":self.params["database"],"schema":self.schema,"table":name})
    }
    fn table_input(&self, name: &str, page: usize) -> Value {
        json!({"table":self.table(name),"column":"g","viewport":{"west":-180,"south":-80,"east":180,"north":80,"world":true},"page_size":page})
    }
    async fn wrapper(&self, wkt: &str, srid: i32, native: &str) -> Value {
        let sql = format!("SELECT ST_GeomFromText($1,$2)::{native}");
        let row = self.client.query_one(&sql, &[&wkt, &srid]).await.unwrap();
        postgresql_plugin::extract::extract_value_checked(&row, 0).unwrap()
    }
    async fn wait_active(&self, tag: &str) -> bool {
        self.wait_active_count(tag, 1).await
    }
    async fn wait_active_count(&self, tag: &str, minimum: i64) -> bool {
        tokio::time::timeout(Duration::from_secs(3),async {
            loop {
                let pattern=format!("%{tag}%");
                let count:i64=self.client.query_one("SELECT count(*) FROM pg_stat_activity WHERE pid<>pg_backend_pid() AND state='active' AND query LIKE $1 AND wait_event='PgSleep'",&[&pattern]).await.unwrap().get(0);
                if count>=minimum {return true;}
                tokio::task::yield_now().await;
            }
        }).await.unwrap_or(false)
    }
    async fn cancel_and_measure(
        &self,
        cancel: Value,
        task: tokio::task::JoinHandle<Value>,
    ) -> (Value, Value, i32) {
        let response = self.send(cancel).await;
        let finished = task.await.unwrap();
        let count = self
            .client
            .query_one(
                &format!("SELECT counter FROM {}.writes WHERE id=1", self.schema),
                &[],
            )
            .await
            .unwrap()
            .get(0);
        (response, finished, count)
    }
    async fn transform_and_measure(&self, wrapper: Value) -> (Value, i32) {
        let response = self
            .call("spatial_transform_values", json!({"values":[wrapper]}))
            .await;
        let count = self
            .client
            .query_one(
                &format!("SELECT counter FROM {}.writes WHERE id=1", self.schema),
                &[],
            )
            .await
            .unwrap()
            .get(0);
        (response, count)
    }
    async fn transform_and_preserved_bytes(
        &self,
        input: Value,
        table: &str,
    ) -> (Value, Vec<Vec<u8>>) {
        let response = self.call("spatial_transform_values", input).await;
        let relation = postgresql_plugin::utils::identifiers::qualified(&self.schema, table);
        let bytes = self
            .client
            .query(
                &format!("SELECT ST_AsEWKB(g,'XDR') FROM {relation} ORDER BY id"),
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect();
        (response, bytes)
    }
    async fn finish_and_measure(&self, request: Value) -> (Value, i32) {
        let response = self.send(request).await;
        let count = self
            .client
            .query_one(
                &format!("SELECT counter FROM {}.writes WHERE id=1", self.schema),
                &[],
            )
            .await
            .unwrap()
            .get(0);
        (response, count)
    }
    async fn serialize_and_commit(
        &self,
        second: Value,
        first: tokio::task::JoinHandle<Value>,
        finish: Value,
    ) -> (Value, Value, Value, i32) {
        let second = self.send(second).await;
        let first = first.await.unwrap();
        let (finished, counter) = self.finish_and_measure(finish).await;
        (first, second, finished, counter)
    }
    async fn cap_and_probe(
        &self,
        request: Value,
        handle: Option<(Value, bool)>,
    ) -> (Value, Value, i64) {
        let result = self.send(request).await;
        let probe = if let Some((handle, read_only)) = handle {
            let mut probe = self.request(
                "execute_query_exact",
                json!({"text":"SELECT 1","parameters":[]}),
            );
            probe["params"]["driver_context"]["session_handle"] = handle;
            probe["params"]["driver_context"]["read_only"] = json!(read_only);
            self.send(probe).await
        } else {
            Value::Null
        };
        let sum = self
            .client
            .query_one(
                &format!("SELECT sum(counter)::bigint FROM {}.writes", self.schema),
                &[],
            )
            .await
            .unwrap()
            .get(0);
        (result, probe, sum)
    }
    async fn capped_reads_and_new_backends(&self) -> Vec<(Value, Value)> {
        let mut results = Vec::new();
        for _ in 0..16 {
            let capped=self.call("execute_query_exact",json!({"text":"SELECT pg_backend_pid(),i FROM generate_series(1,3) i","parameters":[],"max_rows":1})).await;
            let next = self.exact("SELECT pg_backend_pid()".into()).await;
            results.push((capped, next));
        }
        results
    }
    async fn cancel_isolated(
        &self,
        cancel: Value,
        first: tokio::task::JoinHandle<Value>,
        second_cancel: Value,
        second: tokio::task::JoinHandle<Value>,
        tag: &str,
    ) -> (Value, Value, bool, Value) {
        let response = self.send(cancel).await;
        let first = first.await.unwrap();
        let second_active = self.wait_active(tag).await;
        self.send(second_cancel).await;
        let second = second.await.unwrap();
        (response, first, second_active, second)
    }
    async fn send_and_discard(&self, request: Value, handle: Value) -> Value {
        let result = self.send(request).await;
        let mut discard = self.request("service_session_discard", json!({}));
        discard["params"]["driver_context"]["session_handle"] = handle;
        self.send(discard).await;
        result
    }
    async fn terminate_and_measure(
        &self,
        pid: i32,
        task: tokio::task::JoinHandle<Value>,
    ) -> (Value, i32) {
        self.client
            .query_one("SELECT pg_terminate_backend($1)", &[&pid])
            .await
            .unwrap();
        let response = task.await.unwrap();
        let counter = self
            .client
            .query_one(
                &format!("SELECT counter FROM {}.writes WHERE id=1", self.schema),
                &[],
            )
            .await
            .unwrap()
            .get(0);
        (response, counter)
    }
}

struct Plugin {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: mpsc::Receiver<Value>,
}
impl Plugin {
    fn new() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_postgresql-plugin"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                let Ok(value) = serde_json::from_str(&line) else {
                    break;
                };
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            responses,
        }
    }
    fn send(&mut self, request: &Value) {
        writeln!(self.stdin.as_mut().unwrap(), "{request}").unwrap();
        self.stdin.as_mut().unwrap().flush().unwrap();
    }
    fn cancel_and_drain(mut self, cancel: Value, cleanup: Vec<Value>) -> (Value, Duration, bool) {
        let start = Instant::now();
        self.send(&cancel);
        let mut response = Value::Null;
        while start.elapsed() < Duration::from_secs(3) {
            let Ok(next) = self
                .responses
                .recv_timeout(Duration::from_secs(3).saturating_sub(start.elapsed()))
            else {
                break;
            };
            if next["id"] == cancel["id"] {
                response = next;
                break;
            }
        }
        let elapsed = start.elapsed();
        for cancel in cleanup {
            self.send(&cancel);
        }
        self.stdin.take();
        let success = self.child.wait().unwrap().success();
        (response, elapsed, success)
    }
}
impl Drop for Plugin {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = Command::new("psql")
            .args([
                "-h",
                "127.0.0.1",
                "-p",
                &self.port.to_string(),
                "-U",
                "postgres",
                "-d",
                self.params["database"].as_str().unwrap(),
                "-c",
                &format!("DROP SCHEMA IF EXISTS {} CASCADE", self.schema),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[tokio::test]
async fn 원본_ewkb와_null_empty_z_m_zm을_두_postgis에서_보존한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let text="SELECT NULL::geometry,ST_GeomFromText('POINT EMPTY',4326),ST_GeomFromText('POINT Z(1 2 NaN)',4326),ST_GeomFromText('POINT M(1 2 3)',4326),ST_GeomFromText('POINT ZM(1 2 3 4)',4326)".to_string();
        // when
        let response = f.exact(text).await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert!(response["result"]["rows"][0][0].is_null());
        assert_eq!(response["result"]["rows"][0][1]["is_empty"], true);
        assert_eq!(response["result"]["rows"][0][2]["dimensions"], "XYZ");
        assert_eq!(response["result"]["rows"][0][3]["dimensions"], "XYM");
        assert_eq!(response["result"]["rows"][0][4]["dimensions"], "XYZM");
        assert_eq!(
            response["result"]["rows"][0][2]["value"],
            "AQEAAKDmEAAAAAAAAAAA8D8AAAAAAAAAQAAAAAAAAPh/"
        );
    }
}

#[tokio::test]
async fn 저장된_ewkb만_변환하고_3857의_독립_기대좌표와_차원_경고를_반환한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let wrapper = f
            .wrapper("POINT Z(1113194.9079327357 0 NaN)", 3857, "geometry")
            .await;
        let before = wrapper.clone();
        // when
        let response = f
            .call("spatial_transform_values", json!({"values":[wrapper]}))
            .await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert!(
            (response["result"]["geometries"][0]["coordinates"][0]
                .as_f64()
                .unwrap()
                - 10.0)
                .abs()
                < 1e-8
        );
        assert_eq!(response["result"]["geometries"][0]["coordinates"][1], 0.0);
        assert_eq!(
            response["result"]["warnings"][0],
            "DIMENSIONS_REDUCED_TO_XY"
        );
        assert_eq!(before["srid"], 3857);
        assert_eq!(before["dimensions"], "XYZ");
    }
}

#[tokio::test]
async fn srid_없는_원본은_명시적인_좌표계로만_표시_변환한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let wrapper = f
            .wrapper("POINT(1113194.9079327357 0)", 0, "geometry")
            .await;
        // when
        let response = f
            .call(
                "spatial_transform_values",
                json!({"values":[wrapper],"source_srid":3857}),
            )
            .await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert!(
            (response["result"]["geometries"][0]["coordinates"][0]
                .as_f64()
                .unwrap()
                - 10.0)
                .abs()
                < 1e-8
        );
    }
}

#[tokio::test]
async fn 알_수_없는_srid와_알려진_srid의_상충_override를_명확히_거부한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let unknown = f.wrapper("POINT(1 2)", 0, "geometry").await;
        let known = f.wrapper("POINT(1 2)", 4326, "geometry").await;
        let requests = [
            f.request("spatial_transform_values", json!({"values":[unknown]})),
            f.request(
                "spatial_transform_values",
                json!({"values":[known],"source_srid":3857}),
            ),
        ];
        // when
        let responses =
            futures_util::future::join_all(requests.into_iter().map(|r| f.send(r))).await;
        // then
        assert_eq!(responses[0]["error"]["data"]["code"], "SRID_REQUIRED");
        assert_eq!(responses[1]["error"]["data"]["code"], "INVALID_ARGUMENT");
    }
}

#[tokio::test]
async fn 공간_domain의_oid와_typmod_pk_열_순서를_catalog에서_반환한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let input = json!({"table":f.table("domain_table")});
        // when
        let response = f.call("spatial_columns", input).await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["columns"][0]["name"], "g");
        assert_eq!(response["result"]["columns"][0]["column_index"], 1);
        assert_eq!(response["result"]["columns"][0]["srid"], 3857);
        assert_eq!(response["result"]["columns"][0]["dimensions"], "XYZ");
        assert_ne!(
            response["result"]["columns"][0]["type_oid"],
            response["result"]["columns"][0]["base_oid"]
        );
        assert_eq!(
            response["result"]["columns"][0]["primary_key"],
            json!(["id"])
        );
    }
}

#[tokio::test]
async fn 삭제된_열을_제외한_catalog_ordinal과_재정렬된_결과_ordinal을_구별한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        f.client.batch_execute(&format!("CREATE TABLE {}.dropped(id integer PRIMARY KEY,removed text,g geometry(Point,4326)); ALTER TABLE {}.dropped DROP COLUMN removed; INSERT INTO {}.dropped VALUES(1,NULL)",f.schema,f.schema,f.schema)).await.unwrap();
        let requests = vec![
            f.request("spatial_columns", json!({"table":f.table("dropped")})),
            f.request(
                "execute_query_exact",
                json!({"text":format!("SELECT g,id FROM {}.dropped",f.schema),"parameters":[]}),
            ),
        ];
        // when
        let responses = f.send_all(requests).await;
        // then
        assert!(responses[0].get("error").is_none(), "{}", responses[0]);
        assert!(responses[1].get("error").is_none(), "{}", responses[1]);
        assert_eq!(responses[0]["result"]["columns"][0]["column_index"], 1);
        assert_eq!(
            responses[1]["result"]["column_metadata"][0]["column_index"],
            0
        );
        assert_eq!(
            responses[1]["result"]["column_metadata"][0]["native_type"],
            "geometry"
        );
    }
}

#[tokio::test]
async fn geometry와_동명인_사용자_타입은_공간값으로_추정하지_않는다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        f.client
            .batch_execute(&format!(
                "CREATE TYPE {}.geometry AS ENUM('opaque')",
                f.schema
            ))
            .await
            .unwrap();
        let text = format!("SELECT 'opaque'::{}.geometry AS g", f.schema);
        let requests = vec![
            f.request("execute_query_exact", json!({"text":text,"parameters":[]})),
            json!({"id":113,"method":"execute_query","params":{"params":f.params,"query":text}}),
        ];
        // when
        let responses = f.send_all(requests).await;
        // then
        assert!(responses[0].get("error").is_none(), "{}", responses[0]);
        assert!(responses[1].get("error").is_none(), "{}", responses[1]);
        assert_eq!(
            responses[0]["result"]["column_metadata"][0]["native_type"],
            Value::Null
        );
        assert_eq!(responses[0]["result"]["rows"], json!([["opaque"]]));
        assert_eq!(responses[1]["result"]["rows"], json!([["opaque"]]));
    }
}

#[tokio::test]
async fn xdr_원본의_hole_multi_collection과_empty는_독립_geojson과_방향_oracle에_맞는다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let wkts=["POINT Z(1 2 3)","LINESTRING(0 0,1 1)",
            "POLYGON((0 0,0 4,4 4,4 0,0 0),(1 1,2 1,2 2,1 2,1 1))",
            "MULTIPOLYGON(((0 0,0 1,1 0,0 0)),((2 0,2 1,3 0,2 0)))",
            "GEOMETRYCOLLECTION(POINT(1 2),LINESTRING(170 0,-170 0),POLYGON((0 0,0 4,4 4,4 0,0 0)))","POINT EMPTY"];
        f.client
            .batch_execute(&format!(
                "CREATE TABLE {}.shapes(id integer PRIMARY KEY,g geometry)",
                f.schema
            ))
            .await
            .unwrap();
        for (index, wkt) in wkts.iter().enumerate() {
            f.client
                .execute(
                    &format!(
                        "INSERT INTO {}.shapes SELECT $1,ST_GeomFromText($2,4326)",
                        f.schema
                    ),
                    &[&(index as i32), wkt],
                )
                .await
                .unwrap();
        }
        let raw = f
            .client
            .query(
                &format!(
                    "SELECT ST_AsEWKB(g,'XDR') FROM {}.shapes ORDER BY id",
                    f.schema
                ),
                &[],
            )
            .await
            .unwrap();
        let before = raw
            .iter()
            .map(|row| row.get::<_, Vec<u8>>(0))
            .collect::<Vec<_>>();
        let wrappers = before
            .iter()
            .map(|bytes| {
                serde_json::to_value(
                    postgresql_plugin::handlers::spatial::ewkb::SpatialValue::from_bytes(
                        "geometry", bytes,
                    )
                    .unwrap(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let expected = json!([
            {"type":"Point","coordinates":[1,2]},
            {"type":"LineString","coordinates":[[0,0],[1,1]]},
            {"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]],[[1,1],[1,2],[2,2],[2,1],[1,1]]]},
            {"type":"MultiPolygon","coordinates":[[[[0,0],[1,0],[0,1],[0,0]]],[[[2,0],[3,0],[2,1],[2,0]]]]},
            {"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]},{"type":"LineString","coordinates":[[170,0],[-170,0]]},{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]]]}]},
            {"type":"Point","coordinates":[]}
        ]);
        // when
        let (response, after) = f
            .transform_and_preserved_bytes(json!({"values":wrappers}), "shapes")
            .await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["geometries"], expected);
        assert_eq!(
            response["result"]["warnings"],
            json!(["DIMENSIONS_REDUCED_TO_XY"])
        );
        assert_eq!(after, before);
        assert_eq!(
            before[0],
            vec![
                0, 160, 0, 0, 1, 0, 0, 16, 230, 63, 240, 0, 0, 0, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0,
                64, 8, 0, 0, 0, 0, 0, 0
            ]
        );
    }
}

#[tokio::test]
async fn 따옴표와_sql_기호가_있는_테이블과_열도_식별자로만_조회한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let table = "지점 \"table';--";
        let column = "g \"shape';--";
        let relation = postgresql_plugin::utils::identifiers::qualified(&f.schema, table);
        let quoted = postgresql_plugin::utils::identifiers::quote_identifier(column);
        f.client.batch_execute(&format!("CREATE TABLE {relation}(id integer PRIMARY KEY,{quoted} geometry(Point,4326)); INSERT INTO {relation} VALUES(1,ST_SetSRID(ST_MakePoint(1,2),4326))")).await.unwrap();
        let mut input = f.table_input(table, 1);
        input["column"] = json!(column);
        let requests = vec![
            f.request("spatial_columns", json!({"table":f.table(table)})),
            f.request("spatial_query_table", input),
        ];
        // when
        let responses = f.send_all(requests).await;
        // then
        assert!(responses[0].get("error").is_none(), "{}", responses[0]);
        assert!(responses[1].get("error").is_none(), "{}", responses[1]);
        assert_eq!(responses[0]["result"]["columns"][0]["name"], column);
        assert_eq!(
            responses[1]["result"]["features"][0]["geometry"],
            json!({"type":"Point","coordinates":[1,2]})
        );
        assert_eq!(
            responses[1]["result"]["row_references"][0]["identity"],
            json!({"columns":["id"],"values":[1]})
        );
    }
}

#[tokio::test]
async fn 북극을_포함한_geography_polygon은_오해한_평면_도형으로_표시하지_않는다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let value = f
            .wrapper(
                "POLYGON((-135 80,-45 80,45 80,135 80,-135 80))",
                4326,
                "geography",
            )
            .await;
        // when
        let response = f
            .call("spatial_transform_values", json!({"values":[value]}))
            .await;
        // then
        assert_eq!(response["error"]["data"]["code"], "UNSUPPORTED_TYPE");
    }
}

#[tokio::test]
async fn 날짜변경선을_분할한_polygon도_경계_안의_hole을_보존한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let value=f.wrapper("POLYGON((170 -10,190 -10,190 10,170 10,170 -10),(175 -5,175 5,178 5,178 -5,175 -5))",4326,"geometry").await;
        // when
        let response = f
            .call(
                "spatial_transform_values",
                json!({"values":[value],"longitude_mode":"shortest"}),
            )
            .await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["geometries"][0]["type"], "MultiPolygon");
        assert_eq!(
            response["result"]["geometries"][0]["coordinates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(response["result"]["geometries"][0]["coordinates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|polygon| polygon.as_array().unwrap().len() == 2));
    }
}

#[tokio::test]
async fn nonfinite_xy는_원본을_조회할_수_있지만_지도_표시는_거부한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        f.client.batch_execute(&format!("CREATE TABLE {}.nonfinite(id integer PRIMARY KEY,g geometry); INSERT INTO {}.nonfinite VALUES(1,ST_SetSRID(ST_MakePoint('Infinity'::double precision,2),4326))",f.schema,f.schema)).await.unwrap();
        let before: Vec<u8> = f
            .client
            .query_one(
                &format!("SELECT ST_AsEWKB(g,'XDR') FROM {}.nonfinite", f.schema),
                &[],
            )
            .await
            .unwrap()
            .get(0);
        let original = f
            .exact(format!("SELECT g FROM {}.nonfinite", f.schema))
            .await;
        let wrapper = original["result"]["rows"][0][0].clone();
        // when
        let (response, after) = f
            .transform_and_preserved_bytes(json!({"values":[wrapper]}), "nonfinite")
            .await;
        // then
        assert!(original.get("error").is_none(), "{original}");
        assert_eq!(original["result"]["rows"][0][0]["kind"], "spatial");
        assert_eq!(original["result"]["rows"][0][0]["is_empty"], false);
        assert_eq!(response["error"]["data"]["code"], "INVALID_GEOMETRY");
        assert_eq!(after, vec![before]);
    }
}

#[tokio::test]
async fn tin과_polyhedral_surface도_원본_차원을_보존하고_표시만_거부한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let tin = f
            .wrapper("TIN Z(((0 0 0,1 0 0,0 1 0,0 0 0)))", 4326, "geometry")
            .await;
        let polyhedral = f
            .wrapper(
                "POLYHEDRALSURFACE Z(((0 0 0,1 0 0,0 1 0,0 0 0)))",
                4326,
                "geometry",
            )
            .await;
        // when
        let response = f
            .call(
                "spatial_transform_values",
                json!({"values":[tin.clone(),polyhedral.clone()],"skip_invalid":true}),
            )
            .await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["geometries"], json!([null, null]));
        assert_eq!(
            response["result"]["feature_errors"][0]["code"],
            "UNSUPPORTED_TYPE"
        );
        assert_eq!(
            response["result"]["feature_errors"][1]["code"],
            "UNSUPPORTED_TYPE"
        );
        assert_eq!(tin["dimensions"], "XYZ");
        assert_eq!(polyhedral["dimensions"], "XYZ");
    }
}

#[tokio::test]
async fn null_공간열과_중복_열_이름도_ordinal_메타데이터로_구별한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let text = format!(
            "SELECT id AS same,g AS same FROM {}.domain_table WHERE id=1",
            f.schema
        );
        // when
        let response = f.exact(text).await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["columns"], json!(["same", "same"]));
        assert_eq!(response["result"]["column_metadata"][1]["column_index"], 1);
        assert_eq!(
            response["result"]["column_metadata"][1]["native_type"],
            "geometry"
        );
        assert!(response["result"]["rows"][0][1].is_null());
    }
}

#[tokio::test]
async fn 원본_sql의_limit을_재작성하지_않고_완성된_행_단위로_제한한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let input = json!({"text":"SELECT i FROM generate_series(1,10) i LIMIT 3 OFFSET 2","parameters":[],"max_rows":2});
        // when
        let response = f.call("execute_query_exact", input).await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["rows"], json!([[3], [4]]));
        assert_eq!(response["result"]["limits"]["truncated"], true);
        assert_eq!(
            response["result"]["limits"]["reasons"],
            json!(["ROW_LIMIT"])
        );
    }
}

#[tokio::test]
async fn 값_매개변수는_원문_달러_인덱스에_타입을_맞춰_바인딩한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let input = json!({"text":"SELECT $1::integer,$2::text,$3::boolean,$4::integer","parameters":[
            {"name":"$1","value":42},{"name":"$2","value":"'; DROP TABLE writes; --"},{"name":"$3","value":true},{"name":"$4","value":null}]});
        // when
        let response = f.call("execute_query_exact", input).await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(
            response["result"]["rows"],
            json!([[42, "'; DROP TABLE writes; --", true, null]])
        );
    }
}

#[tokio::test]
async fn 읽기_전용_트랜잭션은_cte와_함수의_쓰기를_db에서_거부한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let texts=[format!("WITH written AS (UPDATE {}.writes SET counter=1 RETURNING *) SELECT * FROM written",f.schema),format!("SELECT {}.counter_shape()",f.schema)];
        // when
        let responses =
            futures_util::future::join_all(texts.into_iter().map(|text| f.exact(text))).await;
        // then
        assert_eq!(
            responses[0]["error"]["data"]["code"], "WRITE_NOT_ALLOWED",
            "{}",
            responses[0]
        );
        assert_eq!(
            responses[1]["error"]["data"]["code"], "WRITE_NOT_ALLOWED",
            "{}",
            responses[1]
        );
        assert_eq!(responses[1]["error"]["data"]["outcome"], "not_applied");
    }
}

#[tokio::test]
async fn 복합_pk_keyset은_누락과_중복_없이_다음_행을_반환한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut input = f.table_input("projected", 1);
        let first = f.call("spatial_query_table", input.clone()).await;
        input["next_token"] = first["result"]["page"]["next_token"].clone();
        // when
        let second = f.call("spatial_query_table", input).await;
        // then
        assert!(first.get("error").is_none(), "{first}");
        assert!(second.get("error").is_none(), "{second}");
        assert_eq!(
            first["result"]["row_references"][0]["identity"]["values"],
            json!([1, 1])
        );
        assert_eq!(
            second["result"]["row_references"][0]["identity"]["values"],
            json!([1, 2])
        );
        assert_ne!(
            first["result"]["features"][0]["id"],
            second["result"]["features"][0]["id"]
        );
        assert_eq!(second["result"]["limits"]["truncated"], false);
    }
}

#[tokio::test]
async fn 지원하지_않는_pk_타입은_행_쿼리와_커서_생성_전에_거부한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        f.client.batch_execute(&format!("CREATE TYPE {}.pk_pair AS (a integer,b integer); CREATE DOMAIN {}.json_key AS jsonb",f.schema,f.schema)).await.unwrap();
        let cases = [
            ("key_json", "jsonb", "'{\"key\":1}'::jsonb".to_string()),
            ("key_array", "integer[]", "ARRAY[1]".to_string()),
            ("key_binary", "bytea", "decode('01','hex')".to_string()),
            (
                "key_record",
                "pk_pair",
                format!("ROW(1,2)::{}.pk_pair", f.schema),
            ),
            (
                "key_spatial",
                "geometry(Point,4326)",
                "ST_SetSRID(ST_MakePoint(1,2),4326)".to_string(),
            ),
            ("key_domain", "json_key", "'{\"key\":1}'::jsonb".to_string()),
        ];
        let mut tables = Vec::new();
        let mut requests = Vec::new();
        for (table, ty, value) in cases {
            let ty = if matches!(ty, "pk_pair" | "json_key") {
                format!("{}.{}", f.schema, ty)
            } else {
                ty.to_string()
            };
            f.client.batch_execute(&format!("CREATE TABLE {}.{table}(id {ty} PRIMARY KEY,g geometry(Point,4326)); INSERT INTO {}.{table} VALUES({value},ST_SetSRID(ST_MakePoint(1,2),4326))",f.schema,f.schema)).await.unwrap();
            tables.push(table.to_string());
            requests.push(f.request("spatial_query_table", f.table_input(table, 1)));
        }
        let before = f.scan_count(&tables).await;
        // when
        let (responses, after) = f.reject_and_measure_scans(requests, &tables).await;
        // then
        assert!(
            responses
                .iter()
                .all(|response| response["error"]["data"]["code"] == "UNSUPPORTED_OPERATION"),
            "{responses:?}"
        );
        assert!(responses
            .iter()
            .all(|response| response.get("result").is_none()));
        assert_eq!(after, before);
    }
}

#[tokio::test]
async fn scalar_domain_pk는_기반_타입으로_바인딩해_keyset을_이어간다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        f.client.batch_execute(&format!("CREATE DOMAIN {}.int_key AS integer; CREATE TABLE {}.key_scalar(id {}.int_key PRIMARY KEY,g geometry(Point,4326)); INSERT INTO {}.key_scalar VALUES(1,ST_SetSRID(ST_MakePoint(1,0),4326)),(2,ST_SetSRID(ST_MakePoint(2,0),4326))",f.schema,f.schema,f.schema,f.schema)).await.unwrap();
        let mut input = f.table_input("key_scalar", 1);
        let first = f.call("spatial_query_table", input.clone()).await;
        input["next_token"] = first["result"]["page"]["next_token"].clone();
        // when
        let second = f.call("spatial_query_table", input).await;
        // then
        assert!(first.get("error").is_none(), "{first}");
        assert!(second.get("error").is_none(), "{second}");
        assert_eq!(first["result"]["features"][0]["properties"]["id"], 1);
        assert_eq!(second["result"]["features"][0]["properties"]["id"], 2);
        assert_eq!(first["result"]["page"]["resume_mode"], "native");
        assert_eq!(second["result"]["page"]["has_more"], false);
    }
}

#[tokio::test]
async fn pk가_없는_다음_페이지는_db_변경_후에도_원본_snapshot을_읽는다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut input = f.table_input("no_pk", 1);
        let first = f.call("spatial_query_table", input.clone()).await;
        f.client
            .batch_execute(&format!("DELETE FROM {}.no_pk", f.schema))
            .await
            .unwrap();
        input["next_token"] = first["result"]["page"]["next_token"].clone();
        // when
        let second = f.call("spatial_query_table", input).await;
        // then
        assert!(first.get("error").is_none(), "{first}");
        assert!(second.get("error").is_none(), "{second}");
        assert_eq!(
            second["result"]["features"][0]["properties"]["label"],
            "원본2"
        );
        assert!(second["result"]["row_references"][0]["identity"].is_null());
        assert_eq!(
            second["result"]["row_references"][0]["snapshot_id"],
            first["result"]["row_references"][0]["snapshot_id"]
        );
    }
}

#[tokio::test]
async fn 날짜변경선과_세계_영_너비_viewport를_서로_다르게_조회한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut dateline = f.table_input("dateline", 100);
        dateline["viewport"] = json!({"west":170,"south":-10,"east":-170,"north":10});
        let mut zero = f.table_input("dateline", 100);
        zero["viewport"] = json!({"west":0,"south":-10,"east":0,"north":10});
        let inputs = [dateline, zero, f.table_input("dateline", 100)];
        // when
        let responses = futures_util::future::join_all(
            inputs
                .into_iter()
                .map(|input| f.call("spatial_query_table", input)),
        )
        .await;
        // then
        assert!(
            responses.iter().all(|r| r.get("error").is_none()),
            "{responses:?}"
        );
        assert_eq!(
            responses[0]["result"]["features"].as_array().unwrap().len(),
            4
        );
        assert_eq!(
            responses[1]["result"]["features"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            responses[2]["result"]["features"].as_array().unwrap().len(),
            5
        );
    }
}

#[tokio::test]
async fn mixed_srid_table은_필터한_원본_타입으로_교차를_판정한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut input = f.table_input("mixed", 100);
        input["viewport"] = json!({"west":0.5,"south":-1,"east":1.5,"north":1});
        input["source_srid"] = json!(3857);
        input["srid_filter"] = json!(3857);
        // when
        let response = f.call("spatial_query_table", input).await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["features"].as_array().unwrap().len(), 1);
        assert_eq!(response["result"]["features"][0]["properties"]["id"], 3);
        assert_eq!(
            response["result"]["features"][0]["properties"]["g"]["srid"],
            3857
        );
    }
}

#[tokio::test]
async fn 취소는_지정한_연결의_쿼리만_중단하고_쓰기_rollback을_확인한다() {
    for port in ports() {
        // given
        let f = Arc::new(Fixture::new(port).await);
        let mut request = f.request(
            "execute_query_exact",
            json!({"text":format!("SELECT {}.delayed_write()",f.schema),"parameters":[]}),
        );
        request["params"]["driver_context"]["read_only"] = json!(false);
        let target = request["params"]["driver_context"]["request_id"]
            .as_str()
            .unwrap()
            .to_string();
        let task = {
            let f = f.clone();
            tokio::spawn(async move { f.send(request).await })
        };
        let active = f.wait_active(&format!("{}.delayed_write", f.schema)).await;
        let cancel = json!({"jsonrpc":"2.0","id":777,"method":"cancel_request","params":{"connection_id":f.schema,"request_id":target}});
        // when
        let (response, finished, counter) = f.cancel_and_measure(cancel, task).await;
        // then
        assert!(active);
        assert_eq!(response["result"]["accepted"], true);
        assert_eq!(finished["error"]["data"]["code"], "CANCELLED");
        assert_eq!(finished["error"]["data"]["outcome"], "not_applied");
        assert_eq!(counter, 0);
    }
}

#[tokio::test]
async fn 원래_sql의_함수를_삭제해도_캐시된_ewkb_변환은_재실행하지_않는다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut request = f.request(
            "execute_query_exact",
            json!({"text":format!("SELECT {}.counter_shape()",f.schema),"parameters":[]}),
        );
        request["params"]["driver_context"]["read_only"] = json!(false);
        let original = f.send(request).await;
        let wrapper = original["result"]["rows"][0][0].clone();
        f.client
            .batch_execute(&format!("DROP FUNCTION {}.counter_shape()", f.schema))
            .await
            .unwrap();
        // when
        let (response, counter) = f.transform_and_measure(wrapper).await;
        // then
        assert!(original.get("error").is_none(), "{original}");
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(
            response["result"]["geometries"][0]["coordinates"],
            json!([10, 0])
        );
        assert_eq!(counter, 1);
    }
}

#[tokio::test]
async fn public_밖의_공간_extension_schema를_인용해_메타데이터와_변환을_실행한다() {
    for port in ports() {
        // given
        let database = std::env::var("TABULARIS_P1_POSTGIS_EXTENSION_DB")
            .expect("인용을 검증할 extension fixture DB를 지정해야 한다");
        let extension_schema = std::env::var("TABULARIS_P1_POSTGIS_EXTENSION_SCHEMA")
            .expect("인용을 검증할 extension fixture schema를 지정해야 한다");
        let f = Fixture::new_in_database(port, &database, &extension_schema).await;
        let wrapper = f
            .wrapper("POINT(1113194.9079327357 0)", 3857, "geometry")
            .await;
        let requests = [
            f.request("spatial_columns", json!({"table":f.table("projected")})),
            f.request("spatial_transform_values", json!({"values":[wrapper]})),
        ];
        // when
        let responses =
            futures_util::future::join_all(requests.into_iter().map(|request| f.send(request)))
                .await;
        // then
        assert!(
            responses.iter().all(|r| r.get("error").is_none()),
            "{responses:?}"
        );
        assert_eq!(responses[0]["result"]["columns"][0]["srid"], 3857);
        assert!(
            (responses[1]["result"]["geometries"][0]["coordinates"][0]
                .as_f64()
                .unwrap()
                - 10.0)
                .abs()
                < 1e-8
        );
    }
}

#[tokio::test]
async fn preserve_선과_명시적인_shortest_날짜변경선_분할을_구별한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let wrapper = f
            .wrapper("LINESTRING(170 0,-170 0)", 4326, "geometry")
            .await;
        let inputs = [
            json!({"values":[wrapper.clone()],"longitude_mode":"preserve"}),
            json!({"values":[wrapper],"longitude_mode":"shortest"}),
        ];
        // when
        let responses = futures_util::future::join_all(
            inputs
                .into_iter()
                .map(|input| f.call("spatial_transform_values", input)),
        )
        .await;
        // then
        assert!(
            responses.iter().all(|r| r.get("error").is_none()),
            "{responses:?}"
        );
        assert_eq!(
            responses[0]["result"]["geometries"][0]["type"],
            "LineString"
        );
        assert_eq!(
            responses[0]["result"]["geometries"][0]["coordinates"],
            json!([[170, 0], [-170, 0]])
        );
        assert_eq!(
            responses[1]["result"]["geometries"][0]["type"],
            "MultiLineString"
        );
        assert_eq!(
            responses[1]["result"]["geometries"][0]["coordinates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(responses[1]["result"]["geometries"][0]["coordinates"]
            .as_array()
            .unwrap()
            .contains(&json!([[170, 0], [180, 0]])));
        assert!(responses[1]["result"]["geometries"][0]["coordinates"]
            .as_array()
            .unwrap()
            .contains(&json!([[-180, 0], [-170, 0]])));
    }
}

#[tokio::test]
async fn geography의_날짜변경선_선은_구면_경로를_분할하고_원본_타입을_보존한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let wrapper = f
            .wrapper("LINESTRING(170 0,-170 0)", 4326, "geography")
            .await;
        let before = wrapper.clone();
        // when
        let response = f
            .call("spatial_transform_values", json!({"values":[wrapper]}))
            .await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(before["native_type"], "geography");
        assert_eq!(
            response["result"]["geometries"][0]["type"],
            "MultiLineString"
        );
        assert!(
            response["result"]["geometries"][0]["coordinates"][0]
                .as_array()
                .unwrap()
                .len()
                > 2
        );
    }
}

#[tokio::test]
async fn 지원하지_않는_곡선과_잘못된_폴리곤은_명시적으로만_건너뛴다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let curve = f
            .wrapper("CIRCULARSTRING(0 0,1 1,2 0)", 4326, "geometry")
            .await;
        let invalid = f
            .wrapper("POLYGON((0 0,2 2,0 2,2 0,0 0))", 4326, "geometry")
            .await;
        let requests = [
            f.request(
                "spatial_transform_values",
                json!({"values":[curve.clone()]}),
            ),
            f.request(
                "spatial_transform_values",
                json!({"values":[invalid.clone()]}),
            ),
            f.request(
                "spatial_transform_values",
                json!({"values":[curve,invalid],"skip_invalid":true}),
            ),
        ];
        // when
        let responses =
            futures_util::future::join_all(requests.into_iter().map(|request| f.send(request)))
                .await;
        // then
        assert_eq!(responses[0]["error"]["data"]["code"], "UNSUPPORTED_TYPE");
        assert_eq!(responses[1]["error"]["data"]["code"], "INVALID_GEOMETRY");
        assert_eq!(responses[2]["result"]["geometries"], json!([null, null]));
        assert_eq!(
            responses[2]["result"]["feature_errors"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            responses[2]["result"]["limits"]["reasons"],
            json!(["SKIPPED_FEATURES"])
        );
    }
}

#[tokio::test]
async fn 물리_세션은_같은_backend를_예약하고_전용_commit으로만_쓰기를_확정한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut open = f.request("service_session_open", json!({"mode":"write"}));
        open["params"]["driver_context"]["read_only"] = json!(false);
        let opened = f.send(open).await;
        let handle = opened["result"]["session_handle"].clone();
        let mut write=f.request("execute_query_exact",json!({"text":format!("UPDATE {}.writes SET counter=9 WHERE id=1 RETURNING pg_backend_pid()",f.schema),"parameters":[]}));
        write["params"]["driver_context"]["session_handle"] = handle.clone();
        write["params"]["driver_context"]["read_only"] = json!(false);
        let written = f.send(write).await;
        let mut same_backend = f.request(
            "execute_query_exact",
            json!({"text":"SELECT pg_backend_pid()","parameters":[]}),
        );
        same_backend["params"]["driver_context"]["session_handle"] = handle.clone();
        same_backend["params"]["driver_context"]["read_only"] = json!(false);
        let same_backend = f.send(same_backend).await;
        let before: i32 = f
            .client
            .query_one(
                &format!("SELECT counter FROM {}.writes WHERE id=1", f.schema),
                &[],
            )
            .await
            .unwrap()
            .get(0);
        let mut finish = f.request("service_session_finish", json!({"action":"commit"}));
        finish["params"]["driver_context"]["session_handle"] = handle;
        finish["params"]["driver_context"]["read_only"] = json!(false);
        // when
        let (response, counter) = f.finish_and_measure(finish).await;
        // then
        assert!(opened.get("error").is_none(), "{opened}");
        assert!(written.get("error").is_none(), "{written}");
        assert_eq!(
            written["result"]["rows"][0][0],
            same_backend["result"]["rows"][0][0]
        );
        assert_eq!(before, 0);
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["in_transaction"], false);
        assert_eq!(counter, 9);
    }
}

#[tokio::test]
async fn 읽기_전용_세션의_모드를_요청_context로_뒤집을_수_없다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let opened = f
            .call("service_session_open", json!({"mode":"read_only"}))
            .await;
        let mut request = f.request(
            "execute_query_exact",
            json!({"text":"SELECT 1","parameters":[]}),
        );
        request["params"]["driver_context"]["session_handle"] =
            opened["result"]["session_handle"].clone();
        request["params"]["driver_context"]["read_only"] = json!(false);
        // when
        let response = f
            .send_and_discard(request, opened["result"]["session_handle"].clone())
            .await;
        // then
        assert_eq!(response["error"]["data"]["code"], "WRITE_NOT_ALLOWED");
    }
}

#[tokio::test]
async fn 같은_물리_세션의_동시_요청은_순서대로_완료한_뒤_한번만_commit한다() {
    for port in ports() {
        // given
        let f = Arc::new(Fixture::new(port).await);
        let mut open = f.request("service_session_open", json!({"mode":"write"}));
        open["params"]["driver_context"]["read_only"] = json!(false);
        let opened = f.send(open).await;
        let handle = opened["result"]["session_handle"].clone();
        let tag = format!("{}_session_serial", f.schema);
        let mut first=f.request("execute_query_exact",json!({"text":format!("WITH delay AS MATERIALIZED (SELECT pg_sleep(0.3)) UPDATE {}.writes SET counter=counter+1 FROM delay WHERE id=1 RETURNING counter,pg_backend_pid() /* {tag} */",f.schema),"parameters":[]}));
        first["params"]["driver_context"]["session_handle"] = handle.clone();
        first["params"]["driver_context"]["read_only"] = json!(false);
        let task = {
            let f = f.clone();
            tokio::spawn(async move { f.send(first).await })
        };
        let active = f.wait_active(&tag).await;
        let mut second=f.request("execute_query_exact",json!({"text":format!("UPDATE {}.writes SET counter=counter+1 WHERE id=1 RETURNING counter,pg_backend_pid()",f.schema),"parameters":[]}));
        second["params"]["driver_context"]["session_handle"] = handle.clone();
        second["params"]["driver_context"]["read_only"] = json!(false);
        let mut finish = f.request("service_session_finish", json!({"action":"commit"}));
        finish["params"]["driver_context"]["session_handle"] = handle;
        finish["params"]["driver_context"]["read_only"] = json!(false);
        // when
        let (first, second, finished, counter) = f.serialize_and_commit(second, task, finish).await;
        // then
        assert!(active);
        assert!(first.get("error").is_none(), "{first}");
        assert!(second.get("error").is_none(), "{second}");
        assert_eq!(first["result"]["rows"][0][0], 1);
        assert_eq!(second["result"]["rows"][0][0], 2);
        assert_eq!(
            first["result"]["rows"][0][1],
            second["result"]["rows"][0][1]
        );
        assert!(finished.get("error").is_none(), "{finished}");
        assert_eq!(counter, 2);
    }
}

#[tokio::test]
async fn 예약된_읽기_세션의_응답_상한은_전체_rollback과_세션_닫힘을_명시한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let opened = f
            .call("service_session_open", json!({"mode":"read_only"}))
            .await;
        let handle = opened["result"]["session_handle"].clone();
        let mut request = f.request(
            "execute_query_exact",
            json!({"text":"SELECT generate_series(1,3)","parameters":[],"max_rows":1}),
        );
        request["params"]["driver_context"]["session_handle"] = handle.clone();
        // when
        let (result, probe, counter) = f.cap_and_probe(request, Some((handle, true))).await;
        // then
        assert_eq!(result["error"]["data"]["code"], "RESOURCE_LIMIT");
        assert_eq!(result["error"]["data"]["outcome"], "not_applied");
        assert_eq!(result["error"]["data"]["details"]["session_closed"], true);
        assert_eq!(probe["error"]["data"]["code"], "CURSOR_EXPIRED");
        assert_eq!(counter, 0);
    }
}

#[tokio::test]
async fn 예약된_쓰기_세션의_응답_상한은_앞선_쓰기까지_rollback하고_닫는다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut open = f.request("service_session_open", json!({"mode":"write"}));
        open["params"]["driver_context"]["read_only"] = json!(false);
        let opened = f.send(open).await;
        let handle = opened["result"]["session_handle"].clone();
        let mut write=f.request("execute_query_exact",json!({"text":format!("UPDATE {}.writes SET counter=9 WHERE id=1",f.schema),"parameters":[]}));
        write["params"]["driver_context"]["read_only"] = json!(false);
        write["params"]["driver_context"]["session_handle"] = handle.clone();
        let written = f.send(write).await;
        let mut request = f.request(
            "execute_query_exact",
            json!({"text":"SELECT generate_series(1,3)","parameters":[],"max_rows":1}),
        );
        request["params"]["driver_context"]["read_only"] = json!(false);
        request["params"]["driver_context"]["session_handle"] = handle.clone();
        // when
        let (result, probe, counter) = f.cap_and_probe(request, Some((handle, false))).await;
        // then
        assert!(written.get("error").is_none(), "{written}");
        assert_eq!(result["error"]["data"]["code"], "RESOURCE_LIMIT");
        assert_eq!(result["error"]["data"]["outcome"], "not_applied");
        assert_eq!(result["error"]["data"]["details"]["session_closed"], true);
        assert_eq!(probe["error"]["data"]["code"], "CURSOR_EXPIRED");
        assert_eq!(counter, 0);
    }
}

#[tokio::test]
async fn 개별_쓰기_응답의_상한은_부분_성공_대신_전체_rollback을_반환한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        f.client
            .batch_execute(&format!(
                "INSERT INTO {}.writes VALUES(2,0),(3,0)",
                f.schema
            ))
            .await
            .unwrap();
        let mut request=f.request("execute_query_exact",json!({"text":format!("UPDATE {}.writes SET counter=counter+1 RETURNING id,counter",f.schema),"parameters":[],"max_rows":1}));
        request["params"]["driver_context"]["read_only"] = json!(false);
        // when
        let (result, _, counter) = f.cap_and_probe(request, None).await;
        // then
        assert_eq!(result["error"]["data"]["code"], "RESOURCE_LIMIT");
        assert_eq!(result["error"]["data"]["outcome"], "not_applied");
        assert_eq!(counter, 0);
    }
}

#[tokio::test]
async fn 읽기_상한의_취소가_늦어도_다음_요청은_새_backend에서_완료한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        // when
        let results = f.capped_reads_and_new_backends().await;
        // then
        assert!(
            results
                .iter()
                .all(|(capped, next)| capped.get("error").is_none() && next.get("error").is_none()),
            "{results:?}"
        );
        assert!(results.iter().all(|(capped, _)| capped["result"]["rows"]
            .as_array()
            .unwrap()
            .len()
            == 1
            && capped["result"]["truncated"] == true));
        assert!(results
            .iter()
            .all(|(capped, next)| capped["result"]["rows"][0][0] != next["result"]["rows"][0][0]));
    }
}

#[tokio::test]
async fn 실행_기한을_넘긴_읽기는_취소와_구별되는_deadline_오류를_반환한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut request = f.request(
            "execute_query_exact",
            json!({"text":"SELECT pg_sleep(20)","parameters":[]}),
        );
        request["params"]["driver_context"]["deadline_ms"] = json!(150);
        // when
        let result = f.send(request).await;
        // then
        assert_eq!(result["error"]["data"]["code"], "DEADLINE_EXCEEDED");
        assert_eq!(result["error"]["data"]["outcome"], "not_applied");
    }
}

#[tokio::test]
async fn 실제_두_연결_중_하나를_취소해도_다른_조회는_실행을_유지한다() {
    for port in ports() {
        // given
        let f = Arc::new(Fixture::new(port).await);
        let first_tag = format!("{}_first", f.schema);
        let second_tag = format!("{}_second", f.schema);
        let mut first = f.request(
            "execute_query_exact",
            json!({"text":format!("SELECT pg_sleep(20) /* {first_tag} */"),"parameters":[]}),
        );
        let mut second = f.request(
            "execute_query_exact",
            json!({"text":format!("SELECT pg_sleep(20) /* {second_tag} */"),"parameters":[]}),
        );
        first["params"]["driver_context"]["deadline_ms"] = json!(30000);
        second["params"]["driver_context"]["deadline_ms"] = json!(30000);
        second["params"]["driver_context"]["connection_id"] = json!(format!("{}_other", f.schema));
        let first_cancel = json!({"id":"first_cancel","method":"cancel_request","params":{"connection_id":first["params"]["driver_context"]["connection_id"],"request_id":first["params"]["driver_context"]["request_id"]}});
        let second_cancel = json!({"id":"second_cancel","method":"cancel_request","params":{"connection_id":second["params"]["driver_context"]["connection_id"],"request_id":second["params"]["driver_context"]["request_id"]}});
        let first_task = {
            let f = f.clone();
            tokio::spawn(async move { f.send(first).await })
        };
        let second_task = {
            let f = f.clone();
            tokio::spawn(async move { f.send(second).await })
        };
        let first_active = f.wait_active(&first_tag).await;
        let second_active = f.wait_active(&second_tag).await;
        // when
        let (response, first_result, still_active, second_result) = f
            .cancel_isolated(
                first_cancel,
                first_task,
                second_cancel,
                second_task,
                &second_tag,
            )
            .await;
        // then
        assert!(first_active && second_active);
        assert_eq!(response["result"]["accepted"], true);
        assert_eq!(first_result["error"]["data"]["code"], "CANCELLED");
        assert!(still_active);
        assert_eq!(second_result["error"]["data"]["code"], "CANCELLED");
    }
}

#[tokio::test]
async fn 모든_worker와_작업_큐가_차도_stdio_취소는_즉시_접수한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut plugin = Plugin::new();
        let mut cleanup = Vec::new();
        for index in 0..4 {
            let mut request=f.request("execute_query_exact",json!({"text":format!("SELECT pg_sleep(20) /* {}_queue_{index} */",f.schema),"parameters":[]}));
            request["params"]["driver_context"]["deadline_ms"] = json!(30000);
            cleanup.push(json!({"method":"cancel_request","params":{"connection_id":f.schema,"request_id":request["params"]["driver_context"]["request_id"]}}));
            plugin.send(&request);
        }
        let active = f
            .wait_active_count(&format!("{}_queue_", f.schema), 4)
            .await;
        let mut queued = f.request(
            "execute_query_exact",
            json!({"text":format!("SELECT {}.counter_shape()",f.schema),"parameters":[]}),
        );
        queued["params"]["driver_context"]["read_only"] = json!(false);
        plugin.send(&queued);
        for _ in 0..80 {
            plugin.send(&f.request(
                "execute_query_exact",
                json!({"text":"SELECT 1","parameters":[]}),
            ));
        }
        let cancel = json!({"id":"control","method":"cancel_request","params":{"connection_id":f.schema,"request_id":queued["params"]["driver_context"]["request_id"]}});
        // when
        let (response, elapsed, success) =
            tokio::task::spawn_blocking(move || plugin.cancel_and_drain(cancel, cleanup))
                .await
                .unwrap();
        // then
        assert!(active);
        assert_eq!(response["result"]["accepted"], true, "{response}");
        assert!(elapsed < Duration::from_secs(3));
        assert!(success);
    }
}

#[tokio::test]
async fn 공간_배열은_legacy_쿼리에서도_null로_숨기지_않고_오류를_반환한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let request = json!({"id":111,"method":"execute_query","params":{"params":f.params,"query":"SELECT ARRAY[ST_SetSRID(ST_MakePoint(1,2),4326)]"}});
        // when
        let response = f.send(request).await;
        // then
        assert_eq!(response["error"]["data"]["code"], "UNSUPPORTED_TYPE");
    }
}

#[tokio::test]
async fn geography_위도_경계로_누락할_수_있는_viewport는_명확히_거부한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        f.client.batch_execute(&format!("INSERT INTO {}.geographic VALUES(4,ST_SetSRID(ST_MakePoint(0.5,10.0001),4326)::geography)",f.schema)).await.unwrap();
        let spherical_match:bool=f.client.query_one("SELECT ST_Intersects(ST_SetSRID(ST_MakePoint(0.5,10.0001),4326)::geography,ST_Segmentize(ST_MakeEnvelope(0,10,1,20,4326),1)::geography)",&[]).await.unwrap().get(0);
        let mut input = f.table_input("geographic", 100);
        input["viewport"] = json!({"west":0,"south":10,"east":1,"north":20});
        // when
        let response = f.call("spatial_query_table", input).await;
        // then
        assert!(!spherical_match);
        assert_eq!(response["error"]["data"]["code"], "UNSUPPORTED_OPERATION");
    }
}

#[tokio::test]
async fn geography의_전세계_조회만_위도_제한_없이_원본_타입을_보존한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut full = f.table_input("geographic", 100);
        full["viewport"] = json!({"west":0,"south":-90,"east":0,"north":90,"world":true});
        let partial = f.table_input("geographic", 100);
        let requests = vec![
            f.request("spatial_query_table", full),
            f.request("spatial_query_table", partial),
        ];
        // when
        let responses = f.send_all(requests).await;
        // then
        assert!(responses[0].get("error").is_none(), "{}", responses[0]);
        assert_eq!(
            responses[0]["result"]["features"].as_array().unwrap().len(),
            3
        );
        assert!(responses[0]["result"]["features"]
            .as_array()
            .unwrap()
            .iter()
            .all(|feature| feature["properties"]["g"]["native_type"] == "geography"));
        assert_eq!(
            responses[1]["error"]["data"]["code"],
            "UNSUPPORTED_OPERATION"
        );
    }
}

#[tokio::test]
async fn 세계_경도_flag도_부분_위도는_필터하고_3857의_극지_범위를_거부한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        f.client
            .batch_execute(&format!(
                "UPDATE {}.dateline SET g=ST_SetSRID(ST_MakePoint(179,60),4326) WHERE id=1",
                f.schema
            ))
            .await
            .unwrap();
        let mut partial = f.table_input("dateline", 100);
        partial["viewport"] = json!({"west":42,"south":-10,"east":42,"north":10,"world":true});
        let mut polar = f.table_input("projected", 100);
        polar["viewport"] = json!({"west":0,"south":-90,"east":0,"north":80,"world":true});
        let requests = vec![
            f.request("spatial_query_table", partial),
            f.request("spatial_query_table", polar),
        ];
        // when
        let responses = f.send_all(requests).await;
        // then
        assert!(responses[0].get("error").is_none(), "{}", responses[0]);
        assert_eq!(
            responses[0]["result"]["features"]
                .as_array()
                .unwrap()
                .iter()
                .map(|feature| feature["properties"]["id"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 5]
        );
        assert_eq!(
            responses[1]["error"]["data"]["code"],
            "UNSUPPORTED_OPERATION"
        );
    }
}

#[tokio::test]
async fn 전체_feature_상한은_안전한_페이지_끝에서_truncated를_표시한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut input = f.table_input("projected", 1);
        input["limits"] = json!({"max_features":1});
        // when
        let response = f.call("spatial_query_table", input).await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["features"].as_array().unwrap().len(), 1);
        assert_eq!(response["result"]["page"]["next_token"], Value::Null);
        assert_eq!(response["result"]["limits"]["truncated"], true);
        assert_eq!(
            response["result"]["limits"]["reasons"],
            json!(["LAYER_LIMIT"])
        );
    }
}

#[tokio::test]
async fn 한글_속성의_byte_상한은_완성된_feature의_응답_전체에_적용한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        f.client
            .batch_execute(&format!(
                "UPDATE {}.projected SET label=repeat('한글',200)",
                f.schema
            ))
            .await
            .unwrap();
        let mut input = f.table_input("projected", 10);
        input["limits"] = json!({"max_bytes":3000});
        // when
        let response = f.call("spatial_query_table", input).await;
        // then
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["features"].as_array().unwrap().len(), 1);
        assert!(serde_json::to_vec(&response["result"]).unwrap().len() <= 3000);
        assert_eq!(response["result"]["limits"]["truncated"], true);
    }
}

#[tokio::test]
async fn 단일_과대_geometry는_좌표를_중간에서_자르지_않고_거부한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let original=f.exact("SELECT ST_SetSRID(ST_MakeLine(ST_MakePoint(i/100000.0,0) ORDER BY i),4326) FROM generate_series(0,100000) i".into()).await;
        let wrapper = original["result"]["rows"][0][0].clone();
        // when
        let response = f
            .call("spatial_transform_values", json!({"values":[wrapper]}))
            .await;
        // then
        assert!(original.get("error").is_none(), "{original}");
        assert_eq!(response["error"]["data"]["code"], "FEATURE_TOO_LARGE");
    }
}

#[tokio::test]
async fn viewport가_바뀌면_이전_커서를_재사용하지_못한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let mut input = f.table_input("projected", 1);
        let original = f.call("spatial_query_table", input.clone()).await;
        input["next_token"] = original["result"]["page"]["next_token"].clone();
        input["viewport"] = json!({"west":0,"south":-10,"east":40,"north":10});
        // when
        let response = f.call("spatial_query_table", input).await;
        // then
        assert_eq!(response["error"]["data"]["code"], "INVALID_PAGE_TOKEN");
    }
}

#[tokio::test]
async fn 준비된_단일_sql은_다중_문장을_거부하고_문자열의_제어_단어는_허용한다() {
    for port in ports() {
        // given
        let f = Fixture::new(port).await;
        let texts = [
            "SELECT 1; COMMIT; SELECT 2".to_string(),
            "SELECT 'COMMIT', $$ROLLBACK$$".to_string(),
        ];
        // when
        let responses =
            futures_util::future::join_all(texts.into_iter().map(|text| f.exact(text))).await;
        // then
        assert_eq!(responses[0]["error"]["data"]["code"], "INVALID_ARGUMENT");
        assert_eq!(
            responses[1]["result"]["rows"],
            json!([["COMMIT", "ROLLBACK"]])
        );
    }
}

#[tokio::test]
async fn 쓰기_응답_연결을_잃으면_rollback을_추측하지_않고_outcome_unknown을_반환한다() {
    for port in ports() {
        // given
        let f = Arc::new(Fixture::new(port).await);
        let text = format!("SELECT {}.delayed_write()", f.schema);
        let mut request = f.request("execute_query_exact", json!({"text":text,"parameters":[]}));
        request["params"]["driver_context"]["read_only"] = json!(false);
        let task = {
            let f = f.clone();
            tokio::spawn(async move { f.send(request).await })
        };
        let active = f.wait_active(&format!("{}.delayed_write", f.schema)).await;
        let pid:i32=f.client.query_one("SELECT pid FROM pg_stat_activity WHERE query=$1 AND pid<>pg_backend_pid() AND state='active'",&[&text]).await.unwrap().get(0);
        // when
        let (response, counter) = f.terminate_and_measure(pid, task).await;
        // then
        assert!(active);
        assert_eq!(response["error"]["data"]["code"], "OUTCOME_UNKNOWN");
        assert_eq!(response["error"]["data"]["outcome"], "unknown");
        assert_eq!(response["error"]["data"]["retryable"], false);
        assert_eq!(counter, 0);
    }
}
