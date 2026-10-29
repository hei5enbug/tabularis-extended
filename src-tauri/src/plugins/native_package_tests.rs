use std::collections::{BTreeMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::{extensions, install_cancellation, installer, runtime_version};

const HOST_VERSION: &str = "0.27.0+extended.1";
const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;
const POISONING_KEYS: [&str; 8] = [
    "NODE_OPTIONS",
    "NODE_PATH",
    "NODE_TLS_REJECT_UNAUTHORIZED",
    "NODE_EXTRA_CA_CERTS",
    "NODE_USE_SYSTEM_CA",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "OPENSSL_CONF",
];

#[test]
fn 배포_호스트_버전과_컴파일러_최소_버전은_다섯_메타데이터에_일치한다() {
    // given
    let package: Value =
        serde_json::from_str(include_str!("../../../package.json")).expect("package metadata");
    let config: Value =
        serde_json::from_str(include_str!("../../tauri.conf.json")).expect("Tauri metadata");
    let cargo = include_str!("../../Cargo.toml");
    let lock = include_str!("../../Cargo.lock");
    let frontend = include_str!("../../../packages/web-ui/src/version.ts");
    // when
    let actual = (
        package["version"].as_str(),
        config["version"].as_str(),
        env!("CARGO_PKG_VERSION"),
        cargo.contains(&format!("version = \"{HOST_VERSION}\"")),
        cargo.contains("rust-version = \"1.96.0\""),
        lock.contains(&format!(
            "name = \"tabularis-extended\"\nversion = \"{HOST_VERSION}\""
        )),
        frontend.contains(&format!("APP_VERSION = \"{HOST_VERSION}\"")),
        runtime_version::evaluate_min_runtime_version(
            "cosmos-nosql",
            Some(HOST_VERSION),
            runtime_version::HOST_VERSION,
            false,
        ),
    );
    // then
    assert_eq!(actual.0, Some(HOST_VERSION));
    assert_eq!(actual.1, Some(HOST_VERSION));
    assert_eq!(actual.2, HOST_VERSION);
    assert!(actual.3 && actual.4 && actual.5 && actual.6);
    assert_eq!(actual.7, runtime_version::RuntimeVersionVerdict::Compatible);
}

#[derive(Debug)]
struct ArchiveEvidence {
    id: String,
    sha256: String,
    zip_bytes: usize,
    files: usize,
    expanded_bytes: u64,
    kind: extensions::PackageKind,
    assets: Vec<String>,
    manifest: Value,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fixture_path(name: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(std::env::var_os(name).ok_or("Production ZIP path is required")?);
    if !path.is_absolute()
        || !std::fs::symlink_metadata(&path)
            .map_err(|_| "Production ZIP is unavailable")?
            .is_file()
    {
        return Err("Production ZIP must be an absolute regular file".into());
    }
    Ok(path)
}

fn archive_files(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Production ZIP is invalid")?;
    if archive.len() > 50_000 {
        return Err("Production ZIP file limit exceeded".into());
    }
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|_| "Production ZIP entry is invalid")?;
        if !entry.is_file()
            || entry.enclosed_name().is_none()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 != 0o100000)
            || entry.size() > 256 * 1024 * 1024
        {
            return Err("Production ZIP requires bounded regular files".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("Production ZIP size overflow")?;
        if total > 512 * 1024 * 1024 {
            return Err("Production ZIP expanded limit exceeded".into());
        }
        let name = entry.name().to_owned();
        let expected = entry.size();
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|_| "Production ZIP entry cannot be read")?;
        if content.len() as u64 != expected || files.insert(name, content).is_some() {
            return Err("Production ZIP entry differs".into());
        }
    }
    Ok(files)
}

