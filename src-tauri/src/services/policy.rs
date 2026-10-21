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
        if request.operation != super::contracts::Operation::QueryExecute {
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
        let permission = self.access.permission(connection, caller).await?;
        if !connection.allow_writes || !permission.allow_writes {
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
