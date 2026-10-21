use std::{collections::HashMap,sync::{Arc,LazyLock,Mutex},time::{Duration,Instant}};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD,Engine};
use futures_util::{pin_mut,TryStreamExt};
use serde::{Deserialize,Serialize};
use serde_json::{json,Map,Value};
use tokio_postgres::{Client,Row};

use super::{catalog::{self,Catalog,TableCatalog},ewkb::SpatialValue,sql,transform::{self,LongitudeMode},
    types::{FeatureLimits,RowIdentity,TableRef,Viewport,positions},SpatialError};

#[derive(Clone,Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct TableInput {
    pub table: TableRef,
    pub column: String,
    pub viewport: Viewport,
    pub source_srid: Option<i32>,
    pub srid_filter: Option<i32>,
    #[serde(default)]
    pub longitude_mode: LongitudeMode,
    #[serde(default)]
    pub skip_invalid: bool,
    pub keyset: Option<RowIdentity>,
    pub next_token: Option<String>,
    #[serde(default="page_size")]
    pub page_size: usize,
    #[serde(default)]
    pub limits: FeatureLimits,
    #[serde(default)]
    pub export_limit_bytes: Option<usize>,
}
fn page_size() -> usize { 1000 }

impl TableInput {
    pub fn validate(&self) -> Result<(), SpatialError> {
        match self.export_limit_bytes {
            Some(limit)=>self.limits.validate_export(limit)?,
            None=>self.limits.validate()?,
        }
        if self.page_size==0 || self.page_size>1000 {return Err(SpatialError::argument("page_size must be between 1 and 1000"));}
        if self.next_token.is_some() && self.keyset.is_some() {return Err(SpatialError::argument("Specify either keyset or next_token"));}
        Ok(())
    }
}

fn snapshot_input(input: &TableInput) -> TableInput {
    let mut effective=input.clone();
    if input.export_limit_bytes.is_some() {
        let display=FeatureLimits::default();
        effective.limits.max_features=effective.limits.max_features.min(display.max_features);
        effective.limits.max_coordinates=effective.limits.max_coordinates.min(display.max_coordinates);
        effective.limits.max_bytes=effective.limits.max_bytes.min(display.max_bytes);
    }
    effective
}

#[derive(Clone)]
struct Feature { feature: Value,reference: Value,coordinates: usize,bytes: usize }
#[derive(Clone,Default)]
struct Counts { features: usize,coordinates: usize,bytes: usize,ordinal: usize,skipped: usize }
#[derive(Clone)]
enum CursorData {
    Keyset { key: RowIdentity,counts: Counts,truncated: bool },
    Snapshot { features: Arc<Vec<Feature>>,offset: usize,errors: Arc<Vec<Value>>,warnings: Arc<Vec<String>>,truncated: bool },
}
#[derive(Clone)]
struct Cursor { connection: String,fingerprint: String,expires: Instant,data: CursorData }
static CURSORS: LazyLock<Mutex<HashMap<String,Cursor>>> = LazyLock::new(||Mutex::new(HashMap::new()));

pub fn invalidate_connection(connection: &str) -> usize {
    let Ok(mut cursors) = CURSORS.lock() else { return 0; };
    let before = cursors.len();
    cursors.retain(|_, cursor| cursor.connection != connection);
    before - cursors.len()
}

