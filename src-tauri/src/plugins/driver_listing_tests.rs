use super::*;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

fn plugin(root: &Path, directory: &str, id: &str, name: &str, version: &str) {
    let path = root.join("drivers").join(directory);
    fs::create_dir_all(&path).unwrap();
    let marker = root.join(format!("{directory}.executed"));
    fs::write(
        path.join("driver.sh"),
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    let manifest = json!({
        "id": id,
        "name": name,
        "version": version,
        "description": "fixture",
        "executable": "driver.sh",
    });
    fs::write(path.join(".tabularium"), manifest.to_string()).unwrap();
}

fn snapshot(root: &Path) -> Vec<(PathBuf, u64)> {
    fn walk(path: &Path, out: &mut Vec<(PathBuf, u64)>) {
        let mut entries: Vec<_> = fs::read_dir(path).unwrap().map(|e| e.unwrap()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let meta = entry.metadata().unwrap();
            out.push((entry.path(), meta.len()));
            if meta.is_dir() {
                walk(&entry.path(), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn ids(listing: &[DriverSummary]) -> Vec<&str> {
    listing.iter().map(|d| d.id.as_str()).collect()
}

#[test]
fn builtin_manifests_match_the_registry_constant() {
    let listed: Vec<String> = builtin_summaries().into_iter().map(|d| d.id).collect();
    assert_eq!(listed, BUILTIN_DRIVER_IDS);
}

#[test]
fn listing_reads_manifests_without_running_or_writing_anything() {
    let root = tempfile::tempdir().unwrap();
    plugin(root.path(), "postgresql", "postgresql", "PostgreSQL", "0.9.1");
    plugin(root.path(), "sqlserver", "sqlserver", "SQL Server", "1.2.0");
    let before = snapshot(root.path());

    let listing = list_driver_manifests(root.path(), None);

    assert_eq!(
        ids(&listing),
        ["mysql", "postgresql", "sqlite", "sqlserver"]
    );
    let postgresql = listing.iter().find(|d| d.id == "postgresql").unwrap();
    assert_eq!(
        (postgresql.name.as_str(), postgresql.version.as_str(), postgresql.is_builtin),
        ("PostgreSQL", "0.9.1", false)
    );
    assert!(listing
        .iter()
        .filter(|d| d.is_builtin)
        .all(|d| d.id == "mysql" || d.id == "sqlite"));
    assert!(!root.path().join("postgresql.executed").exists());
    assert!(!root.path().join("sqlserver.executed").exists());
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn listing_without_plugins_has_only_mysql_and_sqlite_and_creates_nothing() {
    let parent = tempfile::tempdir().unwrap();
    let missing = parent.path().join("plugins");

    let listing = list_driver_manifests(&missing, None);

    assert_eq!(ids(&listing), ["mysql", "sqlite"]);
    assert!(listing.iter().all(|d| d.is_builtin));
    assert!(!missing.exists());
}

#[test]
fn listing_honors_the_enabled_plugin_preference() {
    let root = tempfile::tempdir().unwrap();
    plugin(root.path(), "postgresql", "postgresql", "PostgreSQL", "0.9.1");
    plugin(root.path(), "sqlserver", "sqlserver", "SQL Server", "1.2.0");

    let enabled = vec!["sqlserver".to_string()];
    let listing = list_driver_manifests(root.path(), Some(&enabled));
    let none = list_driver_manifests(root.path(), Some(&[]));

    assert_eq!(ids(&listing), ["mysql", "sqlite", "sqlserver"]);
    assert_eq!(ids(&none), ["mysql", "sqlite"]);
}

#[test]
fn listing_omits_a_plugin_that_claims_a_builtin_id() {
    let root = tempfile::tempdir().unwrap();
    plugin(root.path(), "mysql", "mysql", "Impostor", "9.9.9");

    let listing = list_driver_manifests(root.path(), None);

    let mysql = listing.iter().find(|d| d.id == "mysql").unwrap();
    assert_eq!((mysql.name.as_str(), mysql.is_builtin), ("MySQL", true));
}

#[test]
fn listing_json_is_an_array_of_id_name_version_and_builtin_flag() {
    let root = tempfile::tempdir().unwrap();
    let json =
        serde_json::to_value(list_driver_manifests(root.path(), None)).unwrap();
    assert_eq!(json[0]["id"], "mysql");
    assert_eq!(json[0]["is_builtin"], true);
    assert_eq!(json[0].as_object().unwrap().len(), 4);
}
