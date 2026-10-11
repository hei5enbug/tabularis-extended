use crate::credential_store::{
    self, CredentialChange, CredentialStore, CredentialTransactionError, KeyringCredentialStore,
};
use crate::keychain_utils;
use directories::ProjectDirs;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::Manager;
use uuid::Uuid;

mod companion;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileImportPreview {
    pub source_path: String,
    pub target_path: String,
    pub source_exists: bool,
    pub target_is_empty: bool,
    pub can_import: bool,
    pub refusal_reason: Option<String>,
    pub connection_count: usize,
    pub plugin_package_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingCredential {
    pub connection_id: String,
    pub slot: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileImportResult {
    pub target_path: String,
    pub backup_path: String,
    pub destination_backup_path: Option<String>,
    pub plugin_backup_path: Option<String>,
    pub connection_count: usize,
    pub missing_credentials: Vec<MissingCredential>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProfileImportError {
    SourceUnavailable { path: String },
    DestinationNotEmpty { path: String, reason: String },
    SameSourceAndDestination { path: String },
    Failed { message: String },
    RecoveryRequired { operation: String, message: String },
}

impl From<CredentialTransactionError> for ProfileImportError {
    fn from(error: CredentialTransactionError) -> Self {
        match error {
            CredentialTransactionError::Failed { message } => Self::Failed { message },
            CredentialTransactionError::RecoveryRequired { message } => Self::RecoveryRequired {
                operation: "profileCredentialImport".to_string(),
                message,
            },
        }
    }
}

pub fn original_profile_default_dir() -> Result<PathBuf, ProfileImportError> {
    let project =
        ProjectDirs::from("", "", "tabularis").ok_or_else(|| ProfileImportError::Failed {
            message: "Original profile directory is unavailable on this platform".to_string(),
        })?;
    Ok(crate::paths::unnested_app_dir(
        project.config_dir(),
        cfg!(target_os = "windows"),
    ))
}

pub fn resolve_original_profile_dir(original_default: &Path, env_value: Option<String>) -> PathBuf {
    crate::storage_location::resolve_override_in(original_default, env_value)
        .unwrap_or_else(|| original_default.to_path_buf())
}

pub fn preview_profile_import_paths(
    source: &Path,
    target: &Path,
    plugin_target: &Path,
) -> ProfileImportPreview {
    preview_profile_import_paths_with_plugins(source, target, plugin_target, &[])
}

fn preview_profile_import_paths_with_plugins(
    source: &Path,
    target: &Path,
    plugin_target: &Path,
    plugin_sources: &[PathBuf],
) -> ProfileImportPreview {
    let source_exists = source.is_dir();
    let target_is_empty = directory_is_empty_or_missing(target);
    let lock_path =
        crate::persistence::connections_lock_path(&crate::paths::resolve_connections_path(target))
            .ok();
    let refusal_reason = if !source_exists {
        Some("Original profile is unavailable".to_string())
    } else if let Some(lock_path) = lock_path.as_deref() {
        profile_import_refusal_reason(source, target, plugin_target, lock_path, false)
    } else {
        Some("Could not inspect the destination profile lock".to_string())
    };
    let connection_count = ["connections.json", "connections.dev.json"]
        .into_iter()
        .map(|name| count_connections(&source.join(name)))
        .max()
        .unwrap_or_default();
    let plugin_package_count = std::iter::once(source.join("plugins"))
        .chain(plugin_sources.iter().cloned())
        .into_iter()
        .map(|path| {
            crate::plugins::layout::driver_directories(&path)
                .map(|items| items.len())
                .unwrap_or(0)
        })
        .sum();
    ProfileImportPreview {
        source_path: source.to_string_lossy().into_owned(),
        target_path: target.to_string_lossy().into_owned(),
        source_exists,
        target_is_empty,
        can_import: refusal_reason.is_none(),
        refusal_reason,
        connection_count,
        plugin_package_count,
    }
}

#[tauri::command]
pub fn preview_original_profile_import() -> Result<ProfileImportPreview, ProfileImportError> {
    let original_default = original_profile_default_dir()?;
    let source = resolve_original_profile_dir(
        &original_default,
        std::env::var(crate::storage_location::ORIGINAL_ENV_VAR).ok(),
    );
    let target = crate::paths::get_app_config_dir();
    Ok(preview_profile_import_paths_with_plugins(
        &source,
        &target,
        &crate::paths::get_plugins_dir(),
        &original_plugin_dirs(),
    ))
}

#[tauri::command]
pub fn import_original_profile(
    app: tauri::AppHandle,
) -> Result<ProfileImportResult, ProfileImportError> {
    let original_default = original_profile_default_dir()?;
    let source = resolve_original_profile_dir(
        &original_default,
        std::env::var(crate::storage_location::ORIGINAL_ENV_VAR).ok(),
    );
    let target = crate::paths::get_app_config_dir();
    let plugin_target = crate::paths::get_plugins_dir();
    let result = import_profile_with_data_sources(
        &source,
        &target,
        &plugin_target,
        &original_plugin_dirs(),
        &original_data_dirs(),
        &KeyringCredentialStore,
        "tabularis",
        keychain_utils::SERVICE_NAME,
    )?;
    let icons_dir = crate::paths::get_connection_icons_dir();
    if icons_dir.is_dir() {
        if let Err(error) = app.asset_protocol_scope().allow_directory(&icons_dir, true) {
            log::warn!(
                "Imported profile icons may not load because asset access could not be enabled: {error}"
            );
        }
    }
    Ok(result)
}

#[cfg(test)]
fn import_profile_with_store(
    source: &Path,
    target: &Path,
    plugin_target: &Path,
    plugin_sources: &[PathBuf],
    store: &dyn CredentialStore,
    source_service: &str,
    target_service: &str,
) -> Result<ProfileImportResult, ProfileImportError> {
    import_profile_with_data_sources_and_verifier(
        source,
        target,
        plugin_target,
        plugin_sources,
        &[],
        store,
        source_service,
        target_service,
        companion::verify_sqlserver_plus_fixture,
    )
}

fn import_profile_with_data_sources(
    source: &Path,
    target: &Path,
    plugin_target: &Path,
    plugin_sources: &[PathBuf],
    data_sources: &[PathBuf],
    store: &dyn CredentialStore,
    source_service: &str,
    target_service: &str,
) -> Result<ProfileImportResult, ProfileImportError> {
    import_profile_with_data_sources_and_verifier(
        source,
        target,
        plugin_target,
        plugin_sources,
        data_sources,
        store,
        source_service,
        target_service,
        verified_sqlserver_plus,
    )
}

fn import_profile_with_data_sources_and_verifier(
    source: &Path,
    target: &Path,
    plugin_target: &Path,
    plugin_sources: &[PathBuf],
    data_sources: &[PathBuf],
    store: &dyn CredentialStore,
    source_service: &str,
    target_service: &str,
    verify_sqlserver_plus: fn(&Path) -> bool,
) -> Result<ProfileImportResult, ProfileImportError> {
    if !source.is_dir() {
        return Err(ProfileImportError::SourceUnavailable {
            path: source.to_string_lossy().into_owned(),
        });
    }
    if paths_overlap(source, target).map_err(profile_error)? {
        return Err(ProfileImportError::SameSourceAndDestination {
            path: target.to_string_lossy().into_owned(),
        });
    }
    if fs::symlink_metadata(target).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(ProfileImportError::DestinationNotEmpty {
            path: target.to_string_lossy().into_owned(),
            reason: "Fork profile cannot be a symbolic link".to_string(),
        });
    }
    let target_parent = target.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(target_parent).map_err(profile_error)?;
    let target_existed_before_import = target.exists();
    let target_connection_path = crate::paths::resolve_connections_path(target);
    let _lock = crate::persistence::LockedConnectionsFile::open(&target_connection_path)
        .map_err(profile_error)?;
    let target_lock_path = crate::persistence::connections_lock_path(&target_connection_path)
        .map_err(profile_error)?;
    if let Some(reason) =
        profile_import_refusal_reason(source, target, plugin_target, &target_lock_path, true)
    {
        return Err(ProfileImportError::DestinationNotEmpty {
            path: target.to_string_lossy().into_owned(),
            reason,
        });
    }

    let backup = target_parent.join(format!(
        ".{}-original-backup-{}",
        target.file_name().unwrap_or_default().to_string_lossy(),
        Uuid::new_v4()
    ));
    let stage = target_parent.join(format!(
        ".{}-import-stage-{}",
        target.file_name().unwrap_or_default().to_string_lossy(),
        Uuid::new_v4()
    ));
    create_private_directory(&backup).map_err(profile_error)?;
    if let Err(error) = copy_tree_contents(source, &backup) {
        let _ = fs::remove_dir(&backup);
        return Err(profile_error(error));
    }

    let plugin_nested_in_profile = path_is_within(plugin_target, target).map_err(profile_error)?;
    let plugin_parent = plugin_target.parent().unwrap_or_else(|| Path::new("."));
    let plugin_stage_parent = if plugin_nested_in_profile {
        target_parent
    } else {
        plugin_parent
    };
    fs::create_dir_all(plugin_parent).map_err(profile_error)?;
    let plugin_stage =
        plugin_stage_parent.join(format!(".plugins-import-stage-{}", Uuid::new_v4()));
    create_private_directory(&stage).map_err(profile_error)?;
    let mut staging = ImportStaging::new(stage.clone(), plugin_stage.clone());
    staging.own_profile_stage();
    copy_profile_contents(source, &stage).map_err(profile_error)?;
    preserve_target_logs(target, &stage).map_err(profile_error)?;
    for data_source in data_sources {
        copy_missing_data_tree(
            &data_source.join("connection-icons"),
            &stage.join("connection-icons"),
        )
        .map_err(profile_error)?;
    }

    if plugin_target.exists() {
        create_private_directory(&plugin_stage).map_err(profile_error)?;
        staging.own_plugin_stage();
        copy_tree_contents(plugin_target, &plugin_stage).map_err(profile_error)?;
    } else {
        create_private_directory(&plugin_stage).map_err(profile_error)?;
        staging.own_plugin_stage();
    }
    for plugin_source in plugin_sources {
        copy_missing_children(plugin_source, &plugin_stage).map_err(profile_error)?;
    }
    if plugin_nested_in_profile {
        copy_tree_contents(&plugin_stage, &stage.join("plugins")).map_err(profile_error)?;
    }

    let sqlserver_plus_verified = verify_sqlserver_plus(&plugin_stage);
    let mut changes = Vec::new();
    let mut missing = Vec::new();
    let mut connection_count = 0usize;
    for name in ["connections.json", "connections.dev.json"] {
        let path = stage.join(name);
        if path.is_file() {
            connection_count = connection_count.max(transform_connection_file(
                &path,
                source_service,
                store,
                sqlserver_plus_verified,
                &mut changes,
                &mut missing,
            )?);
        }
    }
    let ssh_path = stage.join("ssh_connections.json");
    if ssh_path.is_file() {
        transform_ssh_file(&ssh_path, source_service, store, &mut changes, &mut missing)?;
    }

    let undo = credential_store::apply_changes(store, target_service, &changes)?;
    for change in changes.iter().filter(|change| change.value.is_some()) {
        let expected = change.value.as_deref().unwrap_or_default();
        match store.get(target_service, &change.account) {
            Ok(Some(actual)) if actual == expected => {}
            Ok(_) => {
                return rollback_import(
                    store,
                    &undo,
                    ProfileImportError::Failed {
                        message: "Fork keychain verification failed".to_string(),
                    },
                )
            }
            Err(message) => {
                return rollback_import(store, &undo, ProfileImportError::Failed { message })
            }
        }
    }

    let plugin_backup = plugin_parent.join(format!(".plugins-import-backup-{}", Uuid::new_v4()));
    let target_backup = target_parent.join(format!(".profile-import-bootstrap-{}", Uuid::new_v4()));
    let had_plugins = plugin_target.exists();
    let mut plugin_moved = false;
    let mut plugin_stage_installed = false;
    if !plugin_nested_in_profile && had_plugins {
        if let Err(error) = fs::rename(plugin_target, &plugin_backup) {
            return rollback_import(store, &undo, profile_error(error));
        }
        plugin_moved = true;
    }
    if !plugin_nested_in_profile {
        if let Err(error) = fs::rename(&plugin_stage, plugin_target) {
            let restore = restore_external_plugins(
                plugin_target,
                &plugin_backup,
                plugin_parent,
                plugin_moved,
                false,
                had_plugins,
            );
            return rollback_commit_failure(
                store,
                &undo,
                "profileDirectoryImport",
                profile_error(error),
                restore.err(),
            );
        }
        plugin_stage_installed = true;
    }
    if let Err(error) = fs::rename(target, &target_backup) {
        let restore = restore_external_plugins(
            plugin_target,
            &plugin_backup,
            plugin_parent,
            plugin_moved,
            plugin_stage_installed,
            had_plugins,
        );
        return rollback_commit_failure(
            store,
            &undo,
            "profileDirectoryImport",
            profile_error(error),
            restore.err(),
        );
    }
    if let Err(error) = fs::rename(&stage, target) {
        let restore_target = fs::rename(&target_backup, target)
            .err()
            .map(|error| error.to_string());
        let restore_plugins = restore_external_plugins(
            plugin_target,
            &plugin_backup,
            plugin_parent,
            plugin_moved,
            plugin_stage_installed,
            had_plugins,
        )
        .err();
        let recovery = [restore_target, restore_plugins]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        return rollback_commit_failure(
            store,
            &undo,
            "profileDirectoryImport",
            profile_error(error),
            (!recovery.is_empty()).then(|| recovery.join("; ")),
        );
    }
    if !target_existed_before_import {
        let _ = fs::remove_dir(&target_backup);
    }

    Ok(ProfileImportResult {
        target_path: target.to_string_lossy().into_owned(),
        backup_path: backup.to_string_lossy().into_owned(),
        destination_backup_path: target_existed_before_import
            .then(|| target_backup.to_string_lossy().into_owned()),
        plugin_backup_path: (!plugin_nested_in_profile && had_plugins)
            .then(|| plugin_backup.to_string_lossy().into_owned()),
        connection_count,
        missing_credentials: missing,
    })
}

fn rollback_import<T>(
    store: &dyn CredentialStore,
    undo: &credential_store::CredentialUndo,
    error: ProfileImportError,
) -> Result<T, ProfileImportError> {
    match credential_store::rollback_changes(store, undo) {
        Ok(()) => Err(error),
        Err(rollback) => Err(ProfileImportError::RecoveryRequired {
            operation: "profileCredentialImport".to_string(),
            message: format!("{error:?}; {rollback}"),
        }),
    }
}

fn rollback_commit_failure<T>(
    store: &dyn CredentialStore,
    undo: &credential_store::CredentialUndo,
    operation: &str,
    original: ProfileImportError,
    filesystem_recovery: Option<String>,
) -> Result<T, ProfileImportError> {
    let credential_recovery = credential_store::rollback_changes(store, undo).err();
    match (filesystem_recovery, credential_recovery) {
        (None, None) => Err(original),
        (filesystem, credentials) => Err(ProfileImportError::RecoveryRequired {
            operation: operation.to_string(),
            message: format!(
                "{original:?}; filesystem restore: {}; credential restore: {}",
                filesystem.unwrap_or_else(|| "completed".to_string()),
                credentials.unwrap_or_else(|| "completed".to_string())
            ),
        }),
    }
}

fn restore_external_plugins(
    plugin_target: &Path,
    plugin_backup: &Path,
    plugin_parent: &Path,
    plugin_moved: bool,
    plugin_stage_installed: bool,
    had_plugins: bool,
) -> Result<(), String> {
    let mut failures = Vec::new();
    if plugin_stage_installed && plugin_target.exists() {
        let failed_plugins =
            plugin_parent.join(format!(".plugins-import-failed-{}", Uuid::new_v4()));
        if let Err(error) = fs::rename(plugin_target, failed_plugins) {
            failures.push(format!("preserve partial plugins: {error}"));
        }
    }
    if plugin_moved && had_plugins {
        if let Err(error) = fs::rename(plugin_backup, plugin_target) {
            failures.push(format!("restore plugins: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn profile_import_refusal_reason(
    source: &Path,
    target: &Path,
    plugin_target: &Path,
    lock_path: &Path,
    lock_is_held: bool,
) -> Option<String> {
    if !source.is_dir() {
        return Some("Original profile is unavailable".to_string());
    }
    if fs::symlink_metadata(source).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Some("Original profile cannot be a symbolic link".to_string());
    }
    if fs::symlink_metadata(target).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Some("Fork profile cannot be a symbolic link".to_string());
    }
    match paths_overlap(source, target) {
        Ok(true) => return Some("Original and fork profile paths overlap".to_string()),
        Err(error) => return Some(error),
        Ok(false) => {}
    }
    if !lock_is_held {
        match profile_lock_is_available(lock_path) {
            Ok(false) => return Some("Fork profile is currently being changed".to_string()),
            Err(error) => return Some(error),
            Ok(true) => {}
        }
    }

    let plugin_is_nested = match path_is_within(plugin_target, target) {
        Ok(nested) => nested,
        Err(error) => return Some(error),
    };
    let entries = match fs::read_dir(target) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => return Some(error.to_string()),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => return Some(error.to_string()),
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => return Some(error.to_string()),
        };
        if metadata.file_type().is_symlink() {
            return Some(format!("Destination contains a symbolic link: {name}"));
        }
        if plugin_is_nested && name == "plugins" {
            if !metadata.is_dir() {
                return Some("The fork plugin path is not a directory".to_string());
            }
            continue;
        }
        match name.as_str() {
            "config.json" => {
                if !is_default_config_file(&path) {
                    return Some(
                        "Fork config.json contains non-default settings or unknown fields"
                            .to_string(),
                    );
                }
            }
            "connections.json" | "connections.dev.json" => {
                if !is_empty_connections_file(&path) {
                    return Some(format!(
                        "Fork {name} contains saved connections or unknown fields"
                    ));
                }
            }
            "ssh_connections.json" | "k8s_connections.json" => {
                if !is_empty_array_file(&path) {
                    return Some(format!("Fork {name} contains saved connection data"));
                }
            }
            "saved_queries" | "notebooks" | "themes" | "theme-packages" | "connection-icons" => {
                if !metadata.is_dir() || !directory_is_empty_or_missing(&path) {
                    return Some(format!("Fork {name} contains saved user data"));
                }
            }
            "logs" | "log" => {
                if !metadata.is_dir() {
                    return Some(format!("Fork {name} is not a directory"));
                }
            }
            "tabularis.alive" | "pending_approvals" | ".mcp_session_state.json" => {}
            _ if metadata.is_file() && metadata.len() == 0 => {}
            _ if metadata.is_dir() && directory_is_empty_or_missing(&path) => {}
            _ => return Some(format!("Fork profile contains unrecognized data: {name}")),
        }
    }
    None
}

fn profile_lock_is_available(lock_path: &Path) -> Result<bool, String> {
    use fs2::FileExt;
    if !lock_path.exists() {
        return Ok(true);
    }
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|error| error.to_string())?;
    match lock.try_lock_exclusive() {
        Ok(()) => {
            let _ = lock.unlock();
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn is_default_config_file(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(raw) = serde_json::from_str::<Value>(&content) else {
        return false;
    };
    let Some(raw_object) = raw.as_object() else {
        return false;
    };
    let Ok(default_value) = serde_json::to_value(crate::config::AppConfig::default()) else {
        return false;
    };
    let Some(default_object) = default_value.as_object() else {
        return false;
    };
    if raw_object
        .keys()
        .any(|key| !default_object.contains_key(key))
    {
        return false;
    }
    if raw_object.is_empty() {
        return true;
    }
    let Ok(config) = serde_json::from_value::<crate::config::AppConfig>(raw) else {
        return false;
    };
    serde_json::to_value(config).is_ok_and(|value| value == default_value)
}

fn is_empty_connections_file(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(raw) = serde_json::from_str::<Value>(&content) else {
        return false;
    };
    match raw {
        Value::Array(items) => items.is_empty(),
        Value::Object(object) => {
            if object
                .keys()
                .any(|key| !["connections", "groups", "tags"].contains(&key.as_str()))
            {
                return false;
            }
            ["connections", "groups", "tags"].into_iter().all(|field| {
                object
                    .get(field)
                    .map_or(true, |value| value.as_array().is_some_and(Vec::is_empty))
            })
        }
        _ => false,
    }
}

fn is_empty_array_file(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<Vec<Value>>(&content).ok())
        .is_some_and(|values| values.is_empty())
}

fn paths_overlap(left: &Path, right: &Path) -> Result<bool, String> {
    let left = crate::paths::canonicalize_with_missing(left).map_err(|error| error.to_string())?;
    let right =
        crate::paths::canonicalize_with_missing(right).map_err(|error| error.to_string())?;
    Ok(left.starts_with(&right) || right.starts_with(&left))
}

fn path_is_within(path: &Path, parent: &Path) -> Result<bool, String> {
    let path = crate::paths::canonicalize_with_missing(path).map_err(|error| error.to_string())?;
    let parent =
        crate::paths::canonicalize_with_missing(parent).map_err(|error| error.to_string())?;
    Ok(path != parent && path.starts_with(parent))
}

fn transform_connection_file(
    path: &Path,
    source_service: &str,
    store: &dyn CredentialStore,
    sqlserver_plus_verified: bool,
    changes: &mut Vec<CredentialChange>,
    missing: &mut Vec<MissingCredential>,
) -> Result<usize, ProfileImportError> {
    let content = fs::read_to_string(path).map_err(profile_error)?;
    let mut root: Value = serde_json::from_str(&content).map_err(profile_error)?;
    let connections = match &mut root {
        Value::Array(items) => items,
        Value::Object(object) => object
            .entry("connections")
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| ProfileImportError::Failed {
                message: "Connections field is not an array".to_string(),
            })?,
        _ => {
            return Err(ProfileImportError::Failed {
                message: "Connections profile has an invalid root".to_string(),
            })
        }
    };
    for connection in connections.iter_mut() {
        let Some(object) = connection.as_object_mut() else {
            continue;
        };
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(params) = object.get_mut("params").and_then(Value::as_object_mut) {
            let driver = params
                .get("driver")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(driver, "postgres" | "postgresql") {
                params.insert(
                    "driver".to_string(),
                    Value::String("postgresql".to_string()),
                );
            } else if sqlserver_plus_verified
                && matches!(driver, "mssql" | "sqlserver" | "sqlserver-entra")
            {
                params.insert(
                    "driver".to_string(),
                    Value::String("sqlserver-plus".to_string()),
                );
            }
            migrate_connection_secrets(&id, params, source_service, store, changes, missing)?;
        }
    }
    let count = connections.len();
    fs::write(
        path,
        serde_json::to_vec_pretty(&root).map_err(profile_error)?,
    )
    .map_err(profile_error)?;
    Ok(count)
}

fn transform_ssh_file(
    path: &Path,
    source_service: &str,
    store: &dyn CredentialStore,
    changes: &mut Vec<CredentialChange>,
    missing: &mut Vec<MissingCredential>,
) -> Result<(), ProfileImportError> {
    let content = fs::read_to_string(path).map_err(profile_error)?;
    let mut roots: Vec<Value> = serde_json::from_str(&content).map_err(profile_error)?;
    for connection in &mut roots {
        let Some(object) = connection.as_object_mut() else {
            continue;
        };
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let saved = object
            .get("save_in_keychain")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        for (field, slot, account_slot) in [
            ("password", "sshPassword", "ssh"),
            ("key_passphrase", "sshKeyPassphrase", "ssh_passphrase"),
        ] {
            let inline = object
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let secret = if inline.is_some() {
                inline
            } else if saved {
                match store.get(source_service, &format!("{id}:{account_slot}")) {
                    Ok(secret) => secret,
                    Err(message) => return Err(ProfileImportError::Failed { message }),
                }
            } else {
                None
            };
            if let Some(secret) = secret {
                changes.push(CredentialChange {
                    account: format!("{id}:{account_slot}"),
                    value: Some(secret),
                });
                object.remove(field);
                object.insert("save_in_keychain".to_string(), Value::Bool(true));
            } else if saved {
                missing.push(MissingCredential {
                    connection_id: id.clone(),
                    slot: slot.to_string(),
                });
            }
        }
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(&roots).map_err(profile_error)?,
    )
    .map_err(profile_error)
}

fn migrate_connection_secrets(
    id: &str,
    params: &mut serde_json::Map<String, Value>,
    source_service: &str,
    store: &dyn CredentialStore,
    changes: &mut Vec<CredentialChange>,
    missing: &mut Vec<MissingCredential>,
) -> Result<(), ProfileImportError> {
    let saved = params
        .get("save_in_keychain")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let entra_auth = params
        .get("extra")
        .and_then(Value::as_object)
        .and_then(|extra| extra.get("auth_mode"))
        .and_then(Value::as_str)
        == Some("entra_user");
    let ssh_enabled = params.get("ssh_enabled").and_then(Value::as_bool) == Some(true);
    let ssh_key_configured = params
        .get("ssh_key_file")
        .and_then(Value::as_str)
        .is_some_and(|path| !path.is_empty());
    for (field, alias, slot, report_slot) in [
        ("password", None, "db", "password"),
        ("ssh_password", None, "ssh", "sshPassword"),
        (
            "ssh_key_passphrase",
            None,
            "ssh_passphrase",
            "sshKeyPassphrase",
        ),
        (
            "connection_uri",
            Some("connectionUri"),
            "connection_uri",
            "connectionUri",
        ),
    ] {
        let inline = params
            .get(field)
            .or_else(|| alias.and_then(|alias| params.get(alias)))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let was_uri_stored = slot == "connection_uri"
            && (params
                .get("connection_uri_in_keychain")
                .and_then(Value::as_bool)
                == Some(true)
                || params
                    .get("connectionUriInKeychain")
                    .and_then(Value::as_bool)
                    == Some(true));
        let should_read_source = match slot {
            "db" => saved && !entra_auth,
            "ssh" => saved && ssh_enabled,
            "ssh_passphrase" => saved && ssh_enabled && ssh_key_configured,
            "connection_uri" => saved && was_uri_stored,
            _ => false,
        };
        let secret = if inline.is_some() {
            inline
        } else if should_read_source {
            match store.get(source_service, &format!("{id}:{slot}")) {
                Ok(secret) => secret,
                Err(message) => return Err(ProfileImportError::Failed { message }),
            }
        } else {
            None
        };
        if let Some(secret) = secret {
            changes.push(CredentialChange {
                account: format!("{id}:{slot}"),
                value: Some(secret),
            });
            params.remove(field);
            if let Some(alias) = alias {
                params.remove(alias);
            }
            params.insert("save_in_keychain".to_string(), Value::Bool(true));
            if slot == "connection_uri" {
                params.insert("connection_uri_in_keychain".to_string(), Value::Bool(true));
            }
        } else if should_read_source {
            missing.push(MissingCredential {
                connection_id: id.to_string(),
                slot: report_slot.to_string(),
            });
        }
    }
    Ok(())
}

fn verified_sqlserver_plus(plugin_root: &Path) -> bool {
    companion::verified_sqlserver_plus(plugin_root)
}

fn original_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(project) = ProjectDirs::from("", "", "tabularis") {
        dirs.push(
            crate::paths::unnested_app_dir(project.data_dir(), cfg!(target_os = "windows"))
                .join("plugins"),
        );
    }
    if let Some(project) = ProjectDirs::from("com", "debba", "tabularis") {
        let legacy = project.data_dir().join("plugins");
        if !dirs.contains(&legacy) {
            dirs.push(legacy);
        }
    }
    dirs
}

fn original_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(project) = ProjectDirs::from("", "", "tabularis") {
        dirs.push(crate::paths::unnested_app_dir(
            project.data_dir(),
            cfg!(target_os = "windows"),
        ));
    }
    if let Some(project) = ProjectDirs::from("com", "debba", "tabularis") {
        let legacy = project.data_dir().to_path_buf();
        if !dirs.contains(&legacy) {
            dirs.push(legacy);
        }
    }
    dirs
}

