use super::super::{
    contracts::{ErrorCode, JobStatus, Operation, ServiceRequest, ServiceResponse},
    credentials::SessionSecretStore,
    dispatch::CallerContext,
    error::ServiceResult,
    runtime::ServiceRuntime,
};
use super::{
    endpoint::RuntimeRouter, test_support::*, EndpointKind, GuiApplyAck, GuiApplyRequest, LocalIpc,
    LocalIpcRouter,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

struct ChildRouter {
    service: RuntimeRouter,
    runtime: Arc<ServiceRuntime>,
    policy: Arc<TestPolicy>,
    connections: Arc<TestConnections>,
    driver: Arc<TestDriver>,
    root: std::path::PathBuf,
}
#[async_trait]
impl LocalIpcRouter for ChildRouter {
    async fn call(&self, request: ServiceRequest, caller: CallerContext) -> ServiceResponse {
        self.policy
            .allowed
            .store(!self.root.join("deny").exists(), Ordering::SeqCst);
        if request.operation == Operation::RowInsert && self.root.join("lose-ack").exists() {
            self.connections
                .saved
                .lock()
                .unwrap()
                .get_mut("conn-a")
                .unwrap()
                .allow_writes = true;
            self.policy.writes_allowed.store(true, Ordering::SeqCst);
            self.policy.approved.store(true, Ordering::SeqCst);
            let response = self.runtime.call_validated(&request, &caller).await;
            assert_eq!(response.status, JobStatus::Succeeded);
            std::fs::write(
                self.root.join("applied-pending"),
                self.driver.calls.load(Ordering::SeqCst).to_string(),
            )
            .unwrap();
            std::fs::rename(self.root.join("applied-pending"), self.root.join("applied")).unwrap();
            std::future::pending::<()>().await;
        }
        if request.operation == Operation::ConnectionList {
            let mut response =
                ServiceResponse::failure(&request, ErrorCode::InvalidArgument, "fixture");
            response.status = JobStatus::Succeeded;
            response.error = None;
            response.data = json!({"local_ipc":caller.transport==super::super::dispatch::CallerTransport::LocalIpc,"caller":caller.caller_id,"owner":caller.owner_instance_id});
            return response;
        }
        self.service.call(request, caller).await
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

pub(super) async fn run(root: &Path) -> bool {
    let policy = Arc::new(TestPolicy::new());
    let connections = Arc::new(TestConnections::new());
    let driver = Arc::new(TestDriver::new());
    let runtime = ServiceRuntime::new(
        root.join("runtime"),
        "child".into(),
        connections.clone(),
        policy.clone(),
        driver.clone(),
        Arc::new(SessionSecretStore::default()),
        Arc::new(TestOAuth::new()),
    )
    .unwrap();
    let router = Arc::new(ChildRouter {
        service: RuntimeRouter::new(runtime.clone()).unwrap(),
        runtime: runtime.clone(),
        policy,
        connections,
        driver,
        root: root.to_owned(),
    });
    let mut endpoint = LocalIpc::start(root, "child".into(), EndpointKind::Gui, router)
        .await
        .unwrap();
    std::fs::write(root.join("ready"), b"ready").unwrap();
    while !root.join("stop").exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    endpoint.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
    std::fs::write(root.join("completed"), b"completed").unwrap();
    !root.join("local-ipc/child.json").exists()
}

pub(super) async fn forward(root: &Path, router: Arc<dyn LocalIpcRouter>) -> bool {
    let mut client = LocalIpc::start(root, "forward-client".into(), EndpointKind::Cli, router)
        .await
        .unwrap();
    let queued: Value =
        serde_json::from_slice(&std::fs::read(root.join("forward-input")).unwrap()).unwrap();
    let input = json!({"job_id":queued["job_id"],"owner_instance_id":"child"});
    let get = client
        .call_owner("child", request(Operation::JobGet, input.clone()))
        .await
        .unwrap();
    let cancel = client
        .call_owner("child", request(Operation::JobCancel, input.clone()))
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let result = client
                .call_owner("child", request(Operation::JobResult, input.clone()))
                .await
                .unwrap();
            if result.error.is_some() || !result.data["response"].is_null() {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(
        root.join("forward-output"),
        json!({"get":get,"cancel":cancel,"result":result,"client_pid":std::process::id()})
            .to_string(),
    )
    .unwrap();
    client.shutdown().await.unwrap();
    result
        .error
        .as_ref()
        .is_none_or(|error| error.code == ErrorCode::PermissionDenied)
}

pub(super) async fn forward_scenario(root: &Path, client: &LocalIpc) -> Value {
    let queued = client.call_owner("child", query("wait")).await.unwrap();
    std::fs::write(
        root.join("forward-input"),
        serde_json::to_vec(&queued).unwrap(),
    )
    .unwrap();
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{}::tests::IPC자식프로세스", super::module_name()),
            "--ignored",
            "--nocapture",
        ])
        .env("TABULARIS_IPC_TEST_ROOT", root)
        .env("TABULARIS_IPC_TEST_MODE", "forward")
        .output()
        .await
        .unwrap();
    if !output.status.success() {
        panic!("{}", String::from_utf8_lossy(&output.stdout));
    }
    serde_json::from_slice(&std::fs::read(root.join("forward-output")).unwrap()).unwrap()
}

pub(super) async fn denied_scenario(root: &Path, client: &LocalIpc) -> ServiceResponse {
    let queued = client.call_owner("child", query("wait")).await.unwrap();
    std::fs::write(root.join("deny"), b"deny").unwrap();
    let error = client
        .call_owner(
            "child",
            request(
                Operation::JobCancel,
                json!({"job_id":queued.job_id,"owner_instance_id":"child"}),
            ),
        )
        .await
        .unwrap();
    std::fs::remove_file(root.join("deny")).unwrap();
    let _ = client
        .call_owner(
            "child",
            request(
                Operation::JobCancel,
                json!({"job_id":queued.job_id,"owner_instance_id":"child"}),
            ),
        )
        .await;
    error
}
