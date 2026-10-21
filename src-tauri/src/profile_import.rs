//! Explicit first-run import of the official Tabularis profile into the fork.
//!
//! The official profile and keychain entries are only ever read. The import
//! backs the source up, builds the fork profile in a sibling staging directory,
//! copies every stored secret into the fork keychain service and verifies it by
//! reading it back, and only then removes inline passwords from the staged
//! JSON and publishes the profile with a directory rename. Any failure removes
//! what the import created, including the fork keychain entries it wrote.

use crate::models::SavedConnection;
use crate::persistence;
use crate::plugins::{installer, layout};
use crate::services::credentials::{Secret, SecretSlot};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Folder name the official build derives its `ProjectDirs` from.
pub const OFFICIAL_APP_DIR_NAME: &str = "tabularis";
/// Keychain service of the official build.
pub const OFFICIAL_KEYCHAIN_SERVICE: &str = "tabularis";
/// Folder under the fork data directory that holds source backups.
pub const BACKUP_DIR_NAME: &str = "official-profile-backups";

const MAX_DEPTH: usize = 24;
const MAX_CONNECTIONS_BYTES: u64 = 16 * 1024 * 1024;
const CONNECTIONS_FILE: &str = "connections.json";
const SSH_FILE: &str = "ssh_connections.json";
const ICONS_DIR: &str = "connection-icons";
const PLUGIN_STAGE_DIR: &str = ".plugin-staging";
/// Providers whose API key or proxy password the host can store.
const AI_PROVIDERS: &[&str] = &[
    "openai",
    "anthropic",
    "minimax",
    "openrouter",
    "ollama",
    "custom-openai",
];
const SERVICE_SLOTS: &[SecretSlot] = &[
    SecretSlot::DbPassword,
    SecretSlot::CosmosAccountKey,
    SecretSlot::EntraClientSecret,
    SecretSlot::EntraRefreshToken,
];

static IMPORT_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failure with a stable code. Messages never carry secret values, account
/// names or connection names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

fn fail(code: &'static str, message: impl Into<String>) -> ImportError {
    ImportError {
        code,
        message: message.into(),
    }
}

fn io_fail(code: &'static str, context: &str, error: &io::Error) -> ImportError {
    fail(code, format!("{context} ({:?})", error.kind()))
}

pub const SOURCE_NOT_FOUND: &str = "IMPORT_SOURCE_NOT_FOUND";
pub const DESTINATION_NOT_EMPTY: &str = "IMPORT_DESTINATION_NOT_EMPTY";
pub const SOURCE_IS_DESTINATION: &str = "IMPORT_SOURCE_IS_DESTINATION";
pub const BACKUP_FAILED: &str = "IMPORT_BACKUP_FAILED";
pub const BUILD_FAILED: &str = "IMPORT_BUILD_FAILED";
pub const SECRET_FAILED: &str = "IMPORT_SECRET_FAILED";
pub const PUBLISH_FAILED: &str = "IMPORT_PUBLISH_FAILED";

// ---------------------------------------------------------------------------
// Keychain port
// ---------------------------------------------------------------------------

/// Opaque keychain failure; it holds no detail that could echo a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultFault;

pub trait SecretPort: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, VaultFault>;
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), VaultFault>;
    fn delete(&self, service: &str, account: &str) -> Result<(), VaultFault>;
}

pub struct SystemKeychain;

impl SecretPort for SystemKeychain {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, VaultFault> {
        let entry = keyring::Entry::new(service, account).map_err(|_| VaultFault)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(VaultFault),
        }
    }
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), VaultFault> {
        keyring::Entry::new(service, account)
            .and_then(|entry| entry.set_password(value))
            .map_err(|_| VaultFault)
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), VaultFault> {
        let entry = keyring::Entry::new(service, account).map_err(|_| VaultFault)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(VaultFault),
        }
    }
}

/// The only keychain surface the import code touches: the official service is
/// read-only, the fork service may be written and cleaned up.
struct Vaults<'a> {
    port: &'a dyn SecretPort,
    official: &'a str,
    fork: &'a str,
}

impl Vaults<'_> {
    fn read_official(&self, account: &str) -> Result<Option<String>, VaultFault> {
        self.port.get(self.official, account)
    }
    fn read_fork(&self, account: &str) -> Result<Option<String>, VaultFault> {
        self.port.get(self.fork, account)
    }
    fn write_fork(&self, account: &str, value: &str) -> Result<(), VaultFault> {
        self.port.set(self.fork, account, value)
    }
    fn delete_fork(&self, account: &str) -> Result<(), VaultFault> {
        self.port.delete(self.fork, account)
    }
}

