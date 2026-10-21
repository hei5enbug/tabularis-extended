use super::{installer::InstalledPluginInfo, manager::ConfigManifest};
use crate::drivers::driver_trait::PluginManifest;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

pub const SERVICE_PROTOCOL: u64 = 1;
pub const EXTENSION_RUNTIME_VERSION: &str = "0.26.1-spatial.1";
pub const ASSET_MAX_BYTES: u64 = 16 * 1024 * 1024;
static ACTIVE: Lazy<Mutex<HashMap<String, PluginManifest>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    Driver,
    Extension,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetEntry {
    path: String,
    mime: String,
}
#[derive(Serialize)]
pub struct PluginAsset {
    pub bytes: Vec<u8>,
    pub mime: String,
}

pub fn parse_manifest(bytes: &[u8]) -> Result<(PackageKind, Value), String> {
    let value = crate::theme_packages::parse_bounded_json(bytes, 4 * 1024 * 1024, 128, 262_144)?;
    let object = value.as_object().ok_or(super::package_kind::KIND_ERROR)?;
    let kind = match object.get("kind") {
        None => PackageKind::Driver,
        Some(Value::String(kind)) if kind == "driver" => PackageKind::Driver,
        Some(Value::String(kind)) if kind == "extension" => PackageKind::Extension,
        _ => return Err(super::package_kind::KIND_ERROR.into()),
    };
    if let Some(protocol) = object.get("required_service_protocol") {
        if protocol.as_u64() != Some(SERVICE_PROTOCOL) {
            return Err("Unsupported required_service_protocol".into());
        }
    }
    if kind == PackageKind::Extension {
        if object
            .get("required_service_protocol")
            .and_then(Value::as_u64)
            != Some(SERVICE_PROTOCOL)
        {
            return Err("Extensions require service protocol 1".into());
        }
        for field in [
            "engine",
            "executable",
            "interpreter",
            "explain_parsers",
            "capabilities",
            "data_types",
            "connection_metadata",
            "default_port",
            "default_username",
            "paradigms",
            "type_mappings",
        ] {
            if object.contains_key(field) {
                return Err(format!("Extension cannot declare driver field '{field}'"));
            }
        }
        let floor = object
            .get("min_runtime_version")
            .and_then(Value::as_str)
            .ok_or("Extension requires min_runtime_version")?;
        let floor =
            semver::Version::parse(floor).map_err(|_| "Invalid extension runtime version")?;
        let host =
            semver::Version::parse(EXTENSION_RUNTIME_VERSION).expect("extension runtime constant");
        if floor > host {
            return Err("Extension requires a newer runtime".into());
        }
    }
    validate_assets(&value)?;
    Ok((kind, value))
}

pub fn read_package(path: &Path) -> Result<(PackageKind, Value), String> {
    let root = path.canonicalize().map_err(|e| e.to_string())?;
    let name = if root.join(".tabularium").exists() {
        ".tabularium"
    } else {
        "manifest.json"
    };
    let manifest = fenced_file(&root, name)?;
    let mut bytes = Vec::new();
    fs::File::open(manifest)
        .map_err(|e| e.to_string())?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    parse_manifest(&bytes)
}

