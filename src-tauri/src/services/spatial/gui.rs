use crate::services::{
    backend::{DriverContext, SavedConnection},
    contracts::ErrorCode,
    error::{failure, ServiceResult},
    local_ipc::{EndpointKind, GuiApplyAck, GuiApplyRequest, InstanceMetadata, LocalIpc},
};
use async_trait::async_trait;
use std::{sync::Weak, time::Duration};

#[async_trait]
pub trait MapGuiBackend: Send + Sync {
    async fn discover(&self) -> ServiceResult<Vec<InstanceMetadata>>;
    async fn apply(&self, gui_id: &str, request: GuiApplyRequest) -> ServiceResult<GuiApplyAck>;
}
pub struct UnavailableMapGui;
#[async_trait]
impl MapGuiBackend for UnavailableMapGui {
    async fn discover(&self) -> ServiceResult<Vec<InstanceMetadata>> {
        Ok(Vec::new())
    }
    async fn apply(&self, _: &str, _: GuiApplyRequest) -> ServiceResult<GuiApplyAck> {
        Err(failure(ErrorCode::GuiUnavailable))
    }
}
pub struct WeakLocalIpcGui {
    ipc: Weak<LocalIpc>,
}
impl WeakLocalIpcGui {
    pub fn new(ipc: Weak<LocalIpc>) -> Self {
        Self { ipc }
    }
}
#[async_trait]
impl MapGuiBackend for WeakLocalIpcGui {
    async fn discover(&self) -> ServiceResult<Vec<InstanceMetadata>> {
        let ipc = self
            .ipc
            .upgrade()
            .ok_or_else(|| failure(ErrorCode::GuiUnavailable))?;
        let instances = ipc.discover(EndpointKind::Gui).await?;
        if instances.len() > 64 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        Ok(instances)
    }
    async fn apply(&self, gui_id: &str, request: GuiApplyRequest) -> ServiceResult<GuiApplyAck> {
        self.ipc
            .upgrade()
            .ok_or_else(|| failure(ErrorCode::GuiUnavailable))?
            .apply_map(gui_id, request)
            .await
    }
}
#[async_trait]
pub trait MapSpatialCapability: Send + Sync {
    async fn supports(
        &self,
        connection: &SavedConnection,
        context: &DriverContext,
    ) -> ServiceResult<bool>;
}
pub struct RegistryMapSpatialCapability;
#[async_trait]
impl MapSpatialCapability for RegistryMapSpatialCapability {
    async fn supports(
        &self,
        connection: &SavedConnection,
        context: &DriverContext,
    ) -> ServiceResult<bool> {
        let driver = crate::drivers::registry::get_driver(&connection.driver)
            .await
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
        let result = tokio::select! {biased;_ = context.signal.cancelled()=>return Err(failure(ErrorCode::Cancelled)),result=tokio::time::timeout(Duration::from_millis(context.deadline_ms),driver.service_capabilities())=>result.map_err(|_|failure(ErrorCode::DeadlineExceeded))?};
        match result {
            Ok(capabilities) => Ok(capabilities.service_protocol == 1 && capabilities.spatial),
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::CapabilityUnavailable | ErrorCode::UnsupportedOperation
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
}
