use super::connections::ProfileConnections;
use crate::{
    models::ConnectionParams,
    service_registry_driver::ServiceConnectionResolver,
    services::{
        backend::SavedConnection,
        contracts::ErrorCode,
        error::{failure, ServiceResult},
    },
};
use async_trait::async_trait;
use std::{path::PathBuf, sync::Arc};

#[async_trait]
pub trait ServiceParamExpander: Send + Sync {
    async fn expand(
        &self,
        params: &ConnectionParams,
        connection_id: &str,
    ) -> ServiceResult<ConnectionParams>;
}

pub struct NoopParamExpander;
#[async_trait]
impl ServiceParamExpander for NoopParamExpander {
    async fn expand(
        &self,
        params: &ConnectionParams,
        connection_id: &str,
    ) -> ServiceResult<ConnectionParams> {
        let mut params = params.clone();
        params.connection_id = Some(connection_id.into());
        Ok(params)
    }
}

pub struct GuiParamExpander<R: tauri::Runtime> {
    app: tauri::AppHandle<R>,
}
impl<R: tauri::Runtime> GuiParamExpander<R> {
    pub fn new(app: tauri::AppHandle<R>) -> Self {
        Self { app }
    }
}
#[async_trait]
impl<R: tauri::Runtime> ServiceParamExpander for GuiParamExpander<R> {
    async fn expand(
        &self,
        params: &ConnectionParams,
        connection_id: &str,
    ) -> ServiceResult<ConnectionParams> {
        let params = crate::commands::expand_ssh_connection_params(&self.app, params)
            .await
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        let params = crate::commands::expand_k8s_connection_params(&self.app, &params)
            .await
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        crate::commands::resolve_connection_params_with_id(&params, connection_id)
            .map_err(|_| failure(ErrorCode::AuthRequired))
    }
}

pub struct HeadlessParamExpander {
    profile: PathBuf,
}
impl HeadlessParamExpander {
    pub fn new(profile: PathBuf) -> Self {
        Self { profile }
    }
}
#[async_trait]
impl ServiceParamExpander for HeadlessParamExpander {
    async fn expand(
        &self,
        params: &ConnectionParams,
        connection_id: &str,
    ) -> ServiceResult<ConnectionParams> {
        let params = crate::mcp::expand_ssh_params_for_mcp_in(&self.profile, params)
            .await
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        let params = crate::mcp::expand_k8s_params_for_mcp_in(&self.profile, &params)
            .await
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        crate::commands::resolve_connection_params_with_id(&params, connection_id)
            .map_err(|_| failure(ErrorCode::AuthRequired))
    }
}

pub struct ProfileConnectionResolver {
    connections: Arc<ProfileConnections>,
    expander: Arc<dyn ServiceParamExpander>,
}
impl ProfileConnectionResolver {
    pub fn new(
        connections: Arc<ProfileConnections>,
        expander: Arc<dyn ServiceParamExpander>,
    ) -> Self {
        Self {
            connections,
            expander,
        }
    }
}
#[async_trait]
impl ServiceConnectionResolver for ProfileConnectionResolver {
    async fn params(&self, connection: &SavedConnection) -> ServiceResult<ConnectionParams> {
        let raw = self.connections.private_params(&connection.id)?;
        if raw.driver != connection.driver
            || raw
                .connection_id
                .as_deref()
                .is_some_and(|id| id != connection.id)
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let mut params = self.expander.expand(&raw, &connection.id).await?;
        if params.driver != connection.driver
            || params
                .connection_id
                .as_deref()
                .is_some_and(|id| id != connection.id)
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        params.connection_id = Some(connection.id.clone());
        Ok(params)
    }
}
