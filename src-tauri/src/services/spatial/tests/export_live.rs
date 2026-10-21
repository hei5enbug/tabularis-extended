use super::super::{
    RegistrySpatialDriverBackend, SpatialDataDomain, SpatialDriverBackend, SpatialExportDomain,
};
use super::{export::SinkFactory, spatial_support, support};
use crate::{
    drivers::{
        driver_trait::{PluginManifest, ServiceRpcMethod},
        registry,
    },
    plugins::driver::RpcDriver,
    services::{
        backend::{DriverBackend, DriverContext, SavedConnection},
        contracts::{Operation, ServiceResponse},
        error::ServiceResult,
        results::{ResultKind, ResultSet},
        security::digest,
    },
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc, Mutex},
};

struct Resolver {
    params: crate::models::ConnectionParams,
}
#[async_trait]
impl crate::service_registry_driver::ServiceConnectionResolver for Resolver {
    async fn params(
        &self,
        connection: &SavedConnection,
    ) -> ServiceResult<crate::models::ConnectionParams> {
        let mut params = self.params.clone();
        params.driver = connection.driver.clone();
        params.connection_id = Some(connection.id.clone());
        Ok(params)
    }
}
struct ObservedDriver {
    native: Arc<RegistrySpatialDriverBackend>,
    methods: Mutex<Vec<ServiceRpcMethod>>,
}
#[async_trait]
impl SpatialDriverBackend for ObservedDriver {
    async fn call(
        &self,
        connection: &SavedConnection,
        method: ServiceRpcMethod,
        input: Value,
        context: DriverContext,
    ) -> ServiceResult<Value> {
        self.methods.lock().unwrap().push(method);
        self.native.call(connection, method, input, context).await
    }
    async fn cancel(&self, connection: &str, request: &str) -> ServiceResult<()> {
        self.native.cancel(connection, request).await
    }
}
struct Evidence {
    explicit: ServiceResponse,
    default: ServiceResponse,
    query: ServiceResponse,
    explicit_collection: Value,
    default_collection: Value,
    query_collection: Value,
    explicit_checksum: String,
    explicit_bytes: usize,
    original_before: Value,
    original_after: Value,
    counter_before: i64,
    counter_after: i64,
    sql_calls: usize,
    methods: Vec<ServiceRpcMethod>,
    original_id: String,
}
fn export_request(source: Value, limit: Option<u64>) -> crate::services::contracts::ServiceRequest {
    let mut input = json!({"source":source,"format":"geojson","filename":"fixture.geojson"});
    if let Some(limit) = limit {
        input["export_limit_bytes"] = json!(limit);
    }
    let mut request = spatial_support::request(Operation::SpatialExport, input);
    request.deadline_ms = 120_000;
    request
}
async fn postgres_export(config: super::live_config::LiveConfig) -> Result<Evidence, String> {
    let port = config.port;
    let _registry_guard = registry::REGISTRY_TEST_LOCK.lock().await;
    let (client, connection) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(port)
        .user("postgres")
        .dbname("spatial_fixture")
        .connect(tokio_postgres::NoTls)
        .await
        .map_err(|_| "export fixture connect")?;
    let task = tokio::spawn(connection);
    let schema = format!("h3b2b2_{}", uuid::Uuid::new_v4().simple());
    let driver_id = format!("h3b2b2-{}", uuid::Uuid::new_v4());
    let manifest:PluginManifest=serde_json::from_value(json!({"id":driver_id,"name":"Export fixture","version":"0.0.0","description":"Isolated export fixture","default_port":port,"capabilities":{"schemas":true,"views":true,"routines":true,"file_based":false}})).map_err(|_|"export manifest")?;
    registry::register_driver(
        RpcDriver::new(manifest, config.plugin, None, vec![], HashMap::new()).await?,
    )
    .await;
    let scenario=async {
        client.batch_execute(&format!("CREATE SCHEMA {schema}; CREATE TABLE {schema}.large(id integer PRIMARY KEY,payload text,geom geometry(Point,4326)); INSERT INTO {schema}.large SELECT id,repeat('x',3072),ST_GeomFromEWKT('SRID=4326;POINT(1 2)') FROM generate_series(1,11001) AS id; CREATE SEQUENCE {schema}.ticks; CREATE TABLE {schema}.projected(id integer PRIMARY KEY,note text,geom geometry(PointZ,3857)); INSERT INTO {schema}.projected VALUES(1,'old',ST_Transform(ST_GeomFromEWKT('SRID=4326;POINT Z(10 10 7)'),3857)),(2,'null',NULL),(3,'empty',ST_GeomFromEWKT('SRID=3857;POINT Z EMPTY'));")).await.map_err(|_|"export fixture setup")?;
        let base=support::Harness::new();base.connections.0.lock().unwrap().get_mut("conn-a").unwrap().driver=driver_id.clone();
        let params=crate::models::ConnectionParams {driver:driver_id.clone(),host:Some("127.0.0.1".into()),port:Some(port),username:Some("postgres".into()),database:crate::models::DatabaseSelection::Single("spatial_fixture".into()),ssl_mode:Some("disable".into()),..Default::default()};
        let backend=Arc::new(crate::service_registry_driver::RegistryDriverBackend::new(Arc::new(Resolver {params})));
        let driver=Arc::new(ObservedDriver {native:RegistrySpatialDriverBackend::new(backend.clone()),methods:Default::default()});
        let data=SpatialDataDomain::attach(&base.runtime,driver.clone(),Some(base.domain.store().clone())).map_err(|error|format!("export data {:?}",error.code))?;
        let sink=SinkFactory::new();SpatialExportDomain::attach(&base.runtime,data,sink.clone()).map_err(|error|format!("export attach {:?}",error.code))?;
        let source=json!({"kind":"table","table":{"database":"spatial_fixture","schema":schema,"table":"large"},"column":"geom","viewport":{"west":-180,"east":180,"south":-85,"north":85},"source_srid":4326,"longitude_mode":"preserve","skip_invalid":false});
        let explicit=base.call(&export_request(source.clone(),Some(64*1024*1024))).await;
        if let Some(error)=&explicit.error {return Err(format!("explicit {:?}/{:?}",error.code,error.outcome));}
        let default=base.call(&export_request(source,None)).await;
        if let Some(error)=&default.error {return Err(format!("default {:?}/{:?}",error.code,error.outcome));}
        let saved=base.connections.0.lock().unwrap()["conn-a"].clone();let seed=spatial_support::request(Operation::QueryExecute,json!({"language":"sql","text":format!("SELECT geom AS duplicate,note AS duplicate,nextval('{schema}.ticks') AS once FROM {schema}.projected ORDER BY id"),"parameters":[]}));
        let context=DriverContext {connection_id:"conn-a".into(),request_id:seed.request_id.clone(),deadline_ms:30_000,signal:Default::default(),transient_auth:None,read_only:false,session_handle:None};
        let mut snapshot=backend.execute(&saved,&seed,context).await.map_err(|error|format!("export seed {:?}",error.code))?.snapshot.ok_or("export snapshot missing")?;
        snapshot.sets.insert(0,ResultSet {kind:ResultKind::Tabular,columns:vec![json!({"name":"unused"})],rows:vec![]});let binding=support::binding();snapshot.revision=binding.result_revision.clone();
        let original_before=serde_json::to_value(&snapshot).map_err(|_|"export snapshot serialize")?;let id=base.runtime.results.insert(binding.clone(),snapshot).map_err(|error|format!("export snapshot store {:?}",error.code))?;
        let counter_before=client.query_one(&format!("SELECT last_value FROM {schema}.ticks"),&[]).await.map_err(|_|"export counter before")?.get(0);
        let query=base.call(&export_request(json!({"kind":"query_result","result_id":id,"result_set_index":1,"column_index":0,"source_srid":3857,"longitude_mode":"preserve","skip_invalid":false}),Some(64*1024*1024))).await;
        if let Some(error)=&query.error {return Err(format!("query {:?}/{:?}",error.code,error.outcome));}
        let counter_after=client.query_one(&format!("SELECT last_value FROM {schema}.ticks"),&[]).await.map_err(|_|"export counter after")?.get(0);
        let original_after=serde_json::to_value(base.runtime.results.snapshot(&id,&binding).map_err(|error|format!("export original {:?}",error.code))?).map_err(|_|"export original serialize")?;
        let state=sink.state.lock().unwrap();let explicit_collection=serde_json::from_slice(&state.published[0]).map_err(|_|"explicit geojson")?;let default_collection=serde_json::from_slice(&state.published[1]).map_err(|_|"default geojson")?;let query_collection=serde_json::from_slice(&state.published[2]).map_err(|_|"query geojson")?;
        let methods=driver.methods.lock().unwrap().clone();
        Ok(Evidence {explicit,default,query,explicit_collection,default_collection,query_collection,explicit_checksum:digest(&state.published[0]),explicit_bytes:state.published[0].len(),original_before,original_after,counter_before,counter_after,sql_calls:base.driver.0.load(Ordering::SeqCst),methods,original_id:id})
    }.await;
    let cleanup = client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .await;
    registry::unregister_driver(&driver_id).await;
    drop(client);
    let _ = task.await;
    cleanup.map_err(|_| "export fixture cleanup")?;
    scenario
}

