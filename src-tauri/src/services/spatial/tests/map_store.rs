use super::super::{MapChange, MapStore, MapStoreLimits};
use super::support::*;
use crate::services::{
    contracts::*,
    jobs::{JobLease, JobManager},
};
use serde_json::json;
use tabularis_spatial_core::{BasemapChange, LayerState, MapState};
fn lease(directory: &Directory, input: &ServiceRequest) -> JobLease {
    JobManager::new("map-owner".into(), directory.0.join("store-jobs"))
        .unwrap()
        .queue(input, &caller(), true, true)
        .unwrap()
}
fn create(directory: &Directory, store: &MapStore, name: &str) -> String {
    let input = request(Operation::MapCreate, None, None, json!({"name":name}));
    let map = MapState::new(uuid::Uuid::new_v4().to_string(), name).unwrap();
    let id = map.map_id.clone();
    store
        .create(&input, &caller(), map, &lease(directory, &input))
        .unwrap();
    id
}
fn replay_create(
    directory: &Directory,
    store: &MapStore,
    input: &ServiceRequest,
) -> (String, String, ErrorCode) {
    let first = store
        .create(
            input,
            &caller(),
            MapState::new(uuid::Uuid::new_v4().to_string(), "fixture").unwrap(),
            &lease(directory, input),
        )
        .unwrap();
    let second = store
        .create(
            input,
            &caller(),
            MapState::new(uuid::Uuid::new_v4().to_string(), "fixture").unwrap(),
            &lease(directory, input),
        )
        .unwrap();
    let mut changed = input.clone();
    changed.input = json!({"name":"other"});
    let error = store
        .create(
            &changed,
            &caller(),
            MapState::new(uuid::Uuid::new_v4().to_string(), "other").unwrap(),
            &lease(directory, &changed),
        )
        .err()
        .unwrap()
        .code;
    (first.map.map_id, second.map.map_id, error)
}
#[test]
fn 생성_재생은_같은_지도를_반환하고_다른_본문은_중복_생성하지_않는다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let input = request(Operation::MapCreate, None, None, json!({"name":"fixture"}));
    // when
    let actual = replay_create(&directory, &store, &input);
    // then
    assert_eq!(actual.0, actual.1);
    assert_eq!(actual.2, ErrorCode::InvalidArgument);
    assert_eq!(store.list(&caller()).unwrap().len(), 1);
}
fn stale_cas(directory: &Directory, store: &MapStore, id: &str) -> (u64, ErrorCode) {
    let observed = store.get(id, &caller()).unwrap();
    let first = request(
        Operation::MapUpdate,
        Some(id),
        Some(0),
        json!({"name":"first"}),
    );
    let second = request(
        Operation::MapUpdate,
        Some(id),
        Some(0),
        json!({"name":"second"}),
    );
    let version = store
        .mutate(
            &first,
            &caller(),
            &observed,
            MapChange::Update {
                name: Some("first".into()),
                basemap: BasemapChange::Keep,
            },
            &lease(directory, &first),
        )
        .unwrap()
        .map
        .version;
    let error = store
        .mutate(
            &second,
            &caller(),
            &observed,
            MapChange::Update {
                name: Some("second".into()),
                basemap: BasemapChange::Keep,
            },
            &lease(directory, &second),
        )
        .err()
        .unwrap()
        .code;
    (version, error)
}
#[test]
fn 같은_경로의_오래된_지도_버전은_현재_파일을_덮어쓰지_않는다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let id = create(&directory, &store, "fixture");
    // when
    let actual = stale_cas(&directory, &store, &id);
    // then
    assert_eq!(actual, (1, ErrorCode::VersionConflict));
    assert_eq!(store.get(&id, &caller()).unwrap().map.name, "first");
}
fn cancelled_write(directory: &Directory, store: &MapStore, id: &str) -> ErrorCode {
    let observed = store.get(id, &caller()).unwrap();
    let input = request(
        Operation::MapUpdate,
        Some(id),
        Some(0),
        json!({"name":"cancelled"}),
    );
    let lease = lease(directory, &input);
    lease.signal().cancel();
    store
        .mutate(
            &input,
            &caller(),
            &observed,
            MapChange::Update {
                name: Some("cancelled".into()),
                basemap: BasemapChange::Keep,
            },
            &lease,
        )
        .err()
        .unwrap()
        .code
}
#[test]
fn 마지막_동기_게시_직전_취소는_파일과_세대를_바꾸지_않는다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let id = create(&directory, &store, "fixture");
    let before = std::fs::read(store.root().join(format!("{id}.json"))).unwrap();
    // when
    let actual = cancelled_write(&directory, &store, &id);
    // then
    assert_eq!(actual, ErrorCode::Cancelled);
    assert_eq!(
        std::fs::read(store.root().join(format!("{id}.json"))).unwrap(),
        before
    );
}
fn map_count_limit(directory: &Directory, store: &MapStore) -> ErrorCode {
    for _ in 0..128 {
        create(directory, store, "fixture");
    }
    let input = request(Operation::MapCreate, None, None, json!({"name":"overflow"}));
    store
        .create(
            &input,
            &caller(),
            MapState::new(uuid::Uuid::new_v4().to_string(), "overflow").unwrap(),
            &lease(directory, &input),
        )
        .err()
        .unwrap()
        .code
}
#[test]
fn 지도_개수는_프로필_전체에서_백이십팔개로_제한한다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    // when
    let actual = map_count_limit(&directory, &store);
    // then
    assert_eq!(actual, ErrorCode::ResourceLimit);
    assert_eq!(store.list(&caller()).unwrap().len(), 128);
}
fn layer_limit(directory: &Directory, store: &MapStore, id: &str) -> ErrorCode {
    for version in 0..33 {
        let observed = store.get(id, &caller()).unwrap();
        let input = request(
            Operation::MapLayerAdd,
            Some(id),
            Some(version),
            json!({"source":table(),"style":{}}),
        );
        let layer = LayerState::new(
            format!("layer-{version}"),
            "conn-a",
            serde_json::from_value(table()).unwrap(),
            Default::default(),
        )
        .unwrap();
        if let Err(error) = store.mutate(
            &input,
            &caller(),
            &observed,
            MapChange::Add {
                layer: Box::new(layer),
                binding: None,
            },
            &lease(directory, &input),
        ) {
            return error.code;
        }
    }
    panic!("missing layer limit")
}
#[test]
fn 레이어는_서른두개를_넘겨_게시할_수_없다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let id = create(&directory, &store, "fixture");
    // when
    let actual = layer_limit(&directory, &store, &id);
    // then
    assert_eq!(actual, ErrorCode::ResourceLimit);
    assert_eq!(store.get(&id, &caller()).unwrap().map.layers.len(), 32);
}
fn record_caps(directory: &Directory) -> Vec<ErrorCode> {
    let mut values = vec![];
    for (state_bytes, record_bytes, name) in [
        (128, 256 * 1024, "fixture"),
        (32 * 1024, 256, "fixture"),
        (32 * 1024, 256 * 1024, "name\n"),
    ] {
        let store = MapStore::with_limits(
            &directory.0.join(uuid::Uuid::new_v4().to_string()),
            MapStoreLimits {
                state_bytes,
                record_bytes,
                ..Default::default()
            },
        )
        .unwrap();
        let input = request(Operation::MapCreate, None, None, json!({"name":name}));
        let map = MapState::new(uuid::Uuid::new_v4().to_string(), name).unwrap();
        values.push(
            store
                .create(&input, &caller(), map, &lease(directory, &input))
                .err()
                .unwrap()
                .code,
        );
    }
    values
}
#[test]
fn 지도와_원장_바이트_제한과_제어문자_검증은_게시_전에_실행한다() {
    // given
    let directory = Directory::new();
    // when
    let actual = record_caps(&directory);
    // then
    assert_eq!(
        actual,
        [
            ErrorCode::ResourceLimit,
            ErrorCode::ResourceLimit,
            ErrorCode::InvalidArgument
        ]
    );
}
#[cfg(unix)]
#[test]
fn 지도_파일과_디렉터리는_소유자만_접근할_수_있다() {
    use std::os::unix::fs::PermissionsExt;
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let id = create(&directory, &store, "fixture");
    // when
    let actual = (
        std::fs::metadata(store.root())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        std::fs::metadata(store.root().join(format!("{id}.json")))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
    );
    // then
    assert_eq!(actual, (0o700, 0o600));
}
#[cfg(unix)]
#[test]
fn 지도_파일의_심볼릭_링크는_따라가지_않는다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let id = create(&directory, &store, "fixture");
    let target = directory.0.join("outside.json");
    let path = store.root().join(format!("{id}.json"));
    std::fs::rename(&path, &target).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    // when
    let actual = store.get(&id, &caller());
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::PermissionDenied);
}
#[test]
#[ignore = "Invoked by the owned two-process CAS regression."]
fn 프로세스_지도_cas_자식_진입점() {
    // given
    let directory = std::mem::ManuallyDrop::new(Directory(PathBuf::from(
        std::env::var("TABULARIS_MAP_TEST_PROFILE").unwrap(),
    )));
    let id = std::env::var("TABULARIS_MAP_TEST_ID").unwrap();
    let child = std::env::var("TABULARIS_MAP_TEST_CHILD").unwrap();
    let store = MapStore::new(&directory.0).unwrap();
    let observed = store.get(&id, &caller()).unwrap();
    let input = request(
        Operation::MapUpdate,
        Some(&id),
        Some(0),
        json!({"name":child}),
    );
    let lease = lease(&directory, &input);
    std::fs::write(directory.0.join(format!("ready-{child}")), b"ready").unwrap();
    // when
    let actual = child_cas(&directory, &store, &input, &observed, &lease, &child);
    // then
    assert!(actual);
}
use std::path::PathBuf;
fn child_cas(
    directory: &Directory,
    store: &MapStore,
    input: &ServiceRequest,
    observed: &super::super::StoredMap,
    lease: &JobLease,
    child: &str,
) -> bool {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !directory.0.join("go").exists() {
        if std::time::Instant::now() >= until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let result = store.mutate(
        input,
        &caller(),
        observed,
        MapChange::Update {
            name: Some(child.into()),
            basemap: BasemapChange::Keep,
        },
        lease,
    );
    let marker = match result {
        Ok(_) => "applied",
        Err(error) if error.code == ErrorCode::VersionConflict => "conflict",
        Err(_) => "unexpected",
    };
    std::fs::write(directory.0.join(format!("result-{child}")), marker).unwrap();
    println!("MAP_CAS_CHILD_COMPLETED");
    marker != "unexpected"
}
fn processes(directory: &Directory, id: &str) -> (Vec<bool>, Vec<String>, Vec<bool>) {
    let executable = std::env::current_exe().unwrap();
    let mut children = vec![];
    for child in ["one", "two"] {
        children.push(
            std::process::Command::new(&executable)
                .args([
                    "--ignored",
                    "--exact",
                    "service_spatial::tests::map_store::프로세스_지도_cas_자식_진입점",
                    "--nocapture",
                ])
                .env("TABULARIS_MAP_TEST_PROFILE", &directory.0)
                .env("TABULARIS_MAP_TEST_ID", id)
                .env("TABULARIS_MAP_TEST_CHILD", child)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while ["one", "two"]
        .iter()
        .any(|child| !directory.0.join(format!("ready-{child}")).exists())
    {
        assert!(std::time::Instant::now() < until, "child readiness missing");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    std::fs::write(directory.0.join("go"), b"go").unwrap();
    let outputs: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    let markers = ["one", "two"]
        .iter()
        .map(|child| std::fs::read_to_string(directory.0.join(format!("result-{child}"))).unwrap())
        .collect();
    (
        outputs
            .iter()
            .map(|output| output.status.success())
            .collect(),
        markers,
        outputs
            .iter()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout).contains("MAP_CAS_CHILD_COMPLETED")
            })
            .collect(),
    )
}
#[test]
fn 실제_두_프로세스가_같은_지도_버전을_저장하면_한번만_적용된다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let id = create(&directory, &store, "fixture");
    // when
    let actual = processes(&directory, &id);
    // then
    assert!(actual.0.iter().all(|value| *value));
    assert!(actual.2.iter().all(|value| *value));
    assert_eq!(
        actual
            .1
            .iter()
            .filter(|value| value.as_str() == "applied")
            .count(),
        1
    );
    assert_eq!(
        actual
            .1
            .iter()
            .filter(|value| value.as_str() == "conflict")
            .count(),
        1
    );
    assert_eq!(store.get(&id, &caller()).unwrap().map.version, 1);
}
fn bounded_ledger(directory: &Directory, store: &MapStore, id: &str) -> serde_json::Value {
    for version in 0..70 {
        let input = request(
            Operation::MapUpdate,
            Some(id),
            Some(version),
            json!({"name":format!("name-{version}")}),
        );
        let observed = store.get(id, &caller()).unwrap();
        store
            .mutate(
                &input,
                &caller(),
                &observed,
                MapChange::Update {
                    name: Some(format!("name-{version}")),
                    basemap: BasemapChange::Keep,
                },
                &lease(directory, &input),
            )
            .unwrap();
    }
    serde_json::from_slice(&std::fs::read(store.root().join(format!("{id}.json"))).unwrap())
        .unwrap()
}
#[test]
fn 원장은_최근_예순네개만_보존하고_과거_전체_지도를_저장하지_않는다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let id = create(&directory, &store, "fixture");
    // when
    let actual = bounded_ledger(&directory, &store, &id);
    // then
    assert_eq!(actual["ledger"].as_array().unwrap().len(), 64);
    assert!(actual["ledger"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry.get("state").is_none() && entry.get("map").is_none()));
    assert_eq!(actual["state"]["version"], 70);
    assert_eq!(actual["apply_generation"], 70);
    assert!(actual.get("create_entry").is_some());
}
fn selection_caps(
    directory: &Directory,
    store: &MapStore,
    id: &str,
    count: usize,
    width: usize,
) -> ErrorCode {
    let observed = store.get(id, &caller()).unwrap();
    let input = request(
        Operation::MapSelectionSet,
        Some(id),
        Some(observed.map.version),
        json!({"feature_refs":[]}),
    );
    let refs = (0..count)
        .map(|index| tabularis_spatial_core::FeatureRef {
            layer_id: "layer".into(),
            feature_id: format!("{index}{}", "x".repeat(width)),
        })
        .collect();
    store
        .mutate(
            &input,
            &caller(),
            &observed,
            MapChange::Selection(refs),
            &lease(directory, &input),
        )
        .err()
        .unwrap()
        .code
}
fn selection_bounds(
    directory: &Directory,
    store: &MapStore,
    first: &str,
    second: &str,
) -> (ErrorCode, ErrorCode) {
    (
        selection_caps(directory, store, first, 1001, 0),
        selection_caps(directory, store, second, 500, 80),
    )
}
fn map_with_layer(directory: &Directory, store: &MapStore) -> String {
    let id = create(directory, store, "fixture");
    let observed = store.get(&id, &caller()).unwrap();
    let input = request(
        Operation::MapLayerAdd,
        Some(&id),
        Some(0),
        json!({"source":table(),"style":{}}),
    );
    let layer = LayerState::new(
        "layer",
        "conn-a",
        serde_json::from_value(table()).unwrap(),
        Default::default(),
    )
    .unwrap();
    store
        .mutate(
            &input,
            &caller(),
            &observed,
            MapChange::Add {
                layer: Box::new(layer),
                binding: None,
            },
            &lease(directory, &input),
        )
        .unwrap();
    id
}
#[test]
fn 선택_참조는_천개_제한과_지도_전체_바이트_제한을_모두_적용한다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let first = map_with_layer(&directory, &store);
    let second = map_with_layer(&directory, &store);
    // when
    let actual = selection_bounds(&directory, &store, &first, &second);
    // then
    assert_eq!(actual, (ErrorCode::ResourceLimit, ErrorCode::ResourceLimit));
    assert!(store
        .get(&first, &caller())
        .unwrap()
        .map
        .selected_feature_refs
        .is_empty());
    assert!(store
        .get(&second, &caller())
        .unwrap()
        .map
        .selected_feature_refs
        .is_empty());
}
fn invalid_selection(directory: &Directory, store: &MapStore, id: &str) -> Vec<ErrorCode> {
    let observed = store.get(id, &caller()).unwrap();
    let base = tabularis_spatial_core::FeatureRef {
        layer_id: "layer".into(),
        feature_id: "feature".into(),
    };
    let cases = vec![
        vec![base.clone(), base.clone()],
        vec![tabularis_spatial_core::FeatureRef {
            layer_id: "missing".into(),
            ..base.clone()
        }],
        vec![tabularis_spatial_core::FeatureRef {
            feature_id: "x".repeat(129),
            ..base.clone()
        }],
        vec![tabularis_spatial_core::FeatureRef {
            feature_id: "bad\n".into(),
            ..base
        }],
    ];
    cases
        .into_iter()
        .map(|refs| {
            let input = request(
                Operation::MapSelectionSet,
                Some(id),
                Some(1),
                json!({"feature_refs":[]}),
            );
            store
                .mutate(
                    &input,
                    &caller(),
                    &observed,
                    MapChange::Selection(refs),
                    &lease(directory, &input),
                )
                .err()
                .unwrap()
                .code
        })
        .collect()
}
#[test]
fn 선택은_중복과_없는_레이어와_잘못된_feature_식별자를_거부한다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let id = map_with_layer(&directory, &store);
    // when
    let actual = invalid_selection(&directory, &store, &id);
    // then
    assert_eq!(actual, vec![ErrorCode::InvalidArgument; 4]);
    assert!(store
        .get(&id, &caller())
        .unwrap()
        .map
        .selected_feature_refs
        .is_empty());
}
#[cfg(unix)]
#[test]
fn 지도_잠금_파일의_심볼릭_링크도_따라가지_않는다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let id = create(&directory, &store, "fixture");
    let target = directory.0.join("outside.lock");
    let path = store.root().join(format!("{id}.lock"));
    std::fs::rename(&path, &target).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    // when
    let actual = store.get(&id, &caller());
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::PermissionDenied);
}
#[cfg(unix)]
#[test]
fn 지도_루트의_심볼릭_링크는_외부_경로를_열지_않는다() {
    // given
    let directory = Directory::new();
    std::fs::create_dir_all(&directory.0).unwrap();
    let target = directory.0.join("outside");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, directory.0.join("maps-v1")).unwrap();
    // when
    let actual = MapStore::new(&directory.0);
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(std::fs::read_dir(target).unwrap().count(), 0);
}
fn canonical_replay(directory: &Directory, store: &MapStore, id: &str) -> (u64, u64) {
    let observed = store.get(id, &caller()).unwrap();
    let mut input = request(
        Operation::MapUpdate,
        Some(id),
        Some(0),
        json!({"name":"changed","basemap":null}),
    );
    let first = store
        .mutate(
            &input,
            &caller(),
            &observed,
            MapChange::Update {
                name: Some("changed".into()),
                basemap: BasemapChange::Clear,
            },
            &lease(directory, &input),
        )
        .unwrap();
    input.input = serde_json::from_str("{\"basemap\":null,\"name\":\"changed\"}").unwrap();
    let replay = store
        .confirm_replay(&input, &caller(), &lease(directory, &input))
        .unwrap();
    (first.map.version, replay.map.version)
}
#[test]
fn 요청_객체의_키_순서가_달라도_같은_게시를_재생한다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let id = create(&directory, &store, "fixture");
    // when
    let actual = canonical_replay(&directory, &store, &id);
    // then
    assert_eq!(actual, (1, 1));
    assert_eq!(store.get(&id, &caller()).unwrap().apply_generation, 1);
}
fn advanced_create_replay(
    directory: &Directory,
    store: &MapStore,
    input: &ServiceRequest,
) -> ServiceError {
    let created = store
        .create(
            input,
            &caller(),
            MapState::new(uuid::Uuid::new_v4().to_string(), "fixture").unwrap(),
            &lease(directory, input),
        )
        .unwrap();
    let update = request(
        Operation::MapUpdate,
        Some(&created.map.map_id),
        Some(0),
        json!({"name":"changed"}),
    );
    store
        .mutate(
            &update,
            &caller(),
            &created,
            MapChange::Update {
                name: Some("changed".into()),
                basemap: BasemapChange::Keep,
            },
            &lease(directory, &update),
        )
        .unwrap();
    store
        .create(
            input,
            &caller(),
            MapState::new(uuid::Uuid::new_v4().to_string(), "fixture").unwrap(),
            &lease(directory, input),
        )
        .err()
        .unwrap()
}
#[test]
fn 생성_요청은_지도가_진행된_뒤에도_중복_생성하지_않고_applied_충돌을_반환한다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let input = request(Operation::MapCreate, None, None, json!({"name":"fixture"}));
    // when
    let actual = advanced_create_replay(&directory, &store, &input);
    // then
    assert_eq!(actual.code, ErrorCode::VersionConflict);
    assert_eq!(actual.outcome, Outcome::Applied);
    assert_eq!(actual.details["current_version"], 1);
    assert_eq!(store.list(&caller()).unwrap().len(), 1);
}
#[test]
fn 지도_이름은_이백오십육자를_넘겨_게시할_수_없다() {
    // given
    let directory = Directory::new();
    let store = MapStore::new(&directory.0).unwrap();
    let name = "x".repeat(257);
    let input = request(Operation::MapCreate, None, None, json!({"name":name}));
    let map = MapState::new(uuid::Uuid::new_v4().to_string(), name).unwrap();
    // when
    let actual = store.create(&input, &caller(), map, &lease(&directory, &input));
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::InvalidArgument);
    assert!(store.list(&caller()).unwrap().is_empty());
}
