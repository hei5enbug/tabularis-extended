use crate::services::{error::ServiceResult, results::CursorBinding};

pub struct ExportDestination {
    pub binding: CursorBinding,
    pub caller_id: String,
    pub driver_id: String,
    pub filename: String,
    pub max_bytes: u64,
    pub deadline_at_ms: u64,
}

#[derive(Clone, Default)]
pub struct ExportStats {
    pub feature_count: u64,
    pub coordinate_count: u64,
    pub truncated: bool,
    pub reasons: Vec<String>,
}

pub struct ExportArtifact {
    pub artifact_id: String,
    pub bytes: u64,
    pub checksum: String,
}

pub trait SpatialExportSink: Send {
    fn append(&mut self, bytes: &[u8]) -> ServiceResult<()>;
    fn finish(self: Box<Self>, stats: ExportStats) -> ServiceResult<ExportArtifact>;
}

pub trait SpatialExportSinkFactory: Send + Sync {
    fn reserve(&self, destination: ExportDestination) -> ServiceResult<Box<dyn SpatialExportSink>>;
}
