use super::*;
use std::{
    io::BufRead,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Self {
        #[cfg(unix)]
        let parent = std::env::temp_dir().canonicalize().unwrap();
        #[cfg(not(unix))]
        let parent = std::env::temp_dir();
        let root = parent.join(format!("tabularis-h3c1-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        #[cfg(windows)]
        {
            super::super::storage::protect_windows_path(&root, true).unwrap();
        }
        Self(root)
    }
    fn output(&self) -> PathBuf {
        self.0.join("저장한 지도.geojson")
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn binding() -> ArtifactBinding {
    ArtifactBinding {
        caller_id: "caller-one".into(),
        driver_id: "postgresql".into(),
        cursor: CursorBinding {
            connection_id: "connection-one".into(),
            permission_scope: "read-only".into(),
            auth_revision: "auth-one".into(),
            query_hash: "query-one".into(),
            result_revision: "result-one".into(),
            sdk_version: Some("sdk-one".into()),
            container_rid: Some("container-one".into()),
            endpoint_identity: Some("endpoint-one".into()),
            partition_hash: Some("partition-one".into()),
        },
    }
}
struct Fixture {
    root: Temporary,
    store: Arc<ArtifactStore>,
    now: Arc<AtomicU64>,
}
impl Fixture {
    fn new(limits: ArtifactLimits) -> Self {
        let root = Temporary::new();
        let now = Arc::new(AtomicU64::new(1000));
        let clock = now.clone();
        let store = ArtifactStore::construct(
            root.0.join("artifacts"),
            limits,
            Arc::new(move || clock.load(Ordering::SeqCst)),
        )
        .unwrap();
        Self { root, store, now }
    }
    fn deadline(&self) -> u64 {
        self.now.load(Ordering::SeqCst) + 1000
    }
    fn publish(&self, bytes: &[u8]) -> ArtifactMetadata {
        let mut writer = self
            .store
            .reserve(
                binding(),
                "지도.geojson".into(),
                bytes.len().max(1) as u64,
                self.deadline(),
            )
            .unwrap();
        for chunk in bytes.chunks(17_003) {
            writer.append(chunk).unwrap();
        }
        writer
            .finish(7, 19, true, vec!["upstream_limit".into()])
            .unwrap()
    }
    fn pending_files(&self) -> usize {
        fs::read_dir(&self.root.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tabularis-artifact-")
            })
            .count()
    }
}
fn code<T>(result: ServiceResult<T>) -> Option<ErrorCode> {
    result.err().map(|error| error.code)
}
fn roundtrip(fixture: &Fixture, bytes: &[u8]) -> (ArtifactMetadata, Vec<u8>, usize, usize) {
    let metadata = fixture.publish(bytes);
    let mut result = Vec::new();
    let mut largest = 0;
    let mut offset = 0;
    loop {
        let chunk = fixture
            .store
            .read_chunk(&metadata.artifact_id, &binding(), offset, MAX_CHUNK_BYTES)
            .unwrap();
        largest = largest.max(serde_json::to_vec(&chunk).unwrap().len());
        let data = STANDARD.decode(&chunk.bytes_base64).unwrap();
        offset += data.len() as u64;
        result.extend(data);
        if chunk.eof {
            break;
        }
    }
    let mut guards = 0;
    fixture
        .store
        .copy_to_new(
            &metadata.artifact_id,
            &binding(),
            &fixture.root.output(),
            &CancellationSignal::default(),
            fixture.deadline(),
            || {
                guards += 1;
                Ok(())
            },
        )
        .unwrap();
    (metadata, result, largest, guards)
}

#[test]
fn 여러_chunk로_쓴_artifact는_원본_bytes와_sha를_읽고_새_파일로_복사한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let bytes = "한글 지도와 데이터 ".repeat(8000).into_bytes();
    // when
    let actual = roundtrip(&fixture, &bytes);
    // then
    assert_eq!(actual.0.bytes, bytes.len() as u64);
    assert_eq!(actual.0.checksum, format!("{:x}", Sha256::digest(&bytes)));
    assert_eq!(actual.1, bytes);
    assert_eq!(fs::read(fixture.root.output()).unwrap(), bytes);
    assert!(actual.2 <= 100 * 1024);
    assert!(actual.3 >= 7);
    assert_eq!(actual.0.mime_type, "application/geo+json");
    assert_eq!((actual.0.feature_count, actual.0.coordinate_count), (7, 19));
    assert!(actual.0.truncated);
    assert_eq!(actual.0.reasons, vec!["upstream_limit"]);
    assert_eq!(fixture.pending_files(), 0);
}

#[test]
fn chunk의_sha와_eof는_골든_값과_빈_마지막_chunk를_보존한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let metadata = fixture.publish(b"abc");
    // when
    let actual = [
        fixture
            .store
            .read_chunk(&metadata.artifact_id, &binding(), 0, 3)
            .unwrap(),
        fixture
            .store
            .read_chunk(&metadata.artifact_id, &binding(), 3, 1)
            .unwrap(),
    ];
    // then
    assert_eq!(actual[0].bytes_base64, "YWJj");
    assert_eq!(
        actual[0].chunk_checksum,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(actual[1].bytes_base64, "");
    assert_eq!(
        actual[1].chunk_checksum,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert!(actual
        .iter()
        .all(|chunk| chunk.eof && chunk.checksum == metadata.checksum));
}

fn unpublished_drop(fixture: &Fixture) -> (Option<ErrorCode>, Option<ErrorCode>, String) {
    let mut writer = fixture
        .store
        .reserve(binding(), "pending.geojson".into(), 100, fixture.deadline())
        .unwrap();
    writer.append(b"private payload").unwrap();
    let id = writer.artifact_id().to_owned();
    let invisible = code(fixture.store.metadata(&id, &binding()));
    let binding_invisible = code(fixture.store.binding_for(&id, "caller-one"));
    drop(writer);
    (invisible, binding_invisible, id)
}
#[test]
fn 공개되지_않은_예약은_읽을_수_없고_drop하면_자기_파일만_정리한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    // when
    let actual = unpublished_drop(&fixture);
    // then
    assert_eq!(actual.0, Some(ErrorCode::ResultExpired));
    assert_eq!(actual.1, Some(ErrorCode::ResultExpired));
    assert!(!fixture.store.path(&actual.2, "json").exists());
    assert!(!fixture.store.path(&actual.2, "part").exists());
    assert_eq!(fixture.store.ids().unwrap().len(), 0);
}

fn append_boundary(fixture: &Fixture) -> (Option<ErrorCode>, ArtifactMetadata) {
    let mut writer = fixture
        .store
        .reserve(binding(), "cap.geojson".into(), 3, fixture.deadline())
        .unwrap();
    writer.append(b"abc").unwrap();
    let error = code(writer.append(b"d"));
    (error, writer.finish(0, 0, false, vec![]).unwrap())
}
#[test]
fn payload_정확상한은_허용하고_다음_byte는_쓰기_전에_거부한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits {
        max_payload_bytes: 3,
        ..ArtifactLimits::default()
    });
    // when
    let actual = append_boundary(&fixture);
    // then
    assert_eq!(actual.0, Some(ErrorCode::ResourceLimit));
    assert_eq!(actual.1.bytes, 3);
    assert_eq!(
        fs::read(fixture.store.path(&actual.1.artifact_id, "geojson")).unwrap(),
        b"abc"
    );
}

