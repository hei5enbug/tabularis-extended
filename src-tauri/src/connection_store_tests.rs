use super::*;
use crate::models::{ConnectionParams, DatabaseSelection};
use crate::persistence::{update_connections_file, FAIL_NEXT_PUBLISH};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Barrier, Mutex};
use tempfile::TempDir;

#[derive(Default)]
struct MemoryBackend {
    secrets: Mutex<HashMap<(String, SecretKind), String>>,
    sets_allowed: Mutex<Option<usize>>,
    failing_sets: Mutex<Vec<SecretKind>>,
    calls: Mutex<usize>,
}

impl MemoryBackend {
    fn with(entries: &[(&str, SecretKind, &str)]) -> Self {
        let backend = Self::default();
        for (id, kind, value) in entries {
            backend
                .secrets
                .lock()
                .unwrap()
                .insert((id.to_string(), *kind), value.to_string());
        }
        backend
    }
    fn value(&self, id: &str, kind: SecretKind) -> Option<String> {
        self.secrets
            .lock()
            .unwrap()
            .get(&(id.to_string(), kind))
            .cloned()
    }
    fn snapshot(&self) -> HashMap<(String, SecretKind), String> {
        self.secrets.lock().unwrap().clone()
    }
    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl SecretBackend for MemoryBackend {
    fn get(&self, id: &str, kind: SecretKind) -> Result<Option<String>, String> {
        *self.calls.lock().unwrap() += 1;
        Ok(self.value(id, kind))
    }
    fn set(&self, id: &str, kind: SecretKind, value: &str) -> Result<(), String> {
        *self.calls.lock().unwrap() += 1;
        if self.failing_sets.lock().unwrap().contains(&kind) {
            return Err("fictional keychain failure".into());
        }
        if let Some(allowed) = self.sets_allowed.lock().unwrap().as_mut() {
            if *allowed == 0 {
                return Err("fictional keychain failure".into());
            }
            *allowed -= 1;
        }
        self.secrets
            .lock()
            .unwrap()
            .insert((id.to_string(), kind), value.to_string());
        Ok(())
    }
    fn delete(&self, id: &str, kind: SecretKind) -> Result<(), String> {
        *self.calls.lock().unwrap() += 1;
        self.secrets.lock().unwrap().remove(&(id.to_string(), kind));
        Ok(())
    }
}

fn fixture() -> Value {
    json!({
        "future_top": {"keep": [1, 2]},
        "groups": [],
        "connections": [
            {
                "id": "c1",
                "name": "one",
                "future_connection": "retained",
                "params": {
                    "driver": "postgresql-plus",
                    "host": "db.example",
                    "port": 5432,
                    "username": "user",
                    "database": ["alpha", "beta"],
                    "ssl_mode": "verify-full",
                    "save_in_keychain": true,
                    "extra": {"region": "west", "mode": "x"},
                    "future_param": {"v": 1}
                }
            },
            {
                "id": "c2",
                "name": "two",
                "future_other": true,
                "params": {
                    "driver": "sqlserver-plus",
                    "host": "sql.example",
                    "database": "main",
                    "save_in_keychain": false,
                    "password": "inline-secret",
                    "future_param": "other"
                }
            }
        ]
    })
}

struct Profile {
    _dir: TempDir,
    path: std::path::PathBuf,
}

impl Profile {
    fn new(content: &Value) -> Self {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        std::fs::write(&path, serde_json::to_vec_pretty(content).unwrap()).unwrap();
        Self { _dir: dir, path }
    }
    fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.path).unwrap()
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes()).unwrap()
    }
    fn connection(&self, id: &str) -> Value {
        self.json()["connections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|connection| connection["id"] == id)
            .unwrap()
            .clone()
    }
}

fn without_nulls(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key.clone(), without_nulls(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_nulls).collect()),
        other => other.clone(),
    }
}

fn edit(id: &str, params: ConnectionParams, expected_revision: u64) -> ConnectionEdit {
    ConnectionEdit {
        id: id.into(),
        name: "renamed".into(),
        params,
        detect_json_in_text_columns: None,
        environment: None,
        expected_revision,
    }
}

fn params_of(profile: &Profile, id: &str) -> ConnectionParams {
    serde_json::from_value(profile.connection(id)["params"].clone()).unwrap()
}

