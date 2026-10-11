use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const SQLSERVER_PLUS_VERSION: &str = "1.0.0-beta.3+plus.1";
const MAX_PINNED_FILE_BYTES: u64 = 64 * 1024 * 1024;
const COMPILED_COMPANION_PIN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../integration/companions.json"
));

pub(super) fn verified_sqlserver_plus(plugin_root: &Path) -> bool {
    verified_sqlserver_plus_from_pin(
        plugin_root,
        COMPILED_COMPANION_PIN,
        &crate::plugins::registry::get_current_platform(),
    )
}

fn verified_sqlserver_plus_from_pin(plugin_root: &Path, pin_json: &str, platform: &str) -> bool {
    let Ok(pin) = serde_json::from_str::<Value>(pin_json) else {
        return false;
    };
    if pin
        .get("sqlserverPlus")
        .and_then(|entry| entry.get("version"))
        .and_then(Value::as_str)
        != Some(SQLSERVER_PLUS_VERSION)
    {
        return false;
    }
    let Some(files) = pin
        .get("sqlserverPlus")
        .and_then(|entry| entry.get("platforms"))
        .and_then(|platforms| platforms.get(platform))
        .and_then(|entry| entry.get("files"))
        .and_then(Value::as_object)
    else {
        return false;
    };
    if files.is_empty() {
        return false;
    }

    let mut expected_files = BTreeMap::new();
    for (path, hash) in files {
        let Some(hash) = hash.as_str() else {
            return false;
        };
        expected_files.insert(path.clone(), hash.to_string());
    }
    verified_sqlserver_plus_with_files(plugin_root, platform, &expected_files)
}

fn verified_sqlserver_plus_with_files(
    plugin_root: &Path,
    platform: &str,
    expected_files: &BTreeMap<String, String>,
) -> bool {
    if !is_non_symlink_directory(plugin_root) || expected_files.is_empty() {
        return false;
    }

    let executable_path = if platform.starts_with("win-") {
        "sqlserver-plugin.exe"
    } else {
        "sqlserver-plugin"
    };
    let required_files = [
        ".tabularium",
        executable_path,
        "ui/dist/index.js",
        "explain/dist/index.iife.js",
    ];
    if !required_files
        .iter()
        .all(|path| expected_files.contains_key(*path))
    {
        return false;
    }

    let Ok(packages) = crate::plugins::layout::driver_directories(plugin_root) else {
        return false;
    };
    packages.into_iter().any(|package| {
        is_non_symlink_directory_chain(plugin_root, &package)
            && verify_package_metadata(&package, expected_files)
            && verify_pinned_files(&package, expected_files)
    })
}

fn verify_package_metadata(package: &Path, expected_files: &BTreeMap<String, String>) -> bool {
    if !is_non_symlink_directory(package) {
        return false;
    }
    let Ok(manifest) = crate::plugins::installer::read_manifest::<Value>(package) else {
        return false;
    };
    let identity = manifest
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| manifest.get("name").and_then(Value::as_str));
    if identity != Some("sqlserver-plus")
        || manifest.get("kind").and_then(Value::as_str) != Some("driver")
        || manifest.get("version").and_then(Value::as_str) != Some(SQLSERVER_PLUS_VERSION)
        || manifest.get("executable").and_then(Value::as_str) != Some("sqlserver-plugin")
    {
        return false;
    }

    let ui_assets = manifest
        .get("ui_extensions")
        .and_then(Value::as_array)
        .filter(|entries| !entries.is_empty())
        .is_some_and(|entries| all_module_assets_are_pinned(entries, expected_files));
    let explain_assets = manifest
        .get("explain_parsers")
        .and_then(Value::as_array)
        .filter(|entries| !entries.is_empty())
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.get("engine").and_then(Value::as_str) == Some("sqlserver"))
                && all_module_assets_are_pinned(entries, expected_files)
        });

    ui_assets && explain_assets
}

