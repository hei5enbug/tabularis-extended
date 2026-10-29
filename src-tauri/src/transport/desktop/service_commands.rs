use crate::{
    service_native::{window_authority, NativeCore, WindowAuthority},
    services::{
        backend::CancellationSignal,
        contracts::ErrorCode,
        contracts::ServiceResponse,
        dispatch::CallerTransport,
        error::{failure, redact_error, ServiceResult},
        local_ipc::GuiApplyAck,
    },
};
use serde_json::Value;
use std::sync::Arc;
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

fn window_core<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
) -> ServiceResult<(Arc<NativeCore>, WindowAuthority)> {
    let app = window.app_handle();
    let url = window
        .url()
        .map_err(|_| failure(ErrorCode::PermissionDenied))?;
    let authority = window_authority(
        window.label(),
        url.as_str(),
        app.config().build.dev_url.as_ref().map(|url| url.as_str()),
        app.get_webview_window(window.label()).is_some(),
    )?;
    let core = app
        .try_state::<Arc<NativeCore>>()
        .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?
        .inner()
        .clone();
    Ok((core, authority))
}
pub fn authorize_gui<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    main_only: bool,
) -> ServiceResult<()> {
    let (_, authority) = window_core(window)?;
    if main_only {
        authority.main_only()?;
    }
    Ok(())
}
#[tauri::command]
pub async fn service_call<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    request: Value,
) -> ServiceResult<ServiceResponse> {
    let (core, authority) = window_core(&window)?;
    authority.operation(core.decode(&request)?.operation)?;
    core.transport
        .submit(request, core.caller(CallerTransport::Gui)?)
        .await
        .map_err(redact_error)
}
#[tauri::command]
pub async fn service_capabilities<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    connection_id: String,
) -> ServiceResult<Value> {
    let (core, _) = window_core(&window)?;
    core.capabilities(&connection_id, &core.caller(CallerTransport::Gui)?)
        .await
        .map_err(redact_error)
}
#[tauri::command]
pub fn service_register_query_write_intent<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    request: Value,
) -> ServiceResult<bool> {
    let (core, authority) = window_core(&window)?;
    authority.main_only()?;
    core.transport
        .register_query_write(request, &core.caller(CallerTransport::Gui)?)
        .map_err(redact_error)?;
    Ok(true)
}
#[tauri::command]
pub async fn service_gui_secret_register<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    connection_id: String,
    request_id: String,
    secret: String,
) -> ServiceResult<Value> {
    let (core, authority) = window_core(&window)?;
    authority.main_only()?;
    let caller = core.caller(CallerTransport::Gui)?;
    let connection = core
        .platform
        .runtime
        .connections
        .get(&connection_id)
        .await?;
    core.platform
        .runtime
        .policy
        .permission(&connection, &caller)
        .await?;
    core.platform
        .runtime
        .secret_inputs
        .register_gui(&caller, &connection_id, &request_id, secret.into_bytes())
        .map_err(redact_error)
}
#[tauri::command]
pub fn service_gui_secret_discard<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    request_id: String,
) -> ServiceResult<()> {
    let (core, authority) = window_core(&window)?;
    authority.main_only()?;
    core.platform
        .runtime
        .secret_inputs
        .discard(&core.caller(CallerTransport::Gui)?, &request_id);
    Ok(())
}
#[tauri::command]
pub fn service_map_renderer<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    subscription_id: String,
    ready: bool,
) -> ServiceResult<()> {
    let (core, authority) = window_core(&window)?;
    authority.main_only()?;
    core.renderer_subscription(&subscription_id, ready)
        .map_err(redact_error)
}
#[tauri::command]
pub fn service_map_ack<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    map_id: String,
    ack: GuiApplyAck,
) -> ServiceResult<()> {
    let (core, authority) = window_core(&window)?;
    authority.main_only()?;
    core.acks.acknowledge(&map_id, ack).map_err(redact_error)
}
#[tauri::command]
pub fn service_approve<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    approval_id: String,
    decision: String,
    reason: Option<String>,
) -> ServiceResult<()> {
    let (core, authority) = window_core(&window)?;
    authority.main_only()?;
    core.approve(
        &approval_id,
        &decision,
        reason,
        &core.caller(CallerTransport::Gui)?,
    )
    .map_err(redact_error)
}
#[tauri::command]
pub async fn service_save_artifact<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    artifact_id: String,
) -> ServiceResult<bool> {
    let (core, authority) = window_core(&window)?;
    authority.main_only()?;
    let caller = core.caller(CallerTransport::Gui)?;
    let binding = core.artifacts.binding(&artifact_id, &caller).await?;
    let metadata = core.artifacts.store.metadata(&artifact_id, &binding)?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .app_handle()
        .dialog()
        .file()
        .set_file_name(metadata.filename)
        .save_file(move |path| {
            let _ = sender.send(path);
        });
    let Some(path) = receiver
        .await
        .map_err(|_| failure(ErrorCode::GuiUnavailable))?
    else {
        return Ok(false);
    };
    core.artifacts
        .save(
            artifact_id,
            caller,
            path.into_path()
                .map_err(|_| failure(ErrorCode::InvalidArgument))?,
            CancellationSignal::default(),
        )
        .await
        .map_err(redact_error)?;
    Ok(true)
}
