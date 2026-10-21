use crate::{
    drivers::driver_trait::ServiceRpcMethod,
    service_registry_driver::RegistryDriverBackend,
    services::{
        backend::{DriverBackend, DriverContext, SavedConnection},
        contracts::ErrorCode,
        error::{failure, ServiceResult},
    },
};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

#[async_trait]
pub trait SpatialDriverBackend: Send + Sync {
    async fn call(
        &self,
        connection: &SavedConnection,
        method: ServiceRpcMethod,
        input: Value,
        context: DriverContext,
    ) -> ServiceResult<Value>;
    async fn cancel(&self, connection_id: &str, request_id: &str) -> ServiceResult<()>;
}

pub struct RegistrySpatialDriverBackend {
    registry: Arc<RegistryDriverBackend>,
}
impl RegistrySpatialDriverBackend {
    pub fn new(registry: Arc<RegistryDriverBackend>) -> Arc<Self> {
        Arc::new(Self { registry })
    }
}
#[async_trait]
impl SpatialDriverBackend for RegistrySpatialDriverBackend {
    async fn call(
        &self,
        connection: &SavedConnection,
        method: ServiceRpcMethod,
        input: Value,
        context: DriverContext,
    ) -> ServiceResult<Value> {
        if !matches!(
            method,
            ServiceRpcMethod::SpatialColumns
                | ServiceRpcMethod::SpatialTransformValues
                | ServiceRpcMethod::SpatialQueryTable
                | ServiceRpcMethod::SpatialReadFeature
        ) {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        self.registry
            .call_spatial(connection, method, input, context)
            .await
    }
    async fn cancel(&self, connection_id: &str, request_id: &str) -> ServiceResult<()> {
        self.registry.cancel(connection_id, request_id).await
    }
}
