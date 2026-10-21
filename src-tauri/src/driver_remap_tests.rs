use super::*;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

struct Fixture {
    _dir: TempDir,
    connections: PathBuf,
    plugins: PathBuf,
}

fn entra_params() -> Value {
    json!({
        "driver": "sqlserver-entra",
        "host": "example.database.windows.net",
        "port": 1433,
        "username": "  user@example.test  ",
        "database": "main",
        "ssl_mode": "verify-full",
        "save_in_keychain": true,
        "extra": {
            "auth_mode": "entra_user",
            "auth_source": " azure_cli ",
            "tenant_id": " tenant-1 ",
            "entra_object_id": "object-1 ",
            "azure_cli_path": "/opt/azure/bin/az",
            "custom_setting": "kept"
        }
    })
}

fn entra_with(change: impl FnOnce(&mut Value)) -> Value {
    let mut params = entra_params();
    change(&mut params);
    params
}

fn connection(id: &str, params: Value) -> Value {
    json!({"id": id, "name": format!("name {id}"), "future_field": [id], "params": params})
}

fn plain(driver: &str) -> Value {
    json!({
        "driver": driver,
        "host": "db.example.test",
        "port": 5432,
        "username": "user",
        "database": ["alpha", "beta"],
        "ssl_mode": "require",
        "save_in_keychain": true,
        "extra": {"integrated_auth": "false", "keep": "yes"}
    })
}

fn package(plugins: &Path, id: &str, executable: bool) {
    let dir = plugins.join("drivers").join(id);
    fs::create_dir_all(&dir).unwrap();
    let manifest = json!({
        "id": id,
        "name": id,
        "version": "1.0.0",
        "description": "fixture",
        "executable": "plugin-bin"
    });
    fs::write(dir.join(".tabularium"), manifest.to_string()).unwrap();
    if executable {
        fs::write(dir.join("plugin-bin"), b"binary").unwrap();
    }
}

fn fixture(connections: Vec<Value>) -> Fixture {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile").join("connections.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = json!({"future_top": {"keep": true}, "groups": [], "connections": connections});
    fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    let plugins = dir.path().join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    Fixture {
        connections: path,
        plugins,
        _dir: dir,
    }
}

fn read(fixture: &Fixture) -> Value {
    serde_json::from_slice(&fs::read(&fixture.connections).unwrap()).unwrap()
}

fn find(file: &Value, id: &str) -> Value {
    file["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|connection| connection["id"] == id)
        .unwrap()
        .clone()
}

fn all_connections() -> Vec<Value> {
    vec![
        connection("pg", plain("postgresql")),
        connection("ms", plain("sqlserver")),
        connection("entra", entra_params()),
        connection("my", plain("mysql")),
    ]
}

#[test]
fn 검증된_패키지가_없으면_연결_파일을_열지도_바꾸지도_않는다() {
    // given
    let fixture = fixture(all_connections());
    let before = fs::read(&fixture.connections).unwrap();

    // when
    let report = remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();

    // then
    assert_eq!(report, RemapReport::default());
    assert_eq!(fs::read(&fixture.connections).unwrap(), before);
    assert!(!fixture
        .connections
        .with_file_name("connections.json.lock")
        .exists());
}

#[test]
fn 실행_파일이_없거나_정체가_다른_패키지는_검증되지_않는다() {
    // given
    let fixture = fixture(all_connections());
    package(&fixture.plugins, POSTGRESQL_PLUS, false);
    let wrong = fixture.plugins.join("drivers").join(SQLSERVER_PLUS);
    fs::create_dir_all(&wrong).unwrap();
    fs::write(
        wrong.join(".tabularium"),
        json!({"name": "another-driver", "version": "1.0.0", "description": "x"}).to_string(),
    )
    .unwrap();
    let before = fs::read(&fixture.connections).unwrap();

    // when
    let report = remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();

    // then
    assert!(report.verified_packages.is_empty());
    assert!(!verified_driver_package(&fixture.plugins, POSTGRESQL_PLUS));
    assert!(!verified_driver_package(&fixture.plugins, SQLSERVER_PLUS));
    assert_eq!(fs::read(&fixture.connections).unwrap(), before);
}

#[test]
fn 실행_파일_경로가_패키지_밖을_가리키면_검증되지_않는다() {
    // given
    let fixture = fixture(Vec::new());
    let dir = fixture.plugins.join("drivers").join(POSTGRESQL_PLUS);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(".tabularium"),
        json!({"name": POSTGRESQL_PLUS, "version": "1.0.0", "description": "x", "executable": "../outside"})
            .to_string(),
    )
    .unwrap();
    fs::write(fixture.plugins.join("drivers").join("outside"), b"x").unwrap();

    // when
    let verified = verified_driver_package(&fixture.plugins, POSTGRESQL_PLUS);

    // then
    assert!(!verified);
}