fn all_module_assets_are_pinned(
    entries: &[Value],
    expected_files: &BTreeMap<String, String>,
) -> bool {
    entries.iter().all(|entry| {
        let Some(module) = entry.get("module").and_then(Value::as_str) else {
            return false;
        };
        let Some(relative_path) = validated_relative_path(module) else {
            return false;
        };
        expected_files.contains_key(&relative_path.to_string_lossy().replace('\\', "/"))
    })
}

fn verify_pinned_files(package: &Path, expected_files: &BTreeMap<String, String>) -> bool {
    expected_files.iter().all(|(relative, expected_hash)| {
        if !is_sha256_hex(expected_hash) {
            return false;
        }
        let Some(relative_path) = validated_relative_path(relative) else {
            return false;
        };
        let Some(path) = non_symlink_file_path(package, &relative_path) else {
            return false;
        };
        let Some(actual_hash) = hash_file_bounded(&path) else {
            return false;
        };
        actual_hash.eq_ignore_ascii_case(expected_hash)
    })
}

fn validated_relative_path(path: &str) -> Option<PathBuf> {
    if path.is_empty() || path.contains('\0') || path.contains('\\') || path.contains(':') {
        return None;
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return None;
    }
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(path.to_path_buf())
}

fn non_symlink_file_path(root: &Path, relative: &Path) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            return None;
        };
        path.push(name);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() {
            return None;
        }
        if index + 1 == components.len() {
            if !metadata.is_file() {
                return None;
            }
        } else if !metadata.is_dir() {
            return None;
        }
    }
    Some(path)
}

fn is_non_symlink_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
}

fn is_non_symlink_directory_chain(root: &Path, descendant: &Path) -> bool {
    if !is_non_symlink_directory(root) {
        return false;
    }
    let Ok(relative) = descendant.strip_prefix(root) else {
        return false;
    };
    let mut current = root.to_path_buf();
    let mut has_child = false;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return false;
        };
        current.push(name);
        if !is_non_symlink_directory(&current) {
            return false;
        }
        has_child = true;
    }
    has_child
}

