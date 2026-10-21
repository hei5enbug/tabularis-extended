use super::*;
use serde_json::json;
use std::collections::BTreeMap;
use tempfile::TempDir;

const OFFICIAL: &str = "official-test-service";
const FORK: &str = "fork-test-service";

const PG_KEYCHAIN: &str = "pg-keychain-secret-1";
const INLINE_DB: &str = "inline-db-secret-2";
const INLINE_SSH: &str = "inline-ssh-secret-3";
const INLINE_PASSPHRASE: &str = "inline-passphrase-secret-4";
const STALE_DB: &str = "stale-official-secret-5";
const SSH_ENTRY_INLINE: &str = "ssh-entry-inline-secret-6";
const SSH_ENTRY_KEYCHAIN: &str = "ssh-entry-keychain-secret-7";
const AI_KEY: &str = "ai-key-secret-8";
const URI_SECRET: &str = "uri-secret-9";
const ALL_SECRETS: &[&str] = &[
    PG_KEYCHAIN,
    INLINE_DB,
    INLINE_SSH,
    INLINE_PASSPHRASE,
    STALE_DB,
    SSH_ENTRY_INLINE,
    SSH_ENTRY_KEYCHAIN,
    AI_KEY,
    URI_SECRET,
];

#[derive(Default)]
struct MemoryVault {
    entries: Mutex<BTreeMap<(String, String), String>>,
    calls: Mutex<Vec<(&'static str, String, String)>>,
    sets_allowed: Mutex<Option<usize>>,
    corrupt_stores: Mutex<bool>,
    probe: Mutex<Option<(PathBuf, Option<bool>)>>,
}

impl MemoryVault {
    fn put(&self, service: &str, account: &str, value: &str) {
        self.entries
            .lock()
            .unwrap()
            .insert((service.into(), account.into()), value.into());
    }
    fn value(&self, service: &str, account: &str) -> Option<String> {
        self.entries
            .lock()
            .unwrap()
            .get(&(service.to_owned(), account.to_owned()))
            .cloned()
    }
    fn snapshot(&self, service: &str) -> BTreeMap<String, String> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|((owner, _), _)| owner == service)
            .map(|((_, account), value)| (account.clone(), value.clone()))
            .collect()
    }
    fn writes(&self, service: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(operation, owner, _)| *operation != "get" && owner == service)
            .count()
    }
    fn record(&self, operation: &'static str, service: &str, account: &str) {
        let mut probe = self.probe.lock().unwrap();
        if let Some((directory, seen)) = probe.as_mut() {
            if seen.is_none() {
                *seen = Some(
                    fs::read_dir(&*directory)
                        .map(|mut entries| entries.next().is_some())
                        .unwrap_or(false),
                );
            }
        }
        self.calls
            .lock()
            .unwrap()
            .push((operation, service.into(), account.into()));
    }
}