fn fingerprint(input: &TableInput) -> Result<String,SpatialError> {
    let mut stable=input.clone();stable.next_token=None;stable.keyset=None;
    serde_json::to_string(&stable).map_err(|_|SpatialError::argument("Cannot encode cursor source"))
}
fn save_cursor(connection: &str,fingerprint: &str,data: CursorData) -> Result<String,SpatialError> {
    let mut cursors=CURSORS.lock().map_err(|_|SpatialError::new("RESOURCE_LIMIT","Cursor store unavailable"))?;
    cursors.retain(|_,cursor|cursor.expires>Instant::now());
    if cursors.len()>=64 { return Err(SpatialError::new("RESOURCE_LIMIT","Cursor store is full")); }
    let retained=cursors.values().filter_map(|cursor|match &cursor.data {
        CursorData::Snapshot { features,.. }=>Some(features.iter().map(|feature|feature.bytes).sum::<usize>()),_=>None,
    }).sum::<usize>();
    let added=match &data { CursorData::Snapshot { features,.. }=>features.iter().map(|feature|feature.bytes).sum(),_=>0 };
    if retained+added>128*1024*1024 { return Err(SpatialError::new("RESOURCE_LIMIT","Snapshot store is full")); }
    let token=uuid::Uuid::new_v4().to_string();
    cursors.insert(token.clone(),Cursor { connection:connection.into(),fingerprint:fingerprint.into(),
        expires:Instant::now()+Duration::from_secs(900),data });
    Ok(token)
}
fn take_cursor(connection: &str,fingerprint: &str,token: &str) -> Result<CursorData,SpatialError> {
    let mut cursors=CURSORS.lock().map_err(|_|SpatialError::new("RESOURCE_LIMIT","Cursor store unavailable"))?;
    let cursor=cursors.get(token).ok_or_else(||SpatialError::new("INVALID_PAGE_TOKEN","Unknown spatial cursor"))?;
    if cursor.connection!=connection || cursor.fingerprint!=fingerprint {
        return Err(SpatialError::new("INVALID_PAGE_TOKEN","Cursor belongs to another connection or viewport"));
    }
    if cursor.expires<=Instant::now() { cursors.remove(token);return Err(SpatialError::new("CURSOR_EXPIRED","Spatial cursor expired")); }
    Ok(cursors.remove(token).unwrap().data)
}

fn feature_id(identity: &RowIdentity) -> Result<String,SpatialError> {
    let bytes=serde_json::to_vec(identity).map_err(|_|SpatialError::argument("Invalid feature identity"))?;
    Ok(format!("pk:{}",URL_SAFE_NO_PAD.encode(bytes)))
}
fn original_row(row: &Row,catalog: &Catalog) -> Result<Map<String,Value>,SpatialError> {
    row.columns().iter().enumerate().map(|(index,column)|
        crate::extract::extract_value_with_catalog(row,index,Some(catalog)).map(|value|(column.name().to_string(),value))).collect()
}
fn identity(properties: &Map<String,Value>,catalog: &TableCatalog) -> Option<RowIdentity> {
    (!catalog.primary_key.is_empty()).then(||RowIdentity { columns:catalog.primary_key.clone(),
        values:catalog.primary_key.iter().map(|name|properties.get(name).cloned().unwrap_or(Value::Null)).collect() })
}

async fn make_feature(client: &Client,catalog: &Catalog,input: &TableInput,properties: Map<String,Value>,
    identity: Option<RowIdentity>,snapshot: &str,ordinal: usize) -> Result<Feature,SpatialError> {
    let wrapper: Option<SpatialValue>=serde_json::from_value(properties.get(&input.column).cloned().unwrap_or(Value::Null))
        .map_err(|_|SpatialError::unsupported("Column is not a spatial scalar"))?;
    let geometry=match wrapper {
        None=>Value::Null,Some(wrapper)=>transform::transform_one(client,catalog,&wrapper,input.source_srid,input.longitude_mode).await?,
    };
    let coordinates=positions(&geometry)?;
    let feature_id=match &identity { Some(key)=>feature_id(key)?,None=>format!("{snapshot}:{ordinal}") };
    let feature=json!({"type":"Feature","id":feature_id,"geometry":geometry,"properties":properties});
    let mut reference=json!({"feature_id":feature_id,"identity":identity,"row_ordinal":ordinal});
    if identity.is_none() { reference["snapshot_id"]=json!(snapshot); }
    let bytes=serde_json::to_vec(&feature).map_err(|_|SpatialError::geometry("Cannot serialize feature"))?.len()
        +serde_json::to_vec(&reference).map_err(|_|SpatialError::geometry("Cannot serialize reference"))?.len();
    if coordinates>input.limits.max_coordinates.min(100_000) || bytes>input.limits.max_bytes.min(8*1024*1024) {
        return Err(SpatialError::new("FEATURE_TOO_LARGE","A complete feature exceeds the page limit"));
    }
    Ok(Feature {feature,reference,coordinates,bytes})
}

fn response(features: &[Feature],next: Option<String>,limits: (bool,Vec<&str>),warnings: Vec<String>,errors: Vec<Value>,materialized: bool) -> Value {
    let has_more=next.is_some();let (truncated,reasons)=limits;
    json!({"features":features.iter().map(|f|&f.feature).collect::<Vec<_>>(),
        "row_references":features.iter().map(|f|&f.reference).collect::<Vec<_>>(),
        "page":{"next_token":next,"has_more":has_more,"resume_mode":if has_more {if materialized {"materialized"} else {"native"}} else {"none"}},
        "limits":{"truncated":truncated,"reasons":reasons},"warnings":warnings,"feature_errors":errors})
}

