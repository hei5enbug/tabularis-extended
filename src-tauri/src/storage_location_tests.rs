use crate::storage_location::{
    clear_pointer, copy_storage, inspect, is_copyable_entry, read_pointer, resolve_override_in,
    validate_target, write_pointer, POINTER_FILE,
};
use std::fs;
use std::path::{Path, PathBuf};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tabularis-storage-location-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn abs(p: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{}", p.replace('/', "\\")))
    } else {
        PathBuf::from(p)
    }
}

// ---------------------------------------------------------------- resolution

#[test]
fn resolve_returns_none_without_env_or_pointer() {
    let dir = temp_dir("resolve-none");
    assert_eq!(resolve_override_in(&dir, None), None);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolve_prefers_env_over_pointer() {
    let dir = temp_dir("resolve-env");
    write_pointer(&dir, &abs("/from/pointer")).unwrap();
    let env = abs("/from/env").to_string_lossy().into_owned();
    assert_eq!(resolve_override_in(&dir, Some(env)), Some(abs("/from/env")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolve_falls_back_to_pointer_when_env_is_empty_or_relative() {
    let dir = temp_dir("resolve-fallback");
    write_pointer(&dir, &abs("/from/pointer")).unwrap();
    assert_eq!(
        resolve_override_in(&dir, Some("   ".to_string())),
        Some(abs("/from/pointer"))
    );
    assert_eq!(
        resolve_override_in(&dir, Some("relative/dir".to_string())),
        Some(abs("/from/pointer"))
    );
    let _ = fs::remove_dir_all(&dir);
}

// -------------------------------------------------------------- pointer file

#[test]
fn pointer_round_trips_and_clears() {
    let dir = temp_dir("pointer");
    assert_eq!(read_pointer(&dir), None);

    write_pointer(&dir, &abs("/custom/location")).unwrap();
    assert!(dir.join(POINTER_FILE).is_file());
    assert_eq!(read_pointer(&dir), Some(abs("/custom/location")));

    clear_pointer(&dir).unwrap();
    assert_eq!(read_pointer(&dir), None);
    // Clearing twice is a no-op, not an error.
    clear_pointer(&dir).unwrap();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pointer_ignores_garbage_and_relative_paths() {
    let dir = temp_dir("pointer-garbage");
    fs::write(dir.join(POINTER_FILE), "not json").unwrap();
    assert_eq!(read_pointer(&dir), None);

    fs::write(dir.join(POINTER_FILE), r#"{"path": "relative/dir"}"#).unwrap();
    assert_eq!(read_pointer(&dir), None);

    fs::write(dir.join(POINTER_FILE), r#"{"path": ""}"#).unwrap();
    assert_eq!(read_pointer(&dir), None);
    let _ = fs::remove_dir_all(&dir);
}

// --------------------------------------------------------------- validation

#[test]
fn validate_rejects_relative_paths() {
    let current = temp_dir("validate-relative");
    assert!(validate_target(Path::new("relative"), &current).is_err());
    let _ = fs::remove_dir_all(&current);
}

#[test]
fn validate_rejects_files() {
    let current = temp_dir("validate-file");
    let file = current
        .parent()
        .unwrap()
        .join("tabularis-storage-location-a-file");
    fs::write(&file, "x").unwrap();
    assert!(validate_target(&file, &current).is_err());
    let _ = fs::remove_file(&file);
    let _ = fs::remove_dir_all(&current);
}

#[test]
fn validate_rejects_current_and_nested_folders() {
    let current = temp_dir("validate-nested");
    assert!(validate_target(&current, &current).is_err());
    assert!(validate_target(&current.join("child"), &current).is_err());
    assert!(validate_target(current.parent().unwrap(), &current).is_err());
    let _ = fs::remove_dir_all(&current);
}

#[cfg(unix)]
#[test]
fn 검증은_소스_심볼릭_별칭의_누락된_자식_경로를_거부한다() {
    use std::os::unix::fs::symlink;

    // Given: 현재 경로에 대한 심볼릭 별칭과 아직 없는 자식 경로가 있다.
    let current = temp_dir("validate-symlink-source");
    let alias = current
        .parent()
        .unwrap()
        .join("tabularis-storage-location-source-alias");
    let _ = fs::remove_file(&alias);
    symlink(&current, &alias).unwrap();

    // When: 별칭 아래의 자식 경로를 새 저장 위치로 검증한다.
    let missing_target = alias.join("not-created-yet");

    // Then: 현재 위치 안쪽 경로로 판별해 거부한다.
    assert!(validate_target(&missing_target, &current).is_err());

    let _ = fs::remove_file(&alias);
    let _ = fs::remove_dir_all(&current);
}

#[test]
fn validate_accepts_sibling_folder_even_if_missing() {
    let current = temp_dir("validate-ok");
    let sibling = current
        .parent()
        .unwrap()
        .join("tabularis-storage-location-sibling-missing");
    let _ = fs::remove_dir_all(&sibling);
    assert!(validate_target(&sibling, &current).is_ok());
    let _ = fs::remove_dir_all(&current);
}

// --------------------------------------------------------------- inspection

#[test]
fn inspect_reports_missing_empty_and_populated_folders() {
    let dir = temp_dir("inspect");

    let missing = dir.join("missing");
    let i = inspect(&missing);
    assert!(!i.exists && !i.is_empty && !i.has_tabularis_data);

    let i = inspect(&dir);
    assert!(i.exists && i.is_empty && !i.has_tabularis_data);

    fs::write(dir.join("notes.txt"), "x").unwrap();
    let i = inspect(&dir);
    assert!(i.exists && !i.is_empty && !i.has_tabularis_data);

    fs::write(dir.join("connections.json"), "{}").unwrap();
    let i = inspect(&dir);
    assert!(i.exists && !i.is_empty && i.has_tabularis_data);
    let _ = fs::remove_dir_all(&dir);
}

// --------------------------------------------------------------------- copy

#[test]
fn copyable_entries_exclude_local_runtime_state() {
    assert!(is_copyable_entry("connections.json"));
    assert!(is_copyable_entry("saved_queries"));
    assert!(is_copyable_entry("themes"));
    assert!(!is_copyable_entry(POINTER_FILE));
    assert!(!is_copyable_entry("plugins"));
    assert!(!is_copyable_entry("tabularis.alive"));
    assert!(!is_copyable_entry("pending_approvals"));
}

#[test]
fn copy_storage_copies_data_skips_excluded_and_keeps_existing() {
    let root = temp_dir("copy");
    let config = root.join("config");
    let data = root.join("data");
    let target = root.join("target");
    fs::create_dir_all(config.join("saved_queries")).unwrap();
    fs::create_dir_all(config.join("plugins")).unwrap();
    fs::create_dir_all(data.join("connection-icons")).unwrap();
    fs::write(config.join("connections.json"), "conns").unwrap();
    fs::write(config.join("saved_queries").join("q.json"), "q").unwrap();
    fs::write(config.join("plugins").join("bin"), "bin").unwrap();
    fs::write(config.join(POINTER_FILE), "{}").unwrap();
    fs::write(data.join("connection-icons").join("a.png"), "png").unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("connections.json"), "keep me").unwrap();

    let copied = copy_storage(&config, &data, &target).unwrap();

    // saved_queries/q.json + connection-icons/a.png; connections.json skipped.
    assert_eq!(copied, 2);
    assert_eq!(
        fs::read_to_string(target.join("connections.json")).unwrap(),
        "keep me"
    );
    assert!(target.join("saved_queries").join("q.json").is_file());
    assert!(target.join("connection-icons").join("a.png").is_file());
    assert!(!target.join("plugins").exists());
    assert!(!target.join(POINTER_FILE).exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn copy_storage_does_not_duplicate_icons_when_dirs_coincide() {
    let root = temp_dir("copy-same");
    let config = root.join("config");
    let target = root.join("target");
    fs::create_dir_all(config.join("connection-icons")).unwrap();
    fs::write(config.join("connection-icons").join("a.png"), "png").unwrap();

    let copied = copy_storage(&config, &config, &target).unwrap();
    assert_eq!(copied, 1);
    let _ = fs::remove_dir_all(&root);
}

// ----------------------------------------------------------- process state

#[test]
fn active_override_follows_the_environment_variable() {
    use crate::storage_location::{active_override, active_source, StorageLocationSource, ENV_VAR};
    let env_dir = std::env::var_os(ENV_VAR)
        .map(|v| PathBuf::from(v.to_string_lossy().into_owned()))
        .filter(|p| p.is_absolute());
    match env_dir {
        Some(dir) => {
            assert_eq!(active_override(), Some(dir.as_path()));
            assert_eq!(active_source(), StorageLocationSource::Env);
            assert_eq!(crate::paths::get_app_config_dir(), dir);
            assert_eq!(crate::paths::get_app_data_dir(), dir);
        }
        None => {
            assert_ne!(active_source(), StorageLocationSource::Env);
        }
    }
    // Plugins use their own override independently of profile storage.
    let plugin_override = std::env::var(crate::paths::EXTENDED_PLUGIN_DIR_ENV_VAR).ok();
    let expected_plugins = plugin_override
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| crate::paths::get_default_app_data_dir().join("plugins"));
    assert_eq!(crate::paths::get_plugins_dir(), expected_plugins);
}
