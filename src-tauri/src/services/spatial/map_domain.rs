use super::{MapChange, MapGuiBackend, MapSpatialCapability, MapStore, StoredMap, MAP_OPERATIONS};
use crate::services::{
    backend::{DriverContext, PermissionScope, SavedConnection},
    contracts::{
        ErrorCode, JobStatus, Limits, Metrics, Operation, Outcome, Page, ServiceRequest,
        ServiceResponse,
    },
    dispatch::CallerContext,
    error::{failure, redact_error, ServiceResult},
    extensions::OperationExtension,
    jobs::JobLease,
    local_ipc::{GuiApplyAck, GuiApplyRequest},
    results::{CursorBinding, ResultKind},
    runtime::ServiceRuntime,
    security::opaque_id,
    storage::now_ms,
};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Weak},
    time::Duration,
};
use tabularis_spatial_core::{BasemapChange, LayerSource, LayerState, MapState};

pub struct MapDomain {
    runtime: Weak<ServiceRuntime>,
    store: Arc<MapStore>,
    gui: Arc<dyn MapGuiBackend>,
    spatial: Arc<dyn MapSpatialCapability>,
}
struct PreparedChange {
    change: MapChange,
    fence: Option<(SavedConnection, PermissionScope)>,
}
impl MapDomain {
    pub fn attach(
        runtime: &Arc<ServiceRuntime>,
        store: Arc<MapStore>,
        gui: Arc<dyn MapGuiBackend>,
        spatial: Arc<dyn MapSpatialCapability>,
    ) -> ServiceResult<Arc<Self>> {
        let domain = Arc::new(Self {
            runtime: Arc::downgrade(runtime),
            store,
            gui,
            spatial,
        });
        runtime.register_extension(domain.clone())?;
        Ok(domain)
    }
    pub fn store(&self) -> &Arc<MapStore> {
        &self.store
    }
    async fn connection(
        &self,
        runtime: &ServiceRuntime,
        id: &str,
        caller: &CallerContext,
    ) -> ServiceResult<(SavedConnection, PermissionScope)> {
        let connection = runtime.connections.get(id).await?;
        let permission = runtime.policy.permission(&connection, caller).await?;
        if connection.id != id || permission.identity.is_empty() || permission.identity.len() > 512
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        Ok((connection, permission))
    }
    async fn authorize(
        &self,
        runtime: &ServiceRuntime,
        map: &StoredMap,
        caller: &CallerContext,
    ) -> ServiceResult<Vec<String>> {
        let mut query_layers = Vec::new();
        for layer in &map.map.layers {
            let (connection, permission) = self
                .connection(runtime, &layer.connection_id, caller)
                .await?;
            if let LayerSource::QueryResult { result_id, .. } = &layer.source {
                let binding = map
                    .source_bindings
                    .get(&layer.layer_id)
                    .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
                check_binding(binding, &connection, &permission)?;
                query_layers.push((result_id, binding));
            }
        }
        let mut warnings = Vec::new();
        for (id, binding) in query_layers {
            match runtime.results.snapshot(id, binding) {
                Ok(_) => {}
                Err(error) if error.code == ErrorCode::ResultExpired => {
                    if warnings.is_empty() {
                        warnings.push("QUERY_RESULT_REFERENCE_EXPIRED".into());
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(warnings)
    }
    async fn prepare_layer(
        &self,
        runtime: &ServiceRuntime,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
    ) -> ServiceResult<PreparedChange> {
        let id = request
            .connection_id
            .as_deref()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let (connection, permission) = self.connection(runtime, id, caller).await?;
        let source: LayerSource = decode(&request.input["source"])?;
        source.validate().map_err(super::map_store::core_error)?;
        if matches!(&source,LayerSource::Table {source_srid:Some(srid),..}if *srid>999999) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let context = DriverContext {
            connection_id: id.into(),
            request_id: request.request_id.clone(),
            deadline_ms: lease.deadline_at_ms().saturating_sub(now_ms()),
            signal: lease.signal(),
            transient_auth: None,
            read_only: true,
            session_handle: None,
        };
        if !self.spatial.supports(&connection, &context).await? {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        let binding = if let LayerSource::QueryResult {
            result_id,
            result_set_index,
            column_index,
            ..
        } = &source
        {
            let binding = runtime.results.result_binding(result_id)?;
            check_binding(&binding, &connection, &permission)?;
            let snapshot = runtime.results.snapshot(result_id, &binding)?;
            let set = snapshot
                .sets
                .get(
                    usize::try_from(*result_set_index)
                        .map_err(|_| failure(ErrorCode::InvalidArgument))?,
                )
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
            if set.kind != ResultKind::Tabular
                || usize::try_from(*column_index)
                    .ok()
                    .map_or(true, |index| index >= set.columns.len())
            {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            Some(binding)
        } else {
            None
        };
        let style = decode(&request.input["style"])?;
        let layer = LayerState::new(uuid::Uuid::new_v4().to_string(), id, source, style)
            .map_err(super::map_store::core_error)?;
        Ok(PreparedChange {
            change: MapChange::Add {
                layer: Box::new(layer),
                binding,
            },
            fence: Some((connection, permission)),
        })
    }
    async fn change(
        &self,
        runtime: &ServiceRuntime,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
    ) -> ServiceResult<PreparedChange> {
        Ok(PreparedChange {
            fence: None,
            change: match request.operation {
                Operation::MapUpdate => MapChange::Update {
                    name: request.input["name"].as_str().map(str::to_owned),
                    basemap: basemap(&request.input)?,
                },
                Operation::MapLayerAdd => {
                    return self.prepare_layer(runtime, request, caller, lease).await
                }
                Operation::MapLayerRemove => {
                    MapChange::Remove(string(&request.input, "layer_id")?.into())
                }
                Operation::MapLayerUpdate => MapChange::Layer {
                    id: string(&request.input, "layer_id")?.into(),
                    visible: request.input["visible"].as_bool(),
                    style: request.input.get("style").map(decode).transpose()?,
                },
                Operation::MapViewportSet => {
                    MapChange::Viewport(decode(&request.input["viewport"])?)
                }
                Operation::MapSelectionSet => {
                    MapChange::Selection(decode(&request.input["feature_refs"])?)
                }
                Operation::MapOpen => MapChange::Open,
                Operation::MapClose => MapChange::Close,
                Operation::MapSave => MapChange::Save,
                _ => return Err(failure(ErrorCode::UnsupportedOperation)),
            },
        })
    }
    async fn apply(
        &self,
        request: &ServiceRequest,
        map: &StoredMap,
        lease: &JobLease,
        gui_id: &str,
    ) -> ServiceResult<GuiApplyAck> {
        let action = match request.operation {
            Operation::MapOpen => "open",
            Operation::MapClose => "close",
            _ => "update",
        };
        let apply = GuiApplyRequest {
            map_id: map.map.map_id.clone(),
            state_version: map.map.version,
            generation: map.apply_generation,
            state: json!({"action":action,"map":map.map}),
        };
        let remaining = lease
            .deadline_at_ms()
            .saturating_sub(now_ms())
            .saturating_sub(1);
        let signal = lease.signal();
        let result = tokio::select! {biased;_ = signal.cancelled()=>Err(failure(ErrorCode::Cancelled)),result=tokio::time::timeout(Duration::from_millis(remaining.min(5000)),self.gui.apply(gui_id,apply))=>result.unwrap_or_else(|_|Err(failure(ErrorCode::ApplyTimeout)))};
        match result {
            Ok(ack)
                if ack.state_version == map.map.version
                    && ack.generation == map.apply_generation
                    && ack.rendered_version == Some(map.map.version)
                    && ack.gui_applied =>
            {
                Ok(ack)
            }
            Ok(_) => Err(failure(ErrorCode::ApplyTimeout)),
            Err(mut error) => {
                if error.code == ErrorCode::GuiUnavailable {
                    let instances = tokio::time::timeout(
                        Duration::from_millis(
                            lease
                                .deadline_at_ms()
                                .saturating_sub(now_ms())
                                .saturating_sub(1)
                                .min(2000),
                        ),
                        self.gui.discover(),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default();
                    error.details =
                        json!({"instances":instances.into_iter().take(64).collect::<Vec<_>>()});
                }
                Err(error)
            }
        }
    }
}
#[async_trait]
impl OperationExtension for MapDomain {
    fn operations(&self) -> &[Operation] {
        MAP_OPERATIONS
    }
    async fn execute(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
    ) -> ServiceResult<ServiceResponse> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
        if request.operation == Operation::MapList {
            let mut maps = Vec::new();
            let mut warnings = Vec::new();
            for map in self.store.list(caller)? {
                match self.authorize(&runtime, &map, caller).await {
                    Ok(found) => {
                        maps.push(map.map);
                        if !found.is_empty() && warnings.is_empty() {
                            warnings = found;
                        }
                    }
                    Err(error)
                        if matches!(
                            error.code,
                            ErrorCode::PermissionDenied
                                | ErrorCode::AuthRequired
                                | ErrorCode::ConnectionNotFound
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
            let mut response = success(request, json!({"maps":maps}));
            response.warnings = warnings;
            return Ok(response);
        }
        let gui_id = if matches!(request.operation, Operation::MapOpen | Operation::MapClose)
            || request.input["await_gui"] == true
        {
            let id = string(&request.input, "gui_instance_id")?;
            opaque_id(id)?;
            Some(id)
        } else {
            None
        };
        let (map, warnings) = if request.operation == Operation::MapCreate {
            let mut map = MapState::new(
                uuid::Uuid::new_v4().to_string(),
                string(&request.input, "name")?,
            )
            .map_err(super::map_store::core_error)?;
            if let Some(viewport) = request.input.get("viewport") {
                map.viewport = decode(viewport)?;
            }
            if let BasemapChange::Set(value) = basemap(&request.input)? {
                map.basemap = Some(value);
            }
            (self.store.create(request, caller, map, lease)?, Vec::new())
        } else {
            let observed = if request.operation == Operation::MapGet {
                self.store.get(
                    request
                        .map_id
                        .as_deref()
                        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
                    caller,
                )?
            } else {
                self.store.inspect(request, caller)?
            };
            let mut warnings = self.authorize(&runtime, &observed, caller).await?;
            if request.operation == Operation::MapGet {
                let mut response = success(request, map_data(&observed));
                response.warnings = warnings;
                return Ok(response);
            }
            if observed.replayed {
                (self.store.confirm_replay(request, caller, lease)?, warnings)
            } else {
                let change = self.change(&runtime, request, caller, lease).await?;
                warnings = self.authorize(&runtime, &observed, caller).await?;
                if let Some((prepared, scope)) = &change.fence {
                    let (connection, permission) =
                        self.connection(&runtime, &prepared.id, caller).await?;
                    if connection.auth_revision != prepared.auth_revision
                        || connection.driver != prepared.driver
                    {
                        return Err(failure(ErrorCode::AuthRequired));
                    }
                    if permission.identity != scope.identity {
                        return Err(failure(ErrorCode::PermissionDenied));
                    }
                    if let MapChange::Add {
                        layer,
                        binding: Some(binding),
                    } = &change.change
                    {
                        check_binding(binding, &connection, &permission)?;
                        if let LayerSource::QueryResult { result_id, .. } = &layer.source {
                            runtime.results.snapshot(result_id, binding)?;
                        }
                    }
                }
                (
                    self.store
                        .mutate(request, caller, &observed, change.change, lease)?,
                    warnings,
                )
            }
        };
        let mut response = success(request, map_data(&map));
        response.warnings = warnings;
        if request.operation == Operation::MapSave {
            response.data["saved"] = json!(true);
        }
        if let Some(gui_id) = gui_id {
            match self.apply(request, &map, lease, gui_id).await {
                Ok(ack) => {
                    response.data["gui_applied"] = json!(true);
                    response.data["state_version"] = json!(ack.state_version);
                    response.data["rendered_version"] = json!(ack.rendered_version);
                }
                Err(mut error) => {
                    error.outcome = Outcome::Applied;
                    if error.code == ErrorCode::ApplyTimeout {
                        error.details = json!({"state_version":map.map.version,"rendered_version":null,"gui_applied":false});
                    }
                    response.status = if error.code == ErrorCode::Cancelled {
                        JobStatus::Cancelled
                    } else {
                        JobStatus::Failed
                    };
                    response.error = Some(redact_error(error));
                    response.data["gui_applied"] = json!(false);
                    response.data["state_version"] = json!(map.map.version);
                    response.data["rendered_version"] = Value::Null;
                }
            }
        }
        Ok(response)
    }
}
fn check_binding(
    binding: &CursorBinding,
    connection: &SavedConnection,
    permission: &PermissionScope,
) -> ServiceResult<()> {
    if binding.connection_id != connection.id || binding.auth_revision != connection.auth_revision {
        return Err(failure(ErrorCode::AuthRequired));
    }
    if binding.permission_scope != permission.identity {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    Ok(())
}
fn basemap(input: &Value) -> ServiceResult<BasemapChange> {
    Ok(match input.get("basemap") {
        None => BasemapChange::Keep,
        Some(Value::Null) => BasemapChange::Clear,
        Some(value) => BasemapChange::Set(decode(value)?),
    })
}
fn string<'a>(value: &'a Value, key: &str) -> ServiceResult<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))
}
fn decode<T: DeserializeOwned>(value: &Value) -> ServiceResult<T> {
    serde_json::from_value(value.clone()).map_err(|_| failure(ErrorCode::InvalidArgument))
}
fn map_data(map: &StoredMap) -> Value {
    json!({"map":map.map,"generation":map.apply_generation})
}
fn success(request: &ServiceRequest, data: Value) -> ServiceResponse {
    ServiceResponse {
        protocol_version: 1,
        request_id: request.request_id.clone(),
        connection_id: request.connection_id.clone(),
        status: JobStatus::Succeeded,
        job_id: None,
        result_id: None,
        data,
        page: Page::default(),
        limits: Limits::default(),
        metrics: Metrics::default(),
        warnings: Vec::new(),
        error: None,
    }
}
