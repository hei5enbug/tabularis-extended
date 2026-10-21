use crate::models::{ConnectionGroup, ConnectionsFile, SavedConnection};
use crate::services::{
    contracts::{ErrorCode, Outcome},
    error::{failure, ServiceResult},
    security::digest,
};
use fs2::FileExt;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

const MAX_CONNECTION_BYTES: u64 = 16 * 1024 * 1024;

/// Parses connections file content already read from disk. Supports both
/// the old format (a bare array of connections) and the new format (an
/// object with groups/tags). Split out from `load_connections_file` so a
/// caller that already has the file's content in hand (e.g. to compare
/// against a later read, as `connection_migrations` does) can parse it
/// without triggering a second `fs::read_to_string`.
pub fn parse_connections_file(content: &str) -> Result<ConnectionsFile, String> {
    // Try parsing as the new format first
    if let Ok(mut file) = serde_json::from_str::<ConnectionsFile>(content) {
        file.source_revision = Some(digest(content.as_bytes()));
        return Ok(file);
    }

    // Fall back to old format (array of connections)
    let connections: Vec<SavedConnection> = serde_json::from_str(content)
        .map_err(|_| "Failed to parse connections file".to_string())?;

    Ok(ConnectionsFile {
        source_revision: Some(digest(content.as_bytes())),
        groups: Vec::new(),
        connections,
        tags: Vec::new(),
    })
}

/// Load connections file (raw, no keychain reads).
/// Supports both old format (array of connections) and new format (with groups).
/// Use `load_connections` or `load_connections_with_passwords` when passwords are needed.
pub fn load_connections_file(path: &Path) -> Result<ConnectionsFile, String> {
    let _lock = service_lock_file(&path.with_file_name(format!(
            "{}.lock",
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| "Invalid connections path".to_owned())?
        )))
    .map_err(legacy_error)?;
    read_connections_locked(path)
        .map(|(file, _)| file)
        .map_err(legacy_error)
}

/// Load connections list (raw, no keychain reads) — for listing UI.
pub fn load_connections(path: &Path) -> Result<Vec<SavedConnection>, String> {
    let file = load_connections_file(path)?;
    Ok(file.connections)
}

pub fn save_connections_file(path: &Path, file: &ConnectionsFile) -> Result<(), String> {
    let result = (|| {
        let _lock = connections_lock(path)?;
        let (current, raw) = read_connections_locked(path)?;
        if current.source_revision != file.source_revision {
            return Err(failure(ErrorCode::VersionConflict));
        }
        publish_connections(path, file, &raw, &current)
    })();
    result.map_err(legacy_error)
}

fn legacy_error(error: crate::services::contracts::ServiceError) -> String {
    if error.code == ErrorCode::VersionConflict {
        "VERSION_CONFLICT".into()
    } else if error.outcome == Outcome::Applied {
        format!("{} (applied)", error.message)
    } else {
        error.message
    }
}

fn connections_lock(path: &Path) -> ServiceResult<File> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    service_lock_file(&path.with_file_name(format!("{name}.lock")))
}

fn guard_path(path: &Path, private: bool, directory: bool) -> ServiceResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(failure(ErrorCode::PermissionDenied)),
    };
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || (private && metadata.mode() & 0o077 != 0)
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(failure(ErrorCode::PermissionDenied));
        }
    }
    Ok(())
}

fn secure_parent(path: &Path) -> ServiceResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    guard_path(parent, false, true)?;
    fs::create_dir_all(parent).map_err(|_| failure(ErrorCode::ResourceLimit))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|_| failure(ErrorCode::PermissionDenied))?;
    }
    #[cfg(windows)]
    {
        crate::services::storage::protect_windows_path(parent, true)?;
    }
    Ok(())
}