fn reservations(fixture: &Fixture) -> (Option<ErrorCode>, Option<ErrorCode>, Option<ErrorCode>) {
    let mut writer = fixture
        .store
        .reserve(binding(), "one.geojson".into(), 8, fixture.deadline())
        .unwrap();
    let pending = code(fixture.store.reserve(
        binding(),
        "two.geojson".into(),
        8,
        fixture.deadline(),
    ));
    writer.append(b"abc").unwrap();
    writer.finish(0, 0, false, vec![]).unwrap();
    let second = fixture
        .store
        .reserve(binding(), "two.geojson".into(), 7, fixture.deadline());
    let released = second.as_ref().err().map(|error| error.code);
    let count = code(fixture.store.reserve(
        binding(),
        "three.geojson".into(),
        1,
        fixture.deadline(),
    ));
    drop(second);
    (pending, released, count)
}
#[test]
fn pending은_최대예약량을_차감하고_ready는_실제량과_개수상한을_사용한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits {
        max_payload_bytes: 10,
        max_profile_bytes: 10,
        max_entries: 2,
        ..ArtifactLimits::default()
    });
    // when
    let actual = reservations(&fixture);
    // then
    assert_eq!(
        actual,
        (
            Some(ErrorCode::ResourceLimit),
            None,
            Some(ErrorCode::ResourceLimit)
        )
    );
    assert_eq!(fixture.store.ids().unwrap().len(), 1);
}

