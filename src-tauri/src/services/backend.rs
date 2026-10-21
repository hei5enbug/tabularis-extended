use super::{
    contracts::{Limits, Metrics, Operation, ServiceRequest},
    dispatch::CallerContext,
    error::ServiceResult,
    results::{CursorState, ResultSnapshot},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::Notify;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedConnection {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub settings: Value,
    pub credential_ref: Option<String>,
    pub version: u64,
    pub allow_writes: bool,
    pub auth_revision: String,
}

#[derive(Clone, Debug)]
pub struct PermissionScope {
    pub identity: String,
    pub allow_writes: bool,
}

#[async_trait]
pub trait ConnectionBackend: Send + Sync {
    async fn validate_settings(&self, driver: &str, settings: &Value) -> ServiceResult<()>;
    async fn rotate_auth_revision(&self, id: &str) -> ServiceResult<String>;
    async fn list(&self) -> ServiceResult<Vec<SavedConnection>>;
    async fn get(&self, id: &str) -> ServiceResult<SavedConnection>;
    async fn create(
        &self,
        name: &str,
        driver: &str,
        settings: Value,
        credential_ref: Option<String>,
    ) -> ServiceResult<SavedConnection>;
    async fn create_guarded(
        &self,
        name: &str,
        driver: &str,
        settings: Value,
        credential_ref: Option<String>,
        lease: &super::jobs::JobLease,
    ) -> ServiceResult<SavedConnection> {
        lease.mark_write_dispatched()?;
        self.create(name, driver, settings, credential_ref).await
    }
    async fn update(
        &self,
        id: &str,
        expected_version: u64,
        patch: Value,
    ) -> ServiceResult<SavedConnection>;
    async fn update_guarded(
        &self,
        id: &str,
        expected_version: u64,
        patch: Value,
        lease: &super::jobs::JobLease,
    ) -> ServiceResult<SavedConnection> {
        lease.mark_write_dispatched()?;
        self.update(id, expected_version, patch).await
    }
    async fn delete(&self, id: &str, expected_version: u64) -> ServiceResult<()>;
}

#[async_trait]
pub trait PolicyBackend: Send + Sync {
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
    async fn consume_write_approval(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
        request: &ServiceRequest,
        request_hash: &str,
    ) -> ServiceResult<()>;
    async fn query_is_write(
        &self,
        _caller: &CallerContext,
        _request: &ServiceRequest,
    ) -> ServiceResult<bool> {
        Ok(false)
    }
}

#[derive(Clone, Default)]
pub struct CancellationSignal {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CancellationSignal {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
    pub async fn cancelled(&self) {
        loop {
            let notification = self.notify.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notification.await;
        }
    }
}

#[derive(Clone)]
pub enum TransientAuth {
    EntraToken {
        access_token: super::credentials::Secret,
        expires_at_ms: u64,
        tenant_id: String,
        client_id: String,
        principal_id: String,
        scope: String,
    },
    AccountKey {
        key: super::credentials::Secret,
        identity: String,
    },
    DbPassword {
        password: super::credentials::Secret,
        identity: String,
    },
}

#[derive(Clone)]
pub struct DriverContext {
    pub connection_id: String,
    pub request_id: String,
    pub deadline_ms: u64,
    pub signal: CancellationSignal,
    pub transient_auth: Option<TransientAuth>,
    pub read_only: bool,
    pub session_handle: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOnlyGuarantee {
    ReadOnlyTransaction,
    RestrictedDatabasePrincipal,
    ReadOnlyDataPlaneRole,
    ReadOnlyAccountKey,
    ReadOnlySdkOperations,
    Unavailable,
}

pub struct DriverOutput {
    pub data: Value,
    pub snapshot: Option<ResultSnapshot>,
    pub cursor: Option<CursorState>,
    pub confirmed_cancelled: bool,
    pub session_closed: bool,
    pub metrics: Metrics,
    pub limits: Limits,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionFinish {
    Commit,
    Rollback,
}

#[async_trait]
pub trait DriverBackend: Send + Sync {
    async fn supports(
        &self,
        connection: &SavedConnection,
        operation: Operation,
    ) -> ServiceResult<bool>;
    async fn read_only_guarantee(
        &self,
        connection: &SavedConnection,
    ) -> ServiceResult<ReadOnlyGuarantee>;
    async fn execute(
        &self,
        connection: &SavedConnection,
        request: &ServiceRequest,
        context: DriverContext,
    ) -> ServiceResult<DriverOutput>;
    async fn continue_cursor(
        &self,
        connection: &SavedConnection,
        state: &CursorState,
        context: DriverContext,
    ) -> ServiceResult<DriverOutput>;
    async fn cancel(&self, connection_id: &str, request_id: &str) -> ServiceResult<()>;
    async fn reserve_session(
        &self,
        connection: &SavedConnection,
        database: &str,
        context: DriverContext,
    ) -> ServiceResult<String>;
    async fn finish_session(
        &self,
        connection: &SavedConnection,
        handle: &str,
        finish: SessionFinish,
        context: DriverContext,
    ) -> ServiceResult<()>;
    async fn discard_session(&self, connection_id: &str, handle: &str);
    async fn invalidate_auth(&self, connection_id: &str, previous_principal: Option<&str>);
}