#[test]
fn 리비전이_없거나_0인_파일은_1로_읽고_바뀌지_않은_연결에는_쓰지_않는다() {
    // given
    let mut content = fixture();
    content["connections"][1]["revision"] = json!(0);
    let profile = Profile::new(&content);
    // when
    let loaded = crate::persistence::load_connections_file(&profile.path).unwrap();
    update_connections_file(&profile.path, |file| {
        file.connections[0].name = "changed".into();
        Ok(())
    })
    .unwrap();
    // then
    assert_eq!(loaded.connections[0].revision, 1);
    assert_eq!(loaded.connections[1].revision, 1);
    assert_eq!(profile.connection("c1")["revision"], 2);
    assert_eq!(profile.connection("c2")["revision"], 0);
}

#[test]
fn 낡은_gui_갱신은_키체인과_파일을_바꾸지_않고_충돌로_거부된다() {
    // given
    let profile = Profile::new(&fixture());
    let backend = MemoryBackend::with(&[("c1", SecretKind::DbPassword, "old-secret")]);
    update_connection(
        &profile.path,
        &backend,
        edit("c1", params_of(&profile, "c1"), 1),
    )
    .unwrap();
    let bytes = profile.bytes();
    let secrets = backend.snapshot();
    let calls = backend.calls();
    let mut params = params_of(&profile, "c1");
    params.password = Some("new-secret".into());
    // when
    let actual = update_connection(&profile.path, &backend, edit("c1", params, 1));
    // then
    assert_eq!(actual.err().unwrap(), "CONNECTION_CONFLICT");
    assert_eq!(profile.bytes(), bytes);
    assert_eq!(backend.snapshot(), secrets);
    assert_eq!(backend.calls(), calls);
}

