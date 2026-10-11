use directories::ProjectDirs;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const EXTENDED_PLUGIN_DIR_ENV_VAR: &str = "TABULARIS_EXTENDED_PLUGIN_DIR";

/// Resolve an absolute path even when its final components do not exist.
/// Existing ancestors are canonicalized first so symlink aliases cannot hide
/// overlap between storage locations.
pub(crate) fn canonicalize_with_missing(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let component = existing.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "Missing path component")
                })?;
                missing.push(component.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "Missing path parent")
                })?;
            }
            Err(error) => return Err(error),
        }
    }
    let mut canonical = fs::canonicalize(existing)?;
    for component in missing.into_iter().rev() {
        if component == ".." {
            canonical.pop();
        } else if component != "." {
            canonical.push(component);
        }
    }
    Ok(canonical)
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "tabularis-extended")
}

/// On Windows the `directories` crate nests a `config`/`data` leaf under
/// `%APPDATA%\tabularis-extended`; strip it so every kind of app data shares a single
/// `tabularis-extended` folder. On other platforms the path is returned unchanged.
/// Pure on its inputs so it stays unit-testable on any host.
pub(crate) fn unnested_app_dir(dir: &Path, strip_leaf: bool) -> PathBuf {
    if strip_leaf {
        dir.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.to_path_buf())
    } else {
        dir.to_path_buf()
    }
}

/// Platform default for the configuration directory, ignoring any custom
/// storage location. This is where the storage-location pointer file lives,
/// so it must never be redirected itself.
pub fn get_default_app_config_dir() -> PathBuf {
    match project_dirs() {
        Some(proj_dirs) => unnested_app_dir(proj_dirs.config_dir(), cfg!(target_os = "windows")),
        // Fallback for weird environments
        None => PathBuf::from(".config/tabularis-extended"),
    }
}

/// Platform default for the data directory, ignoring any custom storage
/// location. Installed plugins always live here: they are platform-specific
/// binaries and must not be synced across machines.
pub fn get_default_app_data_dir() -> PathBuf {
    match project_dirs() {
        Some(proj_dirs) => unnested_app_dir(proj_dirs.data_dir(), cfg!(target_os = "windows")),
        // Fallback for weird environments
        None => PathBuf::from(".local/share/tabularis-extended"),
    }
}

/// Directory for app configuration (connections, settings, themes, saved
/// queries, notebooks, AI activity, ...).
///
/// When the user picked a custom storage location (see
/// [`crate::storage_location`]) this resolves to that folder instead of the
/// platform default. The override is resolved once per process, so a change
/// only takes effect after a restart.
pub fn get_app_config_dir() -> PathBuf {
    match crate::storage_location::active_override() {
        Some(custom) => custom.to_path_buf(),
        None => get_default_app_config_dir(),
    }
}

/// Directory for user data that belongs to the connections (custom connection
/// icons, ...). On Linux this resolves to `~/.local/share/tabularis-extended`; on
/// macOS/Windows it shares the same `tabularis-extended` folder used by
/// [`get_app_config_dir`]. A custom storage location redirects it too, so the
/// icons referenced by `connections.json` travel with the connections.
pub fn get_app_data_dir() -> PathBuf {
    match crate::storage_location::active_override() {
        Some(custom) => custom.to_path_buf(),
        None => get_default_app_data_dir(),
    }
}

/// Directory holding installed plugins. Never follows the custom storage
/// location: plugin binaries are per-platform and must stay local. Installed
/// declarative theme packages live here as well, under `themes/<name>`;
/// driver installs use `drivers/<name>`. Flat bundles remain a discovery fallback.
pub fn get_plugins_dir() -> PathBuf {
    resolve_plugins_dir(
        &get_default_app_data_dir().join("plugins"),
        std::env::var(EXTENDED_PLUGIN_DIR_ENV_VAR).ok().as_deref(),
    )
}

pub(crate) fn resolve_plugins_dir(default: &Path, override_value: Option<&str>) -> PathBuf {
    override_value
        .filter(|value| !value.trim().is_empty())
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default.to_path_buf())
}

/// Directory holding custom connection icons, relative to which
/// `connections.json` stores `connection-icons/<file>` paths.
pub fn get_connection_icons_dir() -> PathBuf {
    let config_dir = get_app_config_dir();
    let data_dir = get_app_data_dir();
    let profile_icons = config_dir.join("connection-icons");
    let profile_icons_have_files =
        std::fs::symlink_metadata(&profile_icons).is_ok_and(|metadata| {
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && std::fs::read_dir(&profile_icons)
                    .is_ok_and(|mut entries| entries.next().is_some())
        });
    resolve_connection_icons_dir(&config_dir, &data_dir, profile_icons_have_files)
}

pub(crate) fn resolve_connection_icons_dir(
    config_dir: &Path,
    data_dir: &Path,
    profile_icons_have_files: bool,
) -> PathBuf {
    if profile_icons_have_files {
        config_dir.join("connection-icons")
    } else {
        data_dir.join("connection-icons")
    }
}

/// Resolve the connections file inside `config_dir`.
///
/// In dev builds (`debug_assertions`) a `connections.dev.json` takes
/// precedence when it exists, so development can run against a separate
/// set of connections without touching the real `connections.json`.
/// Release builds always use `connections.json`.
pub fn resolve_connections_path(config_dir: &Path) -> PathBuf {
    if cfg!(debug_assertions) {
        let dev = config_dir.join("connections.dev.json");
        if dev.exists() {
            return dev;
        }
    }
    config_dir.join("connections.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 플러그인_경로_환경변수는_절대경로만_사용한다() {
        // given
        let default = Path::new("/synthetic/default/plugins");
        let override_path = "/synthetic/test/plugins";

        // when
        let absolute = resolve_plugins_dir(default, Some(override_path));
        let relative = resolve_plugins_dir(default, Some("relative/plugins"));
        let empty = resolve_plugins_dir(default, Some("   "));

        // then
        assert_eq!(absolute, PathBuf::from(override_path));
        assert_eq!(relative, default);
        assert_eq!(empty, default);
    }

    #[test]
    fn 프로필에_가져온_아이콘이_있으면_분리된_config_경로를_사용한다() {
        // given
        let config = Path::new("/synthetic/config");
        let data = Path::new("/synthetic/data");

        // when
        let imported = resolve_connection_icons_dir(config, data, true);
        let legacy = resolve_connection_icons_dir(config, data, false);

        // then
        assert_eq!(imported, config.join("connection-icons"));
        assert_eq!(legacy, data.join("connection-icons"));
    }

    #[test]
    fn resolve_connections_path_defaults_to_connections_json() {
        let dir = std::env::temp_dir().join("tabularis-paths-test-empty");
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(resolve_connections_path(&dir), dir.join("connections.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_connections_path_prefers_dev_file_in_debug_builds() {
        let dir = std::env::temp_dir().join("tabularis-paths-test-dev");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("connections.dev.json"), "{}").unwrap();

        let resolved = resolve_connections_path(&dir);
        if cfg!(debug_assertions) {
            assert_eq!(resolved, dir.join("connections.dev.json"));
        } else {
            assert_eq!(resolved, dir.join("connections.json"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