#[test]
fn 포스트그레스_플러스만_검증되면_포스트그레스_연결만_옮긴다() {
    // given
    let fixture = fixture(all_connections());
    package(&fixture.plugins, POSTGRESQL_PLUS, true);

    // when
    let report = remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();

    // then
    let file = read(&fixture);
    assert_eq!(report.remapped, vec!["pg".to_owned()]);
    assert_eq!(report.verified_packages, vec![POSTGRESQL_PLUS.to_owned()]);
    assert_eq!(find(&file, "pg")["params"]["driver"], POSTGRESQL_PLUS);
    assert_eq!(find(&file, "ms")["params"]["driver"], "sqlserver");
    assert_eq!(find(&file, "entra")["params"]["driver"], "sqlserver-entra");
    assert_eq!(find(&file, "my")["params"]["driver"], "mysql");
}

#[test]
fn 옮겨도_아이디와_대상과_인증_설정과_알_수_없는_필드가_유지되고_리비전이_오른다() {
    // given
    let fixture = fixture(all_connections());
    package(&fixture.plugins, POSTGRESQL_PLUS, true);
    package(&fixture.plugins, SQLSERVER_PLUS, true);
    let before = read(&fixture);

    // when
    remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();

    // then
    let after = read(&fixture);
    assert_eq!(after["future_top"], json!({"keep": true}));
    for id in ["pg", "ms"] {
        let (old, new) = (find(&before, id), find(&after, id));
        assert_eq!(new["id"], old["id"]);
        assert_eq!(new["name"], old["name"]);
        assert_eq!(new["future_field"], old["future_field"]);
        assert_eq!(new["revision"], 2);
        for key in ["host", "port", "username", "database", "ssl_mode", "extra"] {
            assert_eq!(new["params"][key], old["params"][key], "{id}.{key}");
        }
    }
    assert_eq!(find(&after, "ms")["params"]["driver"], SQLSERVER_PLUS);
    assert!(find(&after, "my").get("revision").is_none());
}

#[test]
fn 엔트라_연결은_플러스_계약으로_변환되고_선택한_인증_방식이_유지된다() {
    // given
    let fixture = fixture(vec![connection("entra", entra_params())]);
    package(&fixture.plugins, SQLSERVER_PLUS, true);

    // when
    let report = remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();

    // then
    let converted = find(&read(&fixture), "entra");
    assert_eq!(report.remapped, vec!["entra".to_owned()]);
    assert!(report.unconverted.is_empty());
    let params = &converted["params"];
    assert_eq!(params["driver"], SQLSERVER_PLUS);
    assert_eq!(params["username"], "user@example.test");
    assert_eq!(params["host"], "example.database.windows.net");
    assert_eq!(params["extra"]["auth_mode"], "entra_user");
    assert_eq!(params["extra"]["auth_source"], "azure_cli");
    assert_eq!(params["extra"]["tenant_id"], "tenant-1");
    assert_eq!(params["extra"]["entra_object_id"], "object-1");
    assert_eq!(params["extra"]["azure_cli_path"], "/opt/azure/bin/az");
    assert_eq!(params["extra"]["custom_setting"], "kept");
    assert!(params.get("password").map_or(true, Value::is_null));
}

#[test]
fn 선택적_클라이언트_경로가_없는_엔트라_연결도_변환된다() {
    // given
    let params = entra_with(|params| {
        params["extra"]
            .as_object_mut()
            .unwrap()
            .remove("azure_cli_path");
    });
    let fixture = fixture(vec![connection("entra", params)]);
    package(&fixture.plugins, SQLSERVER_PLUS, true);

    // when
    remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();

    // then
    let converted = find(&read(&fixture), "entra");
    assert_eq!(converted["params"]["driver"], SQLSERVER_PLUS);
    assert!(converted["params"]["extra"].get("azure_cli_path").is_none());
}