#[tokio::test]
#[ignore = "requires explicit local PostGIS fixture inputs"]
async fn pg18의_명시_export는_일만행과_삼십이_mib를_넘고_원본_sql을_다시_실행하지_않는다() {
    // given
    let fixture = postgres_export;
    let config = super::live_config::postgis18();
    // when
    let actual = fixture(config).await.unwrap();
    // then
    assert_eq!(actual.explicit.data["feature_count"], 11001);
    assert_eq!(actual.explicit.data["coordinate_count"], 11001);
    assert!(actual.explicit_bytes > 32 * 1024 * 1024);
    assert_eq!(actual.explicit.data["bytes"], actual.explicit_bytes);
    assert_eq!(actual.explicit.data["checksum"], actual.explicit_checksum);
    assert_eq!(actual.explicit.data["truncated"], false);
    assert_eq!(actual.explicit_collection["type"], "FeatureCollection");
    assert_eq!(
        actual.explicit_collection["features"]
            .as_array()
            .unwrap()
            .len(),
        11001
    );
    assert_eq!(
        actual.explicit_collection["features"][0]["id"]
            .as_str()
            .unwrap()
            .len(),
        67
    );
    assert_eq!(actual.default.data["truncated"], true);
    assert!(actual
        .default
        .limits
        .reasons
        .contains(&"LAYER_LIMIT".into()));
    assert!(actual.default.data["feature_count"].as_u64().unwrap() <= 10000);
    assert_eq!(actual.default_collection["type"], "FeatureCollection");
    assert_eq!(
        actual.default_collection["features"]
            .as_array()
            .unwrap()
            .len() as u64,
        actual.default.data["feature_count"].as_u64().unwrap()
    );
    assert_eq!(actual.query.data["feature_count"], 3);
    assert!(
        (actual.query_collection["features"][0]["geometry"]["coordinates"][0]
            .as_f64()
            .unwrap()
            - 10.0)
            .abs()
            < 1e-9
    );
    assert!(
        (actual.query_collection["features"][0]["geometry"]["coordinates"][1]
            .as_f64()
            .unwrap()
            - 10.0)
            .abs()
            < 1e-9
    );
    assert_eq!(
        actual.query_collection["features"][1]["geometry"],
        Value::Null
    );
    assert_eq!(
        actual.query_collection["features"][2]["geometry"],
        Value::Null
    );
    assert_eq!(
        actual.query_collection["features"][0]["id"],
        format!("{}:1:0", actual.original_id)
    );
    assert_eq!(
        actual.query_collection["features"][0]["properties"]["columns"][1],
        json!({"name":"duplicate","index":1,"value":"old"})
    );
    assert!(actual
        .query
        .warnings
        .contains(&"DIMENSIONS_REDUCED_TO_XY".into()));
    assert_eq!(actual.original_before, actual.original_after);
    assert_eq!(actual.counter_before, 3);
    assert_eq!(actual.counter_after, 3);
    assert_eq!(actual.sql_calls, 0);
    assert!(actual.methods.iter().all(|method| matches!(
        method,
        ServiceRpcMethod::SpatialQueryTable | ServiceRpcMethod::SpatialTransformValues
    )));
}