fn pending_expiry(fixture: &Fixture) -> (Option<ErrorCode>, bool, bool) {
    let mut writer = fixture
        .store
        .reserve(binding(), "crashed.geojson".into(), 10, 2000)
        .unwrap();
    writer.append(b"abc").unwrap();
    let id = writer.artifact_id().to_owned();
    writer.finished = true;
    drop(writer);
    fixture.now.store(6999, Ordering::SeqCst);
    let grace = code(
        fixture
            .store
            .reserve(binding(), "next.geojson".into(), 10, 7999),
    );
    let still_present = fixture.store.path(&id, "part").exists();
    fixture.now.store(7000, Ordering::SeqCst);
    let next = fixture
        .store
        .reserve(binding(), "next.geojson".into(), 10, 8000)
        .unwrap();
    let removed =
        !fixture.store.path(&id, "part").exists() && !fixture.store.path(&id, "json").exists();
    drop(next);
    (grace, still_present, removed)
}
#[test]
fn crash_pending은_deadline_뒤_5초까지_예약을_유지하고_그후_정리한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits {
        max_payload_bytes: 10,
        max_profile_bytes: 10,
        max_entries: 1,
        ..ArtifactLimits::default()
    });
    // when
    let actual = pending_expiry(&fixture);
    // then
    assert_eq!(actual, (Some(ErrorCode::ResourceLimit), true, true));
}

fn ready_expiry(fixture: &Fixture) -> (Option<ErrorCode>, bool, bool) {
    let metadata = fixture.publish(b"abc");
    let unknown = fixture.store.store.root().join("unowned.json");
    fs::write(&unknown, b"keep this unknown file").unwrap();
    fixture.now.store(metadata.expires_at_ms, Ordering::SeqCst);
    let expired = code(fixture.store.metadata(&metadata.artifact_id, &binding()));
    let clock = fixture.now.clone();
    ArtifactStore::construct(
        fixture.store.store.root().to_owned(),
        fixture.store.limits,
        Arc::new(move || clock.load(Ordering::SeqCst)),
    )
    .unwrap();
    (
        expired,
        !fixture
            .store
            .path(&metadata.artifact_id, "geojson")
            .exists(),
        fs::read(unknown).unwrap() == b"keep this unknown file",
    )
}
#[test]
fn ready는_자체_ttl로_만료되고_constructor는_알수없는_파일을_건드리지_않는다() {
    // given
    let fixture = Fixture::new(ArtifactLimits {
        ttl_ms: 100,
        ..ArtifactLimits::default()
    });
    // when
    let actual = ready_expiry(&fixture);
    // then
    assert_eq!(actual, (Some(ErrorCode::ResultExpired), true, true));
}

fn mismatched_bindings(fixture: &Fixture, id: &str) -> Vec<ErrorCode> {
    let original = binding();
    let mut variants = Vec::new();
    for index in 0..11 {
        let mut changed = original.clone();
        match index {
            0 => changed.caller_id.push('x'),
            1 => changed.driver_id.push('x'),
            2 => changed.cursor.connection_id.push('x'),
            3 => changed.cursor.permission_scope.push('x'),
            4 => changed.cursor.auth_revision.push('x'),
            5 => changed.cursor.query_hash.push('x'),
            6 => changed.cursor.result_revision.push('x'),
            7 => changed.cursor.sdk_version = None,
            8 => changed.cursor.container_rid = None,
            9 => changed.cursor.endpoint_identity = None,
            _ => changed.cursor.partition_hash = None,
        }
        variants.push(
            fixture
                .store
                .read_chunk(id, &changed, 0, 3)
                .err()
                .unwrap()
                .code,
        );
    }
    variants
}
#[test]
fn caller와_driver와_아홉_cursor_binding_필드는_모두_읽기를_격리한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let metadata = fixture.publish(b"abc");
    // when
    let actual = mismatched_bindings(&fixture, &metadata.artifact_id);
    // then
    assert_eq!(
        actual[..2],
        [ErrorCode::PermissionDenied, ErrorCode::PermissionDenied]
    );
    assert_eq!(actual[2..], [ErrorCode::InvalidPageToken; 9]);
}

