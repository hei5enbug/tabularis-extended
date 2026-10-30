use crate::services::{
    backend::SavedConnection,
    contracts::{ErrorCode, Operation, ServiceRequest},
    dispatch::{CallerContext, CallerTransport},
    error::{failure, refusal, ServiceResult, APPROVAL_DENIED},
    policy::{ApprovalBackend, ApprovalGrant, ChangeAnnotations},
    security::value_hash,
    storage::{now_ms, PrivateStore},
};
use crate::{
    ai_approval::{self, ApprovalDecision, PendingApproval, ServiceApprovalMarker},
    persistence,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

const CREATE_SCOPE: &str = "__connection_create__";
/// An approval waits for a person for as long as it takes. The binding's
/// `deadline_at_ms` is only a lease that proves a waiter is still alive: the
/// waiter renews it, and a binding whose lease ran out belonged to a waiter
/// that died, so it is cleaned up and can no longer be decided.
const APPROVAL_LEASE_MS: u64 = 30_000;
const APPROVAL_RENEW_MS: u64 = 5_000;
/// The pending file's modification time is refreshed this often so the
/// cleanup of stale approval files never removes one that is still awaited.
const PENDING_TOUCH_MS: u64 = 10 * 60 * 1000;
#[derive(Serialize, Deserialize)]
struct ApprovalBinding {
    caller_id: String,
    owner_instance_id: String,
    connection_id: String,
    request_id: String,
    request_hash: String,
    created_at_ms: u64,
    deadline_at_ms: u64,
    pending: PendingApproval,
}

pub struct ServiceApprovals {
    profile: PathBuf,
    bindings: PrivateStore,
    local_caller_id: String,
    renew_ms: AtomicU64,
    touch_ms: AtomicU64,
}
impl ServiceApprovals {
    pub fn new(profile: PathBuf, local_caller_id: String) -> ServiceResult<Arc<Self>> {
        Ok(Arc::new(Self {
            bindings: PrivateStore::new(profile.join("service-approval-bindings-v1"), 1024 * 1024)?,
            profile,
            local_caller_id,
            renew_ms: AtomicU64::new(APPROVAL_RENEW_MS),
            touch_ms: AtomicU64::new(PENDING_TOUCH_MS),
        }))
    }
    #[cfg(test)]
    pub(crate) fn set_timing(&self, renew_ms: u64, touch_ms: u64) {
        self.renew_ms.store(renew_ms, Ordering::SeqCst);
        self.touch_ms.store(touch_ms, Ordering::SeqCst);
    }
    pub fn profile(&self) -> &std::path::Path {
        &self.profile
    }
    fn id(id: &str) -> ServiceResult<()> {
        let parsed = uuid::Uuid::parse_str(id).map_err(|_| failure(ErrorCode::WriteNotAllowed))?;
        if parsed.to_string() != id || parsed.get_version_num() != 4 {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        Ok(())
    }
    fn path(&self, id: &str, suffix: &str) -> ServiceResult<PathBuf> {
        Self::id(id)?;
        Ok(self
            .profile
            .join("pending_approvals")
            .join(format!("{id}.{suffix}.json")))
    }
    fn read_pending(&self, id: &str) -> ServiceResult<Option<PendingApproval>> {
        let Some(bytes) =
            persistence::service_read_file(&self.path(id, "pending")?, 1024 * 1024, true)?
        else {
            return Ok(None);
        };
        let parsed: PendingApproval =
            serde_json::from_slice(&bytes).map_err(|_| failure(ErrorCode::WriteNotAllowed))?;
        if ai_approval::read_pending_in(&self.profile, id)
            .map_err(|_| failure(ErrorCode::WriteNotAllowed))?
            .as_ref()
            != Some(&parsed)
        {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        Ok(Some(parsed))
    }
    fn read_decision(&self, id: &str) -> ServiceResult<Option<ApprovalDecision>> {
        let Some(bytes) =
            persistence::service_read_file(&self.path(id, "decision")?, 16 * 1024, true)?
        else {
            return Ok(None);
        };
        let parsed: ApprovalDecision =
            serde_json::from_slice(&bytes).map_err(|_| failure(ErrorCode::WriteNotAllowed))?;
        if ai_approval::read_decision_in(&self.profile, id)
            .map_err(|_| failure(ErrorCode::WriteNotAllowed))?
            .as_ref()
            != Some(&parsed)
        {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        Ok(Some(parsed))
    }
    /// Refreshes the modification time of the pending file without touching
    /// its bytes.
    fn touch_pending(&self, id: &str) {
        let Ok(path) = self.path(id, "pending") else {
            return;
        };
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|file| file.set_modified(std::time::SystemTime::now()));
        }
    }
    fn cleanup_locked(&self, id: &str) {
        let _ = self.bindings.remove(id);
        for suffix in ["pending", "decision"] {
            if let Ok(path) = self.path(id, suffix) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    fn cleanup(&self, id: &str) {
        if let Ok(_lock) = self.bindings.lock() {
            self.cleanup_locked(id);
        }
    }
    pub fn approve_pending(
        &self,
        id: &str,
        decision: &ApprovalDecision,
        caller: &CallerContext,
    ) -> ServiceResult<()> {
        Self::id(id)?;
        if caller.caller_id != self.local_caller_id
            || !matches!(
                caller.transport,
                CallerTransport::Gui | CallerTransport::Cli
            )
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let _lock = self.bindings.lock()?;
        let binding = self
            .bindings
            .read::<ApprovalBinding>(id)?
            .ok_or_else(|| failure(ErrorCode::WriteNotAllowed))?;
        if binding.caller_id != caller.caller_id
            || binding.deadline_at_ms <= now_ms()
            || self.read_pending(id)?.as_ref() != Some(&binding.pending)
            || self.read_decision(id)?.is_some()
        {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        validate_decision(id, decision, &binding)?;
        persistence::service_write_atomic(
            &self.path(id, "decision")?,
            &serde_json::to_vec(decision).map_err(|_| failure(ErrorCode::WriteNotAllowed))?,
        )
    }
    async fn wait(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
        request: &ServiceRequest,
        request_hash: &str,
        annotations: ChangeAnnotations,
    ) -> ServiceResult<ApprovalGrant> {
        if caller.caller_id != self.local_caller_id
            || request_hash
                != value_hash(
                    &serde_json::to_value(request)
                        .map_err(|_| failure(ErrorCode::WriteNotAllowed))?,
                )
        {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        let scope = request.connection_id.as_deref().unwrap_or(CREATE_SCOPE);
        if connection.id != scope {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let created = now_ms();
        let mut lease_deadline = created.saturating_add(APPROVAL_LEASE_MS);
        let operation_value = serde_json::to_value(request.operation)
            .map_err(|_| failure(ErrorCode::WriteNotAllowed))?;
        let operation = operation_value
            .as_str()
            .ok_or_else(|| failure(ErrorCode::WriteNotAllowed))?;
        let pending = PendingApproval {
            id: id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            session_id: caller.owner_instance_id.clone(),
            connection_id: request.connection_id.clone().unwrap_or_default(),
            connection_name: connection.name.clone(),
            database: None,
            query: approval_display(request, annotations)?,
            query_kind: format!("service:{operation}"),
            client_hint: Some("service".into()),
            explain_plan: None,
            explain_error: None,
            editable: false,
            service: Some(ServiceApprovalMarker {
                operation: operation.into(),
                request_hash: request_hash.into(),
                credential_retarget: annotations.credential_retarget,
            }),
        };
        let binding = ApprovalBinding {
            caller_id: caller.caller_id.clone(),
            owner_instance_id: caller.owner_instance_id.clone(),
            connection_id: scope.into(),
            request_id: request.request_id.clone(),
            request_hash: request_hash.into(),
            created_at_ms: created,
            deadline_at_ms: lease_deadline,
            pending,
        };
        let _cleanup = ApprovalCleanup {
            approvals: self,
            id: &id,
        };
        {
            let _lock = self.bindings.lock()?;
            for old in self.bindings.ids()? {
                if let Some(old_binding) = self.bindings.read::<ApprovalBinding>(&old)? {
                    if old_binding.deadline_at_ms <= created {
                        self.cleanup_locked(&old);
                    }
                }
            }
            if self.bindings.ids()?.len() >= 64 {
                return Err(failure(ErrorCode::ResourceLimit));
            }
            self.bindings.write(&id, &binding)?;
            persistence::service_write_atomic(
                &self.path(&id, "pending")?,
                &serde_json::to_vec(&binding.pending)
                    .map_err(|_| failure(ErrorCode::WriteNotAllowed))?,
            )
            .map_err(|_| failure(ErrorCode::WriteNotAllowed))?;
        }
        let mut renewed_at = created;
        let mut touched_at = created;
        loop {
            {
                let _lock = self.bindings.lock()?;
                let current = self
                    .bindings
                    .read::<ApprovalBinding>(&id)?
                    .ok_or_else(|| failure(ErrorCode::WriteNotAllowed))?;
                if current.caller_id != caller.caller_id
                    || current.owner_instance_id != caller.owner_instance_id
                    || current.connection_id != scope
                    || current.request_id != request.request_id
                    || current.request_hash != request_hash
                    || current.created_at_ms != created
                    || current.deadline_at_ms != lease_deadline
                    || current.pending != binding.pending
                    || self.read_pending(&id)?.as_ref() != Some(&binding.pending)
                {
                    return Err(failure(ErrorCode::WriteNotAllowed));
                }
                let now = now_ms();
                let current =
                    if now.saturating_sub(renewed_at) >= self.renew_ms.load(Ordering::SeqCst) {
                        lease_deadline = now.saturating_add(APPROVAL_LEASE_MS);
                        renewed_at = now;
                        let renewed = ApprovalBinding {
                            deadline_at_ms: lease_deadline,
                            ..current
                        };
                        self.bindings.write(&id, &renewed)?;
                        renewed
                    } else {
                        current
                    };
                if now.saturating_sub(touched_at) >= self.touch_ms.load(Ordering::SeqCst) {
                    touched_at = now;
                    self.touch_pending(&id);
                }
                if let Some(decision) = self.read_decision(&id)? {
                    validate_decision(&id, &decision, &current)?;
                    self.cleanup_locked(&id);
                    if decision.decision != "approve" {
                        return Err(refusal(ErrorCode::WriteNotAllowed, APPROVAL_DENIED));
                    }
                    return Ok(ApprovalGrant {
                        approval_id: id.clone(),
                        connection_id: scope.into(),
                        caller_id: caller.caller_id.clone(),
                        owner_instance_id: caller.owner_instance_id.clone(),
                        request_id: request.request_id.clone(),
                        request_hash: request_hash.into(),
                    });
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub async fn configuration(
        &self,
        caller: &CallerContext,
        request: &ServiceRequest,
    ) -> ServiceResult<()> {
        self.configuration_annotated(caller, request, ChangeAnnotations::default())
            .await
    }
    pub async fn configuration_annotated(
        &self,
        caller: &CallerContext,
        request: &ServiceRequest,
        annotations: ChangeAnnotations,
    ) -> ServiceResult<()> {
        let scope = request.connection_id.as_deref().unwrap_or(CREATE_SCOPE);
        let connection = SavedConnection {
            id: scope.into(),
            name: request.input["name"].as_str().unwrap_or(scope).into(),
            driver: String::new(),
            settings: Value::Null,
            credential_ref: None,
            version: 0,
            allow_writes: false,
            auth_revision: String::new(),
        };
        let hash = value_hash(
            &serde_json::to_value(request).map_err(|_| failure(ErrorCode::WriteNotAllowed))?,
        );
        self.wait(&connection, caller, request, &hash, annotations)
            .await
            .map(|_| ())
    }
}
struct ApprovalCleanup<'a> {
    approvals: &'a ServiceApprovals,
    id: &'a str,
}
impl Drop for ApprovalCleanup<'_> {
    fn drop(&mut self) {
        self.approvals.cleanup(self.id);
    }
}
#[async_trait]
impl ApprovalBackend for ServiceApprovals {
    async fn consume(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
        request: &ServiceRequest,
        request_hash: &str,
    ) -> ServiceResult<ApprovalGrant> {
        self.wait(
            connection,
            caller,
            request,
            request_hash,
            ChangeAnnotations::default(),
        )
        .await
    }
}

fn validate_decision(
    id: &str,
    decision: &ApprovalDecision,
    binding: &ApprovalBinding,
) -> ServiceResult<()> {
    let timestamp = chrono::DateTime::parse_from_rfc3339(&decision.decided_at)
        .map_err(|_| failure(ErrorCode::WriteNotAllowed))?
        .timestamp_millis();
    if decision.approval_id != id
        || !matches!(decision.decision.as_str(), "approve" | "deny")
        || decision.edited_query.is_some()
        || timestamp < 0
        || now_ms() >= binding.deadline_at_ms
        || (timestamp as u64) < binding.created_at_ms
        || (timestamp as u64) > binding.deadline_at_ms
        || (timestamp as u64) > now_ms().saturating_add(5000)
        || decision
            .reason
            .as_ref()
            .is_some_and(|reason| reason.len() > 4096)
    {
        return Err(failure(ErrorCode::WriteNotAllowed));
    }
    Ok(())
}
fn approval_display(
    request: &ServiceRequest,
    annotations: ChangeAnnotations,
) -> ServiceResult<String> {
    if request.operation == Operation::ConnectionCreate {
        crate::services::security::validate_settings(&request.input["settings"])?;
    }
    if request.operation == Operation::ConnectionUpdate {
        crate::services::security::validate_settings(&request.input["patch"])?;
    }
    let operation =
        serde_json::to_value(request.operation).map_err(|_| failure(ErrorCode::WriteNotAllowed))?;
    let input = match request.operation {
        Operation::DocumentCreate
        | Operation::DocumentReplace
        | Operation::DocumentDelete
        | Operation::RowInsert
        | Operation::RowUpdate
        | Operation::RowDelete => {
            let mut target = serde_json::Map::new();
            for field in [
                "database",
                "schema",
                "table",
                "container",
                "identity",
                "partition_key",
                "if_match",
            ] {
                if let Some(value) = request.input.get(field) {
                    target.insert(field.into(), value.clone());
                }
            }
            if request.operation == Operation::DocumentCreate {
                if let Some(id) = request.input["document"].get("id") {
                    target.insert("id".into(), id.clone());
                }
            }
            let body = request
                .input
                .get("document")
                .or_else(|| request.input.get("values"))
                .or_else(|| request.input.get("changes"));
            let fields = body
                .and_then(Value::as_object)
                .map(|body| body.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            json!({"target":target,"body_hash":body.map(value_hash),"fields":fields,"change_count":body.map(|body|body.as_object().map(|body|body.len()).or_else(||body.as_array().map(|body|body.len())).unwrap_or(1)).unwrap_or(0)})
        }
        Operation::CredentialImport => {
            json!({"connection_id":request.connection_id,"slot":request.input["slot"],"input_channel":request.input["input_channel"],"persistence":request.input["persistence"]})
        }
        _ => request.input.clone(),
    };
    let mut shown = json!({"operation":operation,"input":input});
    if matches!(
        request.operation,
        Operation::ConnectionCreate | Operation::ConnectionUpdate
    ) {
        shown["connection_id"] = json!(request.connection_id);
        shown["credential_retarget"] = json!(annotations.credential_retarget);
    }
    let display = serde_json::to_string(&shown).map_err(|_| failure(ErrorCode::WriteNotAllowed))?;
    if display.len() > 512 * 1024 {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    Ok(display)
}
