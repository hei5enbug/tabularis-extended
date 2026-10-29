//! Tauri glue for the service runtime: the GUI service core, its renderer
//! event publisher, the GUI connection-parameter expander and the per-command
//! core lookup. The Tauri-free service logic stays in `services/`.

use crate::models::ConnectionParams;
use crate::service_native::{catalog, NativeCore, RendererSubscriptions};
use crate::service_platform::{
    read_isolation_paths, restore_saved_secrets, KeychainSecrets, ServiceParamExpander,
    ServicePlatform,
};
use crate::services::{
    contracts::ErrorCode,
    error::{failure, ServiceResult},
    local_ipc::{EndpointKind, GuiApplyRequest},
};
use async_trait::async_trait;
use std::sync::Arc;
use tauri::{Emitter, Manager};

struct TauriPublisher<R: tauri::Runtime> {
    app: tauri::AppHandle<R>,
    subscriptions: RendererSubscriptions,
}
impl<R: tauri::Runtime> crate::service_native::GuiPublisher for TauriPublisher<R> {
    fn renderer_subscriptions(&self) -> Option<&RendererSubscriptions> {
        Some(&self.subscriptions)
    }
    fn publish(&self, request: &GuiApplyRequest) -> ServiceResult<()> {
        self.app
            .get_webview_window("main")
            .ok_or_else(|| failure(ErrorCode::GuiUnavailable))?
            .emit("service-map-apply", request)
            .map_err(|_| failure(ErrorCode::GuiUnavailable))
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
        let params = super::commands::expand_ssh_connection_params(&self.app, params)
            .await
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        let params = super::commands::expand_k8s_connection_params(&self.app, &params)
            .await
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        let cache = self
            .app
            .try_state::<Arc<crate::credential_cache::CredentialCache>>()
            .map(|state| state.inner().clone());
        let params =
            restore_saved_secrets(Arc::new(KeychainSecrets::new(cache)), params, connection_id)
                .await?;
        crate::commands::resolve_connection_params_with_id(&params, connection_id)
            .map_err(|_| failure(ErrorCode::AuthRequired))
    }
}

fn gui_platform<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    instance_id: String,
) -> ServiceResult<ServicePlatform> {
    let isolated = read_isolation_paths()?;
    let profile = isolated
        .data_dir
        .unwrap_or_else(crate::paths::get_app_config_dir);
    ServicePlatform::production(profile, instance_id, Arc::new(GuiParamExpander::new(app)))
}

/// Builds the service core the desktop app manages as Tauri state.
pub async fn gui_core<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> ServiceResult<Arc<NativeCore>> {
    let paths = read_isolation_paths()?;
    let profile = paths
        .data_dir
        .unwrap_or_else(crate::paths::get_app_config_dir);
    let platform = gui_platform(app.clone(), uuid::Uuid::new_v4().to_string())?;
    NativeCore::compose(
        platform,
        profile,
        EndpointKind::Gui,
        Some(Arc::new(TauriPublisher {
            app,
            subscriptions: RendererSubscriptions::default(),
        })),
    )
    .await
}

/// The managed service core when the connection's driver speaks the service
/// contract; `None` for drivers that still use the legacy commands.
pub async fn for_connection<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: &str,
) -> Result<Option<Arc<NativeCore>>, String> {
    let core = app
        .try_state::<Arc<NativeCore>>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| catalog::command_error(failure(ErrorCode::CapabilityUnavailable)))?;
    let connection = core
        .platform
        .runtime
        .connections
        .get(id)
        .await
        .map_err(catalog::command_error)?;
    Ok(catalog::driver_capabilities(&connection.driver)
        .await
        .map_err(catalog::command_error)?
        .map(|_| core))
}
