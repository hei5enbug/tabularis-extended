#![allow(non_snake_case)]

use super::super::{
    contracts::{ErrorCode, JobStatus, Operation, ServiceRequest, ServiceResponse},
    dispatch::{CallerContext, CallerTransport},
    error::ServiceResult,
};
use super::*;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};

struct Echo;
#[async_trait]
impl LocalIpcRouter for Echo {
    async fn call(&self, request: ServiceRequest, caller: CallerContext) -> ServiceResponse {
        let mut response =
            ServiceResponse::failure(&request, ErrorCode::InvalidArgument, "fixture");
        response.status = JobStatus::Succeeded;
        response.error = None;
        response.data = json!({"local_ipc":caller.transport == CallerTransport::LocalIpc,"caller":caller.caller_id,"owner":caller.owner_instance_id});
        response
    }
    async fn apply_map(
        &self,
        request: GuiApplyRequest,
        _caller: CallerContext,
    ) -> ServiceResult<GuiApplyAck> {
        if request.state == json!("timeout") {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        Ok(GuiApplyAck {
            state_version: request.state_version,
            rendered_version: Some(request.state_version),
            generation: request.generation,
            gui_applied: true,
        })
    }
}
fn request() -> ServiceRequest {
    ServiceRequest {
        protocol_version: 1,
        operation: Operation::ConnectionList,
        request_id: uuid::Uuid::new_v4().to_string(),
        connection_id: None,
        session_id: None,
        map_id: None,
        expected_version: None,
        deadline_ms: 30_000,
        input: json!({}),
    }
}
fn map_request() -> GuiApplyRequest {
    GuiApplyRequest {
        map_id: "map-one".into(),
        state_version: 3,
        generation: 8,
        state: json!({"layers":[]}),
    }
}

#[cfg(unix)]
struct ChildFixture {
    root: PathBuf,
    child: std::process::Child,
    client: LocalIpc,
}
#[cfg(unix)]
impl ChildFixture {
    async fn new() -> Self {
        let root = PathBuf::from(format!(
            "/tmp/h2ipc-{}",
            &uuid::Uuid::new_v4().to_string()[..8]
        ));
        std::fs::create_dir_all(&root).unwrap();
        let client = LocalIpc::start(&root, "client".into(), EndpointKind::Cli, Arc::new(Echo))
            .await
            .unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{}::tests::IPC자식프로세스", super::module_name()),
                "--ignored",
                "--nocapture",
            ])
            .env("TABULARIS_IPC_TEST_ROOT", &root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !root.join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        Self {
            root,
            child,
            client,
        }
    }
    fn record(&self, id: &str) -> Value {
        serde_json::from_slice(
            &std::fs::read(self.root.join("local-ipc").join(format!("{id}.json"))).unwrap(),
        )
        .unwrap()
    }
    async fn raw_stream(&self) -> tokio::net::UnixStream {
        let record = self.record("child");
        let basename =
            &super::super::security::digest(record["start_nonce"].as_str().unwrap().as_bytes())
                [..20];
        tokio::net::UnixStream::connect(
            self.root.join("local-ipc").join(format!("{basename}.sock")),
        )
        .await
        .unwrap()
    }
    async fn handshake(&self, stream: &mut tokio::net::UnixStream) {
        let server = self.record("child");
        let client = self.record("client");
        super::wire::write_frame(stream,&json!({"protocol_version":1,"target_nonce":server["start_nonce"],"client_instance":"client","client_nonce":client["start_nonce"]})).await.unwrap();
        let _: Value = super::wire::read_frame(stream).await.unwrap();
    }
}
#[cfg(unix)]
impl Drop for ChildFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
#[tokio::test]
#[ignore]
async fn IPC자식프로세스() {
    // Given
    let root =
        PathBuf::from(std::env::var_os("TABULARIS_IPC_TEST_ROOT").expect("isolated child profile"));
    // When
    let cleaned = run_child_fixture(&root).await;
    // Then
    assert!(cleaned);
}

#[cfg(unix)]
async fn run_child_fixture(root: &std::path::Path) -> bool {
    if std::env::var("TABULARIS_IPC_TEST_MODE").ok().as_deref() == Some("forward") {
        super::child_fixture::forward(root, Arc::new(Echo)).await
    } else {
        super::child_fixture::run(root).await
    }
}