// ---------------------------------------------------------------------------
// Locations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ImportEnvironment {
    pub source_config: PathBuf,
    pub source_data: PathBuf,
    /// Candidate official plugin directories; the first one holding packages wins.
    pub source_plugins: Vec<PathBuf>,
    pub dest_config: PathBuf,
    pub dest_data: PathBuf,
    pub dest_plugins: PathBuf,
    pub backup_root: PathBuf,
    pub official_service: String,
    pub fork_service: String,
}

/// Where the official build keeps its profile. Mirrors the pre-fork
/// `paths.rs`: `ProjectDirs::from("", "", "tabularis")`, with the Windows leaf
/// removed, and the `storage-location.json` pointer in the default config
/// directory redirecting both configuration and connection data. Plugins never
/// follow the pointer. The `TABULARIS_DATA_DIR` variable belongs to the running
/// process, so it is not applied to a profile of another application.
pub struct OfficialLocations {
    pub config: PathBuf,
    pub data: PathBuf,
    pub plugins: Vec<PathBuf>,
}

pub fn official_locations_from(
    default_config: &Path,
    default_data: &Path,
    legacy_plugins: Option<PathBuf>,
) -> OfficialLocations {
    let custom = crate::storage_location::read_pointer(default_config);
    let primary_plugins = default_data.join("plugins");
    let mut plugins = vec![primary_plugins.clone()];
    if let Some(legacy) = legacy_plugins.filter(|legacy| *legacy != primary_plugins) {
        plugins.push(legacy);
    }
    OfficialLocations {
        config: custom
            .clone()
            .unwrap_or_else(|| default_config.to_path_buf()),
        data: custom.unwrap_or_else(|| default_data.to_path_buf()),
        plugins,
    }
}

fn official_default_dirs() -> (PathBuf, PathBuf, Option<PathBuf>) {
    let strip = cfg!(target_os = "windows");
    match directories::ProjectDirs::from("", "", OFFICIAL_APP_DIR_NAME) {
        Some(dirs) => (
            crate::paths::unnested_app_dir(dirs.config_dir(), strip),
            crate::paths::unnested_app_dir(dirs.data_dir(), strip),
            directories::ProjectDirs::from("com", "debba", OFFICIAL_APP_DIR_NAME)
                .map(|legacy| legacy.data_dir().join("plugins")),
        ),
        None => (
            PathBuf::from(format!(".config/{OFFICIAL_APP_DIR_NAME}")),
            PathBuf::from(format!(".local/share/{OFFICIAL_APP_DIR_NAME}")),
            None,
        ),
    }
}

impl ImportEnvironment {
    pub fn system() -> Result<Self, String> {
        let (default_config, default_data, legacy) = official_default_dirs();
        let official = official_locations_from(&default_config, &default_data, legacy);
        let dest_data = crate::paths::get_app_data_dir();
        Ok(Self {
            source_config: official.config,
            source_data: official.data,
            source_plugins: official.plugins,
            dest_config: crate::paths::get_app_config_dir(),
            backup_root: dest_data.join(BACKUP_DIR_NAME),
            dest_data,
            dest_plugins: crate::paths::get_plugins_dir_checked()?,
            official_service: OFFICIAL_KEYCHAIN_SERVICE.to_owned(),
            fork_service: crate::keychain_utils::SERVICE_NAME.to_owned(),
        })
    }
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Private,
    Preserve,
}

#[derive(Default)]
struct CopyStats {
    skipped_links: usize,
}

fn harden(path: &Path, directory: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
        )
    }
    #[cfg(windows)]
    {
        crate::services::storage::protect_windows_path(path, directory)
            .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "protect"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, directory);
        Ok(())
    }
}

fn make_dir(path: &Path, mode: Mode) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(error) => return Err(error),
    }
    if mode == Mode::Private {
        harden(path, true)?;
    }
    Ok(())
}

fn copy_file(source: &Path, target: &Path, mode: Mode) -> io::Result<()> {
    let mut input = fs::File::open(source)?;
    let permissions = input.metadata()?.permissions();
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(target)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    drop(output);
    match mode {
        Mode::Private => harden(target, false),
        Mode::Preserve => fs::set_permissions(target, permissions),
    }
}

/// Copies `source` into `target`. Symbolic links and special files are never
/// followed or copied; links are counted so the caller can report them.
fn copy_tree(
    source: &Path,
    target: &Path,
    mode: Mode,
    skip: &dyn Fn(&str, usize) -> bool,
    stats: &mut CopyStats,
    depth: usize,
) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "too deep"));
    }
    make_dir(target, mode)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if skip(&name, depth) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            stats.skipped_links += 1;
        } else if kind.is_dir() {
            copy_tree(
                &entry.path(),
                &target.join(&*name),
                mode,
                skip,
                stats,
                depth + 1,
            )?;
        } else if kind.is_file() {
            copy_file(&entry.path(), &target.join(&*name), mode)?;
        }
    }
    Ok(())
}

fn sibling(path: &Path, tag: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "profile".to_owned());
    path.with_file_name(format!(".{name}.{tag}-{}", uuid::Uuid::new_v4().simple()))
}

