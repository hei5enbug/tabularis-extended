//! Manifest-only driver listing behind `--list-drivers`.
//!
//! Reads the built-in manifests and the plugin manifests the app would load.
//! It never starts a plugin process, opens a connection, resolves credentials,
//! or writes: the plugin directory is neither created nor migrated, and no
//! connection file is touched.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::manager::ConfigManifest;
use crate::drivers::driver_trait::DatabaseDriver;
use crate::drivers::registry::BUILTIN_DRIVER_IDS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DriverSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub is_builtin: bool,
}

fn builtin_summaries() -> Vec<DriverSummary> {
    let drivers: [&dyn DatabaseDriver; 2] = [
        &crate::drivers::mysql::MysqlDriver::new(),
        &crate::drivers::sqlite::SqliteDriver::new(),
    ];
    drivers
        .into_iter()
        .map(|driver| {
            let manifest = driver.manifest();
            DriverSummary {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                is_builtin: true,
            }
        })
        .collect()
}

/// Built-in drivers plus the enabled plugin manifests found under
/// `plugins_dir`, sorted by id. `enabled_ids` follows the startup rule: `None`
/// means every installed plugin, `Some` filters by plugin directory name.
/// A plugin claiming a built-in id is omitted, as startup refuses it.
pub fn list_driver_manifests(
    plugins_dir: &Path,
    enabled_ids: Option<&[String]>,
) -> Vec<DriverSummary> {
    let mut drivers: BTreeMap<String, DriverSummary> = builtin_summaries()
        .into_iter()
        .map(|summary| (summary.id.clone(), summary))
        .collect();
    let directories = super::layout::driver_directories(plugins_dir).unwrap_or_default();
    for path in directories {
        if let Some(enabled) = enabled_ids {
            let name = path.file_name().and_then(|name| name.to_str());
            if !name.is_some_and(|name| enabled.iter().any(|id| id == name)) {
                continue;
            }
        }
        let Ok(config) = super::installer::read_manifest::<ConfigManifest>(&path) else {
            continue;
        };
        let id = config.id.unwrap_or_else(|| config.name.clone());
        if BUILTIN_DRIVER_IDS.contains(&id.as_str()) {
            continue;
        }
        drivers.insert(
            id.clone(),
            DriverSummary {
                id,
                name: config.name,
                version: config.version,
                is_builtin: false,
            },
        );
    }
    drivers.into_values().collect()
}

/// JSON array printed by `--list-drivers`, from the on-disk configuration.
pub fn list_drivers_json() -> Result<String, String> {
    let plugins_dir = crate::paths::get_plugins_dir_checked()?;
    let enabled = crate::config::load_config_from_disk().active_external_drivers;
    serde_json::to_string(&list_driver_manifests(&plugins_dir, enabled.as_deref()))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "driver_listing_tests.rs"]
mod tests;