#[cfg(unix)]
#[tokio::test]
async fn 새프로세스는동일사용자의ownerjob을조회하고취소할수있다() {
    // Given
    let fixture = ChildFixture::new().await;
    // When
    let actual = super::child_fixture::forward_scenario(&fixture.root, &fixture.client).await;
    // Then
    assert!(actual["get"]["error"].is_null());
    assert_eq!(actual["get"]["data"]["owner_instance_id"], "child");
    assert!(actual["cancel"]["error"].is_null());
    assert_eq!(actual["result"]["data"]["response"]["status"], "cancelled");
    assert_ne!(actual["client_pid"], std::process::id());
}

#[cfg(unix)]
#[tokio::test]
async fn ownerjob취소도현재연결권한을재확인한다() {
    // Given
    let fixture = ChildFixture::new().await;
    // When
    let actual = super::child_fixture::denied_scenario(&fixture.root, &fixture.client).await;
    // Then
    assert_eq!(actual.error.unwrap().code, ErrorCode::PermissionDenied);
}

#[cfg(unix)]
#[tokio::test]
async fn 실제자식프로세스에명시한소유자로요청을전달한다() {
    // Given
    let fixture = ChildFixture::new().await;
    // When
    let response = fixture.client.call_owner("child", request()).await.unwrap();
    // Then
    assert_eq!(response.status, JobStatus::Succeeded);
    assert_eq!(response.data["local_ipc"], true);
    assert_eq!(response.data["owner"], "child");
    assert_eq!(
        response.data["caller"],
        format!("local_user:{}", unsafe { libc::geteuid() })
    );
}

#[cfg(unix)]
async fn rejects_bad_nonce(fixture: &ChildFixture) -> bool {
    let mut stream = fixture.raw_stream().await;
    super::wire::write_frame(&mut stream,&json!({"protocol_version":1,"target_nonce":"wrong","client_instance":"client","client_nonce":fixture.record("client")["start_nonce"]})).await.unwrap();
    super::wire::read_frame::<Value>(&mut stream).await.is_err()
}
#[cfg(unix)]
#[tokio::test]
async fn 다른시작nonce의연결을거부한다() {
    // Given
    let fixture = ChildFixture::new().await;
    // When
    let rejected = rejects_bad_nonce(&fixture).await;
    // Then
    assert!(rejected);
}

#[cfg(unix)]
async fn malformed_frames(fixture: &ChildFixture) -> Vec<bool> {
    use tokio::io::AsyncWriteExt;
    let frames = [
        ((super::wire::MAX_FRAME_BYTES + 1) as u32)
            .to_be_bytes()
            .to_vec(),
        vec![0, 0, 0, 1, 255],
        vec![0, 0, 0, 0],
    ];
    let mut rejected = Vec::new();
    for frame in frames {
        let mut stream = fixture.raw_stream().await;
        fixture.handshake(&mut stream).await;
        stream.write_all(&frame).await.unwrap();
        rejected.push(super::wire::read_frame::<Value>(&mut stream).await.is_err());
    }
    rejected
}
#[cfg(unix)]
#[tokio::test]
async fn 초과프레임과잘못된UTF8및빈프레임을거부한다() {
    // Given
    let fixture = ChildFixture::new().await;
    // When
    let rejected = malformed_frames(&fixture).await;
    // Then
    assert_eq!(rejected, vec![true, true, true]);
}

#[cfg(unix)]
async fn stale_endpoint(fixture: &ChildFixture) -> Vec<bool> {
    let path = fixture.root.join("local-ipc/child.json");
    let original = fixture.record("child");
    let mut record = original.clone();
    record["process_id"] = json!(u32::MAX);
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let pid_rejected = fixture.client.call_owner("child", request()).await.is_err();
    record = original.clone();
    record["start_nonce"] = json!(uuid::Uuid::new_v4().to_string());
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let nonce_rejected = fixture.client.call_owner("child", request()).await.is_err();
    std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    vec![pid_rejected, nonce_rejected]
}
#[cfg(unix)]
#[tokio::test]
async fn 오래된PID와소켓nonce를거부한다() {
    // Given
    let fixture = ChildFixture::new().await;
    // When
    let rejected = stale_endpoint(&fixture).await;
    // Then
    assert_eq!(rejected, vec![true, true]);
}

