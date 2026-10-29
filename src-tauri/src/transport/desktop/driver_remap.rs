//! Desktop hooks for the plus-driver remap: the Tauri command and the plugin
//! install hook. The remap itself lives in `crate::driver_remap`.

use crate::driver_remap::{fork_locations, is_plus_package, remap_connections_in, RemapReport};

async fn remap_fork_profile<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<RemapReport, String> {
    let (connections, plugins_dir) = fork_locations()?;
    let report =
        tokio::task::spawn_blocking(move || remap_connections_in(&connections, &plugins_dir))
            .await
            .map_err(|_| "The driver remap task failed.".to_owned())??;
    if !report.remapped.is_empty() {
        super::commands::invalidate_connection_cache(app);
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
