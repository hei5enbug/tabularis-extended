use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpatialKind { Spatial }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeType { Geometry, Geography }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dimensions { XY, XYZ, XYM, XYZM }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpatialEncoding {
    #[serde(rename = "ewkb-base64")]
    EwkbBase64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpatialValue {
    pub kind: SpatialKind,
    pub native_type: NativeType,
    pub srid: Option<i32>,
    pub dimensions: Dimensions,
    pub encoding: SpatialEncoding,
    pub value: String,
    pub is_empty: bool,
}