fn is_sha256_hex(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hash_file_bounded(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_PINNED_FILE_BYTES {
        return None;
    }

    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut total_read = 0_u64;
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        total_read = total_read.checked_add(read as u64)?;
        if total_read > MAX_PINNED_FILE_BYTES {
            return None;
        }
        hasher.update(&buffer[..read]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
pub(super) const TEST_FIXTURE_MANIFEST: &str = r#"{"kind":"driver","id":"sqlserver-plus","name":"SQL Server Plus","version":"1.0.0-beta.3+plus.1","description":"Synthetic fixture","executable":"sqlserver-plugin","ui_extensions":[{"slot":"connection-modal.extra_fields","module":"ui/dist/index.js","driver":"sqlserver-plus"}],"explain_parsers":[{"engine":"sqlserver","format":"sqlserver-showplan-xml","label":"SQL Server Showplan","module":"explain/dist/index.iife.js"}]}"#;

#[cfg(test)]
fn test_fixture_expected_files(platform: &str) -> BTreeMap<String, String> {
    let executable = if platform.starts_with("win-") {
        "sqlserver-plugin.exe"
    } else {
        "sqlserver-plugin"
    };
    BTreeMap::from([
        (
            ".tabularium".to_string(),
            "dabd10a345dcbaf52776910ae0dd7c7160d1721a0a40bc3b6d4e776058c1f8b0".to_string(),
        ),
        (
            executable.to_string(),
            "eb4ffe43c9edcf310dc3513998df4fa1700fc6570c6c46e32dd3af9e028fe720".to_string(),
        ),
        (
            "ui/dist/index.js".to_string(),
            "3c8924400435b4943db3f3642e163acf7a3c577672179075b3690fc2385bb9c2".to_string(),
        ),
        (
            "explain/dist/index.iife.js".to_string(),
            "d9464e47a7372b93895e5b675d799fb7cf6ffdc6437356cc6d8e8d5ddf96f490".to_string(),
        ),
    ])
}

#[cfg(test)]
pub(super) fn verify_sqlserver_plus_fixture(plugin_root: &Path) -> bool {
    let platform = crate::plugins::registry::get_current_platform();
    let expected_files = test_fixture_expected_files(&platform);
    verified_sqlserver_plus_with_files(plugin_root, &platform, &expected_files)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    fn write_fixture(root: &Path, platform: &str) -> PathBuf {
        let package = root.join("drivers/sqlserver-plus");
        fs::create_dir_all(package.join("ui/dist")).unwrap();
        fs::create_dir_all(package.join("explain/dist")).unwrap();
        fs::write(package.join(".tabularium"), TEST_FIXTURE_MANIFEST).unwrap();
        let executable = if platform.starts_with("win-") {
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

    fn verify_fixture(root: &Path, platform: &str) -> bool {
        verified_sqlserver_plus_with_files(root, platform, &test_fixture_expected_files(platform))
    }

    #[test]
    fn 올바른_고정파일_해시와_메타데이터가_있는_패키지만_검증한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let platform = crate::plugins::registry::get_current_platform();
        let plugin_root = write_fixture(root.path(), &platform);

        // when
        let verified = verify_fixture(&plugin_root, &platform);

        // then
        assert!(verified);
    }

    #[test]
    fn 실행파일_내용이_바뀌면_고정_해시_검증에_실패한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let platform = crate::plugins::registry::get_current_platform();
        let plugin_root = write_fixture(root.path(), &platform);
        let executable = if platform.starts_with("win-") {
            "sqlserver-plugin.exe"
        } else {
            "sqlserver-plugin"
        };

        // when
        fs::write(
            plugin_root.join("drivers/sqlserver-plus").join(executable),
            "modified synthetic executable",
        )
        .unwrap();
        let verified = verify_fixture(&plugin_root, &platform);

        // then
        assert!(!verified);
    }

    #[test]
    fn 정확한_버전이어도_manifest_내용이_바뀌면_고정_해시_검증에_실패한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let platform = crate::plugins::registry::get_current_platform();
        let plugin_root = write_fixture(root.path(), &platform);

        // when
        fs::write(
            plugin_root.join("drivers/sqlserver-plus/.tabularium"),
            format!("{}\n", TEST_FIXTURE_MANIFEST),
        )
        .unwrap();
        let verified = verify_fixture(&plugin_root, &platform);

        // then
        assert!(!verified);
    }

    #[test]
    fn ui와_parser_자산이_변경되면_고정_해시_검증에_실패한다() {
        // given
        let platform = crate::plugins::registry::get_current_platform();

        // when
        for relative in ["ui/dist/index.js", "explain/dist/index.iife.js"] {
            let root = tempdir().expect("임시 폴더 생성");
            let plugin_root = write_fixture(root.path(), &platform);
            fs::write(
                plugin_root.join("drivers/sqlserver-plus").join(relative),
                "modified synthetic bundle",
            )
            .unwrap();

            // then
            assert!(
                !verify_fixture(&plugin_root, &platform),
                "{relative} 변경은 거부되어야 합니다"
            );
        }
    }

    #[test]
    fn ui와_parser_자산이_없으면_검증에_실패한다() {
        // given
        let platform = crate::plugins::registry::get_current_platform();

        // when
        for relative in ["ui/dist/index.js", "explain/dist/index.iife.js"] {
            let root = tempdir().expect("임시 폴더 생성");
            let plugin_root = write_fixture(root.path(), &platform);
            fs::remove_file(plugin_root.join("drivers/sqlserver-plus").join(relative)).unwrap();

            // then
            assert!(
                !verify_fixture(&plugin_root, &platform),
                "{relative} 누락은 거부되어야 합니다"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn 해시가_맞아도_자산의_상위경로가_symlink면_거부한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let platform = crate::plugins::registry::get_current_platform();
        let plugin_root = write_fixture(root.path(), &platform);
        let package = plugin_root.join("drivers/sqlserver-plus");
        let external_ui = root.path().join("external-ui");
        fs::create_dir_all(external_ui.join("dist")).unwrap();
        fs::write(external_ui.join("dist/index.js"), "synthetic ui bundle").unwrap();
        fs::remove_dir_all(package.join("ui/dist")).unwrap();
        fs::remove_dir(package.join("ui")).unwrap();

        // when
        symlink(&external_ui, package.join("ui")).unwrap();
        let verified = verify_fixture(&plugin_root, &platform);

        // then
        assert!(!verified);
    }

    #[cfg(unix)]
    #[test]
    fn 패키지경로의_상위디렉터리가_symlink면_거부한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let platform = crate::plugins::registry::get_current_platform();
        let plugin_root = write_fixture(root.path(), &platform);
        let drivers = plugin_root.join("drivers");
        let external_drivers = root.path().join("external-drivers");
        fs::rename(&drivers, &external_drivers).unwrap();

        // when
        symlink(&external_drivers, &drivers).unwrap();
        let verified = verify_fixture(&plugin_root, &platform);

        // then
        assert!(!verified);
    }

    #[cfg(unix)]
    #[test]
    fn 고정파일_자체가_symlink면_거부한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let platform = crate::plugins::registry::get_current_platform();
        let plugin_root = write_fixture(root.path(), &platform);
        let package = plugin_root.join("drivers/sqlserver-plus");
        let external_file = root.path().join("external-ui.js");
        fs::write(&external_file, "synthetic ui bundle").unwrap();
        fs::remove_file(package.join("ui/dist/index.js")).unwrap();

        // when
        symlink(&external_file, package.join("ui/dist/index.js")).unwrap();
        let verified = verify_fixture(&plugin_root, &platform);

        // then
        assert!(!verified);
    }

    #[test]
    fn 지원되지_않는_platform은_pin에_없어_실패한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let pin = r#"{"sqlserverPlus":{"version":"1.0.0-beta.3+plus.1","platforms":{"darwin-arm64":{"files":{".tabularium":"0000000000000000000000000000000000000000000000000000000000000000"}}}}}"#;

        // when
        let verified = verified_sqlserver_plus_from_pin(root.path(), pin, "linux-x64");

        // then
        assert!(!verified);
    }

    #[test]
    fn 현재_platform의_핀에_있는_모든_파일을_검증한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let platform = crate::plugins::registry::get_current_platform();
        let plugin_root = write_fixture(root.path(), &platform);
        let mut platforms = serde_json::Map::new();
        platforms.insert(
            platform.clone(),
            serde_json::json!({"files": test_fixture_expected_files(&platform)}),
        );
        let pin = serde_json::json!({
            "sqlserverPlus": {
                "version": SQLSERVER_PLUS_VERSION,
                "platforms": platforms,
            }
        });
        let pin = serde_json::to_string(&pin).unwrap();

        // when
        let verified = verified_sqlserver_plus_from_pin(&plugin_root, &pin, &platform);

        // then
        assert!(verified);
    }

    #[test]
    fn pin_파일테이블의_잘못된_hash와_경로를_거부한다() {
        // given
        let root = tempdir().expect("임시 폴더 생성");
        let platform = crate::plugins::registry::get_current_platform();
        let plugin_root = write_fixture(root.path(), &platform);
        let mut files = test_fixture_expected_files(&platform);
        files.insert("../outside".to_string(), "0".repeat(64));

        // when
        let traversal_rejected =
            !verified_sqlserver_plus_with_files(&plugin_root, &platform, &files);
        files.remove("../outside");
        files.insert("LICENSE".to_string(), "invalid-hash".to_string());
        let malformed_hash_rejected =
            !verified_sqlserver_plus_with_files(&plugin_root, &platform, &files);
        files.remove("LICENSE");
        files.remove("ui/dist/index.js");
        let required_asset_missing =
            !verified_sqlserver_plus_with_files(&plugin_root, &platform, &files);

        // then
        assert!(traversal_rejected);
        assert!(malformed_hash_rejected);
        assert!(required_asset_missing);
    }
}
