use crate::paths::{get_app_config_dir, get_app_data_dir, unnested_app_dir};
use std::path::Path;

#[test]
fn unnested_app_dir_strips_trailing_leaf_when_requested() {
    let dir = Path::new("/home/u/.local/share/tabularis/data");
    assert_eq!(
        unnested_app_dir(dir, true),
        Path::new("/home/u/.local/share/tabularis")
    );
}

#[test]
fn unnested_app_dir_keeps_path_when_not_stripping() {
    let dir = Path::new("/home/u/.local/share/tabularis");
    assert_eq!(unnested_app_dir(dir, false), dir.to_path_buf());
}

#[test]
fn unnested_app_dir_keeps_root_when_no_parent() {
    let dir = Path::new("/");
    assert_eq!(unnested_app_dir(dir, true), dir.to_path_buf());
}

#[test]
fn config_and_data_dirs_share_the_fork_folder_name() {
    assert_eq!(
        get_app_config_dir().file_name().and_then(|n| n.to_str()),
        Some("tabularis-extended")
    );
    assert_eq!(
        get_app_data_dir().file_name().and_then(|n| n.to_str()),
        Some("tabularis-extended")
    );
}

#[test]
fn default_dirs_never_resolve_to_the_official_profile_folder() {
    assert_eq!(crate::paths::APP_DIR_NAME, "tabularis-extended");
    for dir in [
        crate::paths::get_default_app_config_dir(),
        crate::paths::get_default_app_data_dir(),
    ] {
        assert!(
            dir.components()
                .all(|part| part.as_os_str() != "tabularis" && part.as_os_str() != "com.debba.tabularis"),
            "{dir:?} points at the official profile"
        );
        assert!(dir.components().any(|part| part.as_os_str() == "tabularis-extended"));
    }
}
