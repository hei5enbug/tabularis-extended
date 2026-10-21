#![allow(non_snake_case)]

use super::*;
use serde_json::json;

fn extension() -> Value {
    json!({"id":"spatial-fixture","name":"Spatial fixture","version":"1.0.0","description":"fixture","kind":"extension","min_runtime_version":"0.26.1-spatial.1","required_service_protocol":1,"ui_extensions":[{"slot":"data-grid.toolbar.actions","module":"ui/index.js"}],"ui_assets":[{"path":"ui/style.css","mime":"text/css"},{"path":"ui/worker.js","mime":"application/javascript"}]})
}
fn bundle(root: &Path, relative: &str, value: &Value) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.join("ui")).unwrap();
    fs::write(path.join(".tabularium"), value.to_string()).unwrap();
    fs::write(path.join("ui/style.css"), b".map { height: 100%; }").unwrap();
    fs::write(path.join("ui/worker.js"), b"self.onmessage = () => {}; ").unwrap();
    fs::write(
        path.join("ui/index.js"),
        b"const __tabularis_plugin__ = () => null;",
    )
    .unwrap();
    path
}

#[test]
fn 명시적extension은기존driver와분리해서발견한다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let extension_path = bundle(root.path(), "extensions/spatial-fixture", &extension());
    let legacy = bundle(
        root.path(),
        "old-ui",
        &json!({"name":"old-ui","version":"1.0.0","description":"legacy","ui_extensions":[]}),
    );
    bundle(
        root.path(),
        "themes/theme",
        &json!({"name":"theme","kind":"theme"}),
    );
    // When
    let discovered = (
        directories(root.path()).unwrap(),
        super::super::layout::driver_directories(root.path()).unwrap(),
    );
    // Then
    assert_eq!(discovered.0, vec![extension_path]);
    assert_eq!(discovered.1, vec![legacy]);
}

#[tokio::test]
async fn extension활성화는DBdriverregistry에등록하지않는다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "extensions/spatial-fixture", &extension());
    // When
    let enabled = enable(&path);
    // Then
    assert!(enabled.is_ok());
    assert!(!crate::drivers::registry::is_registered("spatial-fixture").await);
    assert!(ACTIVE.lock().unwrap().contains_key("spatial-fixture"));
}

#[tokio::test]
async fn 실제loader는extension을프로세스없이metadata로활성화한다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let mut value = extension();
    value["id"] = json!("loader-extension-fixture");
    let path = bundle(root.path(), "extensions/loader-extension-fixture", &value);
    // When
    let loaded = super::super::manager::load_plugin_from_dir(&path, None, HashMap::new()).await;
    // Then
    assert!(loaded.is_ok());
    assert!(!crate::drivers::registry::is_registered("loader-extension-fixture").await);
    assert!(ACTIVE
        .lock()
        .unwrap()
        .contains_key("loader-extension-fixture"));
}

#[tokio::test]
async fn 실제loader는사라진asset이있는extension을활성화하지않는다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let mut value = extension();
    value["id"] = json!("missing-extension-fixture");
    let path = bundle(root.path(), "extensions/missing-extension-fixture", &value);
    fs::remove_file(path.join("ui/style.css")).unwrap();
    // When
    let loaded = super::super::manager::load_plugin_from_dir(&path, None, HashMap::new()).await;
    // Then
    assert!(loaded.is_err());
    assert!(!crate::drivers::registry::is_registered("missing-extension-fixture").await);
    assert!(!ACTIVE
        .lock()
        .unwrap()
        .contains_key("missing-extension-fixture"));
}

#[test]
fn extension은실행파일과driver기능을주장할수없다() {
    // Given
    let fields = [
        "engine",
        "executable",
        "interpreter",
        "explain_parsers",
        "capabilities",
        "data_types",
        "connection_metadata",
        "default_port",
    ];
    let values: Vec<_> = fields
        .iter()
        .map(|field| {
            let mut value = extension();
            value[*field] = Value::Null;
            value
        })
        .collect();
    // When
    let rejected: Vec<_> = values
        .iter()
        .map(|value| parse_manifest(value.to_string().as_bytes()).is_err())
        .collect();
    // Then
    assert!(rejected.iter().all(|rejected| *rejected));
}

#[test]
fn 서비스protocol과extensionruntime은debug에서도엄격히검증한다() {
    // Given
    let mut values = Vec::new();
    for protocol in [json!(0), json!(2), json!("1"), json!(1.5), Value::Null] {
        let mut value = extension();
        value["required_service_protocol"] = protocol;
        values.push(value);
    }
    for version in ["bad", "999.0.0", ""] {
        let mut value = extension();
        value["min_runtime_version"] = json!(version);
        values.push(value);
    }
    // When
    let rejected: Vec<_> = values
        .iter()
        .map(|value| parse_manifest(value.to_string().as_bytes()).is_err())
        .collect();
    // Then
    assert!(rejected.iter().all(|rejected| *rejected));
}