#[test]
fn 내부_binding_lookup은_ready와_소유_caller만_허용하고_경로를_공개하지_않는다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let metadata = fixture.publish(b"abc");
    // when
    let actual = (
        fixture
            .store
            .binding_for(&metadata.artifact_id, "caller-one"),
        code(
            fixture
                .store
                .binding_for(&metadata.artifact_id, "other-caller"),
        ),
        serde_json::to_string(&metadata).unwrap(),
    );
    // then
    assert_eq!(actual.0.unwrap(), binding());
    assert_eq!(actual.1, Some(ErrorCode::PermissionDenied));
    assert!(!actual.2.contains("caller_id"));
    assert!(!actual.2.contains("cursor"));
    assert!(!actual.2.contains(fixture.root.0.to_str().unwrap()));
}

fn tamper(fixture: &Fixture, bytes: &[u8]) -> Option<ErrorCode> {
    let metadata = fixture.publish(b"original");
    fs::write(fixture.store.path(&metadata.artifact_id, "geojson"), bytes).unwrap();
    code(
        fixture
            .store
            .read_chunk(&metadata.artifact_id, &binding(), 0, 8),
    )
}
#[test]
fn 같은크기와_다른크기의_payload_변조는_둘다_result_expired로_거부한다() {
    // given
    let same = Fixture::new(ArtifactLimits::default());
    let different = Fixture::new(ArtifactLimits::default());
    // when
    let actual = [
        tamper(&same, b"tampered"),
        tamper(&different, b"longer than original"),
    ];
    // then
    assert_eq!(actual, [Some(ErrorCode::ResultExpired); 2]);
}

fn invalid_inputs(fixture: &Fixture, id: &str) -> Vec<Option<ErrorCode>> {
    let mut result = [
        "",
        ".",
        "..",
        "../name",
        "folder/name",
        "folder\\name",
        "drive:name",
        "bad\nname",
    ]
    .into_iter()
    .map(|name| {
        code(
            fixture
                .store
                .reserve(binding(), name.into(), 1, fixture.deadline()),
        )
    })
    .collect::<Vec<_>>();
    result.push(code(fixture.store.reserve(
        binding(),
        "가".repeat(86),
        1,
        fixture.deadline(),
    )));
    result.push(code(
        fixture.store.metadata("artifact-not-a-uuid", &binding()),
    ));
    result.push(code(fixture.store.read_chunk(id, &binding(), 0, 0)));
    result.push(code(fixture.store.read_chunk(
        id,
        &binding(),
        0,
        MAX_CHUNK_BYTES + 1,
    )));
    result.push(code(fixture.store.read_chunk(id, &binding(), 4, 1)));
    result.push(code(fixture.store.reserve(
        binding(),
        "zero.geojson".into(),
        0,
        fixture.deadline(),
    )));
    result.push(code(fixture.store.reserve(
        binding(),
        "large.geojson".into(),
        MAX_ARTIFACT_BYTES + 1,
        fixture.deadline(),
    )));
    result.push(code(fixture.store.reserve(
        binding(),
        "deadline.geojson".into(),
        1,
        fixture.now.load(Ordering::SeqCst) + ACTIVE_MS + 1,
    )));
    result
}
#[test]
fn filename과_id와_chunk범위와_payload상한은_입력_단계에서_거부한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let metadata = fixture.publish(b"abc");
    // when
    let actual = invalid_inputs(&fixture, &metadata.artifact_id);
    // then
    assert_eq!(actual, vec![Some(ErrorCode::InvalidArgument); 16]);
    assert_eq!(fixture.store.ids().unwrap().len(), 1);
}

fn expired_writer(fixture: &Fixture) -> (Option<ErrorCode>, Option<ErrorCode>, String) {
    let mut writer = fixture
        .store
        .reserve(binding(), "expired.geojson".into(), 10, fixture.deadline())
        .unwrap();
    let id = writer.artifact_id().to_owned();
    fixture.now.store(fixture.deadline(), Ordering::SeqCst);
    let append = code(writer.append(b"abc"));
    (append, code(writer.finish(0, 0, false, vec![])), id)
}
#[test]
fn deadline이_지난_writer는_append와_publish를_거부하고_정리한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    // when
    let actual = expired_writer(&fixture);
    // then
    assert_eq!(actual.0, Some(ErrorCode::DeadlineExceeded));
    assert_eq!(actual.1, Some(ErrorCode::DeadlineExceeded));
    assert!(!fixture.store.path(&actual.2, "json").exists());
    assert!(!fixture.store.path(&actual.2, "geojson").exists());
}