/// Removes the listed directories when dropped, unless they no longer exist.
struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn comparable(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn overlaps(left: &Path, right: &Path) -> bool {
    let (left, right) = (comparable(left), comparable(right));
    left.starts_with(&right) || right.starts_with(&left)
}

// ---------------------------------------------------------------------------
// Emptiness
// ---------------------------------------------------------------------------

/// Names that are runtime state, logs, earlier backups or installed plugins,
/// never user data. Everything else in a profile counts as user data.
fn is_ignorable(name: &str) -> bool {
    matches!(
        name,
        "logs"
            | BACKUP_DIR_NAME
            | "plugins"
            | "tabularis.alive"
            | ".DS_Store"
            | "storage-location.json"
            | ".mcp_session_state.json"
            | "pending_approvals"
            | "local-ipc"
            | "jobs-v1"
            | "results-v1"
            | "service-approval-bindings-v1"
    ) || name.ends_with(".log")
        || name.ends_with(".lock")
}

fn dir_has_user_data(dir: &Path, depth: usize) -> Result<bool, ImportError> {
    if depth > MAX_DEPTH {
        return Ok(true);
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(io_fail(
                BUILD_FAILED,
                "A profile directory is unreadable",
                &error,
            ))
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|error| io_fail(BUILD_FAILED, "A profile directory is unreadable", &error))?;
        if is_ignorable(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|error| io_fail(BUILD_FAILED, "A profile entry is unreadable", &error))?;
        if !kind.is_dir() || dir_has_user_data(&entry.path(), depth + 1)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// True when `path` holds at least one user data file. A missing or empty
/// directory, and a directory with only logs, lock files, runtime state,
/// plugins or earlier import backups, holds none. A file or a symbolic link in
/// its place counts as data.
pub fn holds_user_data(path: &Path) -> Result<bool, ImportError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_fail(
            BUILD_FAILED,
            "A profile directory is unreadable",
            &error,
        )),
        Ok(metadata) if metadata.is_dir() => dir_has_user_data(path, 0),
        Ok(_) => Ok(true),
    }
}

/// The official profile root may itself be a link (a synced folder); the
/// entries inside it are never followed.
fn source_holds_data(env: &ImportEnvironment) -> Result<bool, ImportError> {
    if !fs::metadata(&env.source_config)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
    {
        return Ok(false);
    }
    dir_has_user_data(&env.source_config, 0)
}

fn destination_holds_data(env: &ImportEnvironment) -> Result<bool, ImportError> {
    if holds_user_data(&env.dest_config)? {
        return Ok(true);
    }
    if comparable(&env.dest_data) != comparable(&env.dest_config) {
        return holds_user_data(&env.dest_data.join(ICONS_DIR));
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCounts {
    pub connections: usize,
    pub groups: usize,
    pub saved_queries: usize,
    pub notebooks: usize,
    pub themes: usize,
    pub plugin_packages: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportStatus {
    pub source_found: bool,
    pub destination_empty: bool,
    pub can_import: bool,
    pub source_dir: String,
    pub destination_dir: String,
    pub counts: ProfileCounts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Package {
    /// `drivers`, `themes`, `extensions`, or `None` for a flat bundle.
    kind_dir: Option<String>,
    name: String,
}

impl Package {
    fn relative(&self) -> PathBuf {
        match &self.kind_dir {
            Some(kind) => Path::new(kind).join(&self.name),
            None => PathBuf::from(&self.name),
        }
    }
    fn is_theme(&self) -> bool {
        self.kind_dir.as_deref() == Some("themes")
    }
}

fn child_dirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect();
    found.sort();
    found
}

fn list_packages(root: &Path) -> Vec<Package> {
    let mut packages = Vec::new();
    for (name, path) in child_dirs(root) {
        if name.starts_with('.') {
            continue;
        }
        if layout::is_kind_directory(&name) {
            for (child, _) in child_dirs(&path) {
                if !child.starts_with('.') {
                    packages.push(Package {
                        kind_dir: Some(name.clone()),
                        name: child,
                    });
                }
            }
        } else if installer::has_manifest(&path) {
            packages.push(Package {
                kind_dir: None,
                name,
            });
        }
    }
    packages
}

fn read_json_file(path: &Path) -> Option<String> {
    let mut content = String::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_CONNECTIONS_BYTES + 1)
        .read_to_string(&mut content)
        .ok()?;
    (content.len() as u64 <= MAX_CONNECTIONS_BYTES).then_some(content)
}

fn count_files_with_suffix(dir: &Path, suffix: &str, depth: usize) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let Ok(kind) = entry.file_type() else {
                return 0;
            };
            if kind.is_file() {
                usize::from(entry.file_name().to_string_lossy().ends_with(suffix))
            } else if kind.is_dir() && depth < 2 {
                count_files_with_suffix(&entry.path(), suffix, depth + 1)
            } else {
                0
            }
        })
        .sum()
}