#[test]
fn 기존kind생략UIplugin과지원protocol1driver는그대로읽는다() {
    // Given
    let old = json!({"name":"old-ui","version":"1.0.0","description":"old","ui_extensions":[]});
    let new = json!({"name":"cosmos-nosql","kind":"driver","required_service_protocol":1});
    // When
    let kinds = [old, new].map(|value| parse_manifest(value.to_string().as_bytes()).unwrap().0);
    // Then
    assert_eq!(kinds, [PackageKind::Driver, PackageKind::Driver]);
}

#[test]
fn 일반drivermanifest조회도지원하지않는서비스protocol을거부한다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let paths=[2,1].map(|protocol|bundle(root.path(),&format!("drivers/protocol-{protocol}"),&json!({"name":"protocol-driver","kind":"driver","version":"1.0.0","description":"fixture","required_service_protocol":protocol})));
    // When
    let accepted =
        paths.map(|path| super::super::installer::read_manifest::<ConfigManifest>(&path).is_ok());
    // Then
    assert_eq!(accepted, [false, true]);
}

#[test]
fn whitelist에선언된CSSasset의바이트와MIME을반환한다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "extensions/spatial-fixture", &extension());
    // When
    let asset = read_asset(&path, "ui/style.css").unwrap();
    // Then
    assert_eq!(asset.mime, "text/css");
    assert_eq!(asset.bytes, b".map { height: 100%; }");
}

#[test]
fn 선언되지않은asset과경로탈출을거부한다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "extensions/spatial-fixture", &extension());
    let paths = [
        ".tabularium",
        "../ui/style.css",
        "/ui/style.css",
        "C:/ui/style.css",
        "ui/../style.css",
        "ui\\style.css",
    ];
    // When
    let rejected: Vec<_> = paths
        .iter()
        .map(|file| read_asset(&path, file).is_err())
        .collect();
    // Then
    assert!(rejected.iter().all(|rejected| *rejected));
}

#[test]
fn 잘못된MIME과중복asset선언은읽기전에거부한다() {
    // Given
    let mut duplicate = extension();
    duplicate["ui_assets"] = json!([{"path":"ui/style.css","mime":"text/css"},{"path":"ui/style.css","mime":"text/css"}]);
    let mut invalid = extension();
    invalid["ui_assets"][0]["mime"] = json!("text/css/extra");
    // When
    let rejected =
        [duplicate, invalid].map(|value| parse_manifest(value.to_string().as_bytes()).is_err());
    // Then
    assert_eq!(rejected, [true, true]);
}

#[cfg(unix)]
#[test]
fn 심볼릭링크로설치root밖의asset을읽을수없다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "extensions/spatial-fixture", &extension());
    fs::write(root.path().join("outside"), b"private fixture").unwrap();
    fs::remove_file(path.join("ui/style.css")).unwrap();
    std::os::unix::fs::symlink(root.path().join("outside"), path.join("ui/style.css")).unwrap();
    // When
    let denied = read_asset(&path, "ui/style.css");
    // Then
    assert!(denied.is_err());
}

#[test]
fn asset이16MiB를초과하면읽기를거부한다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "extensions/spatial-fixture", &extension());
    fs::File::create(path.join("ui/style.css"))
        .unwrap()
        .set_len(ASSET_MAX_BYTES + 1)
        .unwrap();
    // When
    let denied = read_asset(&path, "ui/style.css");
    // Then
    assert!(denied.is_err());
}

#[test]
fn 교체rename실패는기존설치파일을복원한다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let destination = bundle(root.path(), "extensions/spatial-fixture", &extension());
    // When
    let replacement = replace_directory(&root.path().join("missing-temp"), &destination);
    // Then
    assert!(replacement.is_err());
    assert_eq!(
        fs::read(destination.join("ui/style.css")).unwrap(),
        b".map { height: 100%; }"
    );
    assert!(destination.join(".tabularium").exists());
}

