//! Moves imported connections onto the plugin drivers that replace the removed
//! built-ins. It runs only against the fork profile and only for a driver
//! whose package is installed with a manifest this host accepts.

use crate::models::ConnectionParams;
use crate::persistence;
use crate::plugins::{installer, layout, manager::ConfigManifest, runtime_version};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Component, Path};

pub const POSTGRESQL_PLUS: &str = "postgresql-plus";
pub const SQLSERVER_PLUS: &str = "sqlserver-plus";

const ENTRA_MODE: &str = "entra_user";
const ENTRA_SOURCE: &str = "azure_cli";

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemapReport {
    /// Ids of the connections moved to a plus driver.
    pub remapped: Vec<String>,
    /// Ids left unchanged because their fields cannot be converted.
    pub unconverted: Vec<String>,
    /// Plus packages found verified in the plugin directory.
    pub verified_packages: Vec<String>,
}

pub fn is_plus_package(plugin_id: &str) -> bool {
    plugin_id == POSTGRESQL_PLUS || plugin_id == SQLSERVER_PLUS
}

/// A package counts as verified when its driver manifest parses, claims the
/// requested identity, fits this host, and ships the executable it declares.
pub fn verified_driver_package(plugins_dir: &Path, id: &str) -> bool {
    let Ok(dir) = layout::resolve_driver(plugins_dir, id) else {
        return false;
    };
    let Ok(manifest) = installer::read_manifest::<ConfigManifest>(&dir) else {
        return false;
    };
    if manifest.id.as_deref().unwrap_or(&manifest.name) != id {
        return false;
    }
    if runtime_version::check_min_runtime_version(
        id,
        manifest.min_runtime_version.as_deref(),
        runtime_version::HOST_VERSION,
    )
    .is_err()
    {
        return false;
    }
    match manifest.executable.as_deref() {
        None => true,
        Some(executable) => {
            let relative = Path::new(executable);
            if executable.is_empty()
                || relative.is_absolute()
                || relative
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                return false;
            }
            std::fs::symlink_metadata(dir.join(relative))
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
        }
    }
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn nonblank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The SQL Server Plus contract for a legacy `sqlserver-entra` connection:
/// `username` on the params and `auth_mode`, `auth_source`, `tenant_id`,
/// `entra_object_id` and optional `azure_cli_path` in `extra`. The legacy
/// plugin used the same key names, so conversion validates and normalizes them.
/// A connection without an auth mode and without Entra keys is a SQL
/// authentication connection and converts unchanged.
fn convert_entra_params(params: &ConnectionParams) -> Result<ConnectionParams, ()> {
    let extra = &params.extra;
    let mode = extra.get("auth_mode").map(|value| value.trim());
    let source = extra.get("auth_source").map(|value| value.trim());
    let has_identity_keys = ["tenant_id", "entra_object_id", "azure_cli_path"]
        .iter()
        .any(|key| extra.contains_key(*key));
    let Some(mode) = mode else {
        return if source.is_some() || has_identity_keys {
            Err(())
        } else {
            Ok(params.clone())
        };
    };
    if mode != ENTRA_MODE || source != Some(ENTRA_SOURCE) {
        return Err(());
    }
    if extra
        .get("integrated_auth")
        .is_some_and(|value| truthy(value))
        || nonblank(params.password.as_deref()).is_some()
    {
        return Err(());
    }
    let username = nonblank(params.username.as_deref()).ok_or(())?;
    let tenant_id = nonblank(extra.get("tenant_id").map(String::as_str)).ok_or(())?;
    let object_id = nonblank(extra.get("entra_object_id").map(String::as_str)).ok_or(())?;
    let cli_path = match extra.get("azure_cli_path") {
        None => None,
        Some(value) => {
            let value = nonblank(Some(value)).ok_or(())?;
            if !Path::new(&value).is_absolute() {
                return Err(());
            }
            Some(value)
        }
    };

    let mut converted = params.clone();
    converted.username = Some(username);
    let mut next: HashMap<String, String> = extra.clone();
    next.insert("auth_mode".into(), ENTRA_MODE.into());
    next.insert("auth_source".into(), ENTRA_SOURCE.into());
    next.insert("tenant_id".into(), tenant_id);
    next.insert("entra_object_id".into(), object_id);
    if let Some(path) = cli_path {
        next.insert("azure_cli_path".into(), path);
    }
    converted.extra = next;
    Ok(converted)
}

