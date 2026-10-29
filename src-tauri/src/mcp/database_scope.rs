use crate::drivers::driver_trait::DriverCapabilities;
use crate::models::{ConnectionParams, DatabaseSelection};

use super::protocol::JsonRpcError;

/// Reject a `database` argument for connections that cannot switch databases:
/// local paths and implicit databases cannot be selected through MCP input.
/// A network driver accepts any name here and `select_database` then checks
/// that the driver lists it, so the saved selection does not bound the name.
pub(super) fn reject_unsupported_database_override(
    database: Option<&str>,
    capabilities: &DriverCapabilities,
) -> Result<(), JsonRpcError> {
    if database.is_none() {
        return Ok(());
    }

    if capabilities.file_based || capabilities.folder_based || capabilities.single_database {
        return Err(JsonRpcError {
            code: -32602,
            message: "The 'database' parameter is not supported for this connection".to_string(),
            data: None,
        });
    }

    Ok(())
}

/// Select `database` for one call when the driver lists it. The saved
/// selection does not bound the name; the saved connection is never changed.
pub(super) fn select_listed_database(
    params: &mut ConnectionParams,
    listed: &[String],
    database: &str,
) -> Result<(), JsonRpcError> {
    if !listed.iter().any(|name| name == database) {
        return Err(JsonRpcError {
            code: -32602,
            message: "INVALID_ARGUMENT: database is not one the driver lists for this connection"
                .to_string(),
            data: None,
        });
    }
    params.database = DatabaseSelection::Single(database.to_string());
    Ok(())
}

#[cfg(test)]
mod tests;
