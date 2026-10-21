pub const SERVICE_PROTOCOL_VERSION: u8 = 1;
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub mod error;
pub mod geometry;
pub mod limits;
pub mod map;
pub mod source;
pub mod value;

pub use error::{CoreError, ErrorCode};
pub use geometry::{Geometry, Position};
pub use limits::{Feature, FeatureError, FeatureKind, FeaturePage, LayerBudget, LayerLimits, LimitStatus, PageBuilder, PageInfo, PageLimits, PushResult, ResumeMode, RowReference};
pub use map::{Basemap, BasemapChange, FeatureRef, LayerState, LineStyle, MapState, PointStyle, PolygonStyle, ResultReference, Style};
pub use source::{query_feature_id, LongitudeMode, LongitudePiece, RowIdentity, TableRef, Viewport, LayerSource, MERCATOR_MAX_LATITUDE};
pub use value::{Dimensions, NativeType, SpatialEncoding, SpatialKind, SpatialValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BootstrapError { CapabilityUnavailable }

pub fn capability() -> Result<(), BootstrapError> {
    Ok(())
}
