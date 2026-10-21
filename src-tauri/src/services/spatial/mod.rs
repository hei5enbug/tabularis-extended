mod data_domain;
mod driver;
mod export;
mod export_sink;
mod export_stream;
mod feature;
mod gui;
mod map_domain;
mod map_store;
mod paging;
mod query_result;
mod table_query;

pub use data_domain::SpatialDataDomain;
pub use driver::{RegistrySpatialDriverBackend, SpatialDriverBackend};
pub use export::SpatialExportDomain;
pub use export_sink::{
    ExportArtifact, ExportDestination, ExportStats, SpatialExportSink, SpatialExportSinkFactory,
};
pub use gui::{
    MapGuiBackend, MapSpatialCapability, RegistryMapSpatialCapability, UnavailableMapGui,
    WeakLocalIpcGui,
};
pub use map_domain::MapDomain;
pub use map_store::{MapChange, MapStore, MapStoreLimits, StoredMap};

use crate::services::contracts::Operation;
pub const MAP_OPERATIONS: &[Operation] = &[
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

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;