fn count_profile(config: &Path, packages: &[Package]) -> ProfileCounts {
    let (connections, groups) = read_json_file(&config.join(CONNECTIONS_FILE))
        .and_then(|content| persistence::parse_connections_file(&content).ok())
        .map(|file| (file.connections.len(), file.groups.len()))
        .unwrap_or((0, 0));
    let saved_queries = read_json_file(&config.join("saved_queries").join("meta.json"))
        .and_then(|content| serde_json::from_str::<Vec<Value>>(&content).ok())
        .map(|list| list.len())
        .unwrap_or(0);
    ProfileCounts {
        connections,
        groups,
        saved_queries,
        notebooks: count_files_with_suffix(&config.join("notebooks"), ".tabularis-notebook", 0),
        themes: count_files_with_suffix(&config.join("themes"), ".json", 2)
            + packages.iter().filter(|package| package.is_theme()).count(),
        plugin_packages: packages
            .iter()
            .filter(|package| !package.is_theme())
            .count(),
    }
}

fn source_packages(env: &ImportEnvironment) -> (PathBuf, Vec<Package>) {
    for dir in &env.source_plugins {
        let packages = list_packages(dir);
        if !packages.is_empty() {
            return (dir.clone(), packages);
        }
    }
    (
        env.source_plugins.first().cloned().unwrap_or_default(),
        Vec::new(),
    )
}