fn open_guarded(path: &Path, write: bool, create: bool) -> ServiceResult<File> {
    guard_path(path, false, false)?;
    let mut options = OpenOptions::new();
    options.read(true).write(write).create_new(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let file = options
        .open(path)
        .map_err(|_| failure(ErrorCode::PermissionDenied))?;
    if !file
        .metadata()
        .map_err(|_| failure(ErrorCode::PermissionDenied))?
        .is_file()
    {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    #[cfg(windows)]
    {
        crate::services::storage::protect_windows_path(path, false)?;
    }
    Ok(file)
}

pub(crate) fn service_lock_file(path: &Path) -> ServiceResult<File> {
    secure_parent(path)?;
    let file = match open_guarded(path, true, true) {
        Ok(file) => file,
        Err(_) => open_guarded(path, true, false)?,
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| failure(ErrorCode::PermissionDenied))?;
    }
    file.lock_exclusive()
        .map_err(|_| failure(ErrorCode::ResourceLimit))?;
    Ok(file)
}

pub(crate) fn service_read_file(
    path: &Path,
    max_bytes: u64,
    private: bool,
) -> ServiceResult<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(failure(ErrorCode::PermissionDenied)),
        Ok(_) => {}
    }
    guard_path(path, private, false)?;
    let mut file = open_guarded(path, false, false)?;
    if file
        .metadata()
        .map_err(|_| failure(ErrorCode::ResourceLimit))?
        .len()
        > max_bytes
    {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure(ErrorCode::ResourceLimit))?;
    if bytes.len() as u64 > max_bytes {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    Ok(Some(bytes))
}

pub(crate) fn service_write_atomic(path: &Path, bytes: &[u8]) -> ServiceResult<()> {
    secure_parent(path)?;
    guard_path(path, false, false)?;
    let parent = path
        .parent()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    let temporary = parent.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = open_guarded(&temporary, true, true)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        #[cfg(not(windows))]
        fs::rename(&temporary, path).map_err(|_| failure(ErrorCode::ResourceLimit))?;
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            #[link(name = "kernel32")]
            extern "system" {
                fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
            }
            let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 3) } == 0 {
                return Err(failure(ErrorCode::ResourceLimit));
            }
        }
        #[cfg(unix)]
        {
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| {
                    let mut error = failure(ErrorCode::ResourceLimit);
                    error.outcome = Outcome::Applied;
                    error
                })?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn read_connections_locked(path: &Path) -> ServiceResult<(ConnectionsFile, Value)> {
    let Some(bytes) = service_read_file(path, MAX_CONNECTION_BYTES, false)? else {
        let file = ConnectionsFile::default();
        let raw = serde_json::to_value(&file).map_err(|_| failure(ErrorCode::InvalidArgument))?;
        return Ok((file, raw));
    };
    let content = std::str::from_utf8(&bytes).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    let file = parse_connections_file(content).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    let raw = serde_json::from_slice(&bytes).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    Ok((file, raw))
}

fn publish_connections(
    path: &Path,
    file: &ConnectionsFile,
    raw: &Value,
    previous: &ConnectionsFile,
) -> ServiceResult<()> {
    let mut sanitized = file.clone();
    for connection in &mut sanitized.connections {
        if connection.params.save_in_keychain.unwrap_or(false) {
            connection.params.password = None;
            connection.params.ssh_password = None;
        }
    }
    let mut next =
        serde_json::to_value(&sanitized).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    preserve_unknown_fields(raw, previous, &mut next);
    let bytes =
        serde_json::to_vec_pretty(&next).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    if bytes.len() as u64 > MAX_CONNECTION_BYTES {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    service_write_atomic(path, &bytes)
}

pub fn transact_connections<T>(
    path: &Path,
    mutation: impl FnOnce(&mut ConnectionsFile, &mut Value) -> ServiceResult<T>,
) -> ServiceResult<T> {
    let _lock = connections_lock(path)?;
    let (mut file, mut raw) = read_connections_locked(path)?;
    let previous = file.clone();
    let previous_raw = raw.clone();
    let result = mutation(&mut file, &mut raw)?;
    if serde_json::to_value(&file).map_err(|_| failure(ErrorCode::InvalidArgument))?
        != serde_json::to_value(&previous).map_err(|_| failure(ErrorCode::InvalidArgument))?
        || raw != previous_raw
    {
        publish_connections(path, &file, &raw, &previous)?;
    }
    Ok(result)
}

/// Fields of `raw` the current binary's structs do not know: anything present
/// on disk but absent from the round-trip of the typed parse, which is exactly
/// what a save would have dropped. Aliases resolve to their renamed spelling
/// during the round-trip, so they never register as unknown.
fn unknown_fields<'a>(
    raw: &'a Map<String, Value>,
    roundtripped: &Value,
    aliases: &[&str],
) -> Vec<(&'a str, &'a Value)> {
    let Some(known) = roundtripped.as_object() else {
        return Vec::new();
    };
    raw.iter()
        .filter(|(key, _)| !known.contains_key(key.as_str()) && !aliases.contains(&key.as_str()))
        .map(|(key, value)| (key.as_str(), value))
        .collect()
}