/// Remaps the connections of one profile. The file is not opened, created or
/// locked unless at least one plus package is verified.
pub fn remap_connections_in(
    connections_path: &Path,
    plugins_dir: &Path,
) -> Result<RemapReport, String> {
    let mut report = RemapReport::default();
    let postgresql = verified_driver_package(plugins_dir, POSTGRESQL_PLUS);
    let sqlserver = verified_driver_package(plugins_dir, SQLSERVER_PLUS);
    if postgresql {
        report.verified_packages.push(POSTGRESQL_PLUS.to_owned());
    }
    if sqlserver {
        report.verified_packages.push(SQLSERVER_PLUS.to_owned());
    }
    if (!postgresql && !sqlserver) || !connections_path.exists() {
        return Ok(report);
    }

    let mut remapped = Vec::new();
    let mut unconverted = Vec::new();
    persistence::update_connections_file(connections_path, |file| {
        for connection in &mut file.connections {
            let target = match connection.params.driver.as_str() {
                "postgresql" if postgresql => POSTGRESQL_PLUS,
                "sqlserver" | "sqlserver-entra" if sqlserver => SQLSERVER_PLUS,
                _ => continue,
            };
            let Some(revision) = connection.next_revision() else {
                unconverted.push(connection.id.clone());
                continue;
            };
            if connection.params.driver == "sqlserver-entra" {
                match convert_entra_params(&connection.params) {
                    Ok(converted) => connection.params = converted,
                    Err(()) => {
                        unconverted.push(connection.id.clone());
                        continue;
                    }
                }
            }
            connection.params.driver = target.to_owned();
            connection.revision = revision;
            remapped.push(connection.id.clone());
        }
        Ok(())
    })?;
    report.remapped = remapped;
    report.unconverted = unconverted;
    Ok(report)
}

/// Adds each verified plus driver that a saved connection uses to the
/// profile's `activeExternalDrivers`, so the plugin manager starts it. A
/// missing config or list already starts every installed driver and stays
/// unchanged, as does every other config field.
pub fn activate_used_plus_drivers_in(
    config_path: &Path,
    connections_path: &Path,
    plugins_dir: &Path,
) -> Result<Vec<String>, String> {
    if !config_path.exists() || !connections_path.exists() {
        return Ok(Vec::new());
    }
    let connections = persistence::load_connections(connections_path)?;
    let content = std::fs::read(config_path).map_err(|error| error.to_string())?;
    let mut config: serde_json::Value =
        serde_json::from_slice(&content).map_err(|error| error.to_string())?;
    let Some(active) = config
        .get_mut("activeExternalDrivers")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return Ok(Vec::new());
    };
    let mut added = Vec::new();
    for id in [POSTGRESQL_PLUS, SQLSERVER_PLUS] {
        if connections
            .iter()
            .any(|connection| connection.params.driver == id)
            && !active.iter().any(|value| value.as_str() == Some(id))
            && verified_driver_package(plugins_dir, id)
        {
            active.push(id.into());
            added.push(id.to_owned());
        }
    }
    if !added.is_empty() {
        let bytes = serde_json::to_vec_pretty(&config).map_err(|error| error.to_string())?;
        persistence::service_write_atomic(config_path, &bytes)
            .map_err(|_| "The config file could not be written.".to_owned())?;
    }
    Ok(added)
}

/// Startup hook for the fork profile, run before plugins load.
pub fn activate_fork_plus_drivers() -> Result<Vec<String>, String> {
    let (connections, plugins_dir) = fork_locations()?;
    let config = crate::paths::get_app_config_dir().join("config.json");
    activate_used_plus_drivers_in(&config, &connections, &plugins_dir)
}

fn fork_locations() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let plugins_dir = crate::paths::get_plugins_dir_checked()?;
    let connections = crate::paths::resolve_connections_path(&crate::paths::get_app_config_dir());
    Ok((connections, plugins_dir))
}

async fn remap_fork_profile<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<RemapReport, String> {
    let (connections, plugins_dir) = fork_locations()?;
    let report =
        tokio::task::spawn_blocking(move || remap_connections_in(&connections, &plugins_dir))
            .await
            .map_err(|_| "The driver remap task failed.".to_owned())??;
    if !report.remapped.is_empty() {
        crate::commands::invalidate_connection_cache(app);
    }
    Ok(report)
}

#[tauri::command]
pub async fn remap_connections_to_plus_drivers<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<RemapReport, String> {
    remap_fork_profile(&app).await
}

/// Plugin manager hook: runs after an install finished and the package loaded.
/// Failures are logged and never fail the install.
pub async fn after_plugin_install<R: tauri::Runtime>(app: &tauri::AppHandle<R>, plugin_id: &str) {
    if !is_plus_package(plugin_id) {
        return;
    }
    match remap_fork_profile(app).await {
        Ok(report) => {
            if !report.remapped.is_empty() {
                log::info!(
                    "Moved {} connection(s) to plus drivers after installing {}",
                    report.remapped.len(),
                    plugin_id
                );
            }
            if !report.unconverted.is_empty() {
                log::warn!(
                    "Left {} connection(s) on their legacy driver: {}",
                    report.unconverted.len(),
                    report.unconverted.join(",")
                );
            }
        }
        Err(error) => log::warn!("Driver remap after installing {plugin_id} failed: {error}"),
    }
}

#[cfg(test)]
#[path = "driver_remap_tests.rs"]
mod tests;