fn blocked_copy(fixture: &Fixture, mode: &str) -> Option<ErrorCode> {
    let metadata = fixture.publish(&vec![b'x'; MAX_CHUNK_BYTES * 2 + 1]);
    let signal = CancellationSignal::default();
    let mut calls = 0;
    let deadline = fixture.deadline();
    code(fixture.store.copy_to_new(
        &metadata.artifact_id,
        &binding(),
        &fixture.root.output(),
        &signal,
        deadline,
        || {
            calls += 1;
            if calls == 3 {
                match mode {
                    "cancel" => signal.cancel(),
                    "deadline" => fixture.now.store(deadline, Ordering::SeqCst),
                    _ => return Err(failure(ErrorCode::PermissionDenied)),
                }
            }
            Ok(())
        },
    ))
}
#[test]
fn 복사중_cancel과_deadline과_guard_실패는_새_파일을_공개하지_않는다() {
    // given
    let cancel = Fixture::new(ArtifactLimits::default());
    let deadline = Fixture::new(ArtifactLimits::default());
    let guard = Fixture::new(ArtifactLimits::default());
    // when
    let actual = [
        blocked_copy(&cancel, "cancel"),
        blocked_copy(&deadline, "deadline"),
        blocked_copy(&guard, "guard"),
    ];
    // then
    assert_eq!(
        actual,
        [
            Some(ErrorCode::Cancelled),
            Some(ErrorCode::DeadlineExceeded),
            Some(ErrorCode::PermissionDenied)
        ]
    );
    assert!(
        !cancel.root.output().exists()
            && !deadline.root.output().exists()
            && !guard.root.output().exists()
    );
    assert_eq!(
        (
            cancel.pending_files(),
            deadline.pending_files(),
            guard.pending_files()
        ),
        (0, 0, 0)
    );
}

#[test]
fn 이미_존재하는_선택파일은_덮어쓰지_않고_원본_sha를_유지한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let metadata = fixture.publish(b"new data");
    fs::write(fixture.root.output(), b"existing original").unwrap();
    let expected = format!("{:x}", Sha256::digest(b"existing original"));
    // when
    let actual = fixture.store.copy_to_new(
        &metadata.artifact_id,
        &binding(),
        &fixture.root.output(),
        &CancellationSignal::default(),
        fixture.deadline(),
        || Ok(()),
    );
    // then
    assert_eq!(code(actual), Some(ErrorCode::InvalidArgument));
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(fs::read(fixture.root.output()).unwrap())
        ),
        expected
    );
    assert_eq!(fixture.pending_files(), 0);
}

#[test]
fn 공개_뒤_guard가_실패하면_applied를_보고하고_저장된_파일을_삭제하지_않는다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let metadata = fixture.publish(b"abc");
    let destination = fixture.root.output();
    // when
    let actual = fixture.store.copy_to_new(
        &metadata.artifact_id,
        &binding(),
        &destination,
        &CancellationSignal::default(),
        fixture.deadline(),
        || {
            if destination.exists() {
                Err(failure(ErrorCode::PermissionDenied))
            } else {
                Ok(())
            }
        },
    );
    // then
    assert_eq!(actual.as_ref().err().unwrap().outcome, Outcome::Applied);
    assert_eq!(code(actual), Some(ErrorCode::PermissionDenied));
    assert_eq!(fs::read(destination).unwrap(), b"abc");
    assert_eq!(fixture.pending_files(), 0);
}

#[test]
fn 디렉터리의_알수없는_파일이_안전상한을_넘으면_scan을_제한한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    for index in 0..MAX_DIRECTORY_ENTRIES {
        fs::write(
            fixture.store.store.root().join(format!("unknown-{index}")),
            b"keep",
        )
        .unwrap();
    }
    // when
    let actual = ArtifactStore::new(fixture.store.store.root().to_owned());
    // then
    assert_eq!(code(actual), Some(ErrorCode::ResourceLimit));
    assert_eq!(
        fs::read(fixture.store.store.root().join("unknown-0")).unwrap(),
        b"keep"
    );
}