fn copy_profile_contents(source: &Path, stage: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if [
            "storage-location.json",
            "plugins",
            "logs",
            "log",
            "tabularis.alive",
            "pending_approvals",
            ".mcp_session_state.json",
        ]
        .contains(&name.as_ref())
            || name.ends_with(".lock")
            || name.starts_with(".") && name.contains(".tmp")
        {
            continue;
        }
        copy_tree(&entry.path(), &stage.join(entry.file_name()))?;
    }
    Ok(())
}

fn copy_missing_children(source: &Path, target: &Path) -> std::io::Result<()> {
    if !source.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if destination.exists() {
            let source_metadata = fs::symlink_metadata(entry.path())?;
            let target_metadata = fs::symlink_metadata(&destination)?;
            if source_metadata.file_type().is_symlink() || target_metadata.file_type().is_symlink()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Plugin package roots cannot contain symbolic links",
                ));
            }
            if crate::plugins::layout::is_kind_directory(&entry.file_name().to_string_lossy()) {
                if !source_metadata.is_dir() || !target_metadata.is_dir() {
                    continue;
                }
                copy_missing_packages(&entry.path(), &destination)?;
            }
            continue;
        }
        copy_tree(&entry.path(), &destination)?;
    }
    Ok(())
}

fn copy_missing_packages(source: &Path, target: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if destination.exists() {
            let source_metadata = fs::symlink_metadata(entry.path())?;
            let target_metadata = fs::symlink_metadata(&destination)?;
            if source_metadata.file_type().is_symlink() || target_metadata.file_type().is_symlink()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Plugin package roots cannot contain symbolic links",
                ));
            }
            continue;
        }
        copy_tree(&entry.path(), &destination)?;
    }
    Ok(())
}

