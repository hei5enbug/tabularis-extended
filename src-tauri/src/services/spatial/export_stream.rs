use super::{
    export_sink::{ExportArtifact, ExportStats, SpatialExportSink},
    paging,
};
use crate::services::{
    contracts::{ErrorCode, Outcome},
    error::{failure, ServiceResult},
};
use sha2::{Digest, Sha256};
use tabularis_spatial_core::{Feature, FeaturePage};

pub(super) const PREFIX: &[u8] = b"{\"type\":\"FeatureCollection\",\"features\":[";
pub(super) const SUFFIX: &[u8] = b"]}";

#[derive(Clone, Copy)]
pub(super) struct ExportLimits {
    pub bytes: u64,
    pub features: u64,
    pub coordinates: u64,
    pub explicit: bool,
}

impl ExportLimits {
    pub fn from_input(input: &serde_json::Value) -> ServiceResult<Self> {
        let explicit = input.get("export_limit_bytes");
        let bytes = match explicit {
            Some(value) => value
                .as_u64()
                .filter(|bytes| (1..=1_073_741_824).contains(bytes))
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
            None => 32 * 1024 * 1024,
        };
        if bytes < (PREFIX.len() + SUFFIX.len()) as u64 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        Ok(Self {
            bytes,
            features: if explicit.is_some() {
                1_000_000
            } else {
                10_000
            },
            coordinates: if explicit.is_some() {
                10_000_000
            } else {
                250_000
            },
            explicit: explicit.is_some(),
        })
    }
}

pub(super) struct FeatureStream {
    sink: Option<Box<dyn SpatialExportSink>>,
    limits: ExportLimits,
    stats: ExportStats,
    warnings: Vec<String>,
    bytes: u64,
    checksum: Sha256,
    sealed: bool,
}

impl FeatureStream {
    pub fn new(sink: Box<dyn SpatialExportSink>, limits: ExportLimits) -> ServiceResult<Self> {
        if limits.bytes < (PREFIX.len() + SUFFIX.len()) as u64 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let mut stream = Self {
            sink: Some(sink),
            limits,
            stats: ExportStats::default(),
            warnings: vec![],
            bytes: 0,
            checksum: Sha256::new(),
            sealed: false,
        };
        stream.write(PREFIX)?;
        Ok(stream)
    }

    fn write(&mut self, bytes: &[u8]) -> ServiceResult<()> {
        self.sink
            .as_mut()
            .ok_or_else(|| failure(ErrorCode::ResourceLimit))?
            .append(bytes)?;
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| failure(ErrorCode::ResourceLimit))?;
        self.checksum.update(bytes);
        Ok(())
    }

    fn add_code(codes: &mut Vec<String>, code: &str) -> ServiceResult<()> {
        if !paging::safe_code(code) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if !codes.iter().any(|current| current == code) {
            if codes.len() >= 64 {
                return Err(failure(ErrorCode::ResourceLimit));
            }
            codes.push(code.into());
        }
        Ok(())
    }

    fn truncate(&mut self, reason: &str) -> ServiceResult<()> {
        self.stats.truncated = true;
        Self::add_code(&mut self.stats.reasons, reason)?;
        if !self.limits.explicit {
            Self::add_code(&mut self.stats.reasons, "LAYER_LIMIT")?;
        }
        Ok(())
    }

    pub fn merge_page(&mut self, page: &FeaturePage) -> ServiceResult<()> {
        self.stats.truncated |= page.limits.truncated;
        for reason in &page.limits.reasons {
            Self::add_code(&mut self.stats.reasons, reason)?;
        }
        for warning in &page.warnings {
            Self::add_code(&mut self.warnings, warning)?;
        }
        Ok(())
    }

    pub fn feature(&mut self, feature: &Feature) -> ServiceResult<bool> {
        if self.sealed {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if self.stats.feature_count >= self.limits.features {
            self.truncate("EXPORT_FEATURE_LIMIT")?;
            return Ok(false);
        }
        let coordinates = feature.coordinate_count().map_err(paging::core_error)?;
        let next_coordinates = self
            .stats
            .coordinate_count
            .checked_add(coordinates)
            .ok_or_else(|| failure(ErrorCode::ResourceLimit))?;
        if next_coordinates > self.limits.coordinates {
            self.truncate("EXPORT_COORDINATE_LIMIT")?;
            return Ok(false);
        }
        let bytes = serde_json::to_vec(feature).map_err(|_| failure(ErrorCode::ResourceLimit))?;
        let comma = u64::from(self.stats.feature_count > 0);
        let next_bytes = self
            .bytes
            .checked_add(comma)
            .and_then(|count| count.checked_add(bytes.len() as u64))
            .and_then(|count| count.checked_add(SUFFIX.len() as u64))
            .ok_or_else(|| failure(ErrorCode::ResourceLimit))?;
        if next_bytes > self.limits.bytes {
            self.truncate("EXPORT_BYTE_LIMIT")?;
            return Ok(false);
        }
        if comma > 0 {
            self.write(b",")?;
        }
        self.write(&bytes)?;
        self.stats.feature_count += 1;
        self.stats.coordinate_count = next_coordinates;
        Ok(true)
    }

    pub fn seal(&mut self) -> ServiceResult<()> {
        if self.sealed {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        self.write(SUFFIX)?;
        self.sealed = true;
        Ok(())
    }

    pub fn finish(mut self) -> ServiceResult<(ExportArtifact, ExportStats, Vec<String>)> {
        if !self.sealed {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let checksum = format!("{:x}", self.checksum.clone().finalize());
        let artifact = self
            .sink
            .take()
            .ok_or_else(|| failure(ErrorCode::ResourceLimit))?
            .finish(self.stats.clone())?;
        let canonical_id = artifact
            .artifact_id
            .strip_prefix("artifact-")
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .is_some_and(|id| {
                id.get_version_num() == 4
                    && id.as_bytes()[8] & 0xc0 == 0x80
                    && format!("artifact-{id}") == artifact.artifact_id
            });
        if !canonical_id
            || artifact.bytes != self.bytes
            || artifact.bytes > self.limits.bytes
            || artifact.checksum != checksum
        {
            let mut error = failure(ErrorCode::InvalidArgument);
            error.outcome = Outcome::Applied;
            return Err(error);
        }
        Ok((artifact, self.stats.clone(), self.warnings.clone()))
    }
}