fn verify_install(bytes: &[u8], installed: &Path, id: &str) -> Result<ArchiveEvidence, String> {
    let files = archive_files(bytes)?;
    let release: Value = serde_json::from_slice(
        files
            .get("release.json")
            .ok_or("Release ledger is missing")?,
    )
    .map_err(|_| "Release ledger is invalid")?;
    let ledger = release["files"]
        .as_array()
        .ok_or("Release ledger files are missing")?;
    let mut declared = HashSet::new();
    for record in ledger {
        let relative = record["path"]
            .as_str()
            .ok_or("Release ledger path is invalid")?;
        let content = files
            .get(relative)
            .ok_or("Release ledger file is missing")?;
        if !declared.insert(relative)
            || record["bytes"].as_u64() != Some(content.len() as u64)
            || record["sha256"].as_str() != Some(hash(content).as_str())
        {
            return Err("Release ledger hash or size differs".into());
        }
    }
    if declared.len() + 1 != files.len() || declared.contains("release.json") {
        return Err("Release ledger does not cover the archive".into());
    }
    for (relative, expected) in &files {
        let path = installed.join(relative);
        let stat = std::fs::symlink_metadata(&path).map_err(|_| "Installed file is missing")?;
        if !stat.is_file()
            || stat.len() != expected.len() as u64
            || hash(&std::fs::read(path).map_err(|_| "Installed file cannot be read")?)
                != hash(expected)
        {
            return Err("Native installation differs from production ZIP".into());
        }
    }
    let (kind, manifest) = extensions::read_package(installed)?;
    if manifest["id"] != id
        || manifest["version"] != "0.1.0"
        || !matches!(
            manifest["min_runtime_version"].as_str(),
            Some("0.26.0") | Some(HOST_VERSION)
        )
    {
        return Err("Installed manifest differs".into());
    }
    let assets = [
        "ui/dist/index.js",
        "ui/dist/style.css",
        "ui/dist/maplibre-worker.js",
    ]
    .into_iter()
    .filter(|name| files.contains_key(*name))
    .map(str::to_owned)
    .collect();
    Ok(ArchiveEvidence {
        id: id.into(),
        sha256: hash(bytes),
        zip_bytes: bytes.len(),
        files: files.len(),
        expanded_bytes: files.values().map(|content| content.len() as u64).sum(),
        kind,
        assets,
        manifest,
    })
}

async fn bounded_output<R: AsyncRead + Unpin>(reader: R) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    reader
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut output)
        .await
        .map_err(|_| "Child output read failed")?;
    if output.len() as u64 > OUTPUT_LIMIT {
        return Err("Child output limit exceeded".into());
    }
    Ok(output)
}

#[derive(Debug)]
struct ChildEvidence {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn child(
    executable: &Path,
    args: &[&str],
    installed: &Path,
    profile: &Path,
    input: &[u8],
    poison: bool,
) -> Result<ChildEvidence, String> {
    let mut command = tokio::process::Command::new(executable);
    command
        .args(args)
        .current_dir(installed)
        .env_clear()
        .env("PATH", "")
        .env("HOME", profile)
        .env("USERPROFILE", profile)
        .env("TMPDIR", profile)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if poison {
        for key in POISONING_KEYS {
            command.env(
                key,
                if key == "NODE_TLS_REJECT_UNAUTHORIZED" {
                    "0"
                } else {
                    "native-package-poison-canary"
                },
            );
        }
    }
    let mut process = command
        .spawn()
        .map_err(|_| "Installed executable could not start")?;
    let stdout = tokio::spawn(bounded_output(
        process.stdout.take().ok_or("Child stdout is missing")?,
    ));
    let stderr = tokio::spawn(bounded_output(
        process.stderr.take().ok_or("Child stderr is missing")?,
    ));
    if let Some(mut stdin) = process.stdin.take() {
        stdin
            .write_all(input)
            .await
            .map_err(|_| "Child stdin write failed")?;
    }
    let status = match tokio::time::timeout(Duration::from_secs(60), process.wait()).await {
        Ok(status) => status.map_err(|_| "Child exit could not be observed")?,
        Err(_) => {
            let _ = process.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), process.wait()).await;
            stdout.abort();
            stderr.abort();
            return Err("Installed executable timed out".into());
        }
    };
    let stdout = tokio::time::timeout(Duration::from_secs(5), stdout)
        .await
        .map_err(|_| "Child stdout did not close")?
        .map_err(|_| "Child stdout task failed")??;
    let stderr = tokio::time::timeout(Duration::from_secs(5), stderr)
        .await
        .map_err(|_| "Child stderr did not close")?
        .map_err(|_| "Child stderr task failed")??;
    Ok(ChildEvidence {
        success: status.success(),
        stdout,
        stderr,
    })
}