#[cfg(unix)]
#[tokio::test]
async fn 지도ACK는실제자식GUI의세대와버전을반환한다() {
    // Given
    let fixture = ChildFixture::new().await;
    let request = map_request();
    // When
    let ack = fixture.client.apply_map("child", request).await.unwrap();
    // Then
    assert_eq!(ack.generation, 8);
    assert_eq!(ack.state_version, 3);
    assert_eq!(ack.rendered_version, Some(3));
    assert!(ack.gui_applied);
}
#[cfg(unix)]
#[tokio::test]
async fn 지도적용ACK가오지않으면적용성공으로표시하지않는다() {
    // Given
    let fixture = ChildFixture::new().await;
    let mut request = map_request();
    request.state = json!("timeout");
    // When
    let error = fixture
        .client
        .apply_map("child", request)
        .await
        .unwrap_err();
    // Then
    assert_eq!(error.code, ErrorCode::ApplyTimeout);
    assert_eq!(error.details["gui_applied"], false);
}

#[tokio::test]
async fn 이전세대ACK는새대기요청을소비하지않는다() {
    // Given
    let pending = PendingGuiAcks::default();
    let request = map_request();
    let mut receiver = pending.register(request.clone()).unwrap();
    let old = GuiApplyAck {
        state_version: 3,
        rendered_version: Some(3),
        generation: 7,
        gui_applied: true,
    };
    // When
    let rejected = pending.acknowledge(&request.map_id, old);
    // Then
    assert_eq!(rejected.unwrap_err().code, ErrorCode::VersionConflict);
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
}

