use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::ConnectionParams;
use super::SpatialError;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverContext {
    pub protocol_version: u32,
    pub connection_id: String,
    pub request_id: String,
    #[serde(default = "deadline")]
    pub deadline_ms: u64,
    #[serde(default = "read_only")]
    pub read_only: bool,
    #[serde(default)]
    pub auth: Option<Value>,
    #[serde(default)]
    pub session_handle: Option<String>,
}
fn deadline() -> u64 { 30_000 }
fn read_only() -> bool { true }

impl DriverContext {
    pub fn parse(params: &Value, spatial: bool) -> Result<(Self, ConnectionParams), SpatialError> {
        let context: Self = serde_json::from_value(params.get("driver_context").cloned().unwrap_or(Value::Null))
            .map_err(|_| SpatialError::argument("driver_context is required"))?;
        if context.protocol_version != 1 { return Err(SpatialError::new("PROTOCOL_MISMATCH", "Unsupported driver protocol")); }
        if context.connection_id.is_empty() || context.connection_id.chars().count() > 128
            || context.request_id.is_empty() || context.request_id.chars().count() > 128
            || context.deadline_ms == 0 || context.deadline_ms > 120_000 {
            return Err(SpatialError::argument("Invalid driver context"));
        }
        if spatial && !context.read_only { return Err(SpatialError::new("WRITE_NOT_ALLOWED", "Spatial operations require read-only context")); }
        let connection = params.get("params").filter(|p| p.is_object())
            .ok_or_else(|| SpatialError::argument("Connection params are required"))?;
        let mut connection = ConnectionParams::from_value(connection);
        if let Some(auth) = &context.auth {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct PasswordAuth { kind: String, password: String, identity: String }
            if auth.get("kind").and_then(Value::as_str) != Some("db_password") {
                return Err(SpatialError::new("UNSUPPORTED_OPERATION", "Unsupported PostgreSQL authentication kind"));
            }
            let auth: PasswordAuth = serde_json::from_value(auth.clone()).map_err(|_| SpatialError::argument("Invalid database authentication context"))?;
            if auth.kind != "db_password" || auth.identity.is_empty() || auth.identity.chars().count() > 512 {
                return Err(SpatialError::argument("Invalid database authentication identity"));
            }
            connection.password = Some(auth.password);
        }
        Ok((context, connection))
    }
}

pub fn input<T: for<'de> Deserialize<'de>>(params: &Value) -> Result<T, SpatialError> {
    serde_json::from_value(params.get("input").cloned().unwrap_or(Value::Null))
        .map_err(|_| SpatialError::argument("Invalid operation input"))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TableRef {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: String,
}
impl TableRef {
    pub fn schema(&self) -> &str { self.schema.as_deref().unwrap_or("public") }
    pub fn connection(&self, params: &ConnectionParams) -> Result<ConnectionParams, SpatialError> {
        if self.table.is_empty() || self.table.contains('\0') || self.schema().contains('\0') {
            return Err(SpatialError::argument("Invalid table identifier"));
        }
        let mut connection = params.clone();
        if let Some(database) = &self.database { connection.database = Some(database.clone()); }
        Ok(connection)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RowIdentity { pub columns: Vec<String>, pub values: Vec<Value> }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub west: f64, pub south: f64, pub east: f64, pub north: f64,
    #[serde(default)]
    pub world: bool,
}
impl Viewport {
    pub fn pieces(&self, geography: bool) -> Result<Vec<[f64; 4]>, SpatialError> {
        if ![self.west,self.south,self.east,self.north].iter().all(|v| v.is_finite())
            || !(-180.0..=180.0).contains(&self.west) || !(-180.0..=180.0).contains(&self.east)
            || !(-90.0..=90.0).contains(&self.south) || !(-90.0..=90.0).contains(&self.north)
            || self.south > self.north {
            return Err(SpatialError::argument("Viewport is outside longitude/latitude bounds"));
        }
        let longitude = if self.world { vec![[-180.0,180.0]] }
            else if self.west > self.east { vec![[self.west,180.0],[-180.0,self.east]] }
            else { vec![[self.west,self.east]] };
        let mut pieces = Vec::new();
        for [west,east] in longitude {
            let count = if geography { ((east-west)/90.0).ceil().max(1.0) as usize } else { 1 };
            for index in 0..count {
                pieces.push([west+(east-west)*index as f64/count as f64,self.south,
                    west+(east-west)*(index+1) as f64/count as f64,self.north]);
            }
        }
        Ok(pieces)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeatureLimits {
    #[serde(default = "feature_cap")]
    pub max_features: usize,
    #[serde(default = "coordinate_cap")]
    pub max_coordinates: usize,
    #[serde(default = "byte_cap")]
    pub max_bytes: usize,
}
fn feature_cap() -> usize { 10_000 }
fn coordinate_cap() -> usize { 250_000 }
fn byte_cap() -> usize { 32 * 1024 * 1024 }
impl Default for FeatureLimits {
    fn default() -> Self { Self { max_features: feature_cap(),max_coordinates: coordinate_cap(),max_bytes: byte_cap() } }
}
impl FeatureLimits {
    pub fn validate(&self) -> Result<(), SpatialError> {
        if self.max_features == 0 || self.max_features > feature_cap()
            || self.max_coordinates == 0 || self.max_coordinates > coordinate_cap()
            || self.max_bytes == 0 || self.max_bytes > byte_cap() {
            return Err(SpatialError::argument("Spatial limits exceed the supported bounds"));
        }
        Ok(())
    }

    pub fn validate_export(&self, limit: usize) -> Result<(), SpatialError> {
        if limit == 0 || limit > 1024 * 1024 * 1024
            || self.max_features == 0 || self.max_features > 1_000_000
            || self.max_coordinates == 0 || self.max_coordinates > 10_000_000
            || self.max_bytes == 0 || self.max_bytes > limit {
            return Err(SpatialError::argument("Export limits exceed the supported bounds"));
        }
        Ok(())
    }
}

pub fn positions(geometry: &Value) -> Result<usize, SpatialError> {
    fn coordinates(value: &Value) -> Result<usize, SpatialError> {
        let values = value.as_array().ok_or_else(|| SpatialError::geometry("Invalid GeoJSON coordinates"))?;
        if values.first().is_some_and(Value::is_number) {
            if values.len()!=2 || !values.iter().all(|v| v.as_f64().is_some_and(f64::is_finite)) {
                return Err(SpatialError::geometry("Expected finite XY coordinates"));
            }
            return Ok(1);
        }
        values.iter().try_fold(0usize, |count,value| count.checked_add(coordinates(value)?)
            .ok_or_else(|| SpatialError::geometry("Coordinate count overflow")))
    }
    if geometry.is_null() { return Ok(0); }
    if geometry.get("type").and_then(Value::as_str)==Some("GeometryCollection") {
        return geometry.get("geometries").and_then(Value::as_array)
            .ok_or_else(|| SpatialError::geometry("Invalid GeoJSON collection"))?
            .iter().try_fold(0usize, |count,g| count.checked_add(positions(g)?)
                .ok_or_else(|| SpatialError::geometry("Coordinate count overflow")));
    }
    coordinates(&geometry["coordinates"])
}