pub fn import_status(env: &ImportEnvironment) -> ImportStatus {
    let source_found = source_holds_data(env).unwrap_or(false);
    let destination_empty = destination_holds_data(env)
        .map(|holds| !holds)
        .unwrap_or(false);
    let counts = if source_found {
        count_profile(&env.source_config, &source_packages(env).1)
    } else {
        ProfileCounts::default()
    };
    ImportStatus {
        source_found,
        destination_empty,
        can_import: source_found && destination_empty,
        source_dir: env.source_config.to_string_lossy().into_owned(),
        destination_dir: env.dest_config.to_string_lossy().into_owned(),
        counts,
    }
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------

fn backup_skip(name: &str, depth: usize) -> bool {
    (depth == 0 && matches!(name, "plugins" | "tabularis.alive")) || name.ends_with(".lock")
}

/// Copies the source profile into a new timestamped owner-only directory under
/// the fork data directory. Plugin packages are not part of the backup; they
/// stay untouched in the official directory and are copied separately.
fn create_backup(env: &ImportEnvironment) -> Result<(PathBuf, CopyStats), ImportError> {
    let failure = |error: io::Error| {
        io_fail(
            BACKUP_FAILED,
            "The official profile could not be backed up",
            &error,
        )
    };
    fs::create_dir_all(&env.backup_root).map_err(failure)?;
    harden(&env.backup_root, true).map_err(failure)?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let id = uuid::Uuid::new_v4().simple().to_string();
    let backup = env
        .backup_root
        .join(format!("official-{stamp}-{}", &id[..8]));
    fs::create_dir(&backup).map_err(failure)?;
    let mut stats = CopyStats::default();
    let result = (|| -> io::Result<()> {
        harden(&backup, true)?;
        copy_tree(
            &env.source_config,
            &backup.join("profile"),
            Mode::Private,
            &backup_skip,
            &mut stats,
            0,
        )?;
        let icons = env.source_data.join(ICONS_DIR);
        if comparable(&env.source_data) != comparable(&env.source_config) && icons.is_dir() {
            make_dir(&backup.join("data"), Mode::Private)?;
            copy_tree(
                &icons,
                &backup.join("data").join(ICONS_DIR),
                Mode::Private,
                &|_, _| false,
                &mut stats,
                0,
            )?;
        }
        let info = serde_json::json!({
            "created_at": chrono::Utc::now().to_rfc3339(),
            "source": env.source_config.to_string_lossy(),
        });
        let info_path = backup.join("backup-info.json");
        fs::write(&info_path, serde_json::to_vec_pretty(&info)?)?;
        harden(&info_path, false)
    })();
    match result {
        Ok(()) => Ok((backup, stats)),
        Err(error) => {
            let _ = fs::remove_dir_all(&backup);
            Err(failure(error))
        }
    }
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

struct Planned {
    account: String,
    secret: Secret,
}

#[derive(Default)]
struct SecretPlan {
    entries: Vec<Planned>,
    accounts: BTreeSet<String>,
}

fn secret_failure() -> ImportError {
    fail(SECRET_FAILED, "A stored secret could not be imported")
}

impl SecretPlan {
    fn add(&mut self, account: String, value: &str) -> Result<(), ImportError> {
        if value.is_empty() || !self.accounts.insert(account.clone()) {
            return Ok(());
        }
        let secret = Secret::new(value.as_bytes().to_vec()).map_err(|_| secret_failure())?;
        self.entries.push(Planned { account, secret });
        Ok(())
    }
}

fn has_text(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|value| !value.is_empty())
}

fn has_visible_text(value: &Option<String>) -> bool {
    value
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

/// What the keychain should hold for one field. With the keychain flag on the
/// host reads only the keychain, so a stored value wins and an inline value is a
/// fallback. With the flag off the host reads only the inline value and any
/// keychain entry is stale.
fn resolve_field(
    vaults: &Vaults<'_>,
    keychain_in_use: bool,
    account: &str,
    inline: &Option<String>,
    inline_usable: bool,
) -> Result<Option<String>, ImportError> {
    let stored = if keychain_in_use {
        vaults
            .read_official(account)
            .map_err(|_| secret_failure())?
            .filter(|value| !value.is_empty())
    } else {
        None
    };
    Ok(stored.or_else(|| {
        inline
            .clone()
            .filter(|value| inline_usable && !value.is_empty())
    }))
}

fn plan_connection(
    vaults: &Vaults<'_>,
    plan: &mut SecretPlan,
    connection: &SavedConnection,
) -> Result<(), ImportError> {
    let id = &connection.id;
    let params = &connection.params;
    let in_use = params.save_in_keychain.unwrap_or(false);
    let fields = [
        (
            format!("{id}:db"),
            &params.password,
            has_text(&params.password),
        ),
        (
            format!("{id}:ssh"),
            &params.ssh_password,
            has_text(&params.ssh_password),
        ),
        (
            format!("{id}:ssh_passphrase"),
            &params.ssh_key_passphrase,
            has_visible_text(&params.ssh_key_passphrase),
        ),
    ];
    for (account, inline, usable) in fields {
        if let Some(value) = resolve_field(vaults, in_use, &account, inline, usable)? {
            plan.add(account, &value)?;
        }
    }
    let uri_account = format!("{id}:connection_uri");
    let uri_in_keychain = params.connection_uri_in_keychain.unwrap_or(false);
    if let Some(value) = resolve_field(
        vaults,
        uri_in_keychain,
        &uri_account,
        &params.connection_uri,
        has_text(&params.connection_uri),
    )? {
        plan.add(uri_account, &value)?;
    }
    let mut keychain_only = SERVICE_SLOTS
        .iter()
        .map(|slot| format!("{id}:service:{}", slot.name()))
        .collect::<Vec<_>>();
    keychain_only.push(format!("proxy:connection:{id}"));
    for account in keychain_only {
        if let Some(value) = vaults
            .read_official(&account)
            .map_err(|_| secret_failure())?
        {
            plan.add(account, &value)?;
        }
    }
    Ok(())
}

fn text_field<'a>(entry: &'a Value, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

fn plan_ssh_entry(
    vaults: &Vaults<'_>,
    plan: &mut SecretPlan,
    entry: &Value,
) -> Result<(), ImportError> {
    let Some(id) = text_field(entry, "id") else {
        return Ok(());
    };
    let in_use = entry
        .get("save_in_keychain")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let fields = [
        (format!("{id}:ssh"), "password", false),
        (format!("{id}:ssh_passphrase"), "key_passphrase", true),
    ];
    for (account, key, trim) in fields {
        let inline = text_field(entry, key).map(str::to_owned);
        let usable = inline.as_deref().is_some_and(|value| {
            if trim {
                !value.trim().is_empty()
            } else {
                !value.is_empty()
            }
        });
        if let Some(value) = resolve_field(vaults, in_use, &account, &inline, usable)? {
            plan.add(account, &value)?;
        }
    }
    Ok(())
}

fn plan_global(
    vaults: &Vaults<'_>,
    plan: &mut SecretPlan,
    config: Option<&Value>,
) -> Result<(), ImportError> {
    let mut providers: BTreeSet<String> =
        AI_PROVIDERS.iter().map(|name| (*name).to_owned()).collect();
    if let Some(config) = config {
        if let Some(name) = config.get("ai_provider").and_then(Value::as_str) {
            providers.insert(name.to_owned());
        }
        if let Some(map) = config.get("ai_provider_proxies").and_then(Value::as_object) {
            providers.extend(map.keys().cloned());
        }
    }
    let mut accounts = vec!["proxy:global".to_owned()];
    for provider in providers {
        accounts.push(format!("ai_key:{provider}"));
        accounts.push(format!("proxy:ai:{provider}"));
    }
    for account in accounts {
        if let Some(value) = vaults
            .read_official(&account)
            .map_err(|_| secret_failure())?
        {
            plan.add(account, &value)?;
        }
    }
    Ok(())
}

/// Writes planned secrets to the fork service, reading each back. Every change
/// is journaled so a failure can put the fork keychain back.
struct Journal<'a, 'b> {
    vaults: &'a Vaults<'b>,
    undo: Vec<(String, Option<Secret>)>,
}

impl<'a, 'b> Journal<'a, 'b> {
    fn new(vaults: &'a Vaults<'b>) -> Self {
        Self {
            vaults,
            undo: Vec::new(),
        }
    }

    fn store_verified(&mut self, planned: &Planned) -> Result<(), ImportError> {
        let expected = planned.secret.expose();
        let previous = self
            .vaults
            .read_fork(&planned.account)
            .map_err(|_| secret_failure())?;
        if previous.as_deref() == Some(expected) {
            return Ok(());
        }
        let previous = match previous {
            None => None,
            Some(value) => Some(Secret::new(value.into_bytes()).map_err(|_| secret_failure())?),
        };
        self.undo.push((planned.account.clone(), previous));
        self.vaults
            .write_fork(&planned.account, expected)
            .map_err(|_| secret_failure())?;
        let read_back = self
            .vaults
            .read_fork(&planned.account)
            .map_err(|_| secret_failure())?;
        if read_back.as_deref() == Some(expected) {
            Ok(())
        } else {
            Err(secret_failure())
        }
    }

    /// Restores the fork keychain, newest change first. Returns how many
    /// entries could not be restored.
    fn rollback(&mut self) -> usize {
        let mut failed = 0;
        while let Some((account, previous)) = self.undo.pop() {
            let restored = match previous {
                Some(secret) => self.vaults.write_fork(&account, secret.expose()),
                None => self.vaults.delete_fork(&account),
            };
            if restored.is_err() {
                failed += 1;
            }
        }
        failed
    }
}

/// Removes inline secrets from the staged `connections.json`. Runs only after
/// every secret was stored and verified.
fn strip_connection_secrets(path: &Path) -> Result<(), ImportError> {
    persistence::update_connections_file(path, |file| {
        for connection in &mut file.connections {
            let params = &mut connection.params;
            let moved = has_text(&params.password)
                || has_text(&params.ssh_password)
                || has_visible_text(&params.ssh_key_passphrase);
            params.password = None;
            params.ssh_password = None;
            params.ssh_key_passphrase = None;
            if moved {
                params.save_in_keychain = Some(true);
            }
            if has_text(&params.connection_uri) {
                params.connection_uri = None;
                params.connection_uri_in_keychain = Some(true);
            }
        }
        Ok(())
    })
    .map_err(|_| {
        fail(
            BUILD_FAILED,
            "The imported connections could not be rewritten",
        )
    })
}

fn strip_ssh_secrets(entries: &mut [Value]) {
    for entry in entries {
        let Some(object) = entry.as_object_mut() else {
            continue;
        };
        let moved = object
            .get("password")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
            || object
                .get("key_passphrase")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty());
        object.remove("password");
        object.remove("key_passphrase");
        if moved {
            object.insert("save_in_keychain".into(), Value::Bool(true));
        }
    }
}

// ---------------------------------------------------------------------------
// Publishing
// ---------------------------------------------------------------------------

/// Packages renamed into the fork plugin directory, with enough knowledge to
/// remove them again if the profile cannot be published.
struct PluginPublication {
    published: Vec<PathBuf>,
    created_dirs: Vec<PathBuf>,
}

impl PluginPublication {
    fn undo(&mut self) {
        for path in self.published.drain(..) {
            let _ = fs::remove_dir_all(path);
        }
        for dir in self.created_dirs.drain(..).rev() {
            let _ = fs::remove_dir(dir);
        }
    }
}

fn ensure_dir_tracked(dir: &Path, created: &mut Vec<PathBuf>) -> io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        ensure_dir_tracked(parent, created)?;
    }
    fs::create_dir(dir)?;
    created.push(dir.to_path_buf());
    Ok(())
}

