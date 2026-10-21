use super::approvals::ServiceApprovals;
use crate::services::{
    backend::{PermissionScope, SavedConnection},
    contracts::{ErrorCode, Operation, ServiceRequest},
    dispatch::{CallerContext, CallerTransport},
    error::{failure, ServiceResult},
    policy::AccessBackend,
    security::value_hash,
};
use async_trait::async_trait;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

pub struct ProfileAccess {
    profile: PathBuf,
    local_caller_id: String,
    approvals: Arc<ServiceApprovals>,
}
impl ProfileAccess {
    pub fn new(
        profile: PathBuf,
        local_caller_id: String,
        approvals: Arc<ServiceApprovals>,
    ) -> Self {
        Self {
            profile,
            local_caller_id,
            approvals,
        }
    }
}
#[async_trait]
impl AccessBackend for ProfileAccess {
    async fn permission(
        &self,
        connection: &SavedConnection,
        caller: &CallerContext,
    ) -> ServiceResult<PermissionScope> {
        if caller.caller_id != self.local_caller_id {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let config = match crate::persistence::service_read_file(
            &self.profile.join("config.json"),
            16 * 1024 * 1024,
            false,
        )? {
            Some(bytes) => serde_json::from_slice::<crate::config::AppConfig>(&bytes)
                .map_err(|_| failure(ErrorCode::PermissionDenied))?,
            None => crate::config::AppConfig {
                mcp_readonly_default: Some(true),
                ..Default::default()
            },
        };
        let mut connections = config.mcp_readonly_connections.clone().unwrap_or_default();
        connections.sort();
        connections.dedup();
        let identity = value_hash(
            &json!({"caller":caller.caller_id,"connection_id":connection.id,"mcp_readonly_default":config.mcp_readonly_default.unwrap_or(crate::config::DEFAULT_MCP_READONLY_DEFAULT),"mcp_readonly_connections":connections}),
        );
        let allow_writes = caller.transport == CallerTransport::Gui
            || !crate::config::is_connection_readonly(&config, &connection.id);
        Ok(PermissionScope {
            identity,
            allow_writes,
        })
    }
    async fn authorize_connection_change(
        &self,
        caller: &CallerContext,
        request: &ServiceRequest,
    ) -> ServiceResult<()> {
        if caller.caller_id != self.local_caller_id
            || !matches!(
                request.operation,
                Operation::ConnectionCreate
                    | Operation::ConnectionUpdate
                    | Operation::ConnectionDelete
                    | Operation::CredentialImport
            )
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        if matches!(
            caller.transport,
            CallerTransport::Gui | CallerTransport::Cli
        ) {
            return Ok(());
        }
        self.approvals.configuration(caller, request).await
    }
}
