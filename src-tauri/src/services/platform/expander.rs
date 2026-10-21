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

/// Where a saved connection's password and connection URI are kept.
pub trait SavedSecrets: Send + Sync {
    fn db_password(&self, connection_id: &str) -> Option<String>;
    /// `Err` when the keychain cannot be read.
    fn connection_uri(&self, connection_id: &str, in_keychain: bool) -> Result<Option<String>, ()>;
}

pub struct KeychainSecrets {
    cache: Option<Arc<crate::credential_cache::CredentialCache>>,
}
impl KeychainSecrets {
    pub fn new(cache: Option<Arc<crate::credential_cache::CredentialCache>>) -> Self {
        Self { cache }
    }
}
impl SavedSecrets for KeychainSecrets {
    fn db_password(&self, connection_id: &str) -> Option<String> {
        match &self.cache {
            Some(cache) => crate::credential_cache::get_db_password_cached(cache, connection_id),
            None => crate::keychain_utils::get_db_password(connection_id, ""),
        }
        .ok()
        .filter(|password| !password.trim().is_empty())
    }
    fn connection_uri(&self, connection_id: &str, in_keychain: bool) -> Result<Option<String>, ()> {
        match &self.cache {
            Some(cache) => crate::credential_cache::get_connection_uri_cached(
                cache,
                connection_id,
                in_keychain,
            )
            .map_err(|_| ()),
            None if in_keychain => crate::keychain_utils::get_connection_uri(connection_id)
                .map(Some)
                .map_err(|_| ()),
            None => Ok(None),
        }
    }
}

/// Fills in what the saved connection keeps outside `connections.json`, the
/// way the GUI connection test does: the keychain password (never for IAM
/// authentication, whose token must be entered each time) and the connection
/// URI.
pub async fn restore_saved_secrets(
    secrets: Arc<dyn SavedSecrets>,
    mut params: ConnectionParams,
    connection_id: &str,
) -> ServiceResult<ConnectionParams> {
    let id = connection_id.to_owned();
    tokio::task::spawn_blocking(move || {
        if params.use_iam_auth.unwrap_or(false) {
            return Err(failure(ErrorCode::AuthRequired));
        }
        if params.password.is_none() && params.save_in_keychain.unwrap_or(false) {
            params.password = secrets.db_password(&id);
        }
        if params
            .connection_uri
            .as_deref()
            .is_none_or(|uri| uri.trim().is_empty())
        {
            let in_keychain = params.connection_uri_in_keychain.unwrap_or(false);
            match secrets.connection_uri(&id, in_keychain) {
                Ok(Some(uri)) => params.connection_uri = Some(uri),
                Ok(None) if in_keychain => return Err(failure(ErrorCode::AuthRequired)),
                Ok(None) => {}
                Err(()) => return Err(failure(ErrorCode::AuthRequired)),
            }
        }
        Ok(params)
    })
    .await
    .map_err(|_| failure(ErrorCode::ResourceLimit))?
}

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
        let cache =
            tauri::Manager::try_state::<Arc<crate::credential_cache::CredentialCache>>(&self.app)
                .map(|state| state.inner().clone());
        let params =
            restore_saved_secrets(Arc::new(KeychainSecrets::new(cache)), params, connection_id)
                .await?;
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
        let params =
            restore_saved_secrets(Arc::new(KeychainSecrets::new(None)), params, connection_id)
                .await?;
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