fn move_dir(source: &Path, target: &Path) -> io::Result<()> {
    if fs::rename(source, target).is_ok() {
        return Ok(());
    }
    let temporary = sibling(target, "import");
    let mut stats = CopyStats::default();
    let copied = copy_tree(
        source,
        &temporary,
        Mode::Preserve,
        &|_, _| false,
        &mut stats,
        0,
    )
    .and_then(|()| fs::rename(&temporary, target));
    if copied.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    copied?;
    let _ = fs::remove_dir_all(source);
    Ok(())
}

fn publish_plugins(
    stage: &Path,
    packages: &[Package],
    dest: &Path,
) -> Result<(PluginPublication, Vec<String>, Vec<String>), ImportError> {
    let failure = |error: io::Error| {
        io_fail(
            PUBLISH_FAILED,
            "The plugin packages could not be installed",
            &error,
        )
    };
    let mut publication = PluginPublication {
        published: Vec::new(),
        created_dirs: Vec::new(),
    };
    let (mut copied, mut skipped) = (Vec::new(), Vec::new());
    for package in packages {
        let target = dest.join(package.relative());
        if fs::symlink_metadata(&target).is_ok() {
            skipped.push(package.name.clone());
            continue;
        }
        let result = target
            .parent()
            .map_or(Ok(()), |parent| {
                ensure_dir_tracked(parent, &mut publication.created_dirs)
            })
            .and_then(|()| move_dir(&stage.join(package.relative()), &target));
        match result {
            Ok(()) => {
                publication.published.push(target);
                copied.push(package.name.clone());
            }
            Err(error) => {
                publication.undo();
                return Err(failure(error));
            }
        }
    }
    Ok((publication, copied, skipped))
}

