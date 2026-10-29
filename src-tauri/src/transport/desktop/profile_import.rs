//! Tauri commands for importing the official Tabularis profile. The import
//! itself lives in `crate::profile_import`.

use crate::profile_import::{
    import_status, run_import, ImportEnvironment, ImportReport, ImportStatus, SystemKeychain,
};

#[tauri::command]
pub async fn official_profile_import_status() -> Result<ImportStatus, String> {
    let env = ImportEnvironment::system()?;
    tokio::task::spawn_blocking(move || import_status(&env))
        .await
        .map_err(|_| "The profile status task failed.".to_owned())
}

#[tauri::command]
pub async fn import_official_profile<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<ImportReport, String> {
    let env = ImportEnvironment::system()?;
    let plugins_dir = env.dest_plugins.clone();
    let mut report = tokio::task::spawn_blocking(move || run_import(&env, &SystemKeychain))
        .await
        .map_err(|_| "The profile import task failed.".to_owned())?
        .map_err(|error| error.to_string())?;
    super::commands::invalidate_connection_cache(&app);
    let connections = crate::paths::resolve_connections_path(&crate::paths::get_app_config_dir());
    report.remap = tokio::task::spawn_blocking(move || {
        crate::driver_remap::remap_connections_in(&connections, &plugins_dir).ok()
    })
    .await
    .ok()
    .flatten();
    if report
        .remap
        .as_ref()
        .is_some_and(|remap| !remap.remapped.is_empty())
    {
        super::commands::invalidate_connection_cache(&app);
    }
    Ok(report)
}
