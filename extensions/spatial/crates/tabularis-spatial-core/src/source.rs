use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::{CoreError, ErrorCode, SpatialValue, MAX_SAFE_INTEGER};

pub const MERCATOR_MAX_LATITUDE: f64 = 85.05112878;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LongitudeMode { #[default] Preserve, Shortest }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableRef {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: String,
}

impl TableRef {
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.table.is_empty() || self.database.as_ref().is_some_and(String::is_empty) || self.schema.as_ref().is_some_and(String::is_empty) {
            return Err(CoreError::new(ErrorCode::InvalidArgument, "Table identifiers must be nonempty"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowIdentity {
    pub columns: Vec<String>,
    pub values: Vec<Value>,
}

impl RowIdentity {
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.columns.is_empty() || self.columns.len() != self.values.len() || self.columns.iter().any(String::is_empty) {
            return Err(CoreError::new(ErrorCode::InvalidArgument, "Row identity requires matching nonempty columns and values"));
        }
        let mut seen = std::collections::HashSet::new();
        if self.columns.iter().any(|column| !seen.insert(column)) {
            return Err(CoreError::new(ErrorCode::InvalidArgument, "Row identity columns must be unique"));
        }
        Ok(())
    }
}

pub fn query_feature_id(result_id: &str, result_set_index: u64, row_ordinal: u64) -> Result<String, CoreError> {
    if result_id.is_empty() || result_id.chars().count() > 128 || result_set_index > MAX_SAFE_INTEGER || row_ordinal > MAX_SAFE_INTEGER {
        return Err(CoreError::new(ErrorCode::InvalidArgument, "Invalid result identity"));
    }
    Ok(format!("{result_id}:{result_set_index}:{row_ordinal}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub west: f64,
    pub east: f64,
    pub south: f64,
    pub north: f64,
    #[serde(default)]
    pub world: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongitudePiece { pub west: f64, pub east: f64 }

impl Default for Viewport {
    fn default() -> Self { Self { west: -180.0, east: 180.0, south: -90.0, north: 90.0, world: true } }
}

impl Viewport {
    pub fn validate(&self) -> Result<(), CoreError> {
        if !self.west.is_finite() || !self.east.is_finite() || !self.south.is_finite() || !self.north.is_finite()
            || !(-180.0..=180.0).contains(&self.west) || !(-180.0..=180.0).contains(&self.east)
            || !(-90.0..=90.0).contains(&self.south) || !(-90.0..=90.0).contains(&self.north) || self.south > self.north {
            return Err(CoreError::new(ErrorCode::InvalidArgument, "Invalid WGS84 viewport bounds"));
        }
        Ok(())
    }

    pub fn longitude_pieces(&self) -> Result<Vec<LongitudePiece>, CoreError> {
        self.validate()?;
        Ok(if self.world { vec![LongitudePiece { west: -180.0, east: 180.0 }] }
        else if self.west > self.east { vec![LongitudePiece { west: self.west, east: 180.0 }, LongitudePiece { west: -180.0, east: self.east }] }
        else { vec![LongitudePiece { west: self.west, east: self.east }] })
    }

    pub fn is_full_globe(&self) -> bool { self.world && self.south == -90.0 && self.north == 90.0 }

    pub fn mercator_camera(&self) -> Result<Self, CoreError> {
        self.validate()?;
        Ok(Self { south: self.south.clamp(-MERCATOR_MAX_LATITUDE, MERCATOR_MAX_LATITUDE), north: self.north.clamp(-MERCATOR_MAX_LATITUDE, MERCATOR_MAX_LATITUDE), ..*self })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LayerSource {
    QueryResult {
        result_id: String,
        result_set_index: u64,
        column_index: u64,
        #[serde(default)] source_srid: Option<u64>,
        #[serde(default)] longitude_mode: LongitudeMode,
        #[serde(default)] skip_invalid: bool,
    },
    Table {
        table: TableRef,
        column: String,
        viewport: Viewport,
        #[serde(default)] source_srid: Option<u64>,
        #[serde(default)] srid_filter: Option<u32>,
        #[serde(default)] longitude_mode: LongitudeMode,
        #[serde(default)] skip_invalid: bool,
    },
}

impl LayerSource {
    pub fn validate(&self) -> Result<(), CoreError> {
        let source_srid = match self {
            Self::QueryResult { result_id, result_set_index, column_index, source_srid, .. } => {
                if result_id.is_empty() || result_id.chars().count() > 128 || *result_set_index > MAX_SAFE_INTEGER || *column_index > MAX_SAFE_INTEGER {
                    return Err(CoreError::new(ErrorCode::InvalidArgument, "Invalid query result source"));
                }
                source_srid
            }
            Self::Table { table, column, viewport, source_srid, srid_filter, .. } => {
                table.validate()?;
                viewport.validate()?;
                if column.is_empty() || srid_filter.is_some_and(|srid| srid > 999_999) {
                    return Err(CoreError::new(ErrorCode::InvalidArgument, "Invalid spatial table column or SRID filter"));
                }
                source_srid
            }
        };
        if source_srid.is_some_and(|srid| srid == 0 || srid > MAX_SAFE_INTEGER) {
            return Err(CoreError::new(ErrorCode::InvalidArgument, "Source SRID must be a positive safe integer"));
        }
        Ok(())
    }

    pub fn validate_values(&self, values: &[Option<SpatialValue>]) -> Result<(), CoreError> {
        self.validate()?;
        let source_srid = match self { Self::QueryResult { source_srid, .. } | Self::Table { source_srid, .. } => *source_srid };
        for value in values.iter().flatten() {
            match value.srid.filter(|srid| *srid > 0) {
                None if source_srid.is_none() => return Err(CoreError::new(ErrorCode::UnknownSrid, "Unknown SRID requires an explicit source SRID")),
                Some(srid) if source_srid.is_some_and(|source| source != srid as u64) => return Err(CoreError::new(ErrorCode::SridConflict, "Source SRID conflicts with the original spatial value")),
                _ => {}
            }
        }
        Ok(())
    }
}