fn protocol(input_shutdown: bool) -> Vec<u8> {
    let mut requests = vec![
        json!({"jsonrpc":"2.0","id":"initialize","method":"initialize","params":{"settings":{},"service_protocol":1}}),
    ];
    if input_shutdown {
        requests.push(json!({"jsonrpc":"2.0","id":"shutdown","method":"shutdown","params":{}}));
    }
    requests
        .into_iter()
        .map(|request| format!("{request}\n"))
        .collect::<String>()
        .into_bytes()
}

fn parse_protocol(actual: &ChildEvidence, shutdown: bool) -> Result<Value, String> {
    if !actual.success || !actual.stderr.is_empty() || actual.stdout.last() != Some(&b'\n') {
        return Err("Native protocol process did not exit cleanly".into());
    }
    let text = std::str::from_utf8(&actual.stdout).map_err(|_| "Native stdout is not UTF-8")?;
    if text.contains("native-package-poison-canary") {
        return Err("Native output contains a poisoning canary".into());
    }
    let responses = text
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Native stdout is not JSON only")?;
    if responses.len() != if shutdown { 2 } else { 1 } {
        return Err("Native response count differs".into());
    }
    let initialized = responses
        .iter()
        .find(|response| response["id"] == "initialize")
        .ok_or("Initialize response is missing")?;
    if initialized["jsonrpc"] != "2.0" || initialized.get("error").is_some() {
        return Err("Initialize response failed".into());
    }
    if shutdown
        && !responses.iter().any(|response| {
            response["id"] == "shutdown"
                && response["result"].is_null()
                && response.get("error").is_none()
        })
    {
        return Err("Shutdown response failed".into());
    }
    Ok(initialized["result"]["service_capabilities"].clone())
}

#[derive(Debug)]
struct CosmosEvidence {
    arch: String,
    version: String,
    identity: Value,
    eof: Value,
    shutdown: Value,
}
#[derive(Debug)]
struct NativeEvidence {
    archives: Vec<ArchiveEvidence>,
    cosmos: Vec<CosmosEvidence>,
    spatial_ui_only: bool,
}

