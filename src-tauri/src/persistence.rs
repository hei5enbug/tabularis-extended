use crate::models::{ConnectionGroup, ConnectionsFile, SavedConnection};
use fs2::FileExt;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct LockedConnectionsFile {
    path: PathBuf,
    _lock: File,
}

impl LockedConnectionsFile {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let lock_path = connections_lock_path(path)?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(lock_path)
            .map_err(|e| e.to_string())?;
        lock.lock_exclusive().map_err(|e| e.to_string())?;
        Ok(Self {
            path: path.to_path_buf(),
            _lock: lock,
        })
    }

    pub fn load(&self) -> Result<ConnectionsFile, String> {
        load_connections_file(&self.path)
    }

    pub fn save(&self, file: &mut ConnectionsFile) -> Result<(), String> {
        let current_content = match fs::read_to_string(&self.path) {
            Ok(content) => Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let current = current_content
            .as_deref()
            .map(parse_connections_file)
            .transpose()?
            .unwrap_or_default();
        if file.revision != current.revision {
            return Err("Connection file revision conflict".to_string());
        }

        let mut next = file.clone();
        assign_changed_connection_revisions(&current, &mut next)?;
        let mut connections_to_save = Vec::with_capacity(next.connections.len());
        for conn in &next.connections {
            let mut saved = conn.clone();
            if saved.params.save_in_keychain.unwrap_or(false) {
                saved.params.password = None;
                saved.params.ssh_password = None;
                saved.params.ssh_key_passphrase = None;
            }
            saved.params.connection_uri = None;
            connections_to_save.push(saved);
        }

        let mut output = serde_json::to_value(ConnectionsFile {
            revision: String::new(),
            groups: next.groups.clone(),
            connections: connections_to_save,
            tags: next.tags.clone(),
        })
        .map_err(|e| e.to_string())?;
        if let Some(content) = current_content.as_deref() {
            preserve_unknown_fields(content, &mut output);
        }
        let serialized = serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?;
        atomic_replace(&self.path, serialized.as_bytes())?;
        next.revision = content_revision(&serialized);
        *file = next;
        Ok(())
    }
}

pub fn connections_lock_path(path: &Path) -> Result<PathBuf, String> {
    let profile_dir = path
        .parent()
        .ok_or_else(|| "Connections file path has no profile directory".to_string())?;
    let profile_dir = fs::canonicalize(profile_dir).unwrap_or_else(|_| profile_dir.to_path_buf());
    let parent = profile_dir
        .parent()
        .ok_or_else(|| "Profile directory has no parent directory".to_string())?;
    let profile_name = profile_dir
        .file_name()
        .ok_or_else(|| "Profile directory has no name".to_string())?;
    let profile_identity = profile_dir.to_string_lossy();
    let digest = Sha256::digest(profile_identity.as_bytes());
    let suffix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(parent.join(format!(
        ".{}-{suffix}.connections.lock",
        profile_name.to_string_lossy()
    )))
}

pub fn update_connections_file<T>(
    path: &Path,
    update: impl FnOnce(&mut ConnectionsFile) -> Result<T, String>,
) -> Result<T, String> {
    update_connections_file_with_file(path, update).map(|(value, _)| value)
}

pub fn update_connections_file_with_file<T>(
    path: &Path,
    update: impl FnOnce(&mut ConnectionsFile) -> Result<T, String>,
) -> Result<(T, ConnectionsFile), String> {
    let lock = LockedConnectionsFile::open(path)?;
    let mut file = lock.load()?;
    let result = update(&mut file)?;
    lock.save(&mut file)?;
    Ok((result, file))
}

/// Parses connections file content already read from disk. Supports both
/// the old format (a bare array of connections) and the new format (an
/// object with groups/tags). Split out from `load_connections_file` so a
/// caller that already has the file's content in hand (e.g. to compare
/// against a later read, as `connection_migrations` does) can parse it
/// without triggering a second `fs::read_to_string`.
pub fn parse_connections_file(content: &str) -> Result<ConnectionsFile, String> {
    // Try parsing as the new format first
    let mut file = if let Ok(file) = serde_json::from_str::<ConnectionsFile>(content) {
        file
    } else {
        // Fall back to old format (array of connections)
        let connections: Vec<SavedConnection> = serde_json::from_str(content)
            .map_err(|_| "Failed to parse connections file".to_string())?;

        ConnectionsFile {
            revision: String::new(),
            groups: Vec::new(),
            connections,
            tags: Vec::new(),
        }
    };

    hydrate_connection_revisions(&mut file)?;
    file.revision = content_revision(content);
    Ok(file)
}