#[test]
fn 인증_방식이_없는_레거시_엔트라_드라이버_연결은_드라이버만_바뀐다() {
    // given
    let params = json!({
        "driver": "sqlserver-entra",
        "host": "sql.example.test",
        "username": "sa",
        "database": "main",
        "save_in_keychain": true,
        "extra": {"keep": "yes"}
    });
    let fixture = fixture(vec![connection("sql", params)]);
    package(&fixture.plugins, SQLSERVER_PLUS, true);

    // when
    remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();

    // then
    let converted = find(&read(&fixture), "sql");
    assert_eq!(converted["params"]["driver"], SQLSERVER_PLUS);
    assert_eq!(converted["params"]["extra"], json!({"keep": "yes"}));
}

#[test]
fn 변환할_수_없는_엔트라_연결은_그대로_두고_아이디로만_보고한다() {
    // given
    let broken = vec![
        (
            "no-tenant",
            entra_with(|params| {
                params["extra"].as_object_mut().unwrap().remove("tenant_id");
            }),
        ),
        (
            "no-object",
            entra_with(|params| params["extra"]["entra_object_id"] = json!("  ")),
        ),
        (
            "no-user",
            entra_with(|params| params["username"] = Value::Null),
        ),
        (
            "bad-source",
            entra_with(|params| params["extra"]["auth_source"] = json!("device_code")),
        ),
        (
            "bad-mode",
            entra_with(|params| params["extra"]["auth_mode"] = json!("service_principal")),
        ),
        (
            "relative-cli",
            entra_with(|params| params["extra"]["azure_cli_path"] = json!("az")),
        ),
        (
            "with-password",
            entra_with(|params| {
                params["password"] = json!("inline-secret");
                params["save_in_keychain"] = json!(false);
            }),
        ),
        (
            "integrated",
            entra_with(|params| params["extra"]["integrated_auth"] = json!("true")),
        ),
        (
            "orphan-keys",
            entra_with(|params| {
                params["extra"].as_object_mut().unwrap().remove("auth_mode");
            }),
        ),
    ];
    let mut connections: Vec<Value> = broken
        .iter()
        .map(|(id, params)| connection(id, params.clone()))
        .collect();
    connections.push(connection("good", entra_params()));
    let fixture = fixture(connections);
    package(&fixture.plugins, SQLSERVER_PLUS, true);
    let before = read(&fixture);

    // when
    let report = remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();

    // then
    let after = read(&fixture);
    let expected: Vec<String> = broken.iter().map(|(id, _)| (*id).to_owned()).collect();
    assert_eq!(report.unconverted, expected);
    assert_eq!(report.remapped, vec!["good".to_owned()]);
    for (id, _) in &broken {
        let (old, new) = (find(&before, id), find(&after, id));
        for key in [
            "driver", "host", "port", "username", "password", "database", "ssl_mode", "extra",
        ] {
            let (before, after) = (&old["params"][key], &new["params"][key]);
            assert_eq!(after, before, "{id}.{key}");
        }
        assert_eq!(new.get("revision"), old.get("revision"), "{id}");
        assert_eq!(new["params"]["driver"], "sqlserver-entra");
    }
}

#[test]
fn 다시_실행해도_이미_옮긴_연결은_건드리지_않는다() {
    // given
    let fixture = fixture(all_connections());
    package(&fixture.plugins, POSTGRESQL_PLUS, true);
    package(&fixture.plugins, SQLSERVER_PLUS, true);
    remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();
    let once = fs::read(&fixture.connections).unwrap();

    // when
    let report = remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();

    // then
    assert!(report.remapped.is_empty());
    assert_eq!(fs::read(&fixture.connections).unwrap(), once);
}

#[test]
fn 연결_파일이_없으면_아무것도_만들지_않는다() {
    // given
    let fixture = fixture(Vec::new());
    package(&fixture.plugins, SQLSERVER_PLUS, true);
    fs::remove_file(&fixture.connections).unwrap();

    // when
    let report = remap_connections_in(&fixture.connections, &fixture.plugins).unwrap();

    // then
    assert!(report.remapped.is_empty());
    assert!(!fixture.connections.exists());
    assert!(!fixture
        .connections
        .with_file_name("connections.json.lock")
        .exists());
}