/// Copies fields the current binary's structs do not know from the previous
/// on-disk content into the value about to be written, so a save no longer
/// silently drops them (#668: the GUI and an older `--mcp` binary are
/// independent writers of the same file). Matching is by connection id and
/// covers the top level, each connection, and each connection's params. Known
/// fields keep their in-memory value: something this save deliberately
/// dropped (an optional cleared, a connection deleted) is not resurrected.
/// The caller supplies the raw and typed values read under the publication lock.
fn preserve_unknown_fields(prev_raw: &Value, prev_typed: &ConnectionsFile, next: &mut Value) {
    let Ok(prev_roundtrip) = serde_json::to_value(prev_typed) else {
        return;
    };

    // Connections to compare against: the new format stores them under
    // "connections", the old format is the bare array itself. The round-trip
    // of a typed parse is always the new format.
    let prev_conns_raw: &[Value] = match prev_raw {
        Value::Array(a) => a,
        Value::Object(o) => o
            .get("connections")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        _ => &[],
    };
    let prev_rt_conns: &[Value] = prev_roundtrip
        .get("connections")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let raw_conn_by_id: HashMap<&str, &Value> = prev_conns_raw
        .iter()
        .filter_map(|c| c.get("id").and_then(Value::as_str).map(|id| (id, c)))
        .collect();
    let rt_conn_by_id: HashMap<&str, &Value> = prev_rt_conns
        .iter()
        .filter_map(|c| c.get("id").and_then(Value::as_str).map(|id| (id, c)))
        .collect();

    let Some(next_root) = next.as_object_mut() else {
        return;
    };

    if let Value::Object(prev_root) = prev_raw {
        for (key, val) in unknown_fields(prev_root, &prev_roundtrip, &[]) {
            if !next_root.contains_key(key) {
                next_root.insert(key.to_string(), val.clone());
            }
        }
    }

    let Some(next_conns) = next_root
        .get_mut("connections")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for conn in next_conns.iter_mut() {
        let Some(obj) = conn.as_object_mut() else {
            continue;
        };
        let Some(id) = obj.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(raw_conn) = raw_conn_by_id.get(id) else {
            continue;
        };
        let current_roundtrip = Value::Object(obj.clone());
        let rt_conn = rt_conn_by_id.get(id).copied().unwrap_or(&current_roundtrip);
        let Some(raw_conn) = raw_conn.as_object() else {
            continue;
        };
        for (key, val) in unknown_fields(raw_conn, rt_conn, &[]) {
            if !obj.contains_key(key) {
                obj.insert(key.to_string(), val.clone());
            }
        }
        if let (Some(raw_params), Some(next_params), Some(rt_params)) = (
            raw_conn.get("params").and_then(Value::as_object),
            obj.get_mut("params").and_then(Value::as_object_mut),
            rt_conn.get("params"),
        ) {
            for (key, val) in unknown_fields(
                raw_params,
                rt_params,
                &["connectionUri", "connectionUriInKeychain"],
            ) {
                if !next_params.contains_key(key) {
                    next_params.insert(key.to_string(), val.clone());
                }
            }
        }
    }
}

/// Legacy function for backward compatibility - saves using new format
pub fn save_connections(path: &Path, connections: &[SavedConnection]) -> Result<(), String> {
    // Load existing groups if any
    let existing = load_connections_file(path)?;
    let file = ConnectionsFile {
        source_revision: existing.source_revision,
        groups: existing.groups,
        connections: connections.to_vec(),
        tags: existing.tags,
    };
    save_connections_file(path, &file)
}

pub fn load_groups(path: &Path) -> Result<Vec<ConnectionGroup>, String> {
    let file = load_connections_file(path)?;
    Ok(file.groups)
}

pub fn save_groups(path: &Path, groups: &[ConnectionGroup]) -> Result<(), String> {
    let mut file = load_connections_file(path)?;
    file.groups = groups.to_vec();
    save_connections_file(path, &file)
}

#[cfg(test)]
#[path = "persistence/service_platform_tests.rs"]
mod service_platform_tests;