impl SecretPort for MemoryVault {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, VaultFault> {
        self.record("get", service, account);
        Ok(self.value(service, account))
    }
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), VaultFault> {
        self.record("set", service, account);
        if let Some(allowed) = self.sets_allowed.lock().unwrap().as_mut() {
            if *allowed == 0 {
                return Err(VaultFault);
            }
            *allowed -= 1;
        }
        let stored = if *self.corrupt_stores.lock().unwrap() {
            "corrupted"
        } else {
            value
        };
        self.put(service, account, stored);
        Ok(())
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), VaultFault> {
        self.record("delete", service, account);
        self.entries
            .lock()
            .unwrap()
            .remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

struct Fixture {
    root: TempDir,
    env: ImportEnvironment,
    vault: MemoryVault,
}

fn write(path: &Path, content: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn connections_json() -> Value {
    json!({
        "future_top": {"keep": [1, 2]},
        "groups": [
            {"id": "g1", "name": "Group", "collapsed": true, "sort_order": 3},
            {"id": "g2", "name": "Child", "collapsed": false, "sort_order": 0, "parent_id": "g1"}
        ],
        "tags": [{"id": "t1", "name": "Tag", "color": "#f97316"}],
        "connections": [
            {
                "id": "c1", "name": "Keychain pg", "group_id": "g1", "tag_ids": ["t1"],
                "environment": "production", "future_connection": "retained",
                "params": {
                    "driver": "postgresql", "host": "pg.example.test", "port": 5432,
                    "username": "pg", "database": ["alpha", "beta"],
                    "ssl_mode": "verify-full", "save_in_keychain": true,
                    "ssh_enabled": true, "ssh_connection_id": "s1",
                    "extra": {"keep": "yes"}, "future_param": {"v": 1}
                }
            },
            {
                "id": "c2", "name": "Inline mysql",
                "params": {
                    "driver": "mysql", "host": "my.example.test", "port": 3306,
                    "username": "my", "database": "main", "save_in_keychain": false,
                    "password": INLINE_DB, "ssh_enabled": true,
                    "ssh_host": "bastion.example.test", "ssh_user": "ops",
                    "ssh_password": INLINE_SSH, "ssh_key_passphrase": INLINE_PASSPHRASE
                }
            },
            {
                "id": "c3", "name": "Uri",
                "params": {
                    "driver": "mongodb", "database": "docs", "save_in_keychain": true,
                    "connection_uri_in_keychain": true
                }
            },
            {
                "id": "c4", "name": "File",
                "params": {"driver": "sqlite", "database": "/tmp/example.db"}
            }
        ]
    })
}

fn ssh_json() -> Value {
    json!([
        {"id": "s1", "name": "Inline ssh", "host": "h1.example.test", "port": 22, "user": "u",
         "auth_type": "password", "password": SSH_ENTRY_INLINE, "save_in_keychain": false,
         "future_ssh": 1},
        {"id": "s2", "name": "Keychain ssh", "host": "h2.example.test", "port": 22, "user": "u",
         "auth_type": "password", "save_in_keychain": true}
    ])
}

fn fixture(linux_like: bool) -> Fixture {
    let root = TempDir::new().unwrap();
    let base = root.path();
    let source = base.join("official").join("config");
    let source_data = if linux_like {
        base.join("official").join("data")
    } else {
        source.clone()
    };
    let dest = base.join("fork").join("config");
    let dest_data = if linux_like {
        base.join("fork").join("data")
    } else {
        dest.clone()
    };

    write(
        &source.join("connections.json"),
        &serde_json::to_vec_pretty(&connections_json()).unwrap(),
    );
    write(
        &source.join("ssh_connections.json"),
        &serde_json::to_vec_pretty(&ssh_json()).unwrap(),
    );
    write(
        &source.join("config.json"),
        br#"{"theme":"dark","ai_provider":"openai","font_size":14}"#,
    );
    write(
        &source.join("saved_queries/meta.json"),
        br#"[{"id":"q1","name":"Q","filename":"q1.sql","connection_id":"c1"},{"id":"q2","name":"R","filename":"q2.sql","connection_id":"c2"}]"#,
    );
    write(&source.join("saved_queries/q1.sql"), b"select 1;\n");
    write(&source.join("saved_queries/q2.sql"), b"select 2;\n");
    write(
        &source.join("notebooks/c1/n1.tabularis-notebook"),
        b"{\"cells\":[]}",
    );
    write(
        &source.join("notebooks/c2/n2.tabularis-notebook"),
        b"{\"cells\":[1]}",
    );
    write(
        &source.join("themes/custom.json"),
        br##"{"id":"custom","name":"Custom"}"##,
    );
    write(
        &source.join("preferences/c1/preferences.json"),
        b"{\"tabs\":[]}",
    );
    write(
        &source_data.join("connection-icons/icon.png"),
        &[0x89, 0x50, 0x4e, 0x47],
    );
    write(&source.join("tabularis.alive"), b"1");
    write(&source.join("storage-location.json"), b"{}");
    write(&source.join("connections.json.lock"), b"");

    let plugins = base.join("official").join("plugins");
    let manifest = |name: &str| json!({"name": name, "version": "1.0.0", "description": "fixture"});
    for (relative, name) in [
        ("drivers/postgresql-plus", "postgresql-plus"),
        ("drivers/other", "other"),
        ("flat-bundle", "flat-bundle"),
    ] {
        write(
            &plugins.join(relative).join(".tabularium"),
            manifest(name).to_string().as_bytes(),
        );
        write(&plugins.join(relative).join("plugin-bin"), b"binary");
    }
    write(
        &plugins.join("themes/ember/.tabularium"),
        json!({"name": "ember", "kind": "theme", "version": "1.0.0", "description": "x"})
            .to_string()
            .as_bytes(),
    );
    write(&plugins.join(".tmp-partial/leftover"), b"x");

    let vault = MemoryVault::default();
    vault.put(OFFICIAL, "c1:db", PG_KEYCHAIN);
    vault.put(OFFICIAL, "c2:db", STALE_DB);
    vault.put(OFFICIAL, "c3:connection_uri", URI_SECRET);
    vault.put(OFFICIAL, "s2:ssh", SSH_ENTRY_KEYCHAIN);
    vault.put(OFFICIAL, "ai_key:openai", AI_KEY);

    let env = ImportEnvironment {
        source_config: source,
        source_data,
        source_plugins: vec![plugins],
        backup_root: dest_data.join(BACKUP_DIR_NAME),
        dest_config: dest,
        dest_data,
        dest_plugins: base.join("fork").join("plugins"),
        official_service: OFFICIAL.into(),
        fork_service: FORK.into(),
    };
    Fixture { root, env, vault }
}

/// Every regular file under `root` with its bytes, keyed by relative path.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, found);
            } else {
                found.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn connection(file: &Value, id: &str) -> Value {
    file["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap()
        .clone()
}

fn staging_leftovers(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with('.') && name.contains("import"))
                .collect()
        })
        .unwrap_or_default()
}