/// Publishes the staged directory with a rename. A destination that already
/// exists is an empty or log-only directory: it is replaced atomically when it
/// is empty, otherwise moved aside for the instant of the swap and its runtime
/// entries carried into the new profile.
fn publish_profile(staging: &Path, dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(first) = fs::rename(staging, dest) {
        if !dest.exists() {
            return Err(first);
        }
        let aside = sibling(dest, "pre-import");
        fs::rename(dest, &aside)?;
        if let Err(error) = fs::rename(staging, dest) {
            let _ = fs::rename(&aside, dest);
            return Err(error);
        }
        let mut leftover = false;
        if let Ok(entries) = fs::read_dir(&aside) {
            for entry in entries.flatten() {
                let target = dest.join(entry.file_name());
                if target.exists() || fs::rename(entry.path(), &target).is_err() {
                    leftover = true;
                }
            }
        }
        if !leftover {
            let _ = fs::remove_dir(&aside);
        }
    }
    Ok(())
}

fn publish_icons(staged: &Path, dest: &Path) -> io::Result<()> {
    if dest.is_dir() {
        // Only ignorable runtime entries can be in it: the emptiness check passed.
        fs::remove_dir_all(dest)?;
    }
    fs::rename(staged, dest)
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub backup_dir: String,
    pub counts: ProfileCounts,
    pub secrets_stored: usize,
    pub plugin_packages_copied: Vec<String>,
    pub plugin_packages_skipped: Vec<String>,
    pub skipped_links: usize,
    pub remap: Option<crate::driver_remap::RemapReport>,
}

fn build_failure(context: &str) -> impl Fn(io::Error) -> ImportError + '_ {
    move |error| io_fail(BUILD_FAILED, context, &error)
}

fn profile_skip(name: &str, depth: usize) -> bool {
    (depth == 0
        && matches!(
            name,
            "plugins"
                | "tabularis.alive"
                | "pending_approvals"
                | ".mcp_session_state.json"
                | "storage-location.json"
                | "connections.dev.json"
        ))
        || name.ends_with(".lock")
        || name.ends_with(".tmp")
}

