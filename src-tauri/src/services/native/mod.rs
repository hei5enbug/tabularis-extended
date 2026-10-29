mod artifacts;
mod caller;
pub mod catalog;
pub mod cli;
mod credentials;
mod metadata;
mod renderer;
#[cfg(test)]
mod mcp_tests;
#[cfg(test)]
mod server_tools_tests;
#[cfg(test)]
pub(crate) mod tests;

pub use artifacts::NativeArtifacts;
pub use caller::{window_authority, WindowAuthority};
pub use metadata::NativeCatalogMetadata;
pub use credentials::{register_credential, CredentialSource};
pub use renderer::RendererSubscriptions;

use crate::{
    service_platform::ServicePlatform,
    service_spatial::{
        MapDomain, MapStore, RegistryMapSpatialCapability, RegistrySpatialDriverBackend,
        SpatialDataDomain, SpatialExportDomain, WeakLocalIpcGui,
    },
    services::{
        contracts::{ErrorCode, Operation, SchemaValidator, ServiceRequest, ServiceResponse},
        dispatch::{CallerContext, CallerTransport},
        error::{failure, ServiceResult},
        local_ipc::{
            current_local_caller, EndpointKind, GuiApplyAck, GuiApplyRequest, LocalIpc,
            LocalIpcRouter, PendingGuiAcks,
        },
        transport::ServiceTransport,
    },
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
    time::Duration,
};

pub trait GuiPublisher: Send + Sync {
    fn publish(&self, request: &GuiApplyRequest) -> ServiceResult<()>;
    fn renderer_subscriptions(&self) -> Option<&RendererSubscriptions> {
        None
    }
}

