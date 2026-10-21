use super::{
    contracts::{Operation, ServiceRequest, ServiceResponse},
    dispatch::CallerContext,
    error::ServiceResult,
    jobs::JobLease,
};
use async_trait::async_trait;

pub const EXTENSION_OPERATIONS: &[Operation] = &[
    Operation::SpatialColumns,
    Operation::SpatialQueryResult,
    Operation::SpatialTableQuery,
    Operation::SpatialFeature,
    Operation::SpatialExport,
    Operation::MapCreate,
    Operation::MapUpdate,
    Operation::MapGet,
    Operation::MapList,
    Operation::MapOpen,
    Operation::MapClose,
    Operation::MapLayerAdd,
    Operation::MapLayerRemove,
    Operation::MapLayerUpdate,
    Operation::MapViewportSet,
    Operation::MapSelectionSet,
    Operation::MapSave,
];

#[async_trait]
pub trait OperationExtension: Send + Sync {
    fn operations(&self) -> &[Operation];
    async fn execute(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
    ) -> ServiceResult<ServiceResponse>;
}