#[cfg(target_os = "macos")]
async fn install_production(paths: [PathBuf; 3]) -> Result<NativeEvidence, String> {
    use std::os::unix::fs::PermissionsExt;
    if std::env::consts::ARCH != "aarch64" {
        return Err("This fixture requires a native macOS ARM host and Rosetta".into());
    }
    let owned = tempfile::Builder::new()
        .prefix("tabularis-t0e-설치 한글 space-")
        .tempdir()
        .map_err(|_| "Private fixture directory failed")?;
    std::fs::set_permissions(owned.path(), std::fs::Permissions::from_mode(0o700))
        .map_err(|_| "Private fixture permissions failed")?;
    let mut archives = Vec::new();
    let mut cosmos = Vec::new();
    let mut spatial_ui_only = false;
    for (index, path) in paths.into_iter().enumerate() {
        let id = if index == 0 {
            "spatial"
        } else {
            "cosmos-nosql"
        };
        let root = owned.path().join(format!("profile {index}/plugins"));
        let profile = root.parent().ok_or("Private profile is missing")?;
        std::fs::create_dir_all(profile).map_err(|_| "Private profile could not be created")?;
        std::fs::set_permissions(profile, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Private profile permissions failed")?;
        std::fs::create_dir_all(&root).map_err(|_| "Private plugin root could not be created")?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Private plugin root permissions failed")?;
        let bytes = std::fs::read(path).map_err(|_| "Production ZIP could not be read")?;
        let guard = install_cancellation::begin(id)?;
        installer::install_archive(
            root.clone(),
            id,
            bytes.clone(),
            Some("0.1.0"),
            guard.cancellation(),
        )
        .await?;
        drop(guard);
        let installed = super::layout::resolve_driver(&root, id)
            .or_else(|_| extensions::destination(&root, id))?;
        let archive = verify_install(&bytes, &installed, id)?;
        if index == 0 {
            for (name, mime) in [
                ("ui/dist/style.css", "text/css"),
                ("ui/dist/maplibre-worker.js", "text/javascript"),
            ] {
                let asset = extensions::read_asset(&installed, name)?;
                if asset.mime != mime
                    || asset.bytes
                        != std::fs::read(installed.join(name))
                            .map_err(|_| "Installed asset is missing")?
                {
                    return Err(
                        "Native asset access differs from installed production bytes".into(),
                    );
                }
            }
            spatial_ui_only =
                archive.manifest.get("executable").is_none() && !root.join("spatial").exists();
        } else {
            let css = extensions::read_asset(&installed, "ui/dist/style.css")?;
            if css.mime != "text/css"
                || css.bytes
                    != std::fs::read(installed.join("ui/dist/style.css"))
                        .map_err(|_| "Installed CSS is missing")?
            {
                return Err(
                    "Native Cosmos CSS access differs from installed production bytes".into(),
                );
            }
            let arch = if index == 1 { "arm64" } else { "x64" };
            let runtime = installed.join("runtime/bin/node");
            let version = child(&runtime, &["-v"], &installed, profile, b"", false).await?;
            let identity = child(&runtime, &["-p", "JSON.stringify({version:process.version,arch:process.arch,platform:process.platform})"], &installed, profile, b"", false).await?;
            if !version.success
                || !version.stderr.is_empty()
                || !identity.success
                || !identity.stderr.is_empty()
            {
                return Err("Bundled Node identity failed".into());
            }
            let eof = child(
                &installed.join("cosmos-nosql"),
                &[],
                &installed,
                profile,
                &protocol(false),
                true,
            )
            .await?;
            let shutdown = child(
                &installed.join("cosmos-nosql"),
                &[],
                &installed,
                profile,
                &protocol(true),
                true,
            )
            .await?;
            cosmos.push(CosmosEvidence {
                arch: arch.into(),
                version: String::from_utf8(version.stdout)
                    .map_err(|_| "Node version is invalid")?
                    .trim()
                    .into(),
                identity: serde_json::from_slice(&identity.stdout)
                    .map_err(|_| "Node identity is invalid")?,
                eof: parse_protocol(&eof, false)?,
                shutdown: parse_protocol(&shutdown, true)?,
            });
        }
        println!(
            "Production native install: id={} SHA256={} zip_bytes={} files={} expanded_bytes={}",
            archive.id, archive.sha256, archive.zip_bytes, archive.files, archive.expanded_bytes
        );
        archives.push(archive);
    }
    Ok(NativeEvidence {
        archives,
        cosmos,
        spatial_ui_only,
    })
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "Requires three explicit production ZIP paths, native macOS ARM, and Rosetta"]
async fn 생산_zip_세_개는_실제_설치기와_arm_rosetta_번들_실행을_통과한다() {
    // given
    let paths = [
        fixture_path("TABULARIS_T0E_SPATIAL_ZIP").expect("explicit spatial production ZIP"),
        fixture_path("TABULARIS_T0E_COSMOS_ARM64_ZIP").expect("explicit ARM production ZIP"),
        fixture_path("TABULARIS_T0E_COSMOS_X64_ZIP").expect("explicit x64 production ZIP"),
    ];
    // when
    let actual = install_production(paths).await;
    // then
    assert!(actual.is_ok(), "{actual:?}");
    assert_eq!(actual.as_ref().unwrap().archives.len(), 3);
    assert!(matches!(
        actual.as_ref().unwrap().archives[0].kind,
        extensions::PackageKind::Extension | extensions::PackageKind::Driver
    ));
    assert_eq!(
        actual.as_ref().unwrap().archives[0].assets,
        [
            "ui/dist/index.js",
            "ui/dist/style.css",
            "ui/dist/maplibre-worker.js"
        ]
    );
    assert!(actual.as_ref().unwrap().spatial_ui_only);
    assert_eq!(actual.as_ref().unwrap().cosmos.len(), 2);
    assert!(actual.as_ref().unwrap().archives[1..]
        .iter()
        .all(|archive| archive.kind == extensions::PackageKind::Driver
            && archive.manifest["ui_extensions"]
                .as_array()
                .is_some_and(|slots| slots.len() == 5)));
    assert!(actual
        .as_ref()
        .unwrap()
        .cosmos
        .iter()
        .all(|child| child.version == "v24.21.0"
            && child.identity["version"] == "v24.21.0"
            && child.identity["arch"] == child.arch
            && child.identity["platform"] == "darwin"));
    assert!(actual
        .as_ref()
        .unwrap()
        .cosmos
        .iter()
        .flat_map(|child| [&child.eof, &child.shutdown])
        .all(|caps| caps["service_protocol"] == 1
            && caps["protocol_version"] == 1
            && caps["documents_v1"] == true
            && caps["query_page_v1"] == true
            && caps["cancel_v1"] == true
            && caps["sessions_v1"] == false));
}

async fn install_spatial_native(
    root: PathBuf,
    bytes: Vec<u8>,
) -> Result<(ArchiveEvidence, bool, bool), String> {
    let guard = install_cancellation::begin("spatial")?;
    installer::install_archive(
        root.clone(),
        "spatial",
        bytes.clone(),
        Some("0.1.0"),
        guard.cancellation(),
    )
    .await?;
    drop(guard);
    let installed = super::layout::resolve_driver(&root, "spatial")
        .or_else(|_| extensions::destination(&root, "spatial"))?;
    let evidence = verify_install(&bytes, &installed, "spatial")?;
    let mut assets_match = true;
    for (name, mime) in [
        ("ui/dist/style.css", "text/css"),
        ("ui/dist/maplibre-worker.js", "text/javascript"),
    ] {
        let asset = extensions::read_asset(&installed, name)?;
        assets_match &= asset.mime == mime
            && asset.bytes
                == std::fs::read(installed.join(name)).map_err(|_| "Installed asset is missing")?;
    }
    let extension_only =
        evidence.manifest.get("executable").is_none() && !root.join("spatial").exists();
    Ok((evidence, assets_match, extension_only))
}

#[tokio::test]
#[ignore = "Requires TABULARIS_TEST_SPATIAL_ZIP pointing to a production archive"]
async fn 현재_os의_실제_설치기는_지도_zip과_로컬_자산을_그대로_설치한다() {
    // given
    let archive =
        fixture_path("TABULARIS_TEST_SPATIAL_ZIP").expect("explicit spatial production ZIP");
    let bytes = std::fs::read(archive).expect("production ZIP bytes");
    let owned = tempfile::Builder::new()
        .prefix("tabularis-설치 한글 space-")
        .tempdir()
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(owned.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let root = owned.path().join("plugins");
    std::fs::create_dir(&root).unwrap();
    // when
    let actual = install_spatial_native(root, bytes).await;
    // then
    assert!(actual.is_ok(), "{actual:?}");
    assert!(matches!(
        actual.as_ref().unwrap().0.kind,
        extensions::PackageKind::Extension | extensions::PackageKind::Driver
    ));
    assert_eq!(
        actual.as_ref().unwrap().0.assets,
        [
            "ui/dist/index.js",
            "ui/dist/style.css",
            "ui/dist/maplibre-worker.js"
        ]
    );
    assert!(actual.as_ref().unwrap().1);
    assert!(actual.as_ref().unwrap().2);
}