pub struct NativeCore {
    pub platform: ServicePlatform,
    pub transport: Arc<ServiceTransport>,
    pub artifacts: Arc<NativeArtifacts>,
    pub maps: Arc<MapStore>,
    pub acks: PendingGuiAcks,
    ipc: Mutex<Option<Arc<LocalIpc>>>,
    publisher: Option<Arc<dyn GuiPublisher>>,
    kind: EndpointKind,
    ready: AtomicBool,
}
impl NativeCore {
    pub async fn compose(
        platform: ServicePlatform,
        profile: PathBuf,
        kind: EndpointKind,
        publisher: Option<Arc<dyn GuiPublisher>>,
    ) -> ServiceResult<Arc<Self>> {
        let artifacts = NativeArtifacts::new(&profile, &platform.runtime)?;
        let maps = Arc::new(MapStore::new(&profile)?);
        let transport = ServiceTransport::new(
            platform.runtime.clone(),
            platform.policy.clone(),
            artifacts.clone(),
        )?;
        let core = Arc::new(Self {
            platform,
            transport,
            artifacts,
            maps,
            acks: PendingGuiAcks::default(),
            ipc: Mutex::new(None),
            publisher,
            kind,
            ready: AtomicBool::new(false),
        });
        let ipc = Arc::new(
            LocalIpc::start(
                &profile,
                core.platform.runtime.jobs.instance_id().into(),
                kind,
                Arc::new(NativeRouter {
                    core: Arc::downgrade(&core),
                }),
            )
            .await?,
        );
        core.transport.set_ipc(Arc::downgrade(&ipc))?;
        core.transport
            .set_catalog_metadata(Arc::new(NativeCatalogMetadata::new(Arc::downgrade(&core))))?;
        MapDomain::attach(
            &core.platform.runtime,
            core.maps.clone(),
            Arc::new(WeakLocalIpcGui::new(Arc::downgrade(&ipc))),
            Arc::new(RegistryMapSpatialCapability),
        )?;
        let data = SpatialDataDomain::attach(
            &core.platform.runtime,
            RegistrySpatialDriverBackend::new(core.platform.registry_driver.clone()),
            Some(core.maps.clone()),
        )?;
        SpatialExportDomain::attach(&core.platform.runtime, data, core.artifacts.clone())?;
        *core
            .ipc
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))? = Some(ipc);
        core.ready.store(true, Ordering::Release);
        Ok(core)
    }
    pub async fn headless(kind: EndpointKind) -> ServiceResult<Arc<Self>> {
        let paths = crate::service_platform::read_isolation_paths()?;
        let profile = paths
            .data_dir
            .unwrap_or_else(crate::paths::get_app_config_dir);
        let platform = ServicePlatform::headless(uuid::Uuid::new_v4().to_string())?;
        Self::compose(platform, profile, kind, None).await
    }
    pub fn caller(&self, transport: CallerTransport) -> ServiceResult<CallerContext> {
        current_local_caller(self.platform.runtime.jobs.instance_id().into(), transport)
    }
    pub fn renderer_subscription(&self, id: &str, ready: bool) -> ServiceResult<()> {
        self.publisher
            .as_ref()
            .and_then(|publisher| publisher.renderer_subscriptions())
            .ok_or_else(|| failure(ErrorCode::GuiUnavailable))?
            .update(id, ready)
    }
    pub fn decode(&self, raw: &Value) -> ServiceResult<ServiceRequest> {
        if serde_json::to_vec(raw)
            .map_err(|_| failure(ErrorCode::InvalidArgument))?
            .len()
            > crate::services::transport::MAX_FRAME_BYTES
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        SchemaValidator::new()
            .map_err(|_| failure(ErrorCode::CapabilityUnavailable))?
            .decode_request(raw)
            .map_err(|_| failure(ErrorCode::InvalidArgument))
    }
    pub async fn capabilities(&self, id: &str, caller: &CallerContext) -> ServiceResult<Value> {
        let connection = self.platform.runtime.connections.get(id).await?;
        self.platform
            .runtime
            .policy
            .permission(&connection, caller)
            .await?;
        let driver = crate::drivers::registry::get_driver(&connection.driver)
            .await
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
        let capabilities =
            match tokio::time::timeout(Duration::from_secs(30), driver.service_capabilities())
                .await
                .map_err(|_| failure(ErrorCode::DeadlineExceeded))?
            {
                Ok(capabilities) => Some(capabilities),
                Err(error)
                    if matches!(
                        error.code,
                        ErrorCode::UnsupportedOperation | ErrorCode::CapabilityUnavailable
                    ) =>
                {
                    None
                }
                Err(error) => return Err(error),
            };
        let mut operations = vec![
            Operation::ConnectionList,
            Operation::ConnectionCreate,
            Operation::ConnectionUpdate,
            Operation::ConnectionDelete,
            Operation::ResultGet,
            Operation::ResultRelease,
            Operation::JobGet,
            Operation::JobResult,
            Operation::JobCancel,
            Operation::AuthBegin,
            Operation::AuthStatus,
            Operation::AuthCancel,
            Operation::AuthLogout,
            Operation::CredentialImport,
        ];
        operations.extend_from_slice(crate::service_spatial::MAP_OPERATIONS);
        if let Some(caps) = &capabilities {
            if caps.service_protocol != 1 {
                return Err(failure(ErrorCode::ProtocolMismatch));
            }
            if caps.exact_query || caps.documents {
                operations.extend([
                    Operation::ConnectionTest,
                    Operation::QueryExecute,
                    Operation::QueryNext,
                ]);
            }
            if caps.catalog || caps.documents {
                operations.extend([
                    Operation::CatalogDatabases,
                    Operation::CatalogObjects,
                    Operation::CatalogDescribe,
                ]);
                if !caps.documents {
                    operations.push(Operation::CatalogSchemas);
                }
            }
            if caps.explain {
                operations.push(Operation::QueryExplain);
            }
            if caps.rows {
                operations.extend([
                    Operation::RowInsert,
                    Operation::RowUpdate,
                    Operation::RowDelete,
                ]);
            }
            if caps.documents {
                operations.extend([
                    Operation::DocumentRead,
                    Operation::DocumentCreate,
                    Operation::DocumentReplace,
                    Operation::DocumentDelete,
                ]);
            }
            if caps.sessions {
                operations.extend([
                    Operation::SessionOpen,
                    Operation::SessionCommit,
                    Operation::SessionRollback,
                    Operation::SessionClose,
                ]);
            }
            if caps.spatial {
                operations.extend([
                    Operation::SpatialColumns,
                    Operation::SpatialQueryResult,
                    Operation::SpatialTableQuery,
                    Operation::SpatialFeature,
                    Operation::SpatialExport,
                ]);
            }
        }
        let mut result = json!({"service_protocol":1,"operations":operations,"spatial_v1":capabilities.as_ref().is_some_and(|caps|caps.spatial),"documents_v1":capabilities.as_ref().is_some_and(|caps|caps.documents),"cancel_v1":capabilities.as_ref().is_some_and(|caps|caps.cancel)});
        if self.kind == EndpointKind::Gui {
            result["gui_instance_id"] = json!(self.platform.runtime.jobs.instance_id());
        }
        Ok(result)
    }
    pub async fn shutdown(&self) -> ServiceResult<()> {
        self.ready.store(false, Ordering::Release);
        let result =
            match tokio::time::timeout(Duration::from_secs(5), self.platform.runtime.shutdown())
                .await
            {
                Ok(result) => result,
                Err(_) => Err(failure(ErrorCode::DeadlineExceeded)),
            };
        let ipc = self
            .ipc
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .take();
        drop(ipc);
        result
    }
    pub fn approve(
        &self,
        id: &str,
        decision: &str,
        reason: Option<String>,
        caller: &CallerContext,
    ) -> ServiceResult<()> {
        if !matches!(decision, "approve" | "deny")
            || reason
                .as_ref()
                .is_some_and(|reason| reason.len() > 1024 || reason.chars().any(char::is_control))
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        self.platform.approvals.approve_pending(
            id,
            &crate::ai_approval::ApprovalDecision {
                approval_id: id.into(),
                decided_at: chrono::Utc::now().to_rfc3339(),
                decision: decision.into(),
                reason,
                edited_query: None,
            },
            caller,
        )
    }
}
struct NativeRouter {
    core: Weak<NativeCore>,
}
#[async_trait]
impl LocalIpcRouter for NativeRouter {
    async fn call(&self, request: ServiceRequest, caller: CallerContext) -> ServiceResponse {
        let result = async {
            let core = self
                .core
                .upgrade()
                .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
            if !core.ready.load(Ordering::Acquire) {
                return Err(failure(ErrorCode::CapabilityUnavailable));
            }
            core.transport
                .submit(
                    serde_json::to_value(&request)
                        .map_err(|_| failure(ErrorCode::InvalidArgument))?,
                    caller,
                )
                .await
        }
        .await;
        match result {
            Ok(response) => response,
            Err(error) => {
                let mut response = ServiceResponse::failure(&request, error.code, &error.message);
                response.error = Some(crate::services::error::redact_error(error));
                response
            }
        }
    }
    async fn apply_map(
        &self,
        request: GuiApplyRequest,
        _caller: CallerContext,
    ) -> ServiceResult<GuiApplyAck> {
        let core = self
            .core
            .upgrade()
            .ok_or_else(|| failure(ErrorCode::GuiUnavailable))?;
        if !core.ready.load(Ordering::Acquire) || core.kind != EndpointKind::Gui {
            return Err(failure(ErrorCode::GuiUnavailable));
        }
        let publisher = core
            .publisher
            .as_ref()
            .ok_or_else(|| failure(ErrorCode::GuiUnavailable))?;
        tokio::time::timeout(Duration::from_secs(5), async {
            if let Some(subscriptions) = publisher.renderer_subscriptions() {
                subscriptions.wait().await?;
            }
            core.acks
                .apply(request, |request| publisher.publish(request))
                .await
        })
        .await
        .map_err(|_| failure(ErrorCode::ApplyTimeout))?
    }
}
