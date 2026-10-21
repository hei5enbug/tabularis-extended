mod mcp;
mod present;
mod request;
mod stdio;
mod tool_call;
pub mod tool_catalog;

use super::{
    backend::{CancellationSignal, SavedConnection},
    contracts::{
        ErrorCode, JobStatus, Operation, SchemaValidator, ServiceRequest, ServiceResponse,
    },
    dispatch::{CallerContext, CallerTransport},
    error::{failure, redact_error, ServiceResult},
    local_ipc::LocalIpc,
    policy::ApprovalPolicy,
    runtime::ServiceRuntime,
};
use async_trait::async_trait;
use serde_json::Value;
use std::{
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};
use tokio::time::{timeout_at, Instant};

pub use super::policy::mcp_forbidden;
pub use mcp::{ArtifactResource, LegacyFailure, LegacyMcpBackend, ResourceRead};
pub use request::{error_exit_code, exit_code, MAX_FRAME_BYTES};
pub use stdio::{legacy_error_frame, Mode};

#[async_trait]
pub trait TransportArtifacts: Send + Sync {
    async fn metadata(&self, id: &str, caller: &CallerContext) -> ServiceResult<Value>;
    async fn read_chunk(
        &self,
        id: &str,
        caller: &CallerContext,
        offset: u64,
        limit: usize,
    ) -> ServiceResult<Value>;
}

pub struct UnavailableArtifacts;
#[async_trait]
impl TransportArtifacts for UnavailableArtifacts {
    async fn metadata(&self, _id: &str, _caller: &CallerContext) -> ServiceResult<Value> {
        Err(failure(ErrorCode::CapabilityUnavailable))
    }
    async fn read_chunk(
        &self,
        _id: &str,
        _caller: &CallerContext,
        _offset: u64,
        _limit: usize,
    ) -> ServiceResult<Value> {
        Err(failure(ErrorCode::CapabilityUnavailable))
    }
}

/// How a driver writes SQL, for the statements the transport builds itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlFlavor {
    Postgres,
    Mssql,
    Mysql,
    Sqlite,
    Other,
}

/// What a connection's driver offers, read from its capabilities and the
/// connection's own metadata.
#[derive(Clone, Debug)]
pub struct CatalogTraits {
    /// Object kinds the driver can list: `table`, `view`,
    /// `materialized_view`, `routine`, `trigger`.
    pub kinds: Vec<String>,
    pub flavor: SqlFlavor,
    /// Schema the driver uses when a call names none.
    pub default_schema: Option<String>,
}

#[async_trait]
pub trait CatalogMetadata: Send + Sync {
    async fn traits(&self, connection: &SavedConnection) -> ServiceResult<CatalogTraits>;
}

pub struct ServiceTransport {
    runtime: Arc<ServiceRuntime>,
    policy: Arc<ApprovalPolicy>,
    artifacts: Arc<dyn TransportArtifacts>,
    validator: SchemaValidator,
    ipc: OnceLock<Weak<LocalIpc>>,
    legacy: OnceLock<Arc<dyn LegacyMcpBackend>>,
    metadata: OnceLock<Arc<dyn CatalogMetadata>>,
    catalog: Vec<tool_catalog::ToolSpec>,
}

impl ServiceTransport {
    pub fn new(
        runtime: Arc<ServiceRuntime>,
        policy: Arc<ApprovalPolicy>,
        artifacts: Arc<dyn TransportArtifacts>,
    ) -> ServiceResult<Arc<Self>> {
        Ok(Arc::new(Self {
            runtime,
            policy,
            artifacts,
            validator: SchemaValidator::new()
                .map_err(|_| failure(ErrorCode::CapabilityUnavailable))?,
            ipc: OnceLock::new(),
            legacy: OnceLock::new(),
            metadata: OnceLock::new(),
            catalog: tool_catalog::build()?,
        }))
    }

    pub fn set_catalog_metadata(&self, metadata: Arc<dyn CatalogMetadata>) -> ServiceResult<()> {
        self.metadata
            .set(metadata)
            .map_err(|_| failure(ErrorCode::InvalidArgument))
    }