#[cfg(unix)]
#[test]
fn root와_payload와_선택파일의_symlink는_대상파일을_읽거나_덮어쓰지_않는다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let metadata = fixture.publish(b"abc");
    let other = fixture.root.0.join("other");
    fs::write(&other, b"unrelated data").unwrap();
    let root_link = fixture.root.0.join("root-link");
    std::os::unix::fs::symlink(fixture.store.store.root(), &root_link).unwrap();
    let payload = fixture.store.path(&metadata.artifact_id, "geojson");
    fs::remove_file(&payload).unwrap();
    std::os::unix::fs::symlink(&other, &payload).unwrap();
    let destination = fixture.root.output();
    std::os::unix::fs::symlink(&other, &destination).unwrap();
    // when
    let actual = (
        code(ArtifactStore::new(root_link)),
        code(
            fixture
                .store
                .read_chunk(&metadata.artifact_id, &binding(), 0, 3),
        ),
        code(fixture.store.copy_to_new(
            &metadata.artifact_id,
            &binding(),
            &destination,
            &CancellationSignal::default(),
            fixture.deadline(),
            || Ok(()),
        )),
    );
    // then
    assert_eq!(actual.0, Some(ErrorCode::PermissionDenied));
    assert_eq!(actual.1, Some(ErrorCode::ResultExpired));
    assert_eq!(actual.2, Some(ErrorCode::ResultExpired));
    assert_eq!(fs::read(other).unwrap(), b"unrelated data");
}

#[cfg(unix)]
#[test]
fn private_artifact_root와_payload와_metadata는_700과_600을_사용한다() {
    // given
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(ArtifactLimits::default());
    // when
    let metadata = fixture.publish(b"abc");
    // then
    assert_eq!(
        fs::metadata(fixture.store.store.root())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(fixture.store.path(&metadata.artifact_id, "geojson"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(fixture.store.path(&metadata.artifact_id, "json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

fn child_reservation() -> Option<ErrorCode> {
    let root = PathBuf::from(
        std::env::var("TABULARIS_H3C1_CHILD_ROOT").expect("Missing isolated child fixture."),
    );
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    let store = ArtifactStore::with_limits(
        root,
        ArtifactLimits {
            max_payload_bytes: 48,
            max_profile_bytes: 64,
            max_entries: 1,
            ..ArtifactLimits::default()
        },
    )
    .unwrap();
    let writer = store.reserve(binding(), "child.geojson".into(), 48, now_ms() + ACTIVE_MS);
    let error = writer.as_ref().err().map(|error| error.code);
    println!(
        "H3C1_{}",
        if error.is_none() {
            "RESERVED"
        } else {
            "DENIED"
        }
    );
    std::io::stdout().flush().unwrap();
    line.clear();
    std::io::stdin().read_line(&mut line).unwrap();
    drop(writer);
    error
}
#[test]
#[ignore = "The parent test invokes this helper in two real isolated subprocesses."]
fn 자식_프로세스는_공유_저장소의_예약_경쟁에_참여한다() {
    // given
    let action = child_reservation;
    // when
    let actual = action();
    // then
    assert!(actual.is_none() || actual == Some(ErrorCode::ResourceLimit));
}

#[cfg(windows)]
#[test]
fn windows_reparse_payload는_대상파일을_읽지_않고_거부한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let metadata = fixture.publish(b"abc");
    let other = fixture.root.0.join("other");
    fs::write(&other, b"unrelated data").unwrap();
    let payload = fixture.store.path(&metadata.artifact_id, "geojson");
    fs::remove_file(&payload).unwrap();
    std::os::windows::fs::symlink_file(&other, &payload).unwrap();
    // when
    let actual = fixture
        .store
        .read_chunk(&metadata.artifact_id, &binding(), 0, 3);
    // then
    assert_eq!(code(actual), Some(ErrorCode::ResultExpired));
    assert_eq!(fs::read(other).unwrap(), b"unrelated data");
}

#[test]
fn metadata의_64kib_파일상한은_전체내용을_읽기_전에_검사한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let path = fixture.store.store.root().join("bounded-json");
    let file = files::create(&path).unwrap();
    file.set_len(64 * 1024 + 1).unwrap();
    // when
    let actual = files::read_json::<serde_json::Value>(&path);
    // then
    assert_eq!(code(actual), Some(ErrorCode::ResourceLimit));
    assert_eq!(fs::metadata(path).unwrap().len(), 64 * 1024 + 1);
}

#[test]
fn 정규모양이어도_rfc4122_variant가_아닌_artifact_id는_거부한다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    let invalid = "artifact-00000000-0000-4000-0000-000000000000";
    // when
    let actual = fixture.store.metadata(invalid, &binding());
    // then
    assert_eq!(code(actual), Some(ErrorCode::InvalidArgument));
}

fn replace_copy_temporary(fixture: &Fixture) -> (Option<ErrorCode>, PathBuf) {
    let metadata = fixture.publish(b"abc");
    let mut calls = 0;
    let mut replacement = None;
    let error = code(fixture.store.copy_to_new(
        &metadata.artifact_id,
        &binding(),
        &fixture.root.output(),
        &CancellationSignal::default(),
        fixture.deadline(),
        || {
            calls += 1;
            if calls == 4 {
                let path = fs::read_dir(&fixture.root.0)
                    .unwrap()
                    .filter_map(Result::ok)
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".tabularis-artifact-")
                    })
                    .unwrap()
                    .path();
                fs::remove_file(&path).unwrap();
                let mut file = files::create(&path).unwrap();
                file.write_all(b"bad").unwrap();
                file.sync_all().unwrap();
                replacement = Some(path);
            }
            Ok(())
        },
    ));
    (error, replacement.unwrap())
}
#[test]
fn 게시_직전_temp_path가_바뀌면_복사를_거부하고_다른_inode를_삭제하지_않는다() {
    // given
    let fixture = Fixture::new(ArtifactLimits::default());
    // when
    let actual = replace_copy_temporary(&fixture);
    // then
    assert_eq!(actual.0, Some(ErrorCode::ResultExpired));
    assert!(!fixture.root.output().exists());
    assert_eq!(fs::read(actual.1).unwrap(), b"bad");
}