fn copy_missing_data_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    if !source.exists() {
        return Ok(());
    }
    let source_metadata = fs::symlink_metadata(source)?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Profile data root must be a real directory",
        ));
    }
    if !target.exists() {
        return copy_tree(source, target);
    }
    let target_metadata = fs::symlink_metadata(target)?;
    if target_metadata.file_type().is_symlink() || !target_metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Fork profile data root must be a real directory",
        ));
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if destination.exists() {
            let metadata = fs::symlink_metadata(&destination)?;
            let source_metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || source_metadata.file_type().is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Profile icon data cannot contain symbolic links",
                ));
            }
            if metadata.is_dir() && source_metadata.is_dir() {
                copy_missing_data_tree(&entry.path(), &destination)?;
            }
            continue;
        }
        copy_tree(&entry.path(), &destination)?;
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Profile contains a symlink",
        ));
    }
    if metadata.is_dir() {
        fs::create_dir(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
        Ok(())
    } else if metadata.is_file() {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target).map(|_| ())
    } else {
        Ok(())
    }
}

fn copy_tree_contents(source: &Path, target: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Plugin package root must be a real directory",
        ));
    }
    if !target.exists() {
        fs::create_dir(target)?;
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        copy_tree(&entry.path(), &target.join(entry.file_name()))?;
    }
    Ok(())
}