    pub fn set_ipc(&self, ipc: Weak<LocalIpc>) -> ServiceResult<()> {
        if ipc.upgrade().is_none() {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
        self.ipc
            .set(ipc)
            .map_err(|_| failure(ErrorCode::InvalidArgument))
    }

    pub fn set_legacy(&self, legacy: Arc<dyn LegacyMcpBackend>) -> ServiceResult<()> {
        self.legacy
            .set(legacy)
            .map_err(|_| failure(ErrorCode::InvalidArgument))
    }

    pub fn register_query_write(&self, raw: Value, caller: &CallerContext) -> ServiceResult<()> {
        request::validate_caller(caller)?;
        let decoded = request::decode(&self.validator, &raw)?;
        if caller.transport == CallerTransport::LocalIpc {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        if !super::policy::accepts_write_intent(decoded.operation)
            || decoded.connection_id.is_none()
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        self.policy
            .register_query_write(caller, &decoded)
            .map_err(redact_error)
    }

    pub async fn submit(
        self: &Arc<Self>,
        raw: Value,
        caller: CallerContext,
    ) -> ServiceResult<ServiceResponse> {
        request::validate_caller(&caller)?;
        let decoded = request::decode(&self.validator, &raw)?;
        self.submit_request(decoded, caller).await
    }

    async fn submit_request(
        &self,
        decoded: ServiceRequest,
        caller: CallerContext,
    ) -> ServiceResult<ServiceResponse> {
        if caller.transport == CallerTransport::Mcp && mcp_forbidden(decoded.operation) {
            return Err(failure(ErrorCode::OperationNotAllowed));
        }
        let remote = request::control_operation(decoded.operation)
            && matches!(
                decoded.operation,
                Operation::JobGet | Operation::JobResult | Operation::JobCancel
            )
            && decoded.input["owner_instance_id"]
                .as_str()
                .is_some_and(|owner| owner != self.runtime.jobs.instance_id());
        let response = if remote {
            let owner = decoded.input["owner_instance_id"]
                .as_str()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
            let ipc = self
                .ipc
                .get()
                .and_then(Weak::upgrade)
                .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
            ipc.call_owner(owner, decoded.clone())
                .await
                .map_err(redact_error)?
        } else {
            self.runtime.submit(decoded.clone(), caller).await
        };
        request::checked_response(&self.validator, &decoded, response, false)
    }

    pub async fn call(
        self: &Arc<Self>,
        raw: Value,
        caller: CallerContext,
        signal: Option<CancellationSignal>,
    ) -> ServiceResult<ServiceResponse> {
        request::validate_caller(&caller)?;
        let decoded = request::decode(&self.validator, &raw)?;
        let signal = signal.unwrap_or_default();
        if signal.is_cancelled() {
            return Err(failure(ErrorCode::Cancelled));
        }
        let started = Instant::now();
        let original_deadline = started + Duration::from_millis(decoded.deadline_ms);
        let deadline = started
            + Duration::from_millis(
                decoded
                    .deadline_ms
                    .saturating_add(2_000)
                    .min(request::MAX_FOREGROUND_MS),
            );
        let queued = match timeout_at(
            deadline,
            self.submit_request(decoded.clone(), caller.clone()),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Ok(request::unknown_response(&decoded, None)),
        };
        if request::terminal(queued.status) {
            return request::checked_response(&self.validator, &decoded, queued, true);
        }
        let Some(job_id) = queued.job_id.clone() else {
            return Ok(request::unknown_response(&decoded, None));
        };
        let Some(owner) = queued.data["owner_instance_id"].as_str().map(str::to_owned) else {
            return Ok(request::unknown_response(&decoded, Some(job_id)));
        };
        if queued.data["job_id"] != job_id
            || super::security::opaque_id(&owner).is_err()
            || job_id.is_empty()
            || job_id.len() > 128
        {
            return Ok(request::unknown_response(&decoded, Some(job_id)));
        }
        let unknown = || {
            let mut response = request::unknown_response(&decoded, Some(job_id.clone()));
            response.metrics.elapsed_ms =
                started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            response
        };
        let mut cancelled = false;
        loop {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64;
            if remaining == 0 {
                return Ok(unknown());
            }
            if signal.is_cancelled() && !cancelled {
                cancelled = true;
                let control = request::control_request(
                    &decoded,
                    Operation::JobCancel,
                    &job_id,
                    &owner,
                    remaining,
                );
                match timeout_at(deadline, self.submit_request(control, caller.clone())).await {
                    Ok(Ok(response)) if response.error.is_none() => {}
                    _ => return Ok(unknown()),
                }
            }
            let get =
                request::control_request(&decoded, Operation::JobGet, &job_id, &owner, remaining);
            let response =
                match timeout_at(deadline, self.submit_request(get, caller.clone())).await {
                    Ok(Ok(response)) if response.error.is_none() => response,
                    _ => return Ok(unknown()),
                };
            if response.data["job_id"] != job_id || response.data["owner_instance_id"] != owner {
                return Ok(unknown());
            }
            let status: JobStatus = match serde_json::from_value(response.data["status"].clone()) {
                Ok(status) => status,
                Err(_) => return Ok(unknown()),
            };
            if request::terminal(status) {
                let get = request::control_request(
                    &decoded,
                    Operation::JobResult,
                    &job_id,
                    &owner,
                    remaining,
                );
                let stored =
                    match timeout_at(deadline, self.submit_request(get, caller.clone())).await {
                        Ok(Ok(response)) if response.error.is_none() => response,
                        _ => return Ok(unknown()),
                    };
                let terminal: ServiceResponse =
                    match serde_json::from_value(stored.data["response"].clone()) {
                        Ok(response) => response,
                        Err(_) => return Ok(unknown()),
                    };
                if terminal.job_id.as_deref() != Some(job_id.as_str())
                    || terminal.status != status
                    || stored.data["status"] != response.data["status"]
                {
                    return Ok(unknown());
                }
                let mut terminal =
                    match request::checked_response(&self.validator, &decoded, terminal, true) {
                        Ok(response) => response,
                        Err(_) => return Ok(unknown()),
                    };
                self.transient_auth_challenge(&decoded, &caller, original_deadline, &mut terminal)
                    .await;
                return Ok(terminal);
            }
            tokio::select! {
                _ = signal.cancelled(), if !cancelled => {},
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
                _ = tokio::time::sleep_until(deadline) => return Ok(unknown()),
            }
        }
    }

    async fn transient_auth_challenge(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        deadline: Instant,
        response: &mut ServiceResponse,
    ) {
        if request.operation != Operation::AuthBegin
            || response.status != JobStatus::Succeeded
            || !matches!(
                caller.transport,
                CallerTransport::Gui | CallerTransport::Cli
            )
        {
            return;
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining == 0 {
            return;
        }
        let Some(id) = response.data["auth_job_id"].as_str() else {
            return;
        };
        let status = ServiceRequest {
            protocol_version: 1,
            operation: Operation::AuthStatus,
            request_id: uuid::Uuid::new_v4().to_string(),
            connection_id: request.connection_id.clone(),
            session_id: None,
            map_id: None,
            expected_version: None,
            deadline_ms: remaining.min(120_000),
            input: serde_json::json!({"auth_job_id":id}),
        };
        if let Ok(Ok(status)) =
            timeout_at(deadline, self.submit_request(status, caller.clone())).await
        {
            if status.status == JobStatus::Succeeded
                && status.error.is_none()
                && status.data["auth_job_id"] == id
            {
                if let Some(challenge) = status.data.get("challenge") {
                    response.data["challenge"] = challenge.clone();
                }
            }
        }
    }

    pub async fn run_one(
        self: &Arc<Self>,
        raw: Value,
        caller: CallerContext,
        write_intent: bool,
        signal: Option<CancellationSignal>,
    ) -> ServiceResult<ServiceResponse> {
        if signal
            .as_ref()
            .is_some_and(CancellationSignal::is_cancelled)
        {
            return Err(failure(ErrorCode::Cancelled));
        }
        let decoded = request::decode(&self.validator, &raw)?;
        if write_intent && super::policy::accepts_write_intent(decoded.operation) {
            self.register_query_write(raw.clone(), &caller)?;
        }
        self.call(raw, caller, signal).await
    }
}

#[cfg(test)]
#[path = "../tests/support.rs"]
#[allow(dead_code)]
mod test_support;
#[cfg(test)]
mod tests;