struct ChildProcess {
    child: std::process::Child,
}
impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn competing_processes(root: &Path) -> Vec<String> {
    use std::process::{Command, Stdio};
    let executable = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    let (sender, messages) = std::sync::mpsc::channel();
    let mut threads = Vec::new();
    for _ in 0..2 {
        let mut child = Command::new(&executable)
            .args([
                "--ignored",
                "--exact",
                concat!(
                    module_path!(),
                    "::자식_프로세스는_공유_저장소의_예약_경쟁에_참여한다"
                )
                .split_once("::")
                .unwrap()
                .1,
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env("TABULARIS_H3C1_CHILD_ROOT", root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let sender = sender.clone();
        threads.push(std::thread::spawn(move || {
            for line in std::io::BufReader::new(output).lines() {
                let line = line.unwrap();
                if let Some(index) = line.find("H3C1_") {
                    sender.send(line[index..].to_owned()).unwrap();
                }
            }
        }));
        children.push(ChildProcess { child });
    }
    for child in &mut children {
        child
            .child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"reserve\n")
            .unwrap();
    }
    let mut result = (0..2)
        .map(|_| messages.recv_timeout(Duration::from_secs(10)).unwrap())
        .collect::<Vec<_>>();
    for child in &mut children {
        child
            .child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"release\n")
            .unwrap();
        child.child.stdin.take();
    }
    let end = Instant::now() + Duration::from_secs(10);
    for child in &mut children {
        loop {
            if let Some(status) = child.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                Instant::now() < end,
                "Isolated reservation subprocess timed out."
            );
            std::thread::yield_now();
        }
    }
    for thread in threads {
        thread.join().unwrap();
    }
    result.sort();
    result
}
#[test]
fn 두_실제_프로세스가_같은_quota와_개수의_마지막_예약을_경쟁한다() {
    // given
    let root = Temporary::new();
    let store = ArtifactStore::with_limits(
        root.0.join("artifacts"),
        ArtifactLimits {
            max_payload_bytes: 48,
            max_profile_bytes: 64,
            max_entries: 1,
            ..ArtifactLimits::default()
        },
    )
    .unwrap();
    // when
    let actual = competing_processes(store.store.root());
    // then
    assert_eq!(actual, vec!["H3C1_DENIED", "H3C1_RESERVED"]);
    assert_eq!(store.ids().unwrap().len(), 0);
}