#[test]
fn extensionZIP을실행형driver로분류하지않는다() {
    use std::io::{Cursor, Write};
    // Given
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(".tabularium", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer
        .write_all(extension().to_string().as_bytes())
        .unwrap();
    let mut archive =
        zip::ZipArchive::new(Cursor::new(writer.finish().unwrap().into_inner())).unwrap();
    // When
    let kind = super::super::package_kind::install_archive_kind(&mut archive).unwrap();
    // Then
    assert_eq!(kind, PackageKind::Extension);
}

#[test]
#[ignore]
fn 격리된plugin경로자식검증() {
    // Given
    let expected =
        std::env::var_os("TABULARIS_PLUGIN_TEST_EXPECTED").expect("isolated override fixture");
    // When
    let actual = isolated_path_scenario();
    // Then
    assert_eq!(actual, PathBuf::from(expected));
}
fn isolated_path_scenario() -> PathBuf {
    super::super::installer::migrate_legacy_plugins_dir();
    super::super::installer::get_plugins_dir().unwrap()
}
#[test]
fn 절대override는legacy마이그레이션없이격리된디렉터리만사용한다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let plugin_root = root.path().join("isolated-plugins");
    // When
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!(
                "{}::격리된plugin경로자식검증",
                module_path!().split_once("::").unwrap().1
            ),
            "--ignored",
        ])
        .env("TABULARIS_PLUGIN_DIR", &plugin_root)
        .env("TABULARIS_PLUGIN_TEST_EXPECTED", &plugin_root)
        .output()
        .unwrap();
    // Then
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "The isolated child fixture must execute exactly one test."
    );
    assert!(plugin_root.is_dir());
    assert!(fs::read_dir(&plugin_root).unwrap().next().is_none());
}

#[test]
#[ignore]
fn 상대override자식검증() {
    // Given
    let expected = "TABULARIS_PLUGIN_DIR must be an absolute path";
    // When
    let actual = super::super::installer::get_plugins_dir();
    // Then
    assert_eq!(actual.unwrap_err(), expected);
}
#[test]
fn 상대override는실제profile로fallback하지않는다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    // When
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .current_dir(root.path())
        .args([
            "--exact",
            &format!(
                "{}::상대override자식검증",
                module_path!().split_once("::").unwrap().1
            ),
            "--ignored",
        ])
        .env("TABULARIS_PLUGIN_DIR", "relative-plugin-dir")
        .output()
        .unwrap();
    // Then
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "The relative-path child fixture must execute exactly one test."
    );
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

fn archive_bytes(manifest: Value) -> Vec<u8> {
    use std::io::{Cursor, Write};
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in [
        (".tabularium", manifest.to_string().into_bytes()),
        (
            "ui/index.js",
            b"const __tabularis_plugin__ = () => null;".to_vec(),
        ),
        ("ui/style.css", b".map {}".to_vec()),
        ("ui/worker.js", b"self.onmessage = () => {};".to_vec()),
    ] {
        writer
            .start_file(
                path,
                zip::write::SimpleFileOptions::default().unix_permissions(0o755),
            )
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[tokio::test]
async fn 실제설치경로는extension을별도디렉터리에비실행파일로저장한다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let guard = super::super::install_cancellation::begin("spatial-fixture").unwrap();
    let bytes = archive_bytes(extension());
    // When
    let installed = super::super::installer::install_archive(
        root.path().to_owned(),
        "spatial-fixture",
        bytes,
        Some("1.0.0"),
        guard.cancellation(),
    )
    .await;
    // Then
    assert!(installed.is_ok());
    assert!(root
        .path()
        .join("extensions/spatial-fixture/.tabularium")
        .exists());
    assert!(super::super::layout::driver_directories(root.path())
        .unwrap()
        .is_empty());
    assert!(!crate::drivers::registry::is_registered("spatial-fixture").await);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(root.path().join("extensions/spatial-fixture/ui/worker.js"))
                .unwrap()
                .mode()
                & 0o111,
            0
        );
    }
}

#[tokio::test]
async fn protocol불일치ZIP은기존설치를교체하지않고임시파일을남기지않는다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let old = bundle(root.path(), "extensions/spatial-fixture", &extension());
    let guard = super::super::install_cancellation::begin("spatial-fixture-invalid").unwrap();
    let mut invalid = extension();
    invalid["required_service_protocol"] = json!(2);
    // When
    let installed = super::super::installer::install_archive(
        root.path().to_owned(),
        "spatial-fixture",
        archive_bytes(invalid),
        None,
        guard.cancellation(),
    )
    .await;
    // Then
    assert!(installed.is_err());
    assert_eq!(
        fs::read(old.join("ui/style.css")).unwrap(),
        b".map { height: 100%; }"
    );
    assert!(fs::read_dir(root.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".tmp-")));
}

#[tokio::test]
async fn 존재하지않는whitelistasset은설치commit전에거부한다() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let guard = super::super::install_cancellation::begin("spatial-fixture-missing").unwrap();
    let mut invalid = extension();
    invalid["ui_assets"][0]["path"] = json!("ui/missing.css");
    // When
    let installed = super::super::installer::install_archive(
        root.path().to_owned(),
        "spatial-fixture",
        archive_bytes(invalid),
        None,
        guard.cancellation(),
    )
    .await;
    // Then
    assert!(installed.is_err());
    assert!(!root.path().join("extensions/spatial-fixture").exists());
    assert!(fs::read_dir(root.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".tmp-")));
}