fn run(fixture: &Fixture) -> Result<ImportReport, ImportError> {
    run_import(&fixture.env, &fixture.vault)
}

fn assert_clean_failure(fixture: &Fixture) {
    assert!(!fixture.env.dest_config.exists(), "no destination profile");
    assert!(
        fixture.vault.snapshot(FORK).is_empty(),
        "no fork keychain entries"
    );
    assert!(staging_leftovers(fixture.env.dest_config.parent().unwrap()).is_empty());
    assert!(staging_leftovers(&fixture.env.dest_data).is_empty());
    assert!(!fixture.env.dest_data.join("connection-icons").exists());
    assert!(!fixture.env.dest_plugins.join("drivers").exists());
}

// --- status -----------------------------------------------------------------

#[test]
fn 상태는_공식_프로필의_항목_수를_비밀_없이_센다() {
    // given
    let fixture = fixture(false);

    // when
    let status = import_status(&fixture.env);

    // then
    assert!(status.source_found);
    assert!(status.destination_empty);
    assert!(status.can_import);
    assert_eq!(
        status.counts,
        ProfileCounts {
            connections: 4,
            groups: 2,
            saved_queries: 2,
            notebooks: 2,
            themes: 2,
            plugin_packages: 3,
        }
    );
    let rendered = serde_json::to_string(&status).unwrap();
    assert!(ALL_SECRETS.iter().all(|secret| !rendered.contains(secret)));
    assert_eq!(
        fixture.vault.writes(OFFICIAL) + fixture.vault.writes(FORK),
        0
    );
}

#[test]
fn 공식_프로필이_없으면_상태는_찾지_못했다고_알린다() {
    // given
    let mut fixture = fixture(false);
    fixture.env.source_config = fixture.root.path().join("missing");

    // when
    let status = import_status(&fixture.env);

    // then
    assert!(!status.source_found);
    assert!(!status.can_import);
    assert_eq!(status.counts, ProfileCounts::default());
}

#[test]
fn 플러그인만_있는_공식_디렉터리는_프로필로_보지_않는다() {
    // given
    let fixture = fixture(false);
    let only_plugins = fixture.root.path().join("only-plugins");
    write(&only_plugins.join("plugins/drivers/x/.tabularium"), b"{}");
    write(&only_plugins.join("logs/app.log"), b"x");
    let mut env = fixture.env.clone();
    env.source_config = only_plugins;

    // when
    let status = import_status(&env);
    let result = run_import(&env, &fixture.vault);

    // then
    assert!(!status.source_found);
    assert_eq!(result.unwrap_err().code, SOURCE_NOT_FOUND);
}

// --- location ---------------------------------------------------------------

#[test]
fn 공식_위치는_저장_위치_포인터를_따르고_플러그인은_따르지_않는다() {
    // given
    let root = TempDir::new().unwrap();
    let default_config = root.path().join("official-default");
    let default_data = root.path().join("official-data");
    let custom = root.path().join("synced-folder");
    fs::create_dir_all(&default_config).unwrap();
    crate::storage_location::write_pointer(&default_config, &custom).unwrap();
    let legacy = root.path().join("legacy-plugins");

    // when
    let located = official_locations_from(&default_config, &default_data, Some(legacy.clone()));
    let plain = official_locations_from(&root.path().join("none"), &default_data, None);

    // then
    assert_eq!(located.config, custom);
    assert_eq!(located.data, custom);
    assert_eq!(located.plugins, vec![default_data.join("plugins"), legacy]);
    assert_eq!(plain.config, root.path().join("none"));
    assert_eq!(plain.data, default_data);
}

#[test]
fn 공식_앱_이름과_키체인_서비스는_포크와_다르다() {
    // given
    let fork_dir = crate::paths::APP_DIR_NAME;
    let fork_service = crate::keychain_utils::SERVICE_NAME;

    // when
    let official = (OFFICIAL_APP_DIR_NAME, OFFICIAL_KEYCHAIN_SERVICE);

    // then
    assert_eq!(official, ("tabularis", "tabularis"));
    assert_ne!(official.0, fork_dir);
    assert_ne!(official.1, fork_service);
}

