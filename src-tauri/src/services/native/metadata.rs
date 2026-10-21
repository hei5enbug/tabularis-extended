//! What the registered driver of a saved connection can do, for the MCP
//! catalog tools. Object kinds come from the driver manifest, adjusted by the
//! connection's own metadata when the plugin provides it.

use super::NativeCore;
use crate::{
    drivers::driver_trait::SqlDialect,
    services::{
        backend::SavedConnection,
        contracts::ErrorCode,
        error::{failure, ServiceResult},
        transport::{CatalogMetadata, CatalogTraits, SqlFlavor},
    },
};
use async_trait::async_trait;
use std::sync::Weak;

pub struct NativeCatalogMetadata {
    core: Weak<NativeCore>,
}
impl NativeCatalogMetadata {
    pub fn new(core: Weak<NativeCore>) -> Self {
        Self { core }
    }
}

#[async_trait]
impl CatalogMetadata for NativeCatalogMetadata {
    async fn traits(&self, connection: &SavedConnection) -> ServiceResult<CatalogTraits> {
        let core = self
            .core
            .upgrade()
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
        let params = core.platform.connections.private_params(&connection.id)?;
        let driver = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crate::drivers::registry::get_connection_driver(&params),
        )
        .await
        .map_err(|_| failure(ErrorCode::DeadlineExceeded))?
        .map_err(|_| failure(ErrorCode::CapabilityUnavailable))?;
        let capabilities = &driver.manifest().capabilities;
        let mut kinds = vec!["table".to_owned()];
        for (supported, kind) in [
            (capabilities.views, "view"),
            (capabilities.materialized_views, "materialized_view"),
            (capabilities.routines, "routine"),
            (capabilities.triggers, "trigger"),
        ] {
            if supported {
                kinds.push(kind.to_owned());
            }
        }
        let flavor = match capabilities.sql_dialect {
            Some(SqlDialect::Postgres) => SqlFlavor::Postgres,
            Some(SqlDialect::Mssql) => SqlFlavor::Mssql,
            Some(SqlDialect::Mysql) => SqlFlavor::Mysql,
            Some(SqlDialect::Sqlite) => SqlFlavor::Sqlite,
            _ => SqlFlavor::Other,
        };
        let default_schema = match flavor {
            SqlFlavor::Postgres => Some("public".to_owned()),
            SqlFlavor::Mssql => Some("dbo".to_owned()),
            _ => None,
        };
        Ok(CatalogTraits {
            kinds,
            flavor,
            default_schema,
        })
    }
}