/// Load connections file (raw, no keychain reads).
/// Supports both old format (array of connections) and new format (with groups).
/// Use `load_connections` or `load_connections_with_passwords` when passwords are needed.
pub fn load_connections_file(path: &Path) -> Result<ConnectionsFile, String> {
    if !path.exists() {
        return Ok(ConnectionsFile::default());
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_connections_file(&content)
}

/// Load connections list (raw, no keychain reads) — for listing UI.
pub fn load_connections(path: &Path) -> Result<Vec<SavedConnection>, String> {
    let file = load_connections_file(path)?;
    Ok(file.connections)
}

pub fn save_connections_file(path: &Path, file: &ConnectionsFile) -> Result<(), String> {
    let lock = LockedConnectionsFile::open(path)?;
    let mut copy = file.clone();
    lock.save(&mut copy)
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
/// The previous content is re-read here, not passed in, so whichever process
/// wrote last still wins for the fields it knows.
fn preserve_unknown_fields(content: &str, next: &mut Value) {
    let Ok(prev_raw) = serde_json::from_str::<Value>(content) else {
        return;
    };
    let Ok(prev_typed) = parse_connections_file(content) else {
        return;
    };
    let Ok(prev_roundtrip) = serde_json::to_value(&prev_typed) else {
        return;
    };

    // Connections to compare against: the new format stores them under
    // "connections", the old format is the bare array itself. The round-trip
    // of a typed parse is always the new format.
    let prev_conns_raw: &[Value] = match &prev_raw {
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

    if let Value::Object(prev_root) = &prev_raw {
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
        let (Some(raw_conn), Some(rt_conn)) = (raw_conn_by_id.get(id), rt_conn_by_id.get(id))
        else {
            continue;
        };
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

fn content_revision(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn connection_value(conn: &SavedConnection, include_secrets: bool) -> Result<Value, String> {
    let mut value = serde_json::to_value(conn).map_err(|e| e.to_string())?;
    if let Some(object) = value.as_object_mut() {
        object.remove("revision");
        if let Some(params) = object.get_mut("params").and_then(Value::as_object_mut) {
            if !include_secrets {
                for name in [
                    "password",
                    "ssh_password",
                    "ssh_key_passphrase",
                    "connection_uri",
                ] {
                    params.remove(name);
                }
            }
        }
    }
    Ok(value)
}

fn connection_fingerprint(conn: &SavedConnection) -> Result<String, String> {
    let value = connection_value(conn, false)?;
    let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn hydrate_connection_revisions(file: &mut ConnectionsFile) -> Result<(), String> {
    for conn in &mut file.connections {
        let seed = conn
            .revision
            .split_once(':')
            .map(|(seed, _)| seed)
            .filter(|seed| !seed.is_empty())
            .unwrap_or_else(|| {
                if conn.revision.is_empty() {
                    "legacy"
                } else {
                    conn.revision.as_str()
                }
            })
            .to_string();
        conn.revision = format!("{seed}:{}", connection_fingerprint(conn)?);
    }
    Ok(())
}

fn assign_changed_connection_revisions(
    before: &ConnectionsFile,
    after: &mut ConnectionsFile,
) -> Result<(), String> {
    let previous: HashMap<&str, &SavedConnection> = before
        .connections
        .iter()
        .map(|conn| (conn.id.as_str(), conn))
        .collect();
    for conn in &mut after.connections {
        let changed = match previous.get(conn.id.as_str()) {
            Some(old) => connection_value(old, true)? != connection_value(conn, true)?,
            None => true,
        };
        if changed {
            conn.revision = format!("{}:{}", uuid::Uuid::new_v4(), connection_fingerprint(conn)?);
        } else if let Some(old) = previous.get(conn.id.as_str()) {
            conn.revision = old.revision.clone();
        }
    }
    Ok(())
}

fn atomic_replace(path: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temporary.write_all(contents).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), String> {
    atomic_replace(path, contents)
}

/// Legacy function for backward compatibility - saves using new format
pub fn save_connections(path: &Path, connections: &[SavedConnection]) -> Result<(), String> {
    update_connections_file(path, |file| {
        file.connections = connections.to_vec();
        Ok(())
    })
}

pub fn load_groups(path: &Path) -> Result<Vec<ConnectionGroup>, String> {
    let file = load_connections_file(path)?;
    Ok(file.groups)
}

pub fn save_groups(path: &Path, groups: &[ConnectionGroup]) -> Result<(), String> {
    update_connections_file(path, |file| {
        file.groups = groups.to_vec();
        Ok(())
    })
}