// --- nonempty rule ----------------------------------------------------------

#[test]
fn 새로_만든_빈_폴더와_로그만_있는_폴더는_비어_있는_것으로_본다() {
    // given
    let root = TempDir::new().unwrap();
    let fresh = root.path().join("fresh");
    fs::create_dir_all(&fresh).unwrap();
    let logs = root.path().join("logs-only");
    write(&logs.join("logs/session.log"), b"line");
    write(&logs.join("debug.log"), b"line");
    write(&logs.join("connections.json.lock"), b"");
    write(&logs.join("tabularis.alive"), b"1");
    write(&logs.join("plugins/drivers/x/.tabularium"), b"{}");
    write(
        &logs.join(format!(
            "{BACKUP_DIR_NAME}/official-1/profile/connections.json"
        )),
        b"[]",
    );
    fs::create_dir_all(logs.join("saved_queries")).unwrap();

    // when
    let results = (
        holds_user_data(&root.path().join("absent")).unwrap(),
        holds_user_data(&fresh).unwrap(),
        holds_user_data(&logs).unwrap(),
    );

    // then
    assert_eq!(results, (false, false, false));
}

#[test]
fn 사용자_데이터_파일이_하나라도_있으면_비어_있지_않다() {
    // given
    let root = TempDir::new().unwrap();
    for (name, relative) in [
        ("config", "config.json"),
        ("connections", "connections.json"),
        ("nested", "notebooks/c1/n1.tabularis-notebook"),
        ("icons", "connection-icons/icon.png"),
        ("other", "anything.txt"),
    ] {
        write(&root.path().join(name).join(relative), b"x");
    }
    let file = root.path().join("a-file");
    write(&file, b"x");

    // when
    let holds: Vec<bool> = ["config", "connections", "nested", "icons", "other"]
        .iter()
        .map(|name| holds_user_data(&root.path().join(name)).unwrap())
        .collect();

    // then
    assert_eq!(holds, vec![true; 5]);
    assert!(holds_user_data(&file).unwrap());
}

#[test]
fn 대상이_비어_있지_않으면_아무것도_쓰지_않고_거절한다() {
    // given
    let fixture = fixture(false);
    write(&fixture.env.dest_config.join("connections.json"), b"[]");
    let before = tree(&fixture.env.dest_config);
    let official_before = tree(&fixture.env.source_config);

    // when
    let error = run(&fixture).unwrap_err();

    // then
    assert_eq!(error.code, DESTINATION_NOT_EMPTY);
    assert_eq!(tree(&fixture.env.dest_config), before);
    assert_eq!(tree(&fixture.env.source_config), official_before);
    assert!(!fixture.env.backup_root.exists());
    assert!(staging_leftovers(fixture.env.dest_config.parent().unwrap()).is_empty());
    assert!(fixture.vault.calls.lock().unwrap().is_empty());
    assert!(!fixture.env.dest_plugins.exists());
}

#[test]
fn 대상의_연결_아이콘이_비어_있지_않아도_거절한다() {
    // given
    let fixture = fixture(true);
    write(
        &fixture.env.dest_data.join("connection-icons/mine.png"),
        b"x",
    );

    // when
    let error = run(&fixture).unwrap_err();

    // then
    assert_eq!(error.code, DESTINATION_NOT_EMPTY);
    assert!(!fixture.env.dest_config.exists());
    assert!(fixture.vault.calls.lock().unwrap().is_empty());
}

#[test]
fn 원본과_대상이_같거나_겹치면_거절한다() {
    // given
    let mut fixture = fixture(false);
    fixture.env.dest_config = fixture.env.source_config.clone();
    fixture.env.dest_data = fixture.env.source_config.clone();

    // when
    let error = run(&fixture).unwrap_err();

    // then
    assert_eq!(error.code, SOURCE_IS_DESTINATION);
    assert!(fixture.vault.calls.lock().unwrap().is_empty());
}

#[test]
fn 로그만_있는_대상에는_가져오고_기존_로그를_보존한다() {
    // given
    let fixture = fixture(false);
    write(&fixture.env.dest_config.join("logs/old.log"), b"old log");

    // when
    let report = run(&fixture).unwrap();

    // then
    assert_eq!(report.counts.connections, 4);
    assert_eq!(
        fs::read(fixture.env.dest_config.join("logs/old.log")).unwrap(),
        b"old log"
    );
    assert!(fixture.env.dest_config.join("connections.json").exists());
    assert!(Path::new(&report.backup_dir).exists());
    assert!(fs::read_dir(fixture.env.dest_config.parent().unwrap())
        .unwrap()
        .flatten()
        .all(|entry| !entry.file_name().to_string_lossy().contains("pre-import")));
}