fn preserve_target_logs(target: &Path, stage: &Path) -> std::io::Result<()> {
    for name in ["logs", "log"] {
        let source = target.join(name);
        if source.is_dir() {
            copy_tree(&source, &stage.join(name))?;
        }
    }
    Ok(())
}

struct ImportStaging {
    profile_stage: PathBuf,
    plugin_stage: PathBuf,
    owns_profile_stage: bool,
    owns_plugin_stage: bool,
}

impl ImportStaging {
    fn new(profile_stage: PathBuf, plugin_stage: PathBuf) -> Self {
        Self {
            profile_stage,
            plugin_stage,
            owns_profile_stage: false,
            owns_plugin_stage: false,
        }
    }

    fn own_profile_stage(&mut self) {
        self.owns_profile_stage = true;
    }

    fn own_plugin_stage(&mut self) {
        self.owns_plugin_stage = true;
    }
}

impl Drop for ImportStaging {
    fn drop(&mut self) {
        if self.owns_profile_stage {
            let _ = fs::remove_dir_all(&self.profile_stage);
        }
        if self.owns_plugin_stage {
            let _ = fs::remove_dir_all(&self.plugin_stage);
        }
    }
}

fn directory_is_empty_or_missing(path: &Path) -> bool {
    !path.exists()
        || (path.is_dir()
            && fs::read_dir(path)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false))
}

