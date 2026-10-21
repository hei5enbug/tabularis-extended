use super::*;
use serde_json::json;
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("tabularis-platform-cas-{}", uuid::Uuid::new_v4())))
    }
    fn path(&self) -> PathBuf {
        self.0.join("connections.json")
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn raw() -> Value {
    json!({"groups":[],"tags":[],"connections":[{"id":"fixture","name":"original","params":{"driver":"postgresql","host":"localhost","database":"fixture","password":"synthetic-private","save_in_keychain":false,"future_param":{"v":7}},"group_id":"group-a","tag_ids":["tag-a"],"appearance":{"accentColor":"#123456"},"future_connection":{"v":8}}],"future_top":{"v":9}})
}
fn seed(directory: &Directory) {
    service_write_atomic(&directory.path(), &serde_json::to_vec(&raw()).unwrap()).unwrap();
}

fn publish_new_unknown_record(path: &Path) -> Value {
    transact_connections(path, |file, raw| {
        let mut connection = file.connections[0].clone();
        connection.id = "new-record".into();
        let mut record = serde_json::to_value(&connection).unwrap();
        record["service_v1"] = json!({"version":1,"auth_revision":"synthetic-revision"});
        record["future_new"] = json!({"retained":true});
        record["params"]["future_new_param"] = json!({"retained":7});
        record["params"]["connectionUri"] = "mongodb://synthetic-secret".into();
        raw["connections"].as_array_mut().unwrap().push(record);
        file.connections.push(connection);
        Ok(())
    })
    .unwrap();
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
#[test]
fn 새_raw_record의_미지의_필드는_보존하고_비밀_uri_alias는_복원하지_않는다() {
    // given
    let directory = Directory::new();
    seed(&directory);
    // when
    let actual = publish_new_unknown_record(&directory.path());
    // then
    assert_eq!(actual["connections"][1]["service_v1"]["version"], 1);
    assert_eq!(actual["connections"][1]["future_new"]["retained"], true);
    assert_eq!(
        actual["connections"][1]["params"]["future_new_param"]["retained"],
        7
    );
    assert!(actual["connections"][1]["params"]
        .get("connectionUri")
        .is_none());
    assert_eq!(
        actual["connections"][0]["future_connection"],
        json!({"v":8})
    );
}

#[test]
fn 구형_배열과_객체의_원본_revision은_직렬화에_노출되지_않는다() {
    // given
    let object = serde_json::to_string(&raw()).unwrap();
    let array = serde_json::to_string(&raw()["connections"]).unwrap();
    // when
    let actual = [&object, &array].map(|source| {
        let parsed = parse_connections_file(source).unwrap();
        (
            parsed.source_revision.clone(),
            serde_json::to_value(parsed).unwrap(),
            digest(source.as_bytes()),
        )
    });
    // then
    assert!(actual
        .iter()
        .all(|(revision, value, hash)| revision.as_ref() == Some(hash)
            && value.get("source_revision").is_none()));
}

#[test]
fn 같은_revision으로_읽은_두_writer중_나중_저장은_충돌하고_첫_결과를_보존한다() {
    // given
    let directory = Directory::new();
    seed(&directory);
    let mut first = load_connections_file(&directory.path()).unwrap();
    let mut second = first.clone();
    first.connections[0].name = "first".into();
    second.connections[0].name = "second".into();
    // when
    let actual = save_two(&directory.path(), &first, &second);
    // then
    assert!(actual.0.is_ok());
    assert_eq!(actual.1.unwrap_err(), "VERSION_CONFLICT");
    assert_eq!(actual.2.connections[0].name, "first");
}
fn save_two(
    path: &Path,
    first: &ConnectionsFile,
    second: &ConnectionsFile,
) -> (Result<(), String>, Result<(), String>, ConnectionsFile) {
    let one = save_connections_file(path, first);
    let two = save_connections_file(path, second);
    (one, two, load_connections_file(path).unwrap())
}

#[test]
fn 연결_수정은_같은_잠금에서_미지의_필드와_조직_정보를_보존한다() {
    // given
    let directory = Directory::new();
    seed(&directory);
    let mut file = load_connections_file(&directory.path()).unwrap();
    file.connections[0].name = "changed".into();
    // when
    let actual = save_and_read_raw(&directory.path(), &file);
    // then
    assert_eq!(actual["future_top"], raw()["future_top"]);
    assert_eq!(
        actual["connections"][0]["future_connection"],
        raw()["connections"][0]["future_connection"]
    );
    assert_eq!(
        actual["connections"][0]["params"]["future_param"],
        raw()["connections"][0]["params"]["future_param"]
    );
    assert_eq!(actual["connections"][0]["group_id"], "group-a");
    assert_eq!(actual["connections"][0]["tag_ids"], json!(["tag-a"]));
    assert_eq!(
        actual["connections"][0]["appearance"]["accentColor"],
        "#123456"
    );
    assert_eq!(actual["connections"][0]["name"], "changed");
}
fn save_and_read_raw(path: &Path, file: &ConnectionsFile) -> Value {
    save_connections_file(path, file).unwrap();
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn 잘못된_기존_json은_기본값으로_덮어쓰지_않는다() {
    // given
    let directory = Directory::new();
    service_write_atomic(&directory.path(), b"{broken").unwrap();
    // when
    let actual = reject_invalid_file(&directory.path());
    // then
    assert!(actual.0.is_err());
    assert!(actual.1.is_err());
    assert_eq!(actual.2, b"{broken");
}
fn reject_invalid_file(path: &Path) -> (Result<(), String>, Result<(), String>, Vec<u8>) {
    (
        save_connections(path, &[]),
        save_connections_file(path, &ConnectionsFile::default()),
        fs::read(path).unwrap(),
    )
}

#[test]
fn 읽기_상한을_넘는_연결_파일은_거부한다() {
    // given
    let directory = Directory::new();
    service_write_atomic(
        &directory.path(),
        &vec![b' '; MAX_CONNECTION_BYTES as usize + 1],
    )
    .unwrap();
    // when
    let actual = load_connections_file(&directory.path());
    // then
    assert!(actual.is_err());
    assert_eq!(
        fs::metadata(directory.path()).unwrap().len(),
        MAX_CONNECTION_BYTES + 1
    );
}

#[cfg(unix)]
#[test]
fn 연결과_잠금_심볼릭_링크는_대상을_읽거나_변경하지_않는다() {
    // given
    let directory = Directory::new();
    seed(&directory);
    let target = directory.0.join("target");
    fs::write(&target, b"private-target").unwrap();
    let link = directory.0.join("linked.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let lock_path = directory.0.join("connections.json.lock");
    std::os::unix::fs::symlink(&target, &lock_path).unwrap();
    // when
    let actual = read_linked_paths(&link, &directory.path());
    // then
    assert!(actual.0.is_err());
    assert!(actual.1.is_err());
    assert_eq!(fs::read(target).unwrap(), b"private-target");
}
#[cfg(unix)]
fn read_linked_paths(
    link: &Path,
    path: &Path,
) -> (
    Result<ConnectionsFile, String>,
    Result<ConnectionsFile, String>,
) {
    (load_connections_file(link), load_connections_file(path))
}

#[cfg(unix)]
#[test]
fn 새_연결_파일과_잠금은_현재_사용자만_읽고_프로필은_비공개다() {
    // given
    let directory = Directory::new();
    // when
    let actual = publish_and_modes(&directory);
    // then
    assert_eq!(actual, (0o700, 0o600, 0o600));
}
#[cfg(unix)]
fn publish_and_modes(directory: &Directory) -> (u32, u32, u32) {
    use std::os::unix::fs::MetadataExt;
    save_connections_file(&directory.path(), &ConnectionsFile::default()).unwrap();
    (
        fs::metadata(&directory.0).unwrap().mode() & 0o777,
        fs::metadata(directory.path()).unwrap().mode() & 0o777,
        fs::metadata(directory.0.join("connections.json.lock"))
            .unwrap()
            .mode()
            & 0o777,
    )
}

#[test]
fn 두_실제_프로세스의_동시_cas는_한_게시만_허용한다() {
    // given
    let directory = Directory::new();
    seed(&directory);
    // when
    let actual = run_cas_children(&directory);
    // then
    assert_eq!(actual.0, 1);
    assert_eq!(actual.1, 1);
    assert!(matches!(
        actual.2.connections[0].name.as_str(),
        "child-0" | "child-1"
    ));
    assert_eq!(actual.2.connections.len(), 1);
}
fn run_cas_children(directory: &Directory) -> (usize, usize, ConnectionsFile) {
    let executable = std::env::current_exe().unwrap();
    let mut children: Vec<_> = (0..2)
        .map(|index| {
            Command::new(&executable)
                .args([
                    "--exact",
                    "persistence::service_platform_tests::cas_자식_fixture",
                    "--ignored",
                    "--nocapture",
                ])
                .env("TABULARIS_CAS_TEST_PROFILE", &directory.0)
                .env("TABULARIS_CAS_TEST_INDEX", index.to_string())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let started = Instant::now();
    while !(0..2).all(|index| directory.0.join(format!("ready-{index}")).exists()) {
        if started.elapsed() > Duration::from_secs(15) {
            for child in &mut children {
                let _ = child.kill();
            }
            panic!("Both owned child fixtures must load before publication");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(directory.0.join("go"), b"go").unwrap();
    let outputs: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    let mut won = 0;
    let mut conflict = 0;
    for output in outputs {
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.status.success(),
            "Owned child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            text.contains("H3B1_CAS_CHILD_COMPLETED"),
            "Owned child test must actually run"
        );
        won += usize::from(text.contains("H3B1_CAS_CHILD_COMPLETED won"));
        conflict += usize::from(text.contains("H3B1_CAS_CHILD_COMPLETED conflict"));
    }
    (
        won,
        conflict,
        load_connections_file(&directory.path()).unwrap(),
    )
}

#[test]
#[ignore = "Owned child-process fixture"]
fn cas_자식_fixture() {
    // given
    let profile = PathBuf::from(
        std::env::var_os("TABULARIS_CAS_TEST_PROFILE").expect("Owned profile required"),
    );
    let index = std::env::var("TABULARIS_CAS_TEST_INDEX").unwrap();
    let path = profile.join("connections.json");
    let mut loaded = load_connections_file(&path).unwrap();
    loaded.connections[0].name = format!("child-{index}");
    fs::write(profile.join(format!("ready-{index}")), b"ready").unwrap();
    let started = Instant::now();
    while !profile.join("go").exists() {
        assert!(started.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(10));
    }
    // when
    let actual = save_child(&path, &loaded);
    // then
    assert!(
        actual.is_ok() || actual.as_ref().err().map(String::as_str) == Some("VERSION_CONFLICT")
    );
}
fn save_child(path: &Path, loaded: &ConnectionsFile) -> Result<(), String> {
    let result = save_connections_file(path, loaded);
    println!(
        "H3B1_CAS_CHILD_COMPLETED {}",
        if result.is_ok() { "won" } else { "conflict" }
    );
    result
}
