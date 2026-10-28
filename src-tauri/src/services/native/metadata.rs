//! What the registered driver of a saved connection can do, for the MCP
//! catalog tools. Object kinds come from the driver manifest, except for the
//! plus drivers, whose lists are fixed.

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

const POSTGRESQL_PLUS_KINDS: &[&str] = &[
    "table",
    "view",
    "materialized_view",
    "foreign_table",
    "routine",
    "aggregate",
    "trigger",
    "sequence",
    "type",
    "collation",
    "operator",
    "operator_class",
    "operator_family",
];
const SQLSERVER_PLUS_KINDS: &[&str] = &[
    "table",
    "view",
    "routine",
    "trigger",
    "sequence",
    "synonym",
    "type",
    "rule",
    "security_policy",
];

/// The kinds the plus drivers list and describe, whatever their manifest
/// capabilities say.
fn extended_kinds(driver: &str) -> Option<&'static [&'static str]> {
    match driver {
        "postgresql-plus" => Some(POSTGRESQL_PLUS_KINDS),
        "sqlserver-plus" => Some(SQLSERVER_PLUS_KINDS),
        _ => None,
    }
}

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
        let mut extended_describe = false;
        if let Some(extended) = extended_kinds(&connection.driver) {
            kinds = extended.iter().map(|kind| (*kind).to_owned()).collect();
            extended_describe = true;
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
            extended_describe,
            flavor,
            default_schema,
        })
    }
}
