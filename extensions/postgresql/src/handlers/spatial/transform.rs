use serde::Deserialize;
use serde_json::{json, Value};
use tokio_postgres::Client;

use super::{catalog::Catalog, ewkb::SpatialValue, types::positions, SpatialError};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformInput {
    pub values: Vec<Option<SpatialValue>>,
    pub source_srid: Option<i32>,
    #[serde(default)]
    pub longitude_mode: LongitudeMode,
    #[serde(default)]
    pub skip_invalid: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LongitudeMode { #[default] Preserve, Shortest }

pub fn validate_source(value: &SpatialValue, source: Option<i32>) -> Result<i32, SpatialError> {
    if source.is_some_and(|s| !(1..=999_999).contains(&s)) {
        return Err(SpatialError::argument("source_srid must be between 1 and 999999"));
    }
    let (_, info) = value.bytes()?;
    if !info.supported { return Err(SpatialError::unsupported("Curves, TIN and polyhedral surfaces cannot be displayed")); }
    if info.invalid_xy { return Err(SpatialError::geometry("Non-finite XY coordinates cannot be displayed")); }
    if let Some(srid) = info.srid {
        if source.is_some_and(|source| source != srid) {
            return Err(SpatialError::argument("source_srid conflicts with the original SRID"));
        }
        Ok(srid)
    } else { source.ok_or_else(|| SpatialError::new("SRID_REQUIRED", "An explicit source_srid is required")) }
}

pub async fn transform_values(client: &Client, catalog: &Catalog, input: &TransformInput) -> Result<Value, SpatialError> {
    if input.values.len() > 128 { return Err(SpatialError::argument("Transform batches cannot exceed 128 values")); }
    let mut geometries = Vec::with_capacity(input.values.len());
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut bytes = 0usize;
    let mut coordinates = 0usize;
    for (index,value) in input.values.iter().enumerate() {
        if input.skip_invalid { client.batch_execute("SAVEPOINT spatial_transform").await.map_err(SpatialError::database)?; }
        let result = match value {
            None => Ok(Value::Null),
            Some(value) => transform_one(client,catalog,value,input.source_srid,input.longitude_mode).await,
        };
        let result = result.and_then(|geometry| {
            let n = positions(&geometry)?;
            let size = serde_json::to_vec(&geometry).map_err(|_| SpatialError::geometry("Cannot encode GeoJSON"))?.len();
            if n > 100_000 || size > 8*1024*1024 {
                return Err(SpatialError::new("FEATURE_TOO_LARGE", "A geometry exceeds the page limit"));
            }
            if coordinates+n>100_000 || bytes+size>8*1024*1024 {
                return Err(SpatialError::new("RESOURCE_LIMIT", "Transform batch exceeds the page limit"));
            }
            coordinates += n; bytes += size;
            Ok(geometry)
        });
        match result {
            Ok(geometry) => {
                if input.skip_invalid { client.batch_execute("RELEASE SAVEPOINT spatial_transform").await.map_err(SpatialError::database)?; }
                if value.as_ref().is_some_and(|value| value.dimensions!="XY")
                    && !warnings.contains(&"DIMENSIONS_REDUCED_TO_XY") {
                    warnings.push("DIMENSIONS_REDUCED_TO_XY");
                }
                geometries.push(geometry);
            }
            Err(error) if input.skip_invalid && matches!(error.code.as_str(), "UNSUPPORTED_TYPE"|"INVALID_GEOMETRY"|"FEATURE_TOO_LARGE") => {
                client.batch_execute("ROLLBACK TO SAVEPOINT spatial_transform; RELEASE SAVEPOINT spatial_transform")
                    .await.map_err(SpatialError::database)?;
                errors.push(json!({ "index": index, "code": error.code, "message": error.message }));
                geometries.push(Value::Null);
            }
            Err(error) => return Err(error),
        }
    }
    let result=json!({ "geometries": geometries,"feature_errors": errors,"warnings": warnings,
        "limits": { "truncated": !errors.is_empty(), "reasons": if errors.is_empty() { vec![] } else { vec!["SKIPPED_FEATURES"] } } });
    if serde_json::to_vec(&result).map_err(|_|SpatialError::geometry("Cannot encode transform result"))?.len()>8*1024*1024 {
        return Err(SpatialError::new("RESOURCE_LIMIT","Transform result exceeds the byte limit"));
    }
    Ok(result)
}

pub async fn transform_one(client: &Client, catalog: &Catalog, value: &SpatialValue,
    source: Option<i32>, mode: LongitudeMode) -> Result<Value, SpatialError> {
    if value.value.len() > 12*1024*1024 { return Err(SpatialError::new("FEATURE_TOO_LARGE", "EWKB exceeds the input limit")); }
    let (bytes, info) = value.bytes()?;
    if info.positions > 100_000 || bytes.len() > 8*1024*1024 {
        return Err(SpatialError::new("FEATURE_TOO_LARGE", "EWKB exceeds the page limit"));
    }
    let source = validate_source(value,source)?;
    let f = |name| catalog.function(name);
    let decoded = format!("{}($1::bytea)",f("st_geomfromewkb"));
    let g = format!("CASE WHEN {}({decoded})=0 THEN {}({decoded},$2::integer) ELSE {decoded} END",f("st_srid"),f("st_setsrid"));
    let geography = value.native_type=="geography";
    let type_name = format!("{}.geography", crate::utils::identifiers::quote_identifier(&catalog.schema));
    let display = if geography {
        format!("{}({}(({}(g))::{} ,100000.0)::{}.geometry,4326)",f("st_transform"),f("st_segmentize"),f("st_force2d"),type_name,
            crate::utils::identifiers::quote_identifier(&catalog.schema))
    } else { format!("{}({}(g),4326)",f("st_transform"),f("st_force2d")) };
    let polar = if geography {
        format!("CASE WHEN {}(g) IN ('ST_Polygon','ST_MultiPolygon') THEN \
            {}(g::{type_name}) >= 255032810862044.0 OR \
            {}(g::{type_name},{}({}(0,90),4326)::{type_name}) OR \
            {}(g::{type_name},{}({}(0,-90),4326)::{type_name}) ELSE false END",
            f("st_geometrytype"),f("st_area"),f("st_intersects"),f("st_setsrid"),f("st_makepoint"),
            f("st_intersects"),f("st_setsrid"),f("st_makepoint"))
    } else { "false".into() };
    let sql = format!("WITH original AS (SELECT {g} AS g), display AS \
        (SELECT {}({display}) AS d,{}(g) AS valid,{polar} AS polar FROM original), oriented AS \
        (SELECT CASE WHEN {}(d) IN ('ST_Polygon','ST_MultiPolygon') THEN {}(d) ELSE d END AS d,valid,polar FROM display) \
        SELECT valid,polar,{}(d,15,0)::jsonb,{}(d) FROM oriented",
        f("st_force2d"),f("st_isvalid"),f("st_geometrytype"),f("st_forcepolygonccw"),f("st_asgeojson"),f("st_asewkb"));
    let row = client.query_one(&sql,&[&bytes,&source]).await.map_err(SpatialError::database)?;
    if row.get::<_,bool>(1) { return Err(SpatialError::unsupported("Hemisphere and polar geography polygons cannot be displayed")); }
    if !row.get::<_,bool>(0) { return Err(SpatialError::geometry("Invalid geometry; no automatic repair was applied")); }
    let mut geometry: Value = row.get(2);
    orient_polygons(&mut geometry);
    if !geography && mode==LongitudeMode::Preserve { return Ok(geometry); }
    let display_bytes: Vec<u8> = row.get(3);
    let sql = format!("SELECT ({}({}($1::bytea))).geom AS g",f("st_dump"),f("st_geomfromewkb"));
    let components = client.query(&sql,&[&display_bytes]).await.map_err(SpatialError::database)?;
    let mut transformed = Vec::new();
    for component in components {
        let raw = crate::extract::extract_value_checked(&component,0)?;
        let component: SpatialValue = serde_json::from_value(raw).map_err(|_| SpatialError::geometry("Invalid display component"))?;
        let (component_bytes,_) = component.bytes()?;
        let json_sql = format!("SELECT {}({}($1::bytea),15,0)::jsonb",f("st_asgeojson"),f("st_geomfromewkb"));
        let component_json: Value = client.query_one(&json_sql,&[&component_bytes]).await.map_err(SpatialError::database)?.get(0);
        let (dateline,greenwich) = component_edges(&component_json);
        if dateline && greenwich { return Err(SpatialError::unsupported("A connected component requires ambiguous Greenwich/dateline normalization")); }
        let shifted = if dateline { format!("{}(g)",f("st_shiftlongitude")) } else { "g".into() };
        let sql = format!("SELECT {}({}({shifted},180,-360),15,0)::jsonb \
            FROM (SELECT {}($1::bytea) AS g) component",f("st_asgeojson"),f("st_wrapx"),f("st_geomfromewkb"));
        let mut normalized: Value = client.query_one(&sql,&[&component_bytes]).await.map_err(SpatialError::database)?.get(0);
        orient_polygons(&mut normalized);
        transformed.push(normalized);
    }
    if transformed.len()==1 { Ok(transformed.remove(0)) }
    else if transformed.is_empty() { Ok(geometry) }
    else { Ok(json!({ "type": "GeometryCollection", "geometries": transformed })) }
}

fn orient_polygons(geometry: &mut Value) {
    fn polygon(value: &mut Value) {
        let Some(rings)=value.as_array_mut() else {return;};
        for (index,ring) in rings.iter_mut().enumerate() {
            let Some(points)=ring.as_array_mut() else {continue;};
            let area=points.windows(2).map(|edge| {
                let a=&edge[0];let b=&edge[1];
                a[0].as_f64().unwrap_or(0.0)*b[1].as_f64().unwrap_or(0.0)
                    -b[0].as_f64().unwrap_or(0.0)*a[1].as_f64().unwrap_or(0.0)
            }).sum::<f64>();
            if (index==0 && area<0.0)||(index>0 && area>0.0) {points.reverse();}
        }
    }
    match geometry["type"].as_str() {
        Some("Polygon")=>polygon(&mut geometry["coordinates"]),
        Some("MultiPolygon")=>if let Some(polygons)=geometry["coordinates"].as_array_mut(){
            for value in polygons {polygon(value);}
        },
        Some("GeometryCollection")=>if let Some(geometries)=geometry["geometries"].as_array_mut(){
            for geometry in geometries {orient_polygons(geometry);}
        },
        _=>{},
    }
}

fn component_edges(geometry: &Value) -> (bool,bool) {
    fn visit(value: &Value, flags: &mut (bool,bool)) {
        let Some(values) = value.as_array() else { return; };
        if values.first().is_some_and(|v| v.as_array().is_some_and(|p| p.first().is_some_and(Value::is_number))) {
            for edge in values.windows(2) {
                let a = edge[0][0].as_f64().unwrap_or(0.0);
                let b = edge[1][0].as_f64().unwrap_or(0.0);
                if (a-b).abs()>180.0 { flags.0 = true; }
                else if a*b<0.0 { flags.1 = true; }
            }
        } else { for value in values { visit(value,flags); } }
    }
    let mut flags = (false,false);
    visit(&geometry["coordinates"],&mut flags);
    flags
}
