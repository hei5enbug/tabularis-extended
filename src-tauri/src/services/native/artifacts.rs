use crate::{
    service_spatial::{
        ExportArtifact, ExportDestination, ExportStats, SpatialExportSink, SpatialExportSinkFactory,
    },
    services::{
        artifacts::{ArtifactBinding, ArtifactMetadata, ArtifactStore, ArtifactWriter},
        backend::CancellationSignal,
        contracts::ErrorCode,
        dispatch::CallerContext,
        error::{failure, ServiceResult},
        runtime::ServiceRuntime,
        storage::now_ms,
        transport::TransportArtifacts,
    },
};
use async_trait::async_trait;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

pub struct NativeArtifacts {
    pub store: Arc<ArtifactStore>,
    runtime: Weak<ServiceRuntime>,
}
impl NativeArtifacts {
    pub fn new(profile: &Path, runtime: &Arc<ServiceRuntime>) -> ServiceResult<Arc<Self>> {
        Ok(Arc::new(Self {
            store: ArtifactStore::new(profile.join("artifacts-v1"))?,
            runtime: Arc::downgrade(runtime),
        }))
    }
    pub async fn binding(
        &self,
        id: &str,
        caller: &CallerContext,
    ) -> ServiceResult<ArtifactBinding> {
        let binding = self.store.binding_for(id, &caller.caller_id)?;
        self.validate(&binding, caller).await?;
        Ok(binding)
    }
    async fn validate(
        &self,
        binding: &ArtifactBinding,
        caller: &CallerContext,
    ) -> ServiceResult<()> {
        if binding.caller_id != caller.caller_id {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
        let connection = runtime
            .connections
            .get(&binding.cursor.connection_id)
            .await?;
        if connection.driver != binding.driver_id
            || connection.auth_revision != binding.cursor.auth_revision
        {
            return Err(failure(ErrorCode::AuthRequired));
        }
        let permission = runtime.policy.permission(&connection, caller).await?;
        if permission.identity != binding.cursor.permission_scope {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        Ok(())
    }
    pub async fn save(
        self: &Arc<Self>,
        id: String,
        caller: CallerContext,
        destination: PathBuf,
        signal: CancellationSignal,
    ) -> ServiceResult<ArtifactMetadata> {
        let binding = self.binding(&id, &caller).await?;
        let artifacts = self.clone();
        let handle = tokio::runtime::Handle::current();
        let deadline = now_ms().saturating_add(120_000);
        tokio::task::spawn_blocking(move || {
            artifacts
                .store
                .copy_to_new(&id, &binding, &destination, &signal, deadline, || {
                    handle.block_on(artifacts.validate(&binding, &caller))
                })
        })
        .await
        .map_err(|_| failure(ErrorCode::OutcomeUnknown))?
    }
}
#[async_trait]
impl TransportArtifacts for NativeArtifacts {
    async fn metadata(&self, id: &str, caller: &CallerContext) -> ServiceResult<Value> {
        let binding = self.binding(id, caller).await?;
        let metadata = self.store.metadata(id, &binding)?;
        self.validate(&binding, caller).await?;
        serde_json::to_value(metadata).map_err(|_| failure(ErrorCode::ResourceLimit))
    }
    async fn read_chunk(
        &self,
        id: &str,
        caller: &CallerContext,
        offset: u64,
        limit: usize,
    ) -> ServiceResult<Value> {
        let binding = self.binding(id, caller).await?;
        let chunk = self.store.read_chunk(id, &binding, offset, limit)?;
        self.validate(&binding, caller).await?;
        serde_json::to_value(chunk).map_err(|_| failure(ErrorCode::ResourceLimit))
    }
}
impl SpatialExportSinkFactory for NativeArtifacts {
    fn reserve(&self, destination: ExportDestination) -> ServiceResult<Box<dyn SpatialExportSink>> {
        let binding = ArtifactBinding {
            caller_id: destination.caller_id,
            driver_id: destination.driver_id,
            cursor: destination.binding,
        };
        Ok(Box::new(NativeExportWriter {
            writer: Some(self.store.reserve(
                binding,
                destination.filename,
                destination.max_bytes,
                destination.deadline_at_ms,
            )?),
        }))
    }
}
struct NativeExportWriter {
    writer: Option<ArtifactWriter>,
}
impl SpatialExportSink for NativeExportWriter {
    fn append(&mut self, bytes: &[u8]) -> ServiceResult<()> {
        self.writer
            .as_mut()
            .ok_or_else(|| failure(ErrorCode::ResultExpired))?
            .append(bytes)
    }
    fn finish(mut self: Box<Self>, stats: ExportStats) -> ServiceResult<ExportArtifact> {
        let metadata = self
            .writer
            .take()
            .ok_or_else(|| failure(ErrorCode::ResultExpired))?
            .finish(
                stats.feature_count,
                stats.coordinate_count,
                stats.truncated,
                stats.reasons,
            )?;
        Ok(ExportArtifact {
            artifact_id: metadata.artifact_id,
            bytes: metadata.bytes,
            checksum: metadata.checksum,
        })
    }
}