// --- backup -----------------------------------------------------------------

#[cfg(unix)]
fn assert_owner_only(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let metadata = fs::symlink_metadata(&path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        if metadata.is_dir() {
            assert_eq!(mode, 0o700, "{path:?}");
            stack.extend(
                fs::read_dir(&path)
                    .unwrap()
                    .flatten()
                    .map(|entry| entry.path()),
            );
        } else {
            assert_eq!(mode, 0o600, "{path:?}");
        }
    }
}

#[test]
fn 백업은_게시_전에_소유자만_읽을_수_있게_만들어진다() {
    // given
    let fixture = fixture(false);
    *fixture.vault.probe.lock().unwrap() = Some((fixture.env.backup_root.clone(), None));
    let source_before = tree(&fixture.env.source_config);

    // when
    let report = run(&fixture).unwrap();

    // then
    let backup = PathBuf::from(&report.backup_dir);
    assert!(backup.starts_with(&fixture.env.backup_root));
    assert!(backup.starts_with(&fixture.env.dest_data));
    let backed_up = tree(&backup.join("profile"));
    for name in [
        "connections.json",
        "ssh_connections.json",
        "config.json",
        "saved_queries/meta.json",
        "notebooks/c1/n1.tabularis-notebook",
        "themes/custom.json",
        "connection-icons/icon.png",
    ] {
        assert_eq!(backed_up.get(name), source_before.get(name), "{name}");
    }
    assert!(!backed_up.contains_key("tabularis.alive"));
    assert!(backup.join("backup-info.json").exists());
    assert_eq!(
        fixture.vault.probe.lock().unwrap().as_ref().unwrap().1,
        Some(true),
        "backup existed before the first keychain access"
    );
    #[cfg(unix)]
    {
        assert_owner_only(&backup);
        assert_owner_only(&fixture.env.backup_root);
    }
}

#[test]
fn 데이터_폴더가_따로_있으면_아이콘도_백업된다() {
    // given
    let fixture = fixture(true);

    // when
    let report = run(&fixture).unwrap();

    // then
    let backup = PathBuf::from(&report.backup_dir);
    assert_eq!(
        fs::read(backup.join("data/connection-icons/icon.png")).unwrap(),
        vec![0x89, 0x50, 0x4e, 0x47]
    );
    assert!(fixture
        .env
        .dest_data
        .join("connection-icons/icon.png")
        .exists());
    assert!(!fixture.env.dest_config.join("connection-icons").exists());
    #[cfg(unix)]
    assert_owner_only(&backup);
}

#[test]
fn 백업은_실패한_가져오기_뒤에도_남는다() {
    // given
    let fixture = fixture(false);
    *fixture.vault.sets_allowed.lock().unwrap() = Some(0);

    // when
    let error = run(&fixture).unwrap_err();

    // then
    assert_eq!(error.code, SECRET_FAILED);
    let backups: Vec<_> = fs::read_dir(&fixture.env.backup_root)
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].path().join("profile/connections.json").exists());
}

// --- atomic publish ---------------------------------------------------------

#[test]
fn 키체인_쓰기가_중간에_실패하면_대상도_키체인_항목도_남지_않는다() {
    // given
    let fixture = fixture(true);
    *fixture.vault.sets_allowed.lock().unwrap() = Some(2);

    // when
    let error = run(&fixture).unwrap_err();

    // then
    assert_eq!(error.code, SECRET_FAILED);
    assert_clean_failure(&fixture);
}

#[test]
fn 읽어서_확인한_값이_다르면_게시하지_않고_만든_항목을_지운다() {
    // given
    let fixture = fixture(true);
    *fixture.vault.corrupt_stores.lock().unwrap() = true;

    // when
    let error = run(&fixture).unwrap_err();

    // then
    assert_eq!(error.code, SECRET_FAILED);
    assert_clean_failure(&fixture);
}

#[test]
fn 키체인에_쓴_뒤_게시가_실패해도_만든_항목을_모두_되돌린다() {
    // given
    let mut fixture = fixture(true);
    let blocker = fixture.root.path().join("not-a-directory");
    fs::write(&blocker, b"file").unwrap();
    fixture.env.dest_plugins = blocker.join("plugins");

    // when
    let error = run(&fixture).unwrap_err();

    // then
    assert_eq!(error.code, PUBLISH_FAILED);
    assert!(
        fixture.vault.writes(FORK) > 0,
        "the failure came after keychain writes"
    );
    assert_clean_failure(&fixture);
}