fn envelope_size(errors: &[Value],warnings: &[String]) -> Result<usize,SpatialError> {
    let mut reserved=vec!["DIMENSIONS_REDUCED_TO_XY".to_string()];
    for warning in warnings {if !reserved.contains(warning) {reserved.push(warning.clone());}}
    let envelope=response(&[],Some("00000000-0000-0000-0000-000000000000".into()),
        (true,vec!["LAYER_LIMIT","SKIPPED_FEATURES"]),reserved,errors.to_vec(),true);
    serde_json::to_vec(&envelope).map(|bytes|bytes.len()).map_err(|_|SpatialError::geometry("Cannot encode page envelope"))
}

fn bounded_response(value: Value,limit: usize) -> Result<Value,SpatialError> {
    if serde_json::to_vec(&value).map_err(|_|SpatialError::geometry("Cannot serialize feature page"))?.len()>limit.min(8*1024*1024) {
        if let Some(token)=value["page"]["next_token"].as_str() {
            if let Ok(mut cursors)=CURSORS.lock(){cursors.remove(token);}
        }
        return Err(SpatialError::new("RESOURCE_LIMIT","Feature page exceeds its serialized byte limit"));
    }
    Ok(value)
}

fn bound_snapshot(features: &mut Vec<Feature>,input: &TableInput,errors: &[Value],warnings: &[String]) -> Result<bool,SpatialError> {
    let overhead=envelope_size(errors,warnings)?;
    if overhead>input.limits.max_bytes.min(8*1024*1024) {
        return Err(SpatialError::new("RESOURCE_LIMIT","Snapshot metadata exceeds its byte limit"));
    }
    let mut total=0usize;let mut page_bytes=overhead;let mut page_coordinates=0usize;let mut page_features=0usize;
    let mut accepted=0;
    for feature in features.iter() {
        if feature.bytes+2+overhead>input.limits.max_bytes.min(8*1024*1024) {
            return Err(SpatialError::new("FEATURE_TOO_LARGE","A feature cannot fit in the page envelope"));
        }
        if page_features>=input.page_size || page_bytes+feature.bytes+2>input.limits.max_bytes.min(8*1024*1024)
            || page_coordinates+feature.coordinates>input.limits.max_coordinates.min(100_000) {
            page_bytes=overhead;page_coordinates=0;page_features=0;
        }
        let size=feature.bytes+2+if page_features==0 {overhead}else{0};
        if total+size>input.limits.max_bytes {break;}
        total+=size;page_bytes+=feature.bytes+2;page_coordinates+=feature.coordinates;page_features+=1;accepted+=1;
    }
    let truncated=accepted<features.len();features.truncate(accepted);Ok(truncated)
}

fn snapshot_page(connection: &str,fingerprint: &str,input: &TableInput,data: &CursorData) -> Result<Value,SpatialError> {
    let CursorData::Snapshot {features,offset,errors,warnings,truncated}=data else {unreachable!()};
    let offset=*offset;let truncated=*truncated;
    let mut end=offset;let mut coordinates=0;let mut bytes=envelope_size(errors,warnings)?;
    while end<features.len() && end-offset<input.page_size {
        let feature=&features[end];
        if coordinates+feature.coordinates>input.limits.max_coordinates.min(100_000)
            || bytes+feature.bytes+2>input.limits.max_bytes.min(8*1024*1024) { break; }
        coordinates+=feature.coordinates;bytes+=feature.bytes+2;end+=1;
    }
    if bytes>input.limits.max_bytes.min(8*1024*1024) {return Err(SpatialError::new("RESOURCE_LIMIT","Page metadata exceeds its byte limit"));}
    if end==offset && end<features.len() {return Err(SpatialError::new("FEATURE_TOO_LARGE","A feature cannot fit in a page"));}
    let more=end<features.len();
    let next=if more {Some(save_cursor(connection,fingerprint,CursorData::Snapshot {
        features:features.clone(),offset:end,errors:errors.clone(),warnings:warnings.clone(),truncated })?)} else {None};
    let mut reasons=Vec::new();if truncated {reasons.push("LAYER_LIMIT");}
    if !errors.is_empty(){reasons.push("SKIPPED_FEATURES");}
    bounded_response(response(&features[offset..end],next,(truncated||!errors.is_empty(),reasons),
        warnings.as_ref().clone(),errors.as_ref().clone(),true),input.limits.max_bytes)
}

