use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::{CoreError, ErrorCode};

pub type Position = Vec<f64>;
pub const MAX_GEOMETRY_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Geometry {
    Point { coordinates: Position },
    LineString { coordinates: Vec<Position> },
    Polygon { coordinates: Vec<Vec<Position>> },
    MultiPoint { coordinates: Vec<Position> },
    MultiLineString { coordinates: Vec<Vec<Position>> },
    MultiPolygon { coordinates: Vec<Vec<Vec<Position>>> },
    GeometryCollection { geometries: Vec<Geometry> },
}

impl Geometry {
    pub fn from_json(value: &Value) -> Result<Option<Self>, CoreError> {
        if value.is_null() { return Ok(None); }
        validate_types(value, 1)?;
        let geometry: Self = serde_json::from_value(value.clone()).map_err(|_| invalid("Malformed GeoJSON geometry"))?;
        geometry.coordinate_count()?;
        Ok(if geometry.is_empty() { None } else { Some(geometry) })
    }

    pub fn coordinate_count(&self) -> Result<u64, CoreError> { self.count_at_depth(1) }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Point { coordinates } => coordinates.is_empty(),
            Self::LineString { coordinates } | Self::MultiPoint { coordinates } => coordinates.is_empty(),
            Self::Polygon { coordinates } | Self::MultiLineString { coordinates } => coordinates.iter().all(Vec::is_empty),
            Self::MultiPolygon { coordinates } => coordinates.iter().all(|polygon| polygon.iter().all(Vec::is_empty)),
            Self::GeometryCollection { geometries } => geometries.iter().all(Self::is_empty),
        }
    }

    fn count_at_depth(&self, depth: usize) -> Result<u64, CoreError> {
        if depth > MAX_GEOMETRY_DEPTH { return Err(invalid("Geometry nesting exceeds 128 levels")); }
        match self {
            Self::Point { coordinates } => {
                if coordinates.is_empty() { Ok(0) } else { validate_position(coordinates)?; Ok(1) }
            }
            Self::LineString { coordinates } => validate_line(coordinates),
            Self::Polygon { coordinates } => validate_polygon(coordinates),
            Self::MultiPoint { coordinates } => {
                for position in coordinates { validate_position(position)?; }
                length_count(coordinates.len())
            }
            Self::MultiLineString { coordinates } => sum_counts(coordinates.iter().map(|line| validate_line(line))),
            Self::MultiPolygon { coordinates } => sum_counts(coordinates.iter().map(|polygon| validate_polygon(polygon))),
            Self::GeometryCollection { geometries } => sum_counts(geometries.iter().map(|geometry| geometry.count_at_depth(depth + 1))),
        }
    }
}

fn validate_types(value: &Value, depth: usize) -> Result<(), CoreError> {
    if depth > MAX_GEOMETRY_DEPTH { return Err(invalid("Geometry nesting exceeds 128 levels")); }
    let name = value.get("type").and_then(Value::as_str).ok_or_else(|| invalid("Geometry type is required"))?;
    match name {
        "Point" | "LineString" | "Polygon" | "MultiPoint" | "MultiLineString" | "MultiPolygon" => Ok(()),
        "GeometryCollection" => {
            let children = value.get("geometries").and_then(Value::as_array).ok_or_else(|| invalid("GeometryCollection requires a geometries array"))?;
            for child in children { validate_types(child, depth + 1)?; }
            Ok(())
        }
        _ => Err(CoreError::new(ErrorCode::UnsupportedType, "Unsupported GeoJSON geometry type")),
    }
}

fn validate_position(position: &[f64]) -> Result<(), CoreError> {
    if position.len() != 2 || !position[0].is_finite() || !position[1].is_finite()
        || !(-180.0..=180.0).contains(&position[0]) || !(-90.0..=90.0).contains(&position[1]) {
        return Err(invalid("Display positions must contain finite WGS84 XY coordinates"));
    }
    Ok(())
}

fn validate_line(line: &[Position]) -> Result<u64, CoreError> {
    if line.len() == 1 { return Err(invalid("LineString requires zero or at least two positions")); }
    for position in line { validate_position(position)?; }
    length_count(line.len())
}

fn validate_polygon(polygon: &[Vec<Position>]) -> Result<u64, CoreError> {
    for ring in polygon {
        if ring.len() < 4 || ring.first() != ring.last() { return Err(invalid("Polygon rings must be closed with at least four positions")); }
    }
    sum_counts(polygon.iter().map(|ring| {
        for position in ring { validate_position(position)?; }
        length_count(ring.len())
    }))
}

fn length_count(length: usize) -> Result<u64, CoreError> {
    u64::try_from(length).map_err(|_| CoreError::new(ErrorCode::ResourceLimit, "Coordinate count overflow"))
}

fn sum_counts(mut counts: impl Iterator<Item = Result<u64, CoreError>>) -> Result<u64, CoreError> {
    counts.try_fold(0_u64, |total, count| total.checked_add(count?).ok_or_else(|| CoreError::new(ErrorCode::ResourceLimit, "Coordinate count overflow")))
}

fn invalid(message: &str) -> CoreError { CoreError::new(ErrorCode::InvalidGeometry, message) }