#[test]
fn 읽을_수_없는_공식_키체인이면_아무것도_쓰지_않는다() {
    // given
    struct Broken(MemoryVault);
    impl SecretPort for Broken {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>, VaultFault> {
            if service == OFFICIAL {
                return Err(VaultFault);
            }
            self.0.get(service, account)
        }
        fn set(&self, s: &str, a: &str, v: &str) -> Result<(), VaultFault> {
            self.0.set(s, a, v)
        }
        fn delete(&self, s: &str, a: &str) -> Result<(), VaultFault> {
            self.0.delete(s, a)
        }
    }
    let fixture = fixture(true);
    let broken = Broken(MemoryVault::default());

    // when
    let error = run_import(&fixture.env, &broken).unwrap_err();

    // then
    assert_eq!(error.code, SECRET_FAILED);
    assert_eq!(broken.0.writes(FORK), 0);
    assert!(!fixture.env.dest_config.exists());
}

#[test]
fn 데이터_폴더가_프로필과_같아도_실패하면_대상에_백업_말고는_남지_않는다() {
    // given
    let fixture = fixture(false);
    *fixture.vault.sets_allowed.lock().unwrap() = Some(1);

    // when
    let error = run(&fixture).unwrap_err();

    // then
    assert_eq!(error.code, SECRET_FAILED);
    let names: Vec<String> = fs::read_dir(&fixture.env.dest_config)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![BACKUP_DIR_NAME.to_owned()]);
    assert!(!holds_user_data(&fixture.env.dest_config).unwrap());
    assert!(fixture.vault.snapshot(FORK).is_empty());
    assert!(staging_leftovers(fixture.env.dest_config.parent().unwrap()).is_empty());
}

// --- preservation -----------------------------------------------------------

#[test]
fn 연결과_그룹과_태그와_저장_쿼리와_노트북과_테마와_설정이_그대로_옮겨진다() {
    // given
    let fixture = fixture(false);
    let source_before = tree(&fixture.env.source_config);
    let source_file = read_json(&fixture.env.source_config.join("connections.json"));

    // when
    let report = run(&fixture).unwrap();

    // then
    let dest = &fixture.env.dest_config;
    let imported = read_json(&dest.join("connections.json"));
    assert_eq!(imported["future_top"], source_file["future_top"]);
    assert_eq!(imported["groups"], source_file["groups"]);
    assert_eq!(imported["tags"], source_file["tags"]);
    for id in ["c1", "c2", "c3", "c4"] {
        let (old, new) = (connection(&source_file, id), connection(&imported, id));
        assert_eq!(new["id"], old["id"]);
        assert_eq!(new["name"], old["name"]);
        assert_eq!(new["group_id"], old["group_id"]);
        assert_eq!(new["tag_ids"], old["tag_ids"]);
        assert_eq!(new["environment"], old["environment"]);
        for key in [
            "driver", "host", "port", "username", "database", "ssl_mode", "extra",
        ] {
            assert_eq!(new["params"][key], old["params"][key], "{id}.{key}");
        }
    }
    assert_eq!(
        connection(&imported, "c1")["future_connection"],
        json!("retained")
    );
    assert_eq!(
        connection(&imported, "c1")["params"]["database"],
        json!(["alpha", "beta"])
    );
    let imported_files = tree(dest);
    for name in [
        "config.json",
        "saved_queries/meta.json",
        "saved_queries/q1.sql",
        "saved_queries/q2.sql",
        "notebooks/c1/n1.tabularis-notebook",
        "notebooks/c2/n2.tabularis-notebook",
        "themes/custom.json",
        "preferences/c1/preferences.json",
        "connection-icons/icon.png",
    ] {
        assert_eq!(imported_files.get(name), source_before.get(name), "{name}");
    }
    for runtime in [
        "tabularis.alive",
        "storage-location.json",
        "connections.json.lock",
    ] {
        assert!(!imported_files.contains_key(runtime), "{runtime}");
    }
    assert_eq!(
        report.counts,
        ProfileCounts {
            connections: 4,
            groups: 2,
            saved_queries: 2,
            notebooks: 2,
            themes: 2,
            plugin_packages: 3,
        }
    );
    assert!(staging_leftovers(dest.parent().unwrap()).is_empty());
}