pub async fn query_table(client: &Client,catalog: &Catalog,connection: &str,input: &TableInput) -> Result<Value,SpatialError> {
    input.validate()?;
    let fingerprint=fingerprint(input)?;
    let cursor=input.next_token.as_deref().map(|token|take_cursor(connection,&fingerprint,token)).transpose()?;
    if let Some(snapshot @ CursorData::Snapshot {..})=cursor.as_ref() {
        return snapshot_page(connection,&fingerprint,&snapshot_input(input),snapshot);
    }
    let table=catalog::table_columns(client,catalog,input.table.schema(),&input.table.table).await?;
    sql::validate_keyset(&table)?;
    let column=table.columns.iter().find(|column|column.column==input.column)
        .ok_or_else(||SpatialError::unsupported("Column is not a PostGIS geometry/geography scalar"))?;
    if table.primary_key.is_empty() && input.keyset.is_some() {return Err(SpatialError::argument("Keyset requires a primary key"));}
    let effective=table.primary_key.is_empty().then(||snapshot_input(input));
    let input=effective.as_ref().unwrap_or(input);
    let (mut key,mut counts,mut truncated)=match cursor {
        Some(CursorData::Keyset {key,counts,truncated})=>(Some(key),counts,truncated),
        None=>(input.keyset.clone(),Counts::default(),false),_=>unreachable!(),
    };
    let remaining=input.limits.max_features.saturating_sub(counts.features);
    if remaining==0 {return bounded_response(response(&[],None,(true,vec!["LAYER_LIMIT"]),vec![],vec![],false),input.limits.max_bytes);}
    let count=if table.primary_key.is_empty(){remaining+1}else{input.page_size.min(remaining)+1};
    let sql=sql::viewport_query(catalog,&table,column,input,key.as_ref(),count)?;
    let statement=client.prepare_typed(&sql.text,&sql.types).await.map_err(SpatialError::database)?;
    catalog.validate_columns(statement.columns())?;
    let mut rows=Vec::new();let mut row_bytes=0usize;let mut raw_limit=false;
    {
        let stream=client.query_raw(&statement,sql.references()).await.map_err(SpatialError::database)?;
        pin_mut!(stream);
        while let Some(row)=stream.try_next().await.map_err(SpatialError::database)? {
            let properties=original_row(&row,catalog)?;
            let size=serde_json::to_vec(&properties).map_err(|_|SpatialError::geometry("Cannot serialize source row"))?.len();
            if size>32*1024*1024 {return Err(SpatialError::new("FEATURE_TOO_LARGE","A source row exceeds the snapshot limit"));}
            if row_bytes+size>32*1024*1024 {raw_limit=true;break;}
            row_bytes+=size;rows.push(properties);
        }
    }
    let snapshot=uuid::Uuid::new_v4().to_string();
    let mut features=Vec::new();let mut errors=Vec::new();let mut warnings=Vec::new();
    if table.primary_key.is_empty() && input.export_limit_bytes.is_some() {
        warnings.push("PRIMARY_KEYLESS_EXPORT_USES_DISPLAY_LIMITS".to_string());
    }
    let mut coordinates=0usize;let mut bytes=0usize;let mut has_more=raw_limit;let mut counted_envelope=false;
    if table.primary_key.is_empty() && raw_limit {truncated=true;}
    for (raw_ordinal,properties) in rows.into_iter().enumerate() {
        let row_identity=identity(&properties,&table);
        if row_identity.as_ref().is_some_and(|identity|identity.values.iter().any(|value|value.is_null()||value.is_array()||value.is_object())) {
            return Err(SpatialError::new("UNSUPPORTED_OPERATION","Primary key value cannot be represented by a stable scalar keyset binding"));
        }
        if !table.primary_key.is_empty() && (features.len()>=input.page_size || raw_ordinal>=count-1) {
            has_more=true;break;
        }
        if input.skip_invalid {client.batch_execute("SAVEPOINT spatial_feature").await.map_err(SpatialError::database)?;}
        let transformed=make_feature(client,catalog,input,properties,row_identity.clone(),&snapshot,counts.ordinal).await;
        let feature=match transformed {
            Ok(feature)=>{
                if input.skip_invalid {client.batch_execute("RELEASE SAVEPOINT spatial_feature").await.map_err(SpatialError::database)?;}
                feature
            }
            Err(error) if input.skip_invalid && matches!(error.code.as_str(),"UNSUPPORTED_TYPE"|"INVALID_GEOMETRY"|"FEATURE_TOO_LARGE")=>{
                client.batch_execute("ROLLBACK TO SAVEPOINT spatial_feature; RELEASE SAVEPOINT spatial_feature").await.map_err(SpatialError::database)?;
                errors.push(json!({"index":counts.ordinal,"code":error.code,"message":error.message}));
                counts.ordinal+=1;counts.skipped+=1;key=row_identity;continue;
            }
            Err(error)=>return Err(error),
        };
        let overhead=envelope_size(&errors,&warnings)?;
        if feature.bytes+2+overhead>input.limits.max_bytes.min(8*1024*1024) {
            return Err(SpatialError::new("FEATURE_TOO_LARGE","A complete feature cannot fit in the page envelope"));
        }
        let addition=feature.bytes+2+if !table.primary_key.is_empty() && !counted_envelope {overhead}else{0};
        if counts.features+1>input.limits.max_features || counts.coordinates+feature.coordinates>input.limits.max_coordinates
            || counts.bytes+addition>input.limits.max_bytes {
            truncated=true;break;
        }
        if !table.primary_key.is_empty() && (coordinates+feature.coordinates>100_000 || bytes+feature.bytes+2+overhead>input.limits.max_bytes.min(8*1024*1024)) {
            if features.is_empty(){return Err(SpatialError::new("FEATURE_TOO_LARGE","A complete feature cannot fit in the page envelope"));}
            has_more=true;break;
        }
        if feature.feature["properties"][&input.column]["dimensions"].as_str().is_some_and(|d|d!="XY")
            && !warnings.iter().any(|w|w=="DIMENSIONS_REDUCED_TO_XY") {
            warnings.push("DIMENSIONS_REDUCED_TO_XY".to_string());
        }
        counts.features+=1;counts.coordinates+=feature.coordinates;counts.bytes+=addition;counts.ordinal+=1;counted_envelope=true;
        coordinates+=feature.coordinates;bytes+=feature.bytes+2;key=row_identity;features.push(feature);
    }
    if table.primary_key.is_empty() {
        truncated|=bound_snapshot(&mut features,input,&errors,&warnings)?;
        return snapshot_page(connection,&fingerprint,input,&CursorData::Snapshot {features:Arc::new(features),offset:0,errors:Arc::new(errors),warnings:Arc::new(warnings),truncated});
    }
    if envelope_size(&errors,&warnings)?>input.limits.max_bytes.min(8*1024*1024) {return Err(SpatialError::new("RESOURCE_LIMIT","Page metadata exceeds its byte limit"));}
    if has_more && (counts.features>=input.limits.max_features || counts.coordinates>=input.limits.max_coordinates
        || counts.bytes>=input.limits.max_bytes) {has_more=false;truncated=true;}
    let skipped=counts.skipped;
    let next=if has_more {Some(save_cursor(connection,&fingerprint,CursorData::Keyset {
        key:key.ok_or_else(||SpatialError::new("FEATURE_TOO_LARGE","No feature can fit in a page"))?,counts,truncated })?)} else {None};
    let mut reasons=Vec::new();if truncated{reasons.push("LAYER_LIMIT");}
    if skipped>0{reasons.push("SKIPPED_FEATURES");}
    bounded_response(response(&features,next,(truncated||skipped>0,reasons),warnings,errors,false),input.limits.max_bytes)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadInput {pub table: TableRef,pub identity: RowIdentity}

pub async fn read_feature(client: &Client,catalog: &Catalog,input: &ReadInput) -> Result<Value,SpatialError> {
    let table=catalog::table_columns(client,catalog,input.table.schema(),&input.table.table).await?;
    let mut sql=sql::Sql::new();let values=sql::identity_values(&mut sql,&table,&input.identity)?;
    let predicate=table.primary_key.iter().zip(values).map(|(column,value)|format!("{}={value}",crate::utils::identifiers::quote_identifier(column)))
        .collect::<Vec<_>>().join(" AND ");
    sql.text=format!("SELECT * FROM {} WHERE {predicate}",crate::utils::identifiers::qualified(input.table.schema(),&input.table.table));
    let statement=client.prepare_typed(&sql.text,&sql.types).await.map_err(SpatialError::database)?;
    catalog.validate_columns(statement.columns())?;
    let row=client.query_opt(&statement,&sql.references()).await.map_err(SpatialError::database)?;
    Ok(json!({"row":row.as_ref().map(|row|original_row(row,catalog)).transpose()?,"identity":input.identity,"source":"current_row"}))
}