#[test]
fn 같은_리비전을_읽은_동시_갱신은_하나만_성공하고_나머지는_충돌한다() {
    // given
    let profile = Arc::new(Profile::new(&fixture()));
    let backend = Arc::new(MemoryBackend::default());
    let barrier = Arc::new(Barrier::new(8));
    // when
    let results: Vec<Result<(), String>> = (0..8)
        .map(|index| {
            let (profile, backend, barrier) = (profile.clone(), backend.clone(), barrier.clone());
            std::thread::spawn(move || {
                let mut params = params_of(&profile, "c1");
                params.host = Some(format!("host-{index}.example"));
                barrier.wait();
                update_connection(&profile.path, &*backend, edit("c1", params, 1)).map(|_| ())
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    // then
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .all(|error| error == "CONNECTION_CONFLICT"));
    assert_eq!(profile.connection("c1")["revision"], 2);
}

#[test]
fn 동시_작성자의_그룹_태그_연결_변경은_하나도_유실되지_않는다() {
    // given
    let profile = Arc::new(Profile::new(&fixture()));
    let barrier = Arc::new(Barrier::new(12));
    // when
    let handles: Vec<_> = (0..12)
        .map(|index| {
            let (profile, barrier) = (profile.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                update_connections_file(&profile.path, |file| {
                    match index % 3 {
                        0 => file.groups.push(crate::models::ConnectionGroup {
                            id: format!("g{index}"),
                            name: format!("group {index}"),
                            collapsed: false,
                            sort_order: index,
                            parent_id: None,
                        }),
                        1 => file.tags.push(crate::models::ConnectionTag {
                            id: format!("t{index}"),
                            name: format!("tag {index}"),
                            color: "#112233".into(),
                        }),
                        _ => {
                            let mut connection = file.connections[1].clone();
                            connection.id = format!("extra-{index}");
                            file.connections.push(connection);
                        }
                    }
                    Ok(())
                })
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    // then
    let actual = profile.json();
    assert_eq!(actual["groups"].as_array().unwrap().len(), 4);
    assert_eq!(actual["tags"].as_array().unwrap().len(), 4);
    assert_eq!(actual["connections"].as_array().unwrap().len(), 6);
    assert_eq!(actual["future_top"], fixture()["future_top"]);
}

#[test]
fn 모든_작성자는_미지의_필드와_다른_연결을_그대로_둔다() {
    // given
    let original = fixture();
    let writers: Vec<(&str, Box<dyn Fn(&Profile, &MemoryBackend)>)> = vec![
        (
            "update",
            Box::new(|profile, backend| {
                let mut params = params_of(profile, "c1");
                params.host = Some("changed.example".into());
                update_connection(&profile.path, backend, edit("c1", params, 1)).unwrap();
            }),
        ),
        (
            "password",
            Box::new(|profile, backend| {
                update_connection_password(&profile.path, backend, "c1", Some("pw"), 1).unwrap();
            }),
        ),
        (
            "group",
            Box::new(|profile, _| {
                update_connections_file(&profile.path, |file| {
                    file.groups.push(crate::models::ConnectionGroup {
                        id: "g".into(),
                        name: "g".into(),
                        collapsed: false,
                        sort_order: 0,
                        parent_id: None,
                    });
                    file.connections[0].group_id = Some("g".into());
                    Ok(())
                })
                .unwrap();
            }),
        ),
        (
            "create",
            Box::new(|profile, backend| {
                let draft = ConnectionDraft {
                    name: "new".into(),
                    params: params_of(profile, "c2"),
                    detect_json_in_text_columns: None,
                    environment: None,
                };
                create_connection(&profile.path, backend, draft).unwrap();
            }),
        ),
        (
            "delete",
            Box::new(|profile, backend| {
                delete_connection(&profile.path, backend, "c2").unwrap();
            }),
        ),
    ];
    for (name, write) in writers {
        let profile = Profile::new(&original);
        let backend = MemoryBackend::default();
        // when
        write(&profile, &backend);
        // then
        let actual = profile.json();
        assert_eq!(actual["future_top"], original["future_top"], "{name}");
        if name != "delete" {
            assert_eq!(
                without_nulls(&actual["connections"][1]),
                original["connections"][1],
                "{name}"
            );
        }
        if name == "create" {
            assert_eq!(
                without_nulls(&actual["connections"][0]),
                original["connections"][0],
                "{name}"
            );
        } else if name != "delete" {
            assert_eq!(
                actual["connections"][0]["future_connection"], "retained",
                "{name}"
            );
            assert_eq!(
                actual["connections"][0]["params"]["future_param"],
                json!({"v": 1}),
                "{name}"
            );
        }
    }
}

#[test]
fn 키체인_쓰기_실패는_파일을_바꾸지_않고_앞서_바뀐_비밀을_복원한다() {
    // given
    let profile = Profile::new(&fixture());
    let backend = MemoryBackend::with(&[
        ("c1", SecretKind::DbPassword, "previous-db"),
        ("c1", SecretKind::SshPassword, "previous-ssh"),
    ]);
    backend
        .failing_sets
        .lock()
        .unwrap()
        .push(SecretKind::SshPassword);
    let bytes = profile.bytes();
    let secrets = backend.snapshot();
    let mut params = params_of(&profile, "c1");
    params.password = Some("new-db".into());
    params.ssh_enabled = Some(true);
    params.ssh_password = Some("new-ssh".into());
    // when
    let actual = update_connection(&profile.path, &backend, edit("c1", params, 1));
    // then
    assert_eq!(actual.err().unwrap(), "fictional keychain failure");
    assert_eq!(profile.bytes(), bytes);
    assert_eq!(backend.snapshot(), secrets);
}

#[test]
fn 파일_쓰기_실패는_이전_비밀을_복원하고_파일은_그대로다() {
    // given
    let profile = Profile::new(&fixture());
    let backend = MemoryBackend::with(&[("c1", SecretKind::DbPassword, "previous-db")]);
    let bytes = profile.bytes();
    FAIL_NEXT_PUBLISH.with(|flag| flag.set(true));
    // when
    let actual = update_connection_password(&profile.path, &backend, "c1", Some("new-db"), 1);
    // then
    assert!(actual.is_err());
    assert_eq!(profile.bytes(), bytes);
    assert_eq!(
        backend.value("c1", SecretKind::DbPassword).as_deref(),
        Some("previous-db")
    );
}

#[test]
fn 파일_쓰기_실패_뒤_복원도_실패하면_연결_id만_담은_복구_필요_오류를_반환한다() {
    // given
    let profile = Profile::new(&fixture());
    let backend = MemoryBackend::with(&[("c1", SecretKind::DbPassword, "previous-db")]);
    *backend.sets_allowed.lock().unwrap() = Some(1);
    FAIL_NEXT_PUBLISH.with(|flag| flag.set(true));
    // when
    let actual =
        update_connection_password(&profile.path, &backend, "c1", Some("new-db"), 1).unwrap_err();
    // then
    assert_eq!(actual, "CONNECTION_RECOVERY_REQUIRED:c1");
    assert!(!actual.contains("db"));
}

#[test]
fn 새_연결의_파일_쓰기_실패는_방금_쓴_비밀을_지운다() {
    // given
    let profile = Profile::new(&fixture());
    let backend = MemoryBackend::default();
    let mut params = params_of(&profile, "c1");
    params.password = Some("new-db".into());
    let draft = ConnectionDraft {
        name: "new".into(),
        params,
        detect_json_in_text_columns: None,
        environment: None,
    };
    let bytes = profile.bytes();
    FAIL_NEXT_PUBLISH.with(|flag| flag.set(true));
    // when
    let actual = create_connection(&profile.path, &backend, draft);
    // then
    assert!(actual.is_err());
    assert_eq!(profile.bytes(), bytes);
    assert!(backend.snapshot().is_empty());
}

#[test]
fn 비밀번호만_바꾸면_ssl_모드_드라이버_데이터베이스_선택_extra를_그대로_둔다() {
    // given
    let profile = Profile::new(&fixture());
    let before = profile.connection("c1");
    let backend = MemoryBackend::default();
    // when
    let (connection, applied) =
        update_connection_password(&profile.path, &backend, "c1", Some("rotated"), 1).unwrap();
    // then
    let after = profile.connection("c1");
    assert_eq!(
        without_nulls(&after["params"]),
        without_nulls(&before["params"])
    );
    assert_eq!(after["params"]["ssl_mode"], "verify-full");
    assert_eq!(after["params"]["driver"], "postgresql-plus");
    assert_eq!(after["params"]["database"], json!(["alpha", "beta"]));
    assert_eq!(
        after["params"]["extra"],
        json!({"region": "west", "mode": "x"})
    );
    assert_eq!(after["revision"], 2);
    assert_eq!(connection.revision, 2);
    assert_eq!(
        backend.value("c1", SecretKind::DbPassword).as_deref(),
        Some("rotated")
    );
    assert_eq!(applied.len(), 1);
    assert!(matches!(
        connection.params.database,
        DatabaseSelection::Multiple(_)
    ));
}

#[test]
fn 낡은_리비전의_비밀번호_변경은_충돌로_거부된다() {
    // given
    let profile = Profile::new(&fixture());
    let backend = MemoryBackend::default();
    update_connection_password(&profile.path, &backend, "c1", Some("first"), 1).unwrap();
    let bytes = profile.bytes();
    // when
    let actual = update_connection_password(&profile.path, &backend, "c1", Some("second"), 1);
    // then
    assert_eq!(actual.err().unwrap(), "CONNECTION_CONFLICT");
    assert_eq!(profile.bytes(), bytes);
    assert_eq!(
        backend.value("c1", SecretKind::DbPassword).as_deref(),
        Some("first")
    );
}

#[test]
fn 연결_자체가_바뀐_경우에만_리비전이_오른다() {
    // given
    let profile = Profile::new(&fixture());
    // when
    update_connections_file(&profile.path, |file| {
        file.groups.push(crate::models::ConnectionGroup {
            id: "g".into(),
            name: "g".into(),
            collapsed: false,
            sort_order: 0,
            parent_id: None,
        });
        Ok(())
    })
    .unwrap();
    let after_group = profile.json();
    update_connections_file(&profile.path, |file| {
        file.connections[1].sort_order = Some(3);
        Ok(())
    })
    .unwrap();
    // then
    assert!(after_group["connections"][0].get("revision").is_none());
    assert!(after_group["connections"][1].get("revision").is_none());
    assert_eq!(profile.connection("c2")["revision"], 2);
    assert!(profile.connection("c1").get("revision").is_none());
}

#[test]
fn 삭제는_파일_쓰기가_실패하면_지운_비밀을_되살린다() {
    // given
    let profile = Profile::new(&fixture());
    let backend = MemoryBackend::with(&[("c1", SecretKind::DbPassword, "keep-me")]);
    let bytes = profile.bytes();
    FAIL_NEXT_PUBLISH.with(|flag| flag.set(true));
    // when
    let actual = delete_connection(&profile.path, &backend, "c1");
    // then
    assert!(actual.is_err());
    assert_eq!(profile.bytes(), bytes);
    assert_eq!(
        backend.value("c1", SecretKind::DbPassword).as_deref(),
        Some("keep-me")
    );
}