#[cfg(unix)]
async fn shutdown_child(fixture: &mut ChildFixture) -> (bool, Vec<InstanceMetadata>) {
    std::fs::write(fixture.root.join("stop"), b"stop").unwrap();
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = fixture.child.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        status.success() && fixture.root.join("completed").exists(),
        "The child fixture must finish its shutdown scenario."
    );
    let record_removed = !fixture.root.join("local-ipc/child.json").exists();
    (
        record_removed,
        fixture.client.discover(EndpointKind::Gui).await.unwrap(),
    )
}
#[cfg(unix)]
#[tokio::test]
async fn 종료한자식GUI는자기레코드와소켓만정리한다() {
    // Given
    let mut fixture = ChildFixture::new().await;
    // When
    let (removed, instances) = shutdown_child(&mut fixture).await;
    // Then
    assert!(removed);
    assert!(instances.is_empty());
    assert!(fixture.root.join("local-ipc/client.json").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn 레지스트리와소켓은현재사용자만읽을수있다() {
    use std::os::unix::fs::MetadataExt;
    // Given
    let fixture = ChildFixture::new().await;
    // When
    let modes = std::fs::read_dir(fixture.root.join("local-ipc"))
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().mode() & 0o777)
        .collect::<Vec<_>>();
    // Then
    assert!(modes.iter().all(|mode| *mode == 0o600));
    assert_eq!(
        std::fs::metadata(fixture.root.join("local-ipc"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
}

#[cfg(unix)]
async fn applied_with_lost_ack(
    fixture: &mut ChildFixture,
) -> (super::super::contracts::ServiceError, usize) {
    let request = super::test_support::request(
        Operation::RowInsert,
        json!({"table":{"database":"fixture","schema":null,"table":"items"},"values":{"id":1}}),
    );
    let pending = fixture.client.call_owner("child", request);
    tokio::pin!(pending);
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            tokio::select! {
                response=&mut pending=>panic!("The child acknowledged before the injected disconnect: {response:?}"),
                _=tokio::time::sleep(Duration::from_millis(10))=>if fixture.root.join("applied").exists() { break; },
            }
        }
    }).await.unwrap();
    fixture.child.kill().unwrap();
    fixture.child.wait().unwrap();
    let error = pending.await.unwrap_err();
    let calls = std::fs::read_to_string(fixture.root.join("applied"))
        .unwrap()
        .parse()
        .unwrap();
    (error, calls)
}

#[cfg(unix)]
#[tokio::test]
async fn DB쓰기적용뒤ACK가유실되면재실행없이미확정으로반환한다() {
    // Given
    let mut fixture = ChildFixture::new().await;
    std::fs::write(fixture.root.join("lose-ack"), b"approved fixture write").unwrap();
    // When
    let (error, calls) = applied_with_lost_ack(&mut fixture).await;
    // Then
    assert_eq!(error.code, ErrorCode::OutcomeUnknown);
    assert_eq!(error.outcome, super::super::contracts::Outcome::Unknown);
    assert_eq!(calls, 1);
}

#[cfg(unix)]
#[tokio::test]
async fn 긴profile경로에서도유닉스소켓을자기디렉터리에연다() {
    // Given
    let root = super::test_support::TestDirectory(PathBuf::from(format!(
        "/tmp/h2long-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    )));
    let profile = root
        .0
        .join("p".repeat(64 - root.0.to_string_lossy().len() - 1));
    // When
    let endpoint = LocalIpc::start(
        &profile,
        "long-profile".into(),
        EndpointKind::Cli,
        Arc::new(Echo),
    )
    .await;
    // Then
    assert!(endpoint.is_ok());
    assert!(profile.join("local-ipc/long-profile.json").exists());
}

#[cfg(unix)]
async fn reject_forged_caller(fixture: &ChildFixture) -> bool {
    let mut stream = fixture.raw_stream().await;
    let server = fixture.record("child");
    let client = fixture.record("client");
    super::wire::write_frame(&mut stream,&json!({"protocol_version":1,"target_nonce":server["start_nonce"],"client_instance":"client","client_nonce":client["start_nonce"],"caller_id":"trusted-gui","transport":"gui"})).await.unwrap();
    matches!(
        tokio::time::timeout(
            Duration::from_secs(3),
            super::wire::read_frame::<Value>(&mut stream)
        )
        .await,
        Ok(Err(_))
    )
}

#[cfg(unix)]
#[tokio::test]
async fn IPC의JSON주장으로GUI권한이나caller를승격할수없다() {
    // Given
    let fixture = ChildFixture::new().await;
    // When
    let rejected = reject_forged_caller(&fixture).await;
    // Then
    assert!(rejected);
}

#[cfg(unix)]
#[tokio::test]
async fn handshake를보내지않는클라이언트는제한시간후연결이닫힌다() {
    // Given
    let fixture = ChildFixture::new().await;
    let mut stream = fixture.raw_stream().await;
    // When
    let closed = tokio::time::timeout(
        Duration::from_secs(3),
        super::wire::read_frame::<Value>(&mut stream),
    )
    .await;
    // Then
    assert!(matches!(closed, Ok(Err(_))));
}

#[test]
fn 소유자_호출과_서빙_기한은_한_시간짜리_작업을_자르지_않는다() {
    // Given
    let hour = 3_600_000;

    // When
    let actual = [
        endpoint::owner_call_timeout(hour),
        endpoint::owner_call_timeout(hour + 1),
        endpoint::served_deadline(hour),
        endpoint::served_deadline(hour + 1),
    ];

    // Then
    assert_eq!(
        actual,
        [
            Duration::from_millis(hour + 2_000),
            Duration::from_millis(hour + 2_000),
            Duration::from_millis(hour),
            Duration::from_millis(hour),
        ]
    );
}

#[test]
fn 짧은_기한의_소유자_호출은_기존_유예를_유지한다() {
    // Given
    let short = 30_000;

    // When
    let actual = endpoint::owner_call_timeout(short);

    // Then
    assert_eq!(actual, Duration::from_millis(short + 2_000));
}

/// A profile directory short enough for a Unix socket path under it.
#[cfg(unix)]
fn short_profile() -> super::test_support::TestDirectory {
    super::test_support::TestDirectory(PathBuf::from(format!(
        "/tmp/tli-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    )))
}

/// Writes an instance record the way `LocalIpc::start` does and leaves a
/// socket file with no listener at its endpoint path.
#[cfg(unix)]
fn leave_dead_instance(profile: &std::path::Path, id: &str, process_id: u32) -> PathBuf {
    let store =
        super::super::storage::PrivateStore::new(profile.join("local-ipc"), 16 * 1024).unwrap();
    let nonce = uuid::Uuid::new_v4().to_string();
    let socket = store.root().join(format!(
        "{}.sock",
        &super::super::security::digest(nonce.as_bytes())[..20]
    ));
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    store
        .write(
            id,
            &json!({"instance_id":id,"process_id":process_id,"kind":"mcp","start_nonce":nonce}),
        )
        .unwrap();
    socket
}

#[cfg(unix)]
fn exited_process_id() -> u32 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    child.id()
}

#[cfg(unix)]
#[tokio::test]
async fn 끝난_프로세스나_리스너없는_소켓의_기록은_새_인스턴스가_시작할때_정리되어_한도에_세지_않는다(
) {
    // Given
    let root = short_profile();
    let dead = exited_process_id();
    let mut sockets: Vec<PathBuf> = (0..63)
        .map(|index| leave_dead_instance(&root.0, &format!("dead-{index}"), dead))
        .collect();
    sockets.push(leave_dead_instance(
        &root.0,
        "reused-pid",
        std::process::id(),
    ));
    // When
    let endpoint =
        LocalIpc::start(&root.0, "fresh".into(), EndpointKind::Mcp, Arc::new(Echo)).await;
    // Then
    assert!(endpoint.is_ok());
    let left: Vec<String> = std::fs::read_dir(root.0.join("local-ipc"))
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.strip_suffix(".json").map(str::to_owned)
        })
        .collect();
    assert_eq!(left, vec!["fresh".to_owned()]);
    assert!(sockets.iter().all(|socket| !socket.exists()));
}

#[cfg(unix)]
#[tokio::test]
async fn 살아있는_인스턴스의_기록은_정리하지_않는다() {
    // Given
    let root = short_profile();
    let live = LocalIpc::start(&root.0, "live".into(), EndpointKind::Mcp, Arc::new(Echo))
        .await
        .unwrap();
    // When
    let second = LocalIpc::start(&root.0, "second".into(), EndpointKind::Cli, Arc::new(Echo)).await;
    // Then
    assert!(second.is_ok());
    assert!(root.0.join("local-ipc/live.json").exists());
    assert_eq!(live.discover(EndpointKind::Mcp).await.unwrap().len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn 살아있는_인스턴스로_한도가_차면_무엇이_찼는지와_복구방법을_알려준다() {
    // Given
    let root = short_profile();
    let mut running = Vec::new();
    for index in 0..64 {
        running.push(
            LocalIpc::start(
                &root.0,
                format!("live-{index}"),
                EndpointKind::Mcp,
                Arc::new(Echo),
            )
            .await
            .unwrap(),
        );
    }
    // When
    let refused = LocalIpc::start(
        &root.0,
        "one-more".into(),
        EndpointKind::Mcp,
        Arc::new(Echo),
    )
    .await
    .err()
    .map(super::super::error::redact_error)
    .unwrap();
    // Then
    assert_eq!(refused.code, ErrorCode::ResourceLimit);
    assert_eq!(refused.details, json!({"reason":"local_ipc_instances"}));
    assert!(refused.message.contains("64"), "{}", refused.message);
    assert!(refused.message.contains("local-ipc"), "{}", refused.message);
    assert!(refused.message.contains("Close"), "{}", refused.message);
}

#[cfg(unix)]
#[tokio::test]
async fn 종료_신호로_끝나기_전에_이_프로세스가_연_인스턴스의_기록과_소켓을_지운다() {
    // Given
    let root = short_profile();
    let endpoint = LocalIpc::start(
        &root.0,
        "signalled".into(),
        EndpointKind::Mcp,
        Arc::new(Echo),
    )
    .await
    .unwrap();
    let sockets = || {
        std::fs::read_dir(root.0.join("local-ipc"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .and_then(|e| e.to_str())
                    == Some("sock")
            })
            .count()
    };
    assert_eq!(sockets(), 1);
    // When
    endpoint::remove_own_instances_in(&root.0.join("local-ipc"));
    // Then
    assert!(!root.0.join("local-ipc/signalled.json").exists());
    assert_eq!(sockets(), 0);
    drop(endpoint);
}