fn count_connections(path: &Path) -> usize {
    let Ok(content) = fs::read_to_string(path) else {
        return 0;
    };
    let Ok(root) = serde_json::from_str::<Value>(&content) else {
        return 0;
    };
    match root {
        Value::Array(values) => values.len(),
        Value::Object(object) => object
            .get("connections")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default(),
        _ => 0,
    }
}

fn create_private_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)?;
    set_private_directory(path)
}

fn set_private_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn profile_error(error: impl std::fmt::Display) -> ProfileImportError {
    ProfileImportError::Failed {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::tempdir;

    #[derive(Default)]
    struct FakeStore {
        values: Mutex<HashMap<(String, String), String>>,
        writes: Mutex<usize>,
        corrupt_read_after_write: Mutex<Option<(String, String)>>,
    }

    impl FakeStore {
        fn seed(&self, service: &str, account: &str, value: &str) {
            self.values.lock().unwrap().insert(
                (service.to_string(), account.to_string()),
                value.to_string(),
            );
        }

        fn value(&self, service: &str, account: &str) -> Option<String> {
            self.values
                .lock()
                .unwrap()
                .get(&(service.to_string(), account.to_string()))
                .cloned()
        }

        fn corrupt_next_read_after_write(&self, service: &str, account: &str) {
            *self.corrupt_read_after_write.lock().unwrap() =
                Some((service.to_string(), account.to_string()));
        }
    }

    impl CredentialStore for FakeStore {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>, String> {
            let account_key = (service.to_string(), account.to_string());
            if *self.writes.lock().unwrap() > 0
                && self.corrupt_read_after_write.lock().unwrap().as_ref() == Some(&account_key)
            {
                self.corrupt_read_after_write.lock().unwrap().take();
                return Ok(Some("synthetic-verification-mismatch".to_string()));
            }
            Ok(self.value(service, account))
        }

        fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String> {
            self.seed(service, account, value);
            *self.writes.lock().unwrap() += 1;
            Ok(())
        }

        fn delete(&self, service: &str, account: &str) -> Result<(), String> {
            self.values
                .lock()
                .unwrap()
                .remove(&(service.to_string(), account.to_string()));
            *self.writes.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn write_sqlserver_plus_package(root: &Path) -> PathBuf {
        let package = root.join("drivers/sqlserver-plus");
        fs::create_dir_all(package.join("ui/dist")).unwrap();
        fs::create_dir_all(package.join("explain/dist")).unwrap();
        fs::write(
            package.join(".tabularium"),
            companion::TEST_FIXTURE_MANIFEST,
        )
        .unwrap();
        let executable = if cfg!(windows) {
            "sqlserver-plugin.exe"
        } else {
            "sqlserver-plugin"
        };
        fs::write(package.join(executable), "synthetic executable").unwrap();
        fs::write(package.join("ui/dist/index.js"), "synthetic ui bundle").unwrap();
        fs::write(
            package.join("explain/dist/index.iife.js"),
            "synthetic explain bundle",
        )
        .unwrap();
        root.to_path_buf()
    }

    fn write_connection_profile(path: &Path, connections: &[Value]) {
        fs::write(
            path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "futureRoot": {"kept": true},
                "connections": connections,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn 원본_프로필_저장소_환경변수_경로를_우선한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let default = root.path().join("original-default");
        let override_path = root.path().join("original-custom");

        // when
        let resolved = resolve_original_profile_dir(
            &default,
            Some(override_path.to_string_lossy().into_owned()),
        );

        // then
        assert_eq!(resolved, override_path);
    }

    #[test]
    fn 가져오기는_알_수_없는_필드를_보존하고_원본을_변경하지_않는다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let source = root.path().join("original");
        let target = root.path().join("fork");
        let plugins = root.path().join("data/plugins");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("config.json"),
            r#"{"unknownSetting":{"keep":true}}"#,
        )
        .unwrap();
        fs::write(source.join("connections.json"), r#"{"futureRoot":7,"connections":[{"id":"c1","name":"C","params":{"driver":"postgres","host":"postgres.synthetic.invalid","username":"postgres-user","ssl_mode":"verify-full","save_in_keychain":false,"password":"inline-secret","database":"main","extra":{"mode":"preserve"},"futureParam":9},"futureConnection":true}]}"#).unwrap();
        let store = FakeStore::default();

        // when
        let result =
            import_profile_with_store(&source, &target, &plugins, &[], &store, "original", "fork")
                .expect("임시 프로필 가져오기");

        // then
        let imported: Value =
            serde_json::from_slice(&fs::read(target.join("connections.json")).unwrap()).unwrap();
        let connection = &imported["connections"][0];
        assert_eq!(connection["id"], "c1");
        assert_eq!(connection["params"]["driver"], "postgresql");
        assert_eq!(connection["params"]["host"], "postgres.synthetic.invalid");
        assert_eq!(connection["params"]["username"], "postgres-user");
        assert_eq!(connection["params"]["ssl_mode"], "verify-full");
        assert_eq!(connection["futureConnection"], true);
        assert_eq!(connection["params"]["futureParam"], 9);
        assert_eq!(connection["params"]["extra"]["mode"], "preserve");
        assert!(connection["params"].get("password").is_none());
        assert_eq!(
            store.get("fork", "c1:db").unwrap().as_deref(),
            Some("inline-secret")
        );
        assert!(source.join("connections.json").exists());
        assert!(Path::new(&result.backup_path)
            .join("connections.json")
            .exists());
    }

    #[test]
    fn 검증된_드라이버로_열네개_연결의_인증_및_알수없는_필드를_보존한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let source = root.path().join("original");
        let target = root.path().join("fork");
        let plugins = root.path().join("fork-data/plugins");
        let source_plugins = write_sqlserver_plus_package(&root.path().join("source-plugins"));
        fs::create_dir_all(&source).unwrap();

        let mut expected = Vec::new();
        for index in 0..6 {
            let id = format!("entra-{index}");
            let host = format!("sql-{index}.database.windows.net");
            let username = format!("entra-{index}@example.test");
            let database = format!("tenant_db_{index}");
            let tenant = format!("synthetic-tenant-{index}");
            let object_id = format!("synthetic-object-{index}");
            let ssl_mode = if index % 2 == 0 {
                "verify-full"
            } else {
                "verify_identity"
            };
            let extra = serde_json::json!({
                "auth_mode": "entra_user",
                "auth_source": "azure_cli",
                "tenant_id": tenant,
                "entra_object_id": object_id,
                "azure_cli_path": format!("/synthetic/az-{index}"),
                "custom_auth_extension": format!("preserve-{index}"),
            });
            expected.push(serde_json::json!({
                "id": id,
                "name": format!("Entra {index}"),
                "group_id": format!("entra-group-{index}"),
                "params": {
                    "driver": if index % 2 == 0 { "sqlserver-entra" } else { "sqlserver" },
                    "host": host,
                    "port": 1433,
                    "username": username,
                    "database": database,
                    "ssl_mode": ssl_mode,
                    "ssl_ca": format!("/synthetic/ca-{index}.pem"),
                    "trust_server_certificate": false,
                    "save_in_keychain": false,
                    "extra": extra,
                    "futureParam": format!("keep-{index}"),
                },
                "futureConnection": format!("future-{index}"),
            }));
        }
        let mut sql_passwords = Vec::new();
        let store = FakeStore::default();
        for index in 0..2 {
            let id = format!("sql-password-{index}");
            let password = format!("synthetic-sql-password-{index}");
            sql_passwords.push(password.clone());
            let mut params = serde_json::json!({
                "driver": "sqlserver",
                "host": format!("sql-password-{index}.database.windows.net"),
                "port": 1433,
                "username": format!("sql-user-{index}"),
                "database": format!("sql_db_{index}"),
                "ssl_mode": "verify-full",
                "ssl_ca": format!("/synthetic/sql-ca-{index}.pem"),
                "save_in_keychain": index == 1,
                "extra": {"auth_configuration": format!("sql-password-{index}")},
            });
            if index == 0 {
                params["password"] = Value::String(password.clone());
            }
            expected.push(serde_json::json!({
                "id": id,
                "name": format!("SQL password {index}"),
                "params": params,
            }));
            if index == 1 {
                store.seed("original", &format!("{id}:db"), &password);
            }
        }
        let mut postgres_passwords = Vec::new();
        for index in 0..6 {
            let id = format!("postgres-profile-{index}");
            let password = format!("synthetic-postgres-password-{index}");
            postgres_passwords.push(password.clone());
            expected.push(serde_json::json!({
                "id": id,
                "name": format!("PostgreSQL {index}"),
                "group_id": format!("postgres-group-{index}"),
                "params": {
                    "driver": "postgres",
                    "host": format!("postgres-{index}.synthetic.invalid"),
                    "port": 5432,
                    "username": format!("postgres-user-{index}"),
                    "database": format!("postgres_database_{index}"),
                    "ssl_mode": "verify-full",
                    "ssl_ca": format!("/synthetic/postgres-ca-{index}.pem"),
                    "save_in_keychain": true,
                    "extra": {"futureAuthMode": format!("postgres-auth-{index}")},
                    "futureParam": format!("postgres-future-{index}"),
                },
                "futureConnection": format!("postgres-future-connection-{index}"),
            }));
            store.seed("original", &format!("{id}:db"), &password);
        }

        write_connection_profile(&source.join("connections.json"), &expected);

        // when
        let result = import_profile_with_store(
            &source,
            &target,
            &plugins,
            &[source_plugins],
            &store,
            "original",
            "fork",
        )
        .expect("열네 개의 가상 연결 가져오기");

        // then
        let imported: Value =
            serde_json::from_slice(&fs::read(target.join("connections.json")).unwrap()).unwrap();
        let actual = imported["connections"].as_array().unwrap();
        assert_eq!(actual.len(), 14);
        assert_eq!(imported["futureRoot"]["kept"], true);
        assert_eq!(result.connection_count, 14);
        assert!(result.missing_credentials.is_empty());
        for index in 0..6 {
            assert_eq!(actual[index]["id"], expected[index]["id"]);
            assert_eq!(actual[index]["name"], expected[index]["name"]);
            assert_eq!(actual[index]["group_id"], expected[index]["group_id"]);
            assert_eq!(
                actual[index]["futureConnection"],
                expected[index]["futureConnection"]
            );
            assert_eq!(actual[index]["params"]["driver"], "sqlserver-plus");
            for field in [
                "host",
                "port",
                "username",
                "database",
                "ssl_mode",
                "ssl_ca",
                "trust_server_certificate",
                "extra",
                "futureParam",
            ] {
                assert_eq!(
                    actual[index]["params"][field],
                    expected[index]["params"][field]
                );
            }
        }
        for index in 6..8 {
            assert_eq!(actual[index]["id"], expected[index]["id"]);
            assert_eq!(actual[index]["params"]["driver"], "sqlserver-plus");
            for field in [
                "host", "port", "username", "database", "ssl_mode", "ssl_ca", "extra",
            ] {
                assert_eq!(
                    actual[index]["params"][field],
                    expected[index]["params"][field]
                );
            }
            assert!(actual[index]["params"].get("password").is_none());
            assert_eq!(actual[index]["params"]["save_in_keychain"], true);
        }
        for (index, password) in sql_passwords.iter().enumerate() {
            assert_eq!(
                store
                    .value("fork", &format!("sql-password-{index}:db"))
                    .as_deref(),
                Some(password.as_str())
            );
        }
        for index in 0..6 {
            let connection_index = index + 8;
            assert_eq!(
                actual[connection_index]["id"],
                expected[connection_index]["id"]
            );
            assert_eq!(
                actual[connection_index]["name"],
                expected[connection_index]["name"]
            );
            assert_eq!(
                actual[connection_index]["group_id"],
                expected[connection_index]["group_id"]
            );
            assert_eq!(
                actual[connection_index]["futureConnection"],
                expected[connection_index]["futureConnection"]
            );
            assert_eq!(actual[connection_index]["params"]["driver"], "postgresql");
            for field in [
                "host",
                "port",
                "username",
                "database",
                "ssl_mode",
                "ssl_ca",
                "extra",
                "futureParam",
            ] {
                assert_eq!(
                    actual[connection_index]["params"][field],
                    expected[connection_index]["params"][field]
                );
            }
            assert_eq!(actual[connection_index]["params"]["save_in_keychain"], true);
            assert!(actual[connection_index]["params"].get("password").is_none());
            assert_eq!(
                store
                    .value("fork", &format!("postgres-profile-{index}:db"))
                    .as_deref(),
                Some(postgres_passwords[index].as_str())
            );
        }
        assert!(source.join("connections.json").exists());
        assert!(Path::new(&result.backup_path)
            .join("connections.json")
            .exists());
    }

    #[test]
    fn 비어_있지_않은_대상을_변경하지_않고_거부한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let source = root.path().join("original");
        let target = root.path().join("fork");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("keep.txt"), "keep").unwrap();
        let store = FakeStore::default();

        // when
        let result = import_profile_with_store(
            &source,
            &target,
            &root.path().join("plugins"),
            &[],
            &store,
            "original",
            "fork",
        );

        // then
        assert!(matches!(
            result,
            Err(ProfileImportError::DestinationNotEmpty { .. })
        ));
        assert_eq!(fs::read_to_string(target.join("keep.txt")).unwrap(), "keep");
    }

    #[test]
    fn 미리보기는_비어_있는_첫_실행_프로필에서_경로를_만들지_않는다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let source = root.path().join("original");
        let target = root.path().join("fork");
        let plugin_target = target.join("plugins");
        fs::create_dir_all(&source).unwrap();

        // when
        let preview = preview_profile_import_paths(&source, &target, &plugin_target);

        // then
        assert!(
            preview.can_import,
            "비어 있는 첫 실행 프로필에서 가져올 수 있어야 합니다"
        );
        assert!(preview.refusal_reason.is_none());
        assert!(
            !target.exists(),
            "미리보기는 대상 프로필을 만들지 않아야 합니다"
        );
        let connection_path = crate::paths::resolve_connections_path(&target);
        let lock_path = crate::persistence::connections_lock_path(&connection_path).unwrap();
        assert!(
            !lock_path.exists(),
            "미리보기는 프로필 잠금 파일을 만들지 않아야 합니다"
        );
    }

    #[test]
    fn 기본_부트스트랩_가져오기가_로그를_보존한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let source = root.path().join("original");
        let target = root.path().join("fork");
        let plugin_target = root.path().join("fork-data/plugins");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(target.join("saved_queries")).unwrap();
        fs::create_dir_all(target.join("notebooks")).unwrap();
        fs::create_dir_all(target.join("logs")).unwrap();
        fs::write(
            target.join("config.json"),
            serde_json::to_vec(&crate::config::AppConfig::default()).unwrap(),
        )
        .unwrap();
        fs::write(
            target.join("connections.json"),
            r#"{"connections":[],"groups":[],"tags":[]}"#,
        )
        .unwrap();
        fs::write(target.join("logs/bootstrap.log"), "preserve startup log").unwrap();
        fs::write(source.join("config.json"), "{}").unwrap();
        write_connection_profile(&source.join("connections.json"), &[]);

        // when
        let preview = preview_profile_import_paths(&source, &target, &plugin_target);
        let result = import_profile_with_store(
            &source,
            &target,
            &plugin_target,
            &[],
            &FakeStore::default(),
            "original",
            "fork",
        );

        // then
        assert!(
            preview.can_import,
            "기본 부트스트랩 파일은 안전하게 교체할 수 있어야 합니다"
        );
        assert!(
            result.is_ok(),
            "기본 부트스트랩 프로필 가져오기가 가능해야 합니다"
        );
        assert_eq!(
            fs::read_to_string(target.join("logs/bootstrap.log")).unwrap(),
            "preserve startup log"
        );
        assert!(target.join("connections.json").is_file());
        assert!(target.join("config.json").is_file());
    }

    #[test]
    fn 중첩_플러그인과_로그를_보존하고_검증된_패키지만_매핑한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let source = root.path().join("original");
        let target = root.path().join("fork");
        let plugin_target = target.join("plugins");
        let source_plugins = write_sqlserver_plus_package(&root.path().join("source-plugins"));
        let source_data = root.path().join("original-data");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(source_data.join("connection-icons")).unwrap();
        fs::create_dir_all(plugin_target.join("drivers/kept-driver")).unwrap();
        fs::create_dir_all(source_plugins.join("drivers/kept-driver")).unwrap();
        fs::create_dir_all(target.join("logs")).unwrap();
        fs::write(
            source_data.join("connection-icons/custom-icon.png"),
            "synthetic icon",
        )
        .unwrap();
        fs::write(
            source_plugins.join("drivers/kept-driver/source-only.txt"),
            "must not merge into an existing package",
        )
        .unwrap();
        fs::write(
            plugin_target.join("drivers/kept-driver/fixture.txt"),
            "keep plugin",
        )
        .unwrap();
        fs::write(target.join("logs/fixture.log"), "keep log").unwrap();
        write_connection_profile(
            &source.join("connections.json"),
            &[serde_json::json!({
                "id": "sql-connection-id",
                "name": "SQL connection",
                "params": {
                    "driver": "sqlserver-entra",
                    "host": "synthetic.database.windows.net",
                    "username": "synthetic@example.test",
                    "database": "fixture-db",
                    "ssl_mode": "verify-full",
                    "extra": {"auth_mode": "entra_user", "tenant_id": "synthetic-tenant"},
                },
            })],
        );

        // when
        let result = import_profile_with_data_sources_and_verifier(
            &source,
            &target,
            &plugin_target,
            &[source_plugins],
            &[source_data],
            &FakeStore::default(),
            "original",
            "fork",
            companion::verify_sqlserver_plus_fixture,
        )
        .expect("중첩 플러그인 프로필 가져오기");

        // then
        let imported: Value =
            serde_json::from_slice(&fs::read(target.join("connections.json")).unwrap()).unwrap();
        assert_eq!(imported["connections"][0]["id"], "sql-connection-id");
        assert_eq!(
            imported["connections"][0]["params"]["driver"],
            "sqlserver-plus"
        );
        assert_eq!(
            fs::read_to_string(plugin_target.join("drivers/kept-driver/fixture.txt")).unwrap(),
            "keep plugin"
        );
        assert!(!plugin_target
            .join("drivers/kept-driver/source-only.txt")
            .exists());
        assert!(plugin_target
            .join("drivers/sqlserver-plus/.tabularium")
            .is_file());
        assert_eq!(
            fs::read_to_string(target.join("connection-icons/custom-icon.png")).unwrap(),
            "synthetic icon"
        );
        assert_eq!(
            fs::read_to_string(target.join("logs/fixture.log")).unwrap(),
            "keep log"
        );
        assert!(result.destination_backup_path.is_some());
        assert!(result.plugin_backup_path.is_none());
        assert!(!root
            .path()
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".plugins-import-stage-")));
    }

    #[test]
    fn 검증된_sql_server_plus_패키지가_없으면_드라이버_id를_유지한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let source = root.path().join("original");
        let target = root.path().join("fork");
        let plugin_target = root.path().join("fork-data/plugins");
        fs::create_dir_all(&source).unwrap();
        write_connection_profile(
            &source.join("connections.json"),
            &[serde_json::json!({
                "id": "legacy-sql-id",
                "name": "Legacy SQL",
                "params": {"driver": "sqlserver", "database": "fixture"},
            })],
        );

        // when
        import_profile_with_store(
            &source,
            &target,
            &plugin_target,
            &[],
            &FakeStore::default(),
            "original",
            "fork",
        )
        .expect("기존 SQL 연결 가져오기");

        // then
        let imported: Value =
            serde_json::from_slice(&fs::read(target.join("connections.json")).unwrap()).unwrap();
        assert_eq!(imported["connections"][0]["id"], "legacy-sql-id");
        assert_eq!(imported["connections"][0]["params"]["driver"], "sqlserver");
    }

    #[test]
    fn sql_server_plus는_정확한_버전과_ui_explain_자산이_있어야_검증된다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let plugin_root = write_sqlserver_plus_package(root.path());
        let manifest_path = plugin_root.join("drivers/sqlserver-plus/.tabularium");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();

        // when
        assert!(companion::verify_sqlserver_plus_fixture(&plugin_root));
        manifest["version"] = Value::String("1.0.0".to_string());
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let wrong_version_is_rejected = !companion::verify_sqlserver_plus_fixture(&plugin_root);
        fs::write(&manifest_path, companion::TEST_FIXTURE_MANIFEST).unwrap();
        fs::remove_file(plugin_root.join("drivers/sqlserver-plus/explain/dist/index.iife.js"))
            .unwrap();

        // then
        assert!(wrong_version_is_rejected);
        assert!(!companion::verify_sqlserver_plus_fixture(&plugin_root));
    }

    #[test]
    fn 기본값이_아닌_부트스트랩_설정을_보존하고_가져오기를_거부한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let source = root.path().join("original");
        let target = root.path().join("fork");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("config.json"), r#"{"theme":"dark"}"#).unwrap();
        let store = FakeStore::default();

        // when
        let result = import_profile_with_store(
            &source,
            &target,
            &root.path().join("plugins"),
            &[],
            &store,
            "original",
            "fork",
        );

        // then
        assert!(matches!(
            result,
            Err(ProfileImportError::DestinationNotEmpty { .. })
        ));
        assert_eq!(
            fs::read_to_string(target.join("config.json")).unwrap(),
            r#"{"theme":"dark"}"#
        );
        assert!(store.values.lock().unwrap().is_empty());
    }

    #[test]
    fn 키체인_검증_실패_시_이전_자격증명을_복구하고_stage를_정리한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let source = root.path().join("original");
        let target = root.path().join("fork");
        let plugin_target = root.path().join("fork-data/plugins");
        fs::create_dir_all(&source).unwrap();
        let source_content = br#"{"connections":[{"id":"rollback-id","name":"Synthetic","params":{"driver":"postgres","password":"synthetic-new-secret","database":"fixture"}}]}"#;
        fs::write(source.join("connections.json"), source_content).unwrap();
        let store = FakeStore::default();
        store.seed("fork", "rollback-id:db", "synthetic-old-secret");
        store.corrupt_next_read_after_write("fork", "rollback-id:db");

        // when
        let result = import_profile_with_store(
            &source,
            &target,
            &plugin_target,
            &[],
            &store,
            "original",
            "fork",
        );

        // then
        assert!(matches!(result, Err(ProfileImportError::Failed { .. })));
        assert_eq!(
            store.value("fork", "rollback-id:db").as_deref(),
            Some("synthetic-old-secret")
        );
        assert!(!target.join("connections.json").exists());
        assert_eq!(
            fs::read(source.join("connections.json")).unwrap(),
            source_content
        );
        assert!(!root
            .path()
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| { entry.file_name().to_string_lossy().contains("import-stage") }));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let backup = root
                .path()
                .read_dir()
                .unwrap()
                .filter_map(Result::ok)
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .contains("original-backup")
                })
                .expect("private source backup remains available");
            assert_eq!(
                backup.metadata().unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn 가져오기는_프로필_전체_쓰기_잠금이_풀릴_때까지_기다린다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let source = root.path().join("original");
        let target = root.path().join("fork");
        let plugin_target = root.path().join("fork-data/plugins");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        write_connection_profile(&source.join("connections.json"), &[]);
        let connection_path = crate::paths::resolve_connections_path(&target);
        let writer_lock =
            crate::persistence::LockedConnectionsFile::open(&connection_path).expect("writer lock");
        let source_for_thread = source.clone();
        let target_for_thread = target.clone();
        let plugins_for_thread = plugin_target.clone();
        let (sender, receiver) = std::sync::mpsc::channel();

        // when
        let worker = std::thread::spawn(move || {
            let result = import_profile_with_store(
                &source_for_thread,
                &target_for_thread,
                &plugins_for_thread,
                &[],
                &FakeStore::default(),
                "original",
                "fork",
            );
            sender.send(result.is_ok()).unwrap();
        });
        let blocked = receiver.recv_timeout(std::time::Duration::from_millis(100));
        drop(writer_lock);
        let completed = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("가져오기는 writer lock이 풀린 뒤 진행되어야 합니다");
        worker.join().unwrap();

        // then
        assert!(blocked.is_err(), "importer는 writer lock을 기다려야 합니다");
        assert!(completed);
    }
}