pub fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains('\\')
        || path.contains(':')
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Invalid relative plugin path".into());
    }
    Ok(())
}
pub fn fenced_file(root: &Path, path: &str) -> Result<PathBuf, String> {
    validate_relative_path(path)?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let file = root.join(path).canonicalize().map_err(|e| e.to_string())?;
    if !file.starts_with(&root) || !file.is_file() {
        return Err("Plugin file escapes its installed root".into());
    }
    Ok(file)
}
fn validate_assets(value: &Value) -> Result<Vec<AssetEntry>, String> {
    let Some(assets) = value.get("ui_assets") else {
        return Ok(Vec::new());
    };
    let assets: Vec<AssetEntry> =
        serde_json::from_value(assets.clone()).map_err(|_| "Invalid ui_assets declaration")?;
    if assets.len() > 1024 {
        return Err("Too many plugin assets".into());
    }
    let mut seen = HashSet::new();
    for asset in &assets {
        validate_relative_path(&asset.path)?;
        if !seen.insert(asset.path.clone()) || !valid_mime(&asset.mime) {
            return Err("Invalid or duplicate plugin asset".into());
        }
    }
    Ok(assets)
}
fn valid_mime(mime: &str) -> bool {
    let Some((major, minor)) = mime.split_once('/') else {
        return false;
    };
    !major.is_empty()
        && !minor.is_empty()
        && !minor.contains('/')
        && mime.len() <= 128
        && mime
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/!#$&^_.+-".contains(&b))
}
pub fn read_asset(root: &Path, file_path: &str) -> Result<PluginAsset, String> {
    let (_, manifest) = read_package(root)?;
    let asset = validate_assets(&manifest)?
        .into_iter()
        .find(|asset| asset.path == file_path)
        .ok_or("Plugin asset is not declared in ui_assets")?;
    let path = fenced_file(root, file_path)?;
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > ASSET_MAX_BYTES {
        return Err("Plugin asset exceeds 16 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(ASSET_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > ASSET_MAX_BYTES {
        return Err("Plugin asset exceeds 16 MiB".into());
    }
    Ok(PluginAsset {
        bytes,
        mime: asset.mime,
    })
}
pub fn destination(root: &Path, id: &str) -> Result<PathBuf, String> {
    super::layout::driver_destination(root, id)?;
    Ok(root.join("extensions").join(id))
}
pub fn directories(root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = root.join("extensions");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir()
            || entry.file_name().to_string_lossy().starts_with('.')
        {
            continue;
        }
        if read_package(&entry.path()).is_ok_and(|(kind, _)| kind == PackageKind::Extension) {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}
pub fn resolve(root: &Path, id: &str) -> Result<PathBuf, String> {
    let path = destination(root, id)?;
    let (kind, value) = read_package(&path)?;
    if kind != PackageKind::Extension || identity(&value)? != id {
        return Err("Extension identity mismatch".into());
    }
    Ok(path)
}
pub fn identity(value: &Value) -> Result<&str, String> {
    value
        .get("id")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing plugin identity".into())
}
pub fn info(path: &Path) -> Result<InstalledPluginInfo, String> {
    let (_, value) = read_package(path)?;
    Ok(InstalledPluginInfo {
        id: identity(&value)?.into(),
        name: field(&value, "name")?,
        version: field(&value, "version")?,
        description: field(&value, "description")?,
        kind: Some("extension".into()),
    })
}
fn field(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("Missing plugin field '{field}'"))
}
pub fn manifest(path: &Path) -> Result<PluginManifest, String> {
    let (kind, value) = read_package(path)?;
    if kind != PackageKind::Extension {
        return Err(super::package_kind::KIND_ERROR.into());
    }
    let config: ConfigManifest = serde_json::from_value(value).map_err(|e| e.to_string())?;
    Ok(PluginManifest {
        id: config.id.unwrap_or_else(|| config.name.clone()),
        name: config.name,
        version: config.version,
        description: config.description,
        default_port: None,
        capabilities: Default::default(),
        is_builtin: false,
        engine: None,
        paradigms: Vec::new(),
        default_username: String::new(),
        color: config.color,
        icon: config.icon,
        settings: config.settings,
        ui_extensions: config.ui_extensions,
        explain_parsers: None,
        type_mappings: HashMap::new(),
        deprecated: None,
    })
}
pub fn enable(path: &Path) -> Result<(), String> {
    validate_bundle(path)?;
    let manifest = manifest(path)?;
    if ["mysql", "postgres", "sqlite"].contains(&manifest.id.as_str()) {
        return Err("Extension id collides with a built-in driver".into());
    }
    ACTIVE
        .lock()
        .map_err(|_| "Extension metadata cache is unavailable")?
        .insert(manifest.id.clone(), manifest);
    Ok(())
}
pub fn disable(id: &str) {
    if let Ok(mut cache) = ACTIVE.lock() {
        cache.remove(id);
    }
}

pub fn replace_directory(temporary: &Path, destination: &Path) -> Result<(), String> {
    let backup = destination.with_file_name(format!(".backup-{}", uuid::Uuid::new_v4()));
    let previous = destination.exists();
    if previous {
        fs::rename(destination, &backup)
            .map_err(|e| format!("Failed to preserve installed package: {e}"))?;
    }
    if let Err(error) = fs::rename(temporary, destination) {
        if previous {
            fs::rename(&backup, destination).map_err(|restore| {
                format!("Replacement failed: {error}; restoration failed: {restore}")
            })?;
        }
        return Err(format!("Failed to finalize package installation: {error}"));
    }
    if previous {
        if let Err(error) = fs::remove_dir_all(backup) {
            log::warn!("Could not remove package backup: {error}");
        }
    }
    Ok(())
}

pub fn validate_bundle(root: &Path) -> Result<(), String> {
    let (kind, value) = read_package(root)?;
    if kind == PackageKind::Extension {
        manifest(root)?;
    }
    for asset in validate_assets(&value)? {
        let path = fenced_file(root, &asset.path)?;
        if fs::metadata(path).map_err(|e| e.to_string())?.len() > ASSET_MAX_BYTES {
            return Err("Plugin asset exceeds 16 MiB".into());
        }
    }
    if kind == PackageKind::Extension {
        let declarations = value
            .get("ui_extensions")
            .and_then(Value::as_array)
            .ok_or("Extension requires ui_extensions")?;
        for entry in declarations {
            let path = fenced_file(
                root,
                entry
                    .get("module")
                    .and_then(Value::as_str)
                    .ok_or("Missing UI module path")?,
            )?;
            if fs::metadata(path).map_err(|e| e.to_string())?.len() > ASSET_MAX_BYTES {
                return Err("Plugin UI module exceeds 16 MiB".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "extensions_tests.rs"]
mod tests;