#[test]
fn 공식_플러그인_패키지가_포크_플러그인_폴더로_복사되고_기존_것은_덮어쓰지_않는다() {
    // given
    let fixture = fixture(false);
    write(
        &fixture.env.dest_plugins.join("drivers/other/.tabularium"),
        b"{\"name\":\"other\",\"fork\":true}",
    );
    let official_before = tree(&fixture.env.source_plugins[0]);

    // when
    let report = run(&fixture).unwrap();

    // then
    let plugins = &fixture.env.dest_plugins;
    assert!(plugins.join("drivers/postgresql-plus/.tabularium").exists());
    assert_eq!(
        fs::read(plugins.join("drivers/postgresql-plus/plugin-bin")).unwrap(),
        b"binary"
    );
    assert!(plugins.join("flat-bundle/.tabularium").exists());
    assert!(plugins.join("themes/ember/.tabularium").exists());
    assert!(!plugins.join(".tmp-partial").exists());
    assert_eq!(
        fs::read(plugins.join("drivers/other/.tabularium")).unwrap(),
        b"{\"name\":\"other\",\"fork\":true}"
    );
    assert_eq!(report.plugin_packages_skipped, vec!["other".to_owned()]);
    assert_eq!(report.plugin_packages_copied.len(), 3);
    assert_eq!(tree(&fixture.env.source_plugins[0]), official_before);
}

