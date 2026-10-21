use super::spatial_support::*;
use crate::services::contracts::Operation;
use serde_json::json;
#[tokio::test]
async fn 현재_행_조회는_명시한_테이블과_전체_키를_전달한다() {
    // given
    let h = Harness::new();
    let input = request(
        Operation::SpatialFeature,
        json!({"table":{"database":"fixture","schema":"public","table":"places"},"identity":{"columns":["id"],"values":[1]}}),
    );
    // when
    let actual = h.call(&input).await;
    // then
    assert!(actual.error.is_none());
    assert_eq!(actual.data["source"], "current_row");
    assert_eq!(actual.data["identity"], input.input["identity"]);
    assert_eq!(h.spatial.calls.lock().unwrap()[0].2, input.input);
}

struct LocalResolver {
    params: crate::models::ConnectionParams,
}
#[async_trait::async_trait]
impl crate::service_registry_driver::ServiceConnectionResolver for LocalResolver {
    async fn params(
        &self,
        connection: &crate::services::backend::SavedConnection,
    ) -> crate::services::error::ServiceResult<crate::models::ConnectionParams> {
        let mut params = self.params.clone();
        params.driver = connection.driver.clone();
        params.connection_id = Some(connection.id.clone());
        Ok(params)
    }
}
struct PostgresEvidence {
    projected: crate::services::contracts::ServiceResponse,
    shapes: crate::services::contracts::ServiceResponse,
    unknown: crate::services::contracts::ServiceResponse,
    overridden: crate::services::contracts::ServiceResponse,
    conflict: crate::services::contracts::ServiceResponse,
    geography: crate::services::contracts::ServiceResponse,
    bounded_geography: crate::services::contracts::ServiceResponse,
    columns: crate::services::contracts::ServiceResponse,
    table: crate::services::contracts::ServiceResponse,
    keyless: crate::services::contracts::ServiceResponse,
    cached: crate::services::contracts::ServiceResponse,
    current: crate::services::contracts::ServiceResponse,
    original_before: serde_json::Value,
    original_after: serde_json::Value,
    counter_before: i64,
    counter_after: i64,
    original_id: String,
    original_sql_calls: usize,
}
fn local_context() -> crate::services::backend::DriverContext {
    crate::services::backend::DriverContext {
        connection_id: "conn-a".into(),
        request_id: uuid::Uuid::new_v4().to_string(),
        deadline_ms: 30_000,
        signal: Default::default(),
        transient_auth: None,
        read_only: false,
        session_handle: None,
    }
}
async fn postgres_snapshot(
    base: &super::support::Harness,
    registry: &crate::service_registry_driver::RegistryDriverBackend,
    sql: String,
    multisets: bool,
) -> Result<(String, serde_json::Value), String> {
    use crate::services::backend::DriverBackend;
    let connection = base.connections.0.lock().unwrap()["conn-a"].clone();
    let input = request(
        Operation::QueryExecute,
        json!({"language":"sql","text":sql,"parameters":[]}),
    );
    let mut snapshot = registry
        .execute(&connection, &input, local_context())
        .await
        .map_err(|e| format!("seed {:?}", e.code))?
        .snapshot
        .ok_or("missing seed snapshot")?;
    let binding = super::support::binding();
    snapshot.revision = binding.result_revision.clone();
    if multisets {
        snapshot.sets.insert(
            0,
            crate::services::results::ResultSet {
                kind: crate::services::results::ResultKind::Tabular,
                columns: vec![json!({"name":"unrelated","native_type":"text","column_index":0})],
                rows: vec![json!(["unused"])],
            },
        );
    }
    let before = serde_json::to_value(&snapshot).map_err(|_| "seed serialize")?;
    let id = base
        .runtime
        .results
        .insert(binding, snapshot)
        .map_err(|e| format!("seed store {:?}", e.code))?;
    Ok((id, before))
}
fn fixture_table(schema: &str, name: &str) -> serde_json::Value {
    json!({"database":"spatial_fixture","schema":schema,"table":name})
}
fn fixture_table_request(
    schema: &str,
    name: &str,
    column: &str,
) -> crate::services::contracts::ServiceRequest {
    request(
        Operation::SpatialTableQuery,
        json!({"table":fixture_table(schema,name),"column":column,"viewport":{"west":-180,"east":180,"south":-85,"north":85},"page_size":100}),
    )
}
async fn postgres_scenario(
    config: super::live_config::LiveConfig,
) -> Result<PostgresEvidence, String> {
    use crate::{
        drivers::{driver_trait::PluginManifest, registry},
        plugins::driver::RpcDriver,
    };
    use std::{collections::HashMap, sync::Arc};
    let port = config.port;
    let _registry_guard = registry::REGISTRY_TEST_LOCK.lock().await;
    let (client, connection) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(port)
        .user("postgres")
        .dbname("spatial_fixture")
        .connect(tokio_postgres::NoTls)
        .await
        .map_err(|e| format!("fixture connect {e}"))?;
    let connection_task = tokio::spawn(connection);
    let schema = format!("h3b2b1_{}", uuid::Uuid::new_v4().simple());
    let driver_id = format!("h3b2b1-{}", uuid::Uuid::new_v4());
    let manifest:PluginManifest=serde_json::from_value(json!({"id":driver_id,"name":"Spatial fixture","version":"0.0.0","description":"Isolated native fixture","default_port":port,"capabilities":{"schemas":true,"views":true,"routines":true,"file_based":false}})).map_err(|e|format!("fixture manifest {e}"))?;
    let driver = RpcDriver::new(manifest, config.plugin, None, vec![], HashMap::new()).await?;
    registry::register_driver(driver).await;
    let setup=format!("CREATE SCHEMA {schema}; CREATE SEQUENCE {schema}.ticks; CREATE TABLE {schema}.projected(id integer PRIMARY KEY,note text,geom geometry(PointZ,3857)); INSERT INTO {schema}.projected VALUES (1,'old',ST_Transform(ST_GeomFromEWKT('SRID=4326;POINT Z(10 10 7)'),3857)),(2,'null',NULL),(3,'empty',ST_GeomFromEWKT('SRID=3857;POINT Z EMPTY')); CREATE TABLE {schema}.shapes(id integer PRIMARY KEY,geom geometry); INSERT INTO {schema}.shapes VALUES (1,ST_GeomFromEWKT('SRID=4326;POINT ZM(1 2 3 4)')),(2,ST_GeomFromEWKT('SRID=4326;LINESTRING(0 0,1 1)')),(3,ST_GeomFromEWKT('SRID=4326;POLYGON((0 0,1 0,1 1,0 0))')),(4,ST_GeomFromEWKT('SRID=4326;MULTIPOINT((1 2),(2 3))')),(5,ST_GeomFromEWKT('SRID=4326;MULTILINESTRING((0 0,1 1),(2 2,3 3))')),(6,ST_GeomFromEWKT('SRID=4326;MULTIPOLYGON(((0 0,1 0,1 1,0 0)))')),(7,ST_GeomFromEWKT('SRID=4326;GEOMETRYCOLLECTION(POINT(1 2),LINESTRING(0 0,1 1))')),(8,ST_GeomFromEWKT('SRID=4326;POINT M(1 2 9)')); CREATE TABLE {schema}.unknown(geom geometry); INSERT INTO {schema}.unknown VALUES(ST_GeomFromText('POINT(1 2)')); CREATE TABLE {schema}.geographic(id integer PRIMARY KEY,geom geography(LineString,4326)); INSERT INTO {schema}.geographic VALUES(1,ST_GeogFromText('SRID=4326;LINESTRING(179 0,-179 0)')); CREATE TABLE {schema}.keyless(note text,geom geometry(Point,4326)); INSERT INTO {schema}.keyless VALUES('first',ST_GeomFromEWKT('SRID=4326;POINT(1 2)'));");
    let scenario=async {
        client.batch_execute(&setup).await.map_err(|e|format!("fixture setup {e}"))?;
        let base=super::support::Harness::new(); base.connections.0.lock().unwrap().get_mut("conn-a").unwrap().driver=driver_id.clone();
        let params=crate::models::ConnectionParams {driver:driver_id.clone(),host:Some("127.0.0.1".into()),port:Some(port),username:Some("postgres".into()),database:crate::models::DatabaseSelection::Single("spatial_fixture".into()),ssl_mode:Some("disable".into()),..Default::default()};
        let backend=Arc::new(crate::service_registry_driver::RegistryDriverBackend::new(Arc::new(LocalResolver {params})));
        let spatial=super::super::RegistrySpatialDriverBackend::new(backend.clone());
        let _domain=super::super::SpatialDataDomain::attach(&base.runtime,spatial,Some(base.domain.store().clone())).map_err(|e|format!("attach {:?}",e.code))?;
        let (id,before)=postgres_snapshot(&base,&backend,format!("SELECT geom AS duplicate,note AS duplicate,nextval('{schema}.ticks') AS once FROM {schema}.projected ORDER BY id"),true).await?;
        let counter_before=client.query_one(&format!("SELECT last_value FROM {schema}.ticks"),&[]).await.map_err(|e|format!("counter {e}"))?.get(0);
        let mut input=query(&id,100); input.input["result_set_index"]=json!(1); input.input["source_srid"]=json!(3857);
        let projected=base.call(&input).await;
        let (shapes_id,_)=postgres_snapshot(&base,&backend,format!("SELECT geom FROM {schema}.shapes ORDER BY id"),false).await?;
        let shapes=base.call(&query(&shapes_id,100)).await;
        let (unknown_id,_)=postgres_snapshot(&base,&backend,format!("SELECT geom FROM {schema}.unknown"),false).await?;
        let mut unknown_request=query(&unknown_id,1); unknown_request.input["skip_invalid"]=json!(true);
        let unknown=base.call(&unknown_request).await;
        unknown_request.request_id=uuid::Uuid::new_v4().to_string();unknown_request.input["source_srid"]=json!(4326);
        let overridden=base.call(&unknown_request).await;
        let mut conflicting=input.clone();conflicting.request_id=uuid::Uuid::new_v4().to_string();conflicting.input["source_srid"]=json!(4326);
        let conflict=base.call(&conflicting).await;
        let bounded_geography=base.call(&fixture_table_request(&schema,"geographic","geom")).await;
        let mut geography_request=fixture_table_request(&schema,"geographic","geom");geography_request.input["viewport"]["south"]=json!(-90);geography_request.input["viewport"]["north"]=json!(90);geography_request.input["viewport"]["world"]=json!(true);
        let geography=base.call(&geography_request).await;
        let columns=base.call(&request(Operation::SpatialColumns,json!({"table":fixture_table(&schema,"projected")}))).await;
        let table=base.call(&fixture_table_request(&schema,"projected","geom")).await;
        let keyless=base.call(&fixture_table_request(&schema,"keyless","geom")).await;
        client.execute(&format!("UPDATE {schema}.projected SET note='new' WHERE id=1"),&[]).await.map_err(|e|format!("fixture update {e}"))?;
        let cached=base.call(&request(Operation::SpatialFeature,json!({"result_id":projected.result_id,"feature_id":format!("{id}:1:0")}))).await;
        let current=base.call(&request(Operation::SpatialFeature,json!({"table":fixture_table(&schema,"projected"),"identity":{"columns":["id"],"values":[1]}}))).await;
        let counter_after=client.query_one(&format!("SELECT last_value FROM {schema}.ticks"),&[]).await.map_err(|e|format!("counter {e}"))?.get(0);
        let original_after=serde_json::to_value(base.runtime.results.snapshot(&id,&super::support::binding()).map_err(|e|format!("snapshot {:?}",e.code))?).map_err(|_|"snapshot serialize")?;
        Ok(PostgresEvidence {projected,shapes,unknown,overridden,conflict,geography,bounded_geography,columns,table,keyless,cached,current,original_before:before,original_after,counter_before,counter_after,original_id:id,original_sql_calls:base.driver.0.load(std::sync::atomic::Ordering::SeqCst)})
    }.await;
    let cleanup = client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .await;
    registry::unregister_driver(&driver_id).await;
    drop(client);
    let _ = connection_task.await;
    cleanup.map_err(|e| format!("fixture cleanup {e}"))?;
    scenario
}
fn assert_postgres_evidence(actual: Result<PostgresEvidence, String>) {
    let actual = actual.unwrap();
    for (label, response) in [
        ("projected", &actual.projected),
        ("shapes", &actual.shapes),
        ("overridden", &actual.overridden),
        ("geography", &actual.geography),
        ("columns", &actual.columns),
        ("table", &actual.table),
        ("keyless", &actual.keyless),
        ("cached", &actual.cached),
        ("current", &actual.current),
    ] {
        assert!(
            response.error.is_none(),
            "{label}: {:?}",
            response
                .error
                .as_ref()
                .map(|error| (error.code, error.outcome))
        );
    }
    assert_eq!(
        actual.projected.data["features"].as_array().unwrap().len(),
        3
    );
    assert!(
        (actual.projected.data["features"][0]["geometry"]["coordinates"][0]
            .as_f64()
            .unwrap()
            - 10.0)
            .abs()
            < 1e-9
    );
    assert!(
        (actual.projected.data["features"][0]["geometry"]["coordinates"][1]
            .as_f64()
            .unwrap()
            - 10.0)
            .abs()
            < 1e-9
    );
    assert_eq!(
        actual.projected.data["features"][0]["geometry"]["coordinates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        actual.projected.data["features"][1]["geometry"],
        serde_json::Value::Null
    );
    assert_eq!(
        actual.projected.data["features"][2]["geometry"],
        serde_json::Value::Null
    );
    assert_eq!(
        actual.projected.data["features"][0]["id"],
        format!("{}:1:0", actual.original_id)
    );
    assert_eq!(
        actual.projected.data["features"][0]["properties"]["columns"][1],
        json!({"name":"duplicate","index":1,"value":"old"})
    );
    assert!(actual
        .projected
        .warnings
        .iter()
        .any(|v| v == "DIMENSIONS_REDUCED_TO_XY"));
    assert_eq!(actual.shapes.data["features"].as_array().unwrap().len(), 8);
    assert_eq!(
        actual.shapes.data["features"][0]["geometry"]["type"],
        "Point"
    );
    assert_eq!(
        actual.shapes.data["features"][1]["geometry"]["type"],
        "LineString"
    );
    assert_eq!(
        actual.shapes.data["features"][2]["geometry"]["type"],
        "Polygon"
    );
    assert_eq!(
        actual.shapes.data["features"][3]["geometry"]["type"],
        "MultiPoint"
    );
    assert_eq!(
        actual.shapes.data["features"][4]["geometry"]["type"],
        "MultiLineString"
    );
    assert_eq!(
        actual.shapes.data["features"][5]["geometry"]["type"],
        "MultiPolygon"
    );
    assert_eq!(
        actual.shapes.data["features"][6]["geometry"]["type"],
        "GeometryCollection"
    );
    assert_eq!(
        actual.unknown.error.unwrap().code,
        crate::services::contracts::ErrorCode::SridRequired
    );
    assert_eq!(
        actual.conflict.error.unwrap().code,
        crate::services::contracts::ErrorCode::InvalidArgument
    );
    assert_eq!(
        actual.overridden.data["features"][0]["geometry"],
        json!({"type":"Point","coordinates":[1.0,2.0]})
    );
    assert!(!actual.geography.data["features"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        actual.geography.data["features"][0]["geometry"]["type"],
        "MultiLineString"
    );
    assert_eq!(
        actual.bounded_geography.error.as_ref().unwrap().code,
        crate::services::contracts::ErrorCode::UnsupportedOperation
    );
    assert_eq!(
        actual.bounded_geography.error.as_ref().unwrap().outcome,
        crate::services::contracts::Outcome::NotApplied
    );
    assert_eq!(actual.columns.data["columns"][0]["name"], "geom");
    assert_eq!(actual.columns.data["columns"][0]["srid"], 3857);
    assert_eq!(
        actual.table.data["features"][0]["id"]
            .as_str()
            .unwrap()
            .len(),
        67
    );
    assert_eq!(
        actual.table.data["row_references"][0]["identity"],
        json!({"columns":["id"],"values":[1]})
    );
    assert!(
        actual.keyless.data["features"][0]["id"]
            .as_str()
            .unwrap()
            .len()
            <= 128
    );
    assert!(!actual.keyless.data["features"][0]["id"]
        .as_str()
        .unwrap()
        .starts_with("pk:"));
    assert_eq!(actual.cached.data["row"][1], "old");
    assert_eq!(actual.current.data["row"]["note"], "new");
    assert_eq!(actual.current.data["source"], "current_row");
    assert_eq!(actual.cached.data["source"], "snapshot");
    assert_eq!(actual.original_before, actual.original_after);
    assert_eq!(actual.counter_before, 3);
    assert_eq!(actual.counter_after, 3);
    assert_eq!(actual.original_sql_calls, 0);
}
#[tokio::test]
#[ignore = "requires explicit local PostGIS fixture inputs"]
async fn pg17의_생산_드라이버는_투영과_차원과_원본_스냅샷과_현재_행을_분리한다() {
    // given
    let config = super::live_config::postgis17();
    // when
    let actual = postgres_scenario(config).await;
    // then
    assert_postgres_evidence(actual);
}
#[tokio::test]
#[ignore = "requires explicit local PostGIS fixture inputs"]
async fn pg18의_생산_드라이버는_투영과_차원과_원본_스냅샷과_현재_행을_분리한다() {
    // given
    let config = super::live_config::postgis18();
    // when
    let actual = postgres_scenario(config).await;
    // then
    assert_postgres_evidence(actual);
}

struct NativeObservation {
    connection_id: String,
    request_id: String,
    table: String,
    bytes: usize,
    features: usize,
    native_next: bool,
}
struct ObservedRegistrySpatial {
    inner: std::sync::Arc<super::super::RegistrySpatialDriverBackend>,
    started: std::sync::Mutex<Vec<(String, String, String)>>,
    completed: std::sync::Mutex<Vec<NativeObservation>>,
    cancelled: std::sync::Mutex<Vec<(String, String)>>,
}
#[async_trait::async_trait]
impl super::super::SpatialDriverBackend for ObservedRegistrySpatial {
    async fn call(
        &self,
        connection: &crate::services::backend::SavedConnection,
        method: crate::drivers::driver_trait::ServiceRpcMethod,
        input: serde_json::Value,
        context: crate::services::backend::DriverContext,
    ) -> crate::services::error::ServiceResult<serde_json::Value> {
        let table = input["table"]["table"].as_str().unwrap_or("").to_owned();
        self.started.lock().unwrap().push((
            connection.id.clone(),
            context.request_id.clone(),
            table.clone(),
        ));
        let request_id = context.request_id.clone();
        let result = self.inner.call(connection, method, input, context).await?;
        self.completed.lock().unwrap().push(NativeObservation {
            connection_id: connection.id.clone(),
            request_id,
            table,
            bytes: serde_json::to_vec(&result).unwrap().len(),
            features: result["features"].as_array().map_or(0, Vec::len),
            native_next: result["page"]["next_token"].is_string(),
        });
        Ok(result)
    }
    async fn cancel(
        &self,
        connection_id: &str,
        request_id: &str,
    ) -> crate::services::error::ServiceResult<()> {
        self.cancelled
            .lock()
            .unwrap()
            .push((connection_id.into(), request_id.into()));
        self.inner.cancel(connection_id, request_id).await
    }
}
struct NativeBoundaryEvidence {
    pages: Vec<crate::services::contracts::ServiceResponse>,
    native_bytes: usize,
    native_features: usize,
    native_next: bool,
    fat_calls: usize,
    blocked: usize,
    same_request_connections: usize,
    cancel_response: crate::services::contracts::ServiceResponse,
    first: crate::services::contracts::ServiceResponse,
    second: crate::services::contracts::ServiceResponse,
    other_status: crate::services::contracts::JobStatus,
    cancelled_connections: Vec<String>,
}
async fn wait_owned_blocked(
    client: &tokio_postgres::Client,
    schema: &str,
) -> Result<usize, String> {
    let query="SELECT count(*)::bigint FROM pg_catalog.pg_stat_activity WHERE datname='spatial_fixture' AND state='active' AND wait_event_type='Lock' AND (query LIKE $1 OR query LIKE $2)";
    let a = format!("%{schema}%parallel_a%");
    let b = format!("%{schema}%parallel_b%");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let count: i64 = client
                .query_one(query, &[&a, &b])
                .await
                .map_err(|e| format!("owned observation {e}"))?
                .get(0);
            if count == 2 {
                return Ok(count as usize);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| "owned blocked observation deadline".to_string())?
}
async fn native_boundary_scenario(
    config: super::live_config::LiveConfig,
) -> Result<NativeBoundaryEvidence, String> {
    use crate::{
        drivers::{driver_trait::PluginManifest, registry},
        plugins::driver::RpcDriver,
    };
    use std::{collections::HashMap, sync::Arc};
    let port = config.port;
    let _registry_guard = registry::REGISTRY_TEST_LOCK.lock().await;
    let (client, connection) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(port)
        .user("postgres")
        .dbname("spatial_fixture")
        .connect(tokio_postgres::NoTls)
        .await
        .map_err(|e| format!("boundary connect {e}"))?;
    let connection_task = tokio::spawn(connection);
    let schema = format!("h3b2b1_{}", uuid::Uuid::new_v4().simple());
    let driver_id = format!("h3b2b1-{}", uuid::Uuid::new_v4());
    let manifest:PluginManifest=serde_json::from_value(json!({"id":driver_id,"name":"Native boundary fixture","version":"0.0.0","description":"Isolated bounded fixture","default_port":port,"capabilities":{"schemas":true,"views":true,"routines":true,"file_based":false}})).map_err(|_|"boundary manifest")?;
    registry::register_driver(
        RpcDriver::new(manifest, config.plugin, None, vec![], HashMap::new()).await?,
    )
    .await;
    let mut observer_client = None;
    let mut observer_task = None;
    let scenario=async {
        let (observer,observer_connection)=tokio_postgres::Config::new().host("127.0.0.1").port(port).user("postgres").dbname("spatial_fixture").connect(tokio_postgres::NoTls).await.map_err(|e|format!("boundary observer connect {e}"))?;
        observer_task=Some(tokio::spawn(observer_connection));observer_client=Some(observer);
        client.batch_execute(&format!("CREATE SCHEMA {schema}; CREATE TABLE {schema}.fat(id integer PRIMARY KEY,payload text,geom geometry(Point,4326)); INSERT INTO {schema}.fat SELECT id,repeat('x',4170000),ST_GeomFromEWKT('SRID=4326;POINT(1 2)') FROM generate_series(1,2) AS id; CREATE TABLE {schema}.parallel_a(id integer PRIMARY KEY,geom geometry(Point,4326)); INSERT INTO {schema}.parallel_a VALUES(1,ST_GeomFromEWKT('SRID=4326;POINT(1 2)')); CREATE TABLE {schema}.parallel_b(LIKE {schema}.parallel_a INCLUDING ALL); INSERT INTO {schema}.parallel_b SELECT * FROM {schema}.parallel_a;")).await.map_err(|e|format!("boundary setup {e}"))?;
        let base=super::support::Harness::new();for connection in base.connections.0.lock().unwrap().values_mut(){connection.driver=driver_id.clone();}
        let params=crate::models::ConnectionParams {driver:driver_id.clone(),host:Some("127.0.0.1".into()),port:Some(port),username:Some("postgres".into()),database:crate::models::DatabaseSelection::Single("spatial_fixture".into()),ssl_mode:Some("disable".into()),..Default::default()};
        let backend=Arc::new(crate::service_registry_driver::RegistryDriverBackend::new(Arc::new(LocalResolver {params})));
        let driver=Arc::new(ObservedRegistrySpatial {inner:super::super::RegistrySpatialDriverBackend::new(backend),started:Default::default(),completed:Default::default(),cancelled:Default::default()});
        super::super::SpatialDataDomain::attach(&base.runtime,driver.clone(),Some(base.domain.store().clone())).map_err(|e|format!("boundary attach {:?}",e.code))?;
        let mut input=fixture_table_request(&schema,"fat","geom");input.input["page_size"]=json!(2);let fat_request=input.request_id.clone();let first_page=base.call(&input).await;
        let next=first_page.page.next_token.clone().ok_or_else(||format!("missing host tail {:?}",first_page.error.as_ref().map(|error|error.code)))?;
        input.request_id=uuid::Uuid::new_v4().to_string();input.input["next_token"]=json!(next);let second_page=base.call(&input).await;
        let (native_bytes,native_features,native_next,fat_calls)={let observations=driver.completed.lock().unwrap();let fat=observations.iter().filter(|observation|observation.table=="fat"&&observation.connection_id=="conn-a"&&observation.request_id==fat_request).collect::<Vec<_>>();let first=fat.first().ok_or("missing native fat response")?;(first.bytes,first.features,first.native_next,fat.len())};
        client.batch_execute(&format!("BEGIN; LOCK TABLE {schema}.parallel_a,{schema}.parallel_b IN ACCESS EXCLUSIVE MODE")).await.map_err(|e|format!("boundary lock {e}"))?;
        let parallel=async {
            let a=fixture_table_request(&schema,"parallel_a","geom");let mut b=fixture_table_request(&schema,"parallel_b","geom");b.request_id=a.request_id.clone();b.connection_id=Some("conn-b".into());let original_request=a.request_id.clone();
            let first=base.runtime.submit(a,super::support::caller()).await;let second=base.runtime.submit(b,super::support::caller()).await;
            let blocked=wait_owned_blocked(observer_client.as_ref().unwrap(),&schema).await?;
            let same_request_connections=driver.started.lock().unwrap().iter().filter(|(connection,request,table)|request==&original_request&&((connection=="conn-a"&&table=="parallel_a")||(connection=="conn-b"&&table=="parallel_b"))).count();
            let cancelled=base.call(&request(Operation::JobCancel,json!({"job_id":first.job_id,"owner_instance_id":"map-owner"}))).await;
            let other_status=base.runtime.jobs.get(second.job_id.as_deref().ok_or("missing second job")?,"map-owner",&super::support::caller()).map_err(|e|format!("boundary job {:?}",e.code))?.status;
            client.batch_execute("ROLLBACK").await.map_err(|e|format!("boundary unlock {e}"))?;
            let first_terminal=terminal(&base.runtime,first.job_id.as_deref().ok_or("missing first job")?).await;
            let second_terminal=terminal(&base.runtime,second.job_id.as_deref().ok_or("missing second job")?).await;
            Ok::<_,String>((blocked,same_request_connections,cancelled,first_terminal,second_terminal,other_status))
        }.await;
        let unlock=client.batch_execute("ROLLBACK").await;unlock.map_err(|e|format!("boundary final unlock {e}"))?;
        let (blocked,same_request_connections,cancel_response,first,second,other_status)=parallel?;
        let cancelled_connections=driver.cancelled.lock().unwrap().iter().map(|(connection,_)|connection.clone()).collect();
        Ok(NativeBoundaryEvidence {pages:vec![first_page,second_page],native_bytes,native_features,native_next,fat_calls,blocked,same_request_connections,cancel_response,first,second,other_status,cancelled_connections})
    }.await;
    let rollback = client.batch_execute("ROLLBACK").await;
    let cleanup = client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .await;
    drop(observer_client.take());
    if let Some(task) = observer_task {
        let _ = task.await;
    }
    registry::unregister_driver(&driver_id).await;
    drop(client);
    let _ = connection_task.await;
    rollback.map_err(|e| format!("boundary rollback {e}"))?;
    cleanup.map_err(|e| format!("boundary cleanup {e}"))?;
    scenario
}
fn assert_native_boundary(actual: Result<NativeBoundaryEvidence, String>) {
    let actual = actual.unwrap();
    assert!(actual.pages.iter().all(|page| page.error.is_none()));
    assert_eq!(actual.pages.len(), 2);
    assert_eq!(actual.native_features, 2);
    assert!(!actual.native_next);
    assert_eq!(actual.fat_calls, 1);
    assert!(actual.native_bytes > super::super::paging::PUBLIC_PAGE_BYTES);
    assert!(actual.native_bytes < 8 * 1024 * 1024);
    assert_eq!(
        actual
            .pages
            .iter()
            .map(|page| page.data["features"].as_array().unwrap().len())
            .sum::<usize>(),
        2
    );
    assert_eq!(
        actual.pages[0].page.resume_mode,
        crate::services::contracts::ResumeMode::Materialized
    );
    assert!(!actual.pages[1].page.has_more);
    assert!(actual
        .pages
        .iter()
        .all(|page| serde_json::to_vec(page).unwrap().len() < 8 * 1024 * 1024));
    assert_eq!(
        actual.pages[0].data["features"][0]["properties"]["payload"]
            .as_str()
            .unwrap()
            .len(),
        4170000
    );
    assert_eq!(
        actual.pages[1].data["features"][0]["properties"]["payload"]
            .as_str()
            .unwrap()
            .len(),
        4170000
    );
    assert_ne!(
        actual.pages[0].data["features"][0]["id"],
        actual.pages[1].data["features"][0]["id"]
    );
    assert_eq!(actual.blocked, 2);
    assert_eq!(actual.same_request_connections, 2);
    assert!(actual.cancel_response.error.is_none());
    assert_eq!(
        actual.first.status,
        crate::services::contracts::JobStatus::Cancelled
    );
    assert!(actual.second.error.is_none());
    assert_eq!(actual.second.data["features"].as_array().unwrap().len(), 1);
    assert_eq!(
        actual.other_status,
        crate::services::contracts::JobStatus::Running
    );
    assert_eq!(actual.cancelled_connections, ["conn-a"]);
}
#[tokio::test]
#[ignore = "requires explicit local PostGIS fixture inputs"]
async fn pg17의_대용량_잔여_페이지와_연결별_취소는_실제_물리_요청을_보존한다() {
    // given
    let config = super::live_config::postgis17();
    // when
    let actual = native_boundary_scenario(config).await;
    // then
    assert_native_boundary(actual);
}
#[tokio::test]
#[ignore = "requires explicit local PostGIS fixture inputs"]
async fn pg18의_대용량_잔여_페이지와_연결별_취소는_실제_물리_요청을_보존한다() {
    // given
    let config = super::live_config::postgis18();
    // when
    let actual = native_boundary_scenario(config).await;
    // then
    assert_native_boundary(actual);
}
