mod access;
mod approvals;
mod connections;
mod expander;
mod isolation;

pub use access::ProfileAccess;
pub use approvals::ServiceApprovals;
pub use connections::{ConnectionObservation, ProfileConnections};
pub use expander::{
    GuiParamExpander, HeadlessParamExpander, NoopParamExpander, ProfileConnectionResolver,
    ServiceParamExpander,
};
pub use isolation::{checked_isolation_paths, read_isolation_paths, IsolationPaths};

use crate::{
    service_registry_driver::RegistryDriverBackend,
    services::{
        backend::SavedConnection,
        credentials::SecretStore,
        dispatch::CallerTransport,
        error::ServiceResult,
        oauth::{AzureOAuthProvider, MicrosoftPrincipalResolver, OAuthProvider, ReqwestOAuthHttp},
        policy::ApprovalPolicy,
        ServiceRuntime,
    },
};
use async_trait::async_trait;
use std::{
    path::PathBuf,
    sync::{Arc, Weak},
};

pub struct ServicePlatform {
    pub runtime: Arc<ServiceRuntime>,
    pub policy: Arc<ApprovalPolicy>,
    pub registry_driver: Arc<RegistryDriverBackend>,
    pub connections: Arc<ProfileConnections>,
    pub approvals: Arc<ServiceApprovals>,
}

impl ServicePlatform {
    pub fn isolated(
        profile: PathBuf,
        instance_id: String,
        secrets: Arc<dyn SecretStore>,
        oauth: Arc<dyn OAuthProvider>,
        expander: Arc<dyn ServiceParamExpander>,
    ) -> ServiceResult<Self> {
        checked_isolation_paths(Some(profile.as_os_str()), None)?;
        let local = crate::services::local_ipc::current_local_caller(
            instance_id.clone(),
            CallerTransport::Cli,
        )?;
        let connections = Arc::new(ProfileConnections::new(&profile));
        let resolver = Arc::new(ProfileConnectionResolver::new(
            connections.clone(),
            expander,
        ));
        let registry_driver = Arc::new(RegistryDriverBackend::new(resolver));
        let approvals = ServiceApprovals::new(profile.clone(), local.caller_id.clone())?;
        let access = Arc::new(ProfileAccess::new(
            profile.clone(),
            local.caller_id,
            approvals.clone(),
        ));
        let policy = Arc::new(ApprovalPolicy::new(access, approvals.clone()));
        let runtime = ServiceRuntime::new(
            profile,
            instance_id,
            connections.clone(),
            policy.clone(),
            registry_driver.clone(),
            secrets,
            oauth,
        )?;
        connections.set_observation(Arc::new(RuntimeObservation {
            runtime: Arc::downgrade(&runtime),
        }))?;
        Ok(Self {
            runtime,
            policy,
            registry_driver,
            connections,
            approvals,
        })
    }
    pub fn production(
        profile: PathBuf,
        instance_id: String,
        expander: Arc<dyn ServiceParamExpander>,
    ) -> ServiceResult<Self> {
        read_isolation_paths()?;
        let oauth = Arc::new(AzureOAuthProvider::new(
            Arc::new(ReqwestOAuthHttp::new()?),
            Arc::new(MicrosoftPrincipalResolver),
        ));
        Self::isolated(
            profile,
            instance_id,
            crate::keychain_utils::service_secret_store(),
            oauth,
            expander,
        )
    }
    pub fn gui<R: tauri::Runtime>(
        app: tauri::AppHandle<R>,
        instance_id: String,
    ) -> ServiceResult<Self> {
        let isolated = read_isolation_paths()?;
        let profile = isolated
            .data_dir
            .unwrap_or_else(crate::paths::get_app_config_dir);
        Self::production(profile, instance_id, Arc::new(GuiParamExpander::new(app)))
    }
    pub fn headless(instance_id: String) -> ServiceResult<Self> {
        let isolated = read_isolation_paths()?;
        let profile = isolated
            .data_dir
            .unwrap_or_else(crate::paths::get_app_config_dir);
        Self::production(
            profile.clone(),
            instance_id,
            Arc::new(HeadlessParamExpander::new(profile)),
        )
    }
}

struct RuntimeObservation {
    runtime: Weak<ServiceRuntime>,
}
#[async_trait]
impl ConnectionObservation for RuntimeObservation {
    async fn observe(
        &self,
        connection: &SavedConnection,
        externally_changed: bool,
    ) -> ServiceResult<bool> {
        let Some(runtime) = self.runtime.upgrade() else {
            return Err(crate::services::error::failure(
                crate::services::contracts::ErrorCode::CapabilityUnavailable,
            ));
        };
        let marker = runtime.results.auth_revision_marker(&connection.id)?;
        if externally_changed
            || marker
                .as_ref()
                .is_some_and(|revision| revision != &connection.auth_revision)
        {
            runtime.auth.invalidate(&connection.id, false).await?;
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
