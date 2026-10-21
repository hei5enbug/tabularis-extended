use super::{
    backend::{PermissionScope, PolicyBackend, SavedConnection},
    contracts::{ErrorCode, ServiceRequest},
    dispatch::CallerContext,
    error::{failure, ServiceResult},
    security::value_hash,
    storage::now_ms,
};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

#[async_trait]
pub trait AccessBackend: Send + Sync {
    async fn permission(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
    ) -> ServiceResult<PermissionScope>;
    async fn authorize_connection_change(
        &self,
        caller: &CallerContext,
        request: &ServiceRequest,
    ) -> ServiceResult<()>;
    async fn authorize_connection_change_annotated(
        &self,
        caller: &CallerContext,
        request: &ServiceRequest,
        _annotations: ChangeAnnotations,
    ) -> ServiceResult<()> {
        self.authorize_connection_change(caller, request).await
    }
    async fn approval_mode(
        &self,
        _connection: &SavedConnection,
        _caller: &CallerContext,
    ) -> ServiceResult<ApprovalMode> {
        Ok(ApprovalMode::WritesOnly)
    }
}

/// Facts about a connection change that the approval must show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeAnnotations {
    /// Stored credentials would be sent to a different target.
    pub credential_retarget: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalMode {
    Off,
    WritesOnly,
    All,
}
impl ApprovalMode {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("off") => Self::Off,
            Some("all") => Self::All,
            _ => Self::WritesOnly,
        }
    }
}

pub struct ApprovalGrant {
    pub approval_id: String,
    pub connection_id: String,
    pub caller_id: String,
    pub owner_instance_id: String,
    pub request_id: String,
    pub request_hash: String,
}
#[async_trait]
pub trait ApprovalBackend: Send + Sync {
    async fn consume(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
        request: &ServiceRequest,
        request_hash: &str,
    ) -> ServiceResult<ApprovalGrant>;
}

/// Operations an MCP caller can never run, whatever approval it holds.
pub fn mcp_forbidden(operation: super::contracts::Operation) -> bool {
    use super::contracts::Operation::*;
    matches!(operation, CredentialImport | ConnectionDelete)
}

/// Operations whose caller must state `write: true` before they can run as
/// a write.
pub fn accepts_write_intent(operation: super::contracts::Operation) -> bool {
    use super::contracts::Operation::*;
    matches!(
        operation,
        QueryExecute
            | RowInsert
            | RowUpdate
            | RowDelete
            | DocumentCreate
            | DocumentReplace
            | DocumentDelete
            | SessionCommit
    )
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct IntentKey {
    caller: String,
    owner: String,
    request: String,
    connection: String,
    input_hash: String,
}
pub struct ApprovalPolicy {
    access: Arc<dyn AccessBackend>,
    approvals: Arc<dyn ApprovalBackend>,
    intents: Mutex<HashMap<IntentKey, u64>>,
    consumed: Mutex<HashSet<String>>,
}

impl ApprovalPolicy {
    pub fn new(access: Arc<dyn AccessBackend>, approvals: Arc<dyn ApprovalBackend>) -> Self {
        Self {
            access,
            approvals,
            intents: Mutex::new(HashMap::new()),
            consumed: Mutex::new(HashSet::new()),
        }
    }
    pub fn register_query_write(
        &self,
        caller: &CallerContext,
        request: &ServiceRequest,
    ) -> ServiceResult<()> {
        if !accepts_write_intent(request.operation) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let mut intents = self
            .intents
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        intents.retain(|_, expiry| *expiry > now_ms());
        if intents.len() >= 64 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let key = intent_key(caller, request)?;
        if intents
            .insert(key, now_ms().saturating_add(request.deadline_ms))
            .is_some()
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        Ok(())
    }
}

#[async_trait]
impl PolicyBackend for ApprovalPolicy {
    async fn permission(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
    ) -> ServiceResult<PermissionScope> {
        self.access.permission(connection, caller).await
    }
    async fn authorize_connection_change(
        &self,
        caller: &CallerContext,
        request: &ServiceRequest,
    ) -> ServiceResult<()> {
        self.access
            .authorize_connection_change(caller, request)
            .await
    }
    async fn authorize_connection_change_annotated(
        &self,
        caller: &CallerContext,
        request: &ServiceRequest,
        annotations: ChangeAnnotations,
    ) -> ServiceResult<()> {
        self.access
            .authorize_connection_change_annotated(caller, request, annotations)
            .await
    }
    async fn approval_mode(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
    ) -> ServiceResult<ApprovalMode> {
        self.access.approval_mode(connection, caller).await
    }
    async fn consume_read_approval(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
        request: &ServiceRequest,
        request_hash: &str,
    ) -> ServiceResult<()> {
        self.consume(connection, caller, request, request_hash, false)
            .await
    }
    async fn query_is_write(
        &self,
        caller: &CallerContext,
        request: &ServiceRequest,
    ) -> ServiceResult<bool> {
        Ok(self
            .intents
            .lock()
            .map_err(|_| failure(ErrorCode::WriteNotAllowed))?
            .remove(&intent_key(caller, request)?)
            .is_some_and(|expiry| expiry > now_ms()))
    }
    async fn consume_write_approval(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
        request: &ServiceRequest,
        request_hash: &str,
    ) -> ServiceResult<()> {
        self.consume(connection, caller, request, request_hash, true)
            .await
    }
}

impl ApprovalPolicy {
    async fn consume(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
        request: &ServiceRequest,
        request_hash: &str,
        write: bool,
    ) -> ServiceResult<()> {
        let permission = self.access.permission(connection, caller).await?;
        if write && !permission.allow_writes {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        let grant = self
            .approvals
            .consume(connection, caller, request, request_hash)
            .await?;
        if grant.connection_id != connection.id
            || grant.caller_id != caller.caller_id
            || grant.owner_instance_id != caller.owner_instance_id
            || grant.request_id != request.request_id
            || grant.request_hash != request_hash
            || grant.approval_id.is_empty()
        {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        let mut consumed = self
            .consumed
            .lock()
            .map_err(|_| failure(ErrorCode::WriteNotAllowed))?;
        if consumed.len() >= 16_384 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        if !consumed.insert(grant.approval_id) {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        Ok(())
    }
}

fn intent_key(caller: &CallerContext, request: &ServiceRequest) -> ServiceResult<IntentKey> {
    Ok(IntentKey {
        caller: caller.caller_id.clone(),
        owner: caller.owner_instance_id.clone(),
        request: request.request_id.clone(),
        connection: request
            .connection_id
            .clone()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
        input_hash: value_hash(&request.input),
    })
}
