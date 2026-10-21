use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidArgument,
    InvalidGeometry,
    UnsupportedType,
    UnsupportedOperation,
    UnknownSrid,
    SridConflict,
    ResourceLimit,
    FeatureTooLarge,
    VersionConflict,
    StaleGeneration,
    NotFound,
}

impl ErrorCode {
    pub fn wire_code(self) -> &'static str {
        match self {
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::InvalidGeometry => "INVALID_GEOMETRY",
            Self::UnsupportedType => "UNSUPPORTED_TYPE",
            Self::UnsupportedOperation => "UNSUPPORTED_OPERATION",
            Self::UnknownSrid => "UNKNOWN_SRID",
            Self::SridConflict => "SRID_CONFLICT",
            Self::ResourceLimit => "RESOURCE_LIMIT",
            Self::FeatureTooLarge => "FEATURE_TOO_LARGE",
            Self::VersionConflict => "VERSION_CONFLICT",
            Self::StaleGeneration => "STALE_GENERATION",
            Self::NotFound => "NOT_FOUND",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version: Option<u64>,
}

impl CoreError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), current_version: None }
    }

    pub fn version_conflict(current_version: u64) -> Self {
        Self { code: ErrorCode::VersionConflict, message: "Map version has changed".into(), current_version: Some(current_version) }
    }
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CoreError {}