/// Imports the official profile. Nothing is written until the destination is
/// confirmed empty; the source is backed up before anything else is built.
pub fn run_import(
    env: &ImportEnvironment,
    port: &dyn SecretPort,
) -> Result<ImportReport, ImportError> {
    let _guard = IMPORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let vaults = Vaults {
        port,
        official: &env.official_service,
        fork: &env.fork_service,
    };

    if !source_holds_data(env)? {
        return Err(fail(SOURCE_NOT_FOUND, "No official profile was found"));
    }
    if overlaps(&env.source_config, &env.dest_config) {
        return Err(fail(
            SOURCE_IS_DESTINATION,
            "The official profile is the destination profile",
        ));
    }
    if destination_holds_data(env)? {
        return Err(fail(
            DESTINATION_NOT_EMPTY,
            "The destination profile already holds data and is never overwritten",
        ));
    }

    let (backup, mut stats) = create_backup(env)?;
    let backup_profile = backup.join("profile");

    let staging = sibling(&env.dest_config, "import");
    let mut cleanup = Cleanup(vec![staging.clone()]);
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent).map_err(build_failure("The profile could not be staged"))?;
    }
    copy_tree(
        &backup_profile,
        &staging,
        Mode::Private,
        &profile_skip,
        &mut stats,
        0,
    )
    .map_err(build_failure("The profile could not be staged"))?;

    let staged_icons = if comparable(&env.dest_data) != comparable(&env.dest_config) {
        let source = backup.join("data").join(ICONS_DIR);
        if source.is_dir() {
            let target = sibling(&env.dest_data.join(ICONS_DIR), "import");
            cleanup.0.push(target.clone());
            fs::create_dir_all(&env.dest_data)
                .map_err(build_failure("The icons could not be staged"))?;
            copy_tree(
                &source,
                &target,
                Mode::Private,
                &|_, _| false,
                &mut stats,
                0,
            )
            .map_err(build_failure("The icons could not be staged"))?;
            Some(target)
        } else {
            None
        }
    } else {
        None
    };

    let connections_path = staging.join(CONNECTIONS_FILE);
    let ssh_path = staging.join(SSH_FILE);
    let connections = if connections_path.exists() {
        persistence::load_connections(&connections_path).map_err(|_| {
            fail(
                BUILD_FAILED,
                "The official connections file could not be read",
            )
        })?
    } else {
        Vec::new()
    };
    let mut ssh_entries: Vec<Value> = read_json_file(&ssh_path)
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default();
    let config: Option<Value> = read_json_file(&staging.join("config.json"))
        .and_then(|content| serde_json::from_str(&content).ok());

    let mut plan = SecretPlan::default();
    for connection in &connections {
        plan_connection(&vaults, &mut plan, connection)?;
    }
    for entry in &ssh_entries {
        plan_ssh_entry(&vaults, &mut plan, entry)?;
    }
    plan_global(&vaults, &mut plan, config.as_ref())?;

    let mut journal = Journal::new(&vaults);
    let secrets_stored = plan.entries.len();
    let outcome = finish_import(
        env,
        &staging,
        staged_icons.as_deref(),
        &connections_path,
        &ssh_path,
        &mut ssh_entries,
        &plan,
        &mut journal,
    );
    match outcome {
        Ok((packages_copied, packages_skipped, counts)) => {
            cleanup.0.clear();
            Ok(ImportReport {
                backup_dir: backup.to_string_lossy().into_owned(),
                counts,
                secrets_stored,
                plugin_packages_copied: packages_copied,
                plugin_packages_skipped: packages_skipped,
                skipped_links: stats.skipped_links,
                remap: None,
            })
        }
        Err(error) => {
            let unrestored = journal.rollback();
            drop(cleanup);
            if unrestored > 0 {
                return Err(fail(
                    error.code,
                    format!(
                        "{} {unrestored} imported keychain entries could not be removed",
                        error.message
                    ),
                ));
            }
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_import(
    env: &ImportEnvironment,
    staging: &Path,
    staged_icons: Option<&Path>,
    connections_path: &Path,
    ssh_path: &Path,
    ssh_entries: &mut [Value],
    plan: &SecretPlan,
    journal: &mut Journal<'_, '_>,
) -> Result<(Vec<String>, Vec<String>, ProfileCounts), ImportError> {
    for planned in &plan.entries {
        journal.store_verified(planned)?;
    }

    if connections_path.exists() {
        strip_connection_secrets(connections_path)?;
    }
    if ssh_path.exists() && !ssh_entries.is_empty() {
        strip_ssh_secrets(ssh_entries);
        let bytes = serde_json::to_vec_pretty(&*ssh_entries)
            .map_err(|_| fail(BUILD_FAILED, "The SSH connections could not be rewritten"))?;
        let temporary = staging.join(format!("{SSH_FILE}.rewrite"));
        fs::write(&temporary, bytes)
            .and_then(|()| harden(&temporary, false))
            .and_then(|()| fs::rename(&temporary, ssh_path))
            .map_err(build_failure("The SSH connections could not be rewritten"))?;
    }

    let (source_dir, packages) = source_packages(env);
    let stage = staging.join(PLUGIN_STAGE_DIR);
    let mut stats = CopyStats::default();
    for package in &packages {
        let target = stage.join(package.relative());
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(build_failure("The plugin packages could not be staged"))?;
        }
        copy_tree(
            &source_dir.join(package.relative()),
            &target,
            Mode::Preserve,
            &|name, _| name.starts_with(".tmp-"),
            &mut stats,
            0,
        )
        .map_err(build_failure("The plugin packages could not be staged"))?;
    }

    let counts = count_profile(staging, &packages);
    let lock = staging.join(format!("{CONNECTIONS_FILE}.lock"));
    let _ = fs::remove_file(lock);

    let (mut publication, copied, skipped) = publish_plugins(&stage, &packages, &env.dest_plugins)?;
    let _ = fs::remove_dir_all(&stage);

    let mut published_icons = None;
    if let Some(staged) = staged_icons {
        let target = env.dest_data.join(ICONS_DIR);
        if let Err(error) = publish_icons(staged, &target) {
            publication.undo();
            return Err(io_fail(
                PUBLISH_FAILED,
                "The icons could not be installed",
                &error,
            ));
        }
        published_icons = Some(target);
    }
    if let Err(error) = publish_profile(staging, &env.dest_config) {
        publication.undo();
        if let Some(icons) = published_icons {
            let _ = fs::remove_dir_all(icons);
        }
        return Err(io_fail(
            PUBLISH_FAILED,
            "The profile could not be published",
            &error,
        ));
    }
    Ok((copied, skipped, counts))
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

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
    crate::commands::invalidate_connection_cache(&app);
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
        crate::commands::invalidate_connection_cache(&app);
    }
    Ok(report)
}

#[cfg(test)]
#[path = "profile_import_tests.rs"]
mod tests;