#[cfg(unix)]
#[test]
fn 복사한_플러그인_실행_파일은_실행_권한을_유지한다() {
    use std::os::unix::fs::PermissionsExt;

    // given
    let fixture = fixture(false);
    let binary = fixture.env.source_plugins[0].join("drivers/postgresql-plus/plugin-bin");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

    // when
    run(&fixture).unwrap();

    // then
    let copied = fixture
        .env
        .dest_plugins
        .join("drivers/postgresql-plus/plugin-bin");
    assert_eq!(
        fs::metadata(copied).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[cfg(unix)]
#[test]
fn 심볼릭_링크는_따라가지_않고_건너뛴_수를_알린다() {
    // given
    let fixture = fixture(false);
    let outside = fixture.root.path().join("outside-secret.txt");
    fs::write(&outside, b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, fixture.env.source_config.join("linked.txt")).unwrap();

    // when
    let report = run(&fixture).unwrap();

    // then
    assert_eq!(report.skipped_links, 1);
    assert!(!fixture.env.dest_config.join("linked.txt").exists());
    assert!(tree(Path::new(&report.backup_dir))
        .keys()
        .all(|name| !name.contains("linked")));
}

// --- secrets ----------------------------------------------------------------

#[test]
fn 인라인_비밀번호와_키체인_비밀번호는_포크_키체인에만_남고_파일에서는_지워진다() {
    // given
    let fixture = fixture(false);

    // when
    let report = run(&fixture).unwrap();

    // then
    let fork = fixture.vault.snapshot(FORK);
    assert_eq!(fork.get("c1:db").map(String::as_str), Some(PG_KEYCHAIN));
    assert_eq!(fork.get("c2:db").map(String::as_str), Some(INLINE_DB));
    assert_eq!(fork.get("c2:ssh").map(String::as_str), Some(INLINE_SSH));
    assert_eq!(
        fork.get("c2:ssh_passphrase").map(String::as_str),
        Some(INLINE_PASSPHRASE)
    );
    assert_eq!(
        fork.get("c3:connection_uri").map(String::as_str),
        Some(URI_SECRET)
    );
    assert_eq!(
        fork.get("s1:ssh").map(String::as_str),
        Some(SSH_ENTRY_INLINE)
    );
    assert_eq!(
        fork.get("s2:ssh").map(String::as_str),
        Some(SSH_ENTRY_KEYCHAIN)
    );
    assert_eq!(fork.get("ai_key:openai").map(String::as_str), Some(AI_KEY));
    assert!(!fork.values().any(|value| value == STALE_DB));
    assert_eq!(report.secrets_stored, fork.len());

    let dest = &fixture.env.dest_config;
    let backup_dir = PathBuf::from(&report.backup_dir);
    for (name, bytes) in tree(dest) {
        if Path::new(&name).starts_with(BACKUP_DIR_NAME) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for secret in ALL_SECRETS {
            assert!(!text.contains(secret), "{name} still holds a secret");
        }
    }
    assert!(backup_dir.starts_with(dest.join(BACKUP_DIR_NAME)));
    let imported = read_json(&dest.join("connections.json"));
    let c2 = connection(&imported, "c2");
    assert_eq!(c2["params"]["save_in_keychain"], json!(true));
    assert!(c2["params"]["password"].is_null());
    assert!(c2["params"]["ssh_password"].is_null());
    assert!(c2["params"]["ssh_key_passphrase"].is_null());
    assert_eq!(c2["params"]["ssh_host"], json!("bastion.example.test"));
    assert_eq!(
        connection(&imported, "c3")["params"]["connection_uri_in_keychain"],
        json!(true)
    );
    let ssh = read_json(&dest.join("ssh_connections.json"));
    assert_eq!(ssh[0]["save_in_keychain"], json!(true));
    assert!(ssh[0].get("password").is_none());
    assert_eq!(ssh[0]["future_ssh"], json!(1));
    assert_eq!(ssh[1]["id"], json!("s2"));
}

#[test]
fn 비밀을_지운_연결만_리비전이_오른다() {
    // given
    let fixture = fixture(false);

    // when
    run(&fixture).unwrap();

    // then
    let imported = read_json(&fixture.env.dest_config.join("connections.json"));
    assert_eq!(connection(&imported, "c2")["revision"], json!(2));
    for id in ["c1", "c4"] {
        assert!(connection(&imported, id).get("revision").is_none(), "{id}");
    }
}

#[test]
fn 공식_파일과_공식_키체인은_바이트_단위로_그대로이고_쓰기_호출도_없다() {
    // given
    let fixture = fixture(false);
    let files_before = tree(&fixture.env.source_config);
    let plugins_before = tree(&fixture.env.source_plugins[0]);
    let keychain_before = fixture.vault.snapshot(OFFICIAL);

    // when
    run(&fixture).unwrap();

    // then
    assert_eq!(tree(&fixture.env.source_config), files_before);
    assert_eq!(tree(&fixture.env.source_plugins[0]), plugins_before);
    assert_eq!(fixture.vault.snapshot(OFFICIAL), keychain_before);
    assert_eq!(fixture.vault.writes(OFFICIAL), 0);
    assert!(fixture.vault.writes(FORK) > 0);
}

#[test]
fn 실패한_가져오기도_공식_데이터를_바꾸지_않는다() {
    // given
    let fixture = fixture(false);
    *fixture.vault.sets_allowed.lock().unwrap() = Some(1);
    let files_before = tree(&fixture.env.source_config);
    let keychain_before = fixture.vault.snapshot(OFFICIAL);

    // when
    let error = run(&fixture).unwrap_err();

    // then
    assert_eq!(error.code, SECRET_FAILED);
    assert_eq!(tree(&fixture.env.source_config), files_before);
    assert_eq!(fixture.vault.snapshot(OFFICIAL), keychain_before);
    assert_eq!(fixture.vault.writes(OFFICIAL), 0);
}

#[test]
fn 포크_키체인에_이미_같은_값이_있으면_다시_쓰지_않고_실패해도_지우지_않는다() {
    // given
    let fixture = fixture(false);
    fixture.vault.put(FORK, "c1:db", PG_KEYCHAIN);
    fixture
        .vault
        .put(FORK, "ai_key:openai", "previous-fork-value");
    *fixture.vault.sets_allowed.lock().unwrap() = Some(1);

    // when
    let error = run(&fixture).unwrap_err();

    // then
    assert_eq!(error.code, SECRET_FAILED);
    assert_eq!(
        fixture.vault.value(FORK, "c1:db").as_deref(),
        Some(PG_KEYCHAIN)
    );
    assert_eq!(
        fixture.vault.value(FORK, "ai_key:openai").as_deref(),
        Some("previous-fork-value")
    );
    assert_eq!(fixture.vault.snapshot(FORK).len(), 2);
}

#[test]
fn 오류와_보고서와_상태에는_비밀이_들어가지_않는다() {
    // given
    let failing = fixture(false);
    *failing.vault.sets_allowed.lock().unwrap() = Some(3);
    let refused = fixture(false);
    write(&refused.env.dest_config.join("config.json"), b"{}");
    let succeeding = fixture(false);

    // when
    let mut rendered = vec![
        run(&failing).unwrap_err().to_string(),
        run(&refused).unwrap_err().to_string(),
    ];
    let report = run(&succeeding).unwrap();
    rendered.push(serde_json::to_string(&report).unwrap());
    rendered.push(serde_json::to_string(&import_status(&succeeding.env)).unwrap());
    rendered.push(format!("{:?}", VaultFault));

    // then
    for text in rendered {
        for secret in ALL_SECRETS {
            assert!(!text.contains(secret), "{text}");
        }
    }
}

#[test]
fn 비밀_값은_디버그_출력에_드러나지_않는다() {
    // given
    let secret = Secret::new(PG_KEYCHAIN.as_bytes().to_vec()).unwrap();

    // when
    let rendered = format!("{secret:?}");

    // then
    assert!(!rendered.contains(PG_KEYCHAIN));
}

#[test]
fn 연결이_없는_프로필도_가져온다() {
    // given
    let fixture = fixture(false);
    fs::remove_file(fixture.env.source_config.join("connections.json")).unwrap();
    fs::remove_file(fixture.env.source_config.join("ssh_connections.json")).unwrap();

    // when
    let report = run(&fixture).unwrap();

    // then
    assert_eq!(report.counts.connections, 0);
    assert!(fixture.env.dest_config.join("config.json").exists());
    assert!(!fixture.env.dest_config.join("connections.json").exists());
}
