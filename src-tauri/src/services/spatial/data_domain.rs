use super::{
    driver::SpatialDriverBackend,
    feature,
    paging::{self, Publication},
    query_result, table_query, MapStore,
};
use crate::{
    drivers::driver_trait::ServiceRpcMethod,
    services::{
        backend::{DriverContext, PermissionScope, SavedConnection},
        contracts::{
            ErrorCode, JobStatus, Limits, Metrics, Operation, Page, ServiceRequest, ServiceResponse,
        },
        dispatch::CallerContext,
        error::{failure, ServiceResult},
        extensions::OperationExtension,
        jobs::JobLease,
        results::{CursorBinding, SpatialNextPage, StoredPage},
        runtime::ServiceRuntime,
        storage::now_ms,
    },
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Weak},
    time::Duration,
};
use tabularis_spatial_core::{LayerSource, TableRef};

const OPERATIONS: &[Operation] = &[
    Operation::SpatialColumns,
    Operation::SpatialQueryResult,
    Operation::SpatialTableQuery,
    Operation::SpatialFeature,
];
pub struct SpatialDataDomain {
    runtime: Weak<ServiceRuntime>,
    driver: Arc<dyn SpatialDriverBackend>,
    maps: Option<Arc<MapStore>>,
}
pub struct PreparedExecution {
    pub runtime: Arc<ServiceRuntime>,
    pub connection: SavedConnection,
    pub permission: PermissionScope,
    context: DriverContext,
    request: ServiceRequest,
    caller: CallerContext,
    deadline_at_ms: u64,
    map_fence: Option<(String, u64, u64)>,
}

impl SpatialDataDomain {
    pub fn attach(
        runtime: &Arc<ServiceRuntime>,
        driver: Arc<dyn SpatialDriverBackend>,
        maps: Option<Arc<MapStore>>,
    ) -> ServiceResult<Arc<Self>> {
        let domain = Arc::new(Self {
            runtime: Arc::downgrade(runtime),
            driver,
            maps,
        });
        runtime.register_extension(domain.clone())?;
        Ok(domain)
    }
    pub fn guard(&self, execution: &PreparedExecution) -> ServiceResult<()> {
        if execution.context.signal.is_cancelled() {
            return Err(failure(ErrorCode::Cancelled));
        }
        if now_ms() >= execution.deadline_at_ms {
            return Err(failure(ErrorCode::DeadlineExceeded));
        }
        Ok(())
    }
    async fn prepare(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
        source: Option<&LayerSource>,
    ) -> ServiceResult<PreparedExecution> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
        let id = request
            .connection_id
            .as_deref()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        if request.session_id.is_some() {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        let connection = runtime.connections.get(id).await?;
        let permission = runtime.policy.permission(&connection, caller).await?;
        if connection.id != id || permission.identity.is_empty() || permission.identity.len() > 512
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let remaining = lease.deadline_at_ms().saturating_sub(now_ms()).min(120_000);
        let transient_auth = runtime.auth.context(&connection, remaining).await?;
        let map_fence = if let Some(id) = &request.map_id {
            let store = self
                .maps
                .as_ref()
                .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
            let map = store.get(id, caller)?;
            if request
                .expected_version
                .is_some_and(|version| version != map.map.version)
            {
                return Err(super::map_store::conflict(map.map.version, false));
            }
            let requested_table = request
                .input
                .get("table")
                .map(paging::decode::<TableRef>)
                .transpose()?;
            let layer = map.map.layers.iter().find(|layer| layer.connection_id == connection.id && match source { Some(source) => layer.source == *source, None => matches!((&layer.source, &requested_table), (LayerSource::Table { table, .. }, Some(requested)) if table == requested) }).ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
            if let LayerSource::QueryResult { .. } = &layer.source {
                let binding = map
                    .source_bindings
                    .get(&layer.layer_id)
                    .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
                authorize_binding(binding, &connection, &permission)?;
            }
            Some((id.clone(), map.map.version, map.apply_generation))
        } else {
            None
        };
        let execution = PreparedExecution {
            runtime,
            context: DriverContext {
                connection_id: id.into(),
                request_id: request.request_id.clone(),
                deadline_ms: remaining,
                signal: lease.signal(),
                transient_auth,
                read_only: true,
                session_handle: None,
            },
            connection,
            permission,
            request: request.clone(),
            caller: caller.clone(),
            deadline_at_ms: lease.deadline_at_ms(),
            map_fence,
        };
        self.revalidate(&execution).await?;
        Ok(execution)
    }
    pub async fn revalidate(&self, execution: &PreparedExecution) -> ServiceResult<()> {
        self.guard(execution)?;
        let current = execution
            .runtime
            .connections
            .get(&execution.connection.id)
            .await?;
        let permission = execution
            .runtime
            .policy
            .permission(&current, &execution.caller)
            .await?;
        if current.id != execution.connection.id
            || current.driver != execution.connection.driver
            || current.auth_revision != execution.connection.auth_revision
        {
            return Err(failure(ErrorCode::AuthRequired));
        }
        if permission.identity != execution.permission.identity {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        if let Some((id, version, generation)) = &execution.map_fence {
            let map = self
                .maps
                .as_ref()
                .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?
                .get(id, &execution.caller)?;
            if map.map.version != *version || map.apply_generation != *generation {
                return Err(super::map_store::conflict(map.map.version, false));
            }
        }
        self.guard(execution)
    }
    pub async fn rpc(
        &self,
        execution: &PreparedExecution,
        method: ServiceRpcMethod,
        input: Value,
    ) -> ServiceResult<Value> {
        self.revalidate(execution).await?;
        let mut context = execution.context.clone();
        context.deadline_ms = execution
            .deadline_at_ms
            .saturating_sub(now_ms())
            .min(120_000);
        let signal = context.signal.clone();
        let call = tokio::time::timeout(
            Duration::from_millis(context.deadline_ms),
            self.driver
                .call(&execution.connection, method, input, context),
        );
        let result = tokio::select! { biased; _ = signal.cancelled() => { let _ = self.driver.cancel(&execution.connection.id, &execution.request.request_id).await; Err(failure(ErrorCode::Cancelled)) }, result = call => match result { Ok(result) => result, Err(_) => { let _ = self.driver.cancel(&execution.connection.id, &execution.request.request_id).await; Err(failure(ErrorCode::DeadlineExceeded)) } } }?;
        self.revalidate(execution).await?;
        Ok(result)
    }
    async fn query(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
    ) -> ServiceResult<ServiceResponse> {
        let source = paging::source_from_input(
            &request.input,
            if request.operation == Operation::SpatialQueryResult {
                "query_result"
            } else {
                "table"
            },
        )?;
        let size = paging::page_size(&request.input)?;
        let hash = paging::request_hash(&source, size, request.operation)?;
        let execution = self.prepare(request, caller, lease, Some(&source)).await?;
        let token = request.input["next_token"].as_str();
        let binding = if let LayerSource::QueryResult { result_id, .. } = &source {
            let binding = execution.runtime.results.result_binding(result_id)?;
            authorize_binding(&binding, &execution.connection, &execution.permission)?;
            execution.runtime.results.snapshot(result_id, &binding)?;
            binding
        } else if let Some(token) = token {
            let binding = execution.runtime.results.token_binding(token)?;
            authorize_binding(&binding, &execution.connection, &execution.permission)?;
            if binding.query_hash != paging::source_hash(&source)? {
                return Err(failure(ErrorCode::InvalidPageToken));
            }
            binding
        } else {
            CursorBinding {
                connection_id: execution.connection.id.clone(),
                permission_scope: execution.permission.identity.clone(),
                auth_revision: execution.connection.auth_revision.clone(),
                query_hash: paging::source_hash(&source)?,
                result_revision: uuid::Uuid::new_v4().to_string(),
                sdk_version: None,
                container_rid: None,
                endpoint_identity: None,
                partition_hash: None,
            }
        };
        let state = if let Some(token) = token {
            execution
                .runtime
                .results
                .check_spatial_request(token, &binding, &hash)?;
            match execution
                .runtime
                .results
                .begin_spatial_next(token, &binding)?
            {
                SpatialNextPage::Replay(page) => {
                    self.revalidate(&execution).await?;
                    return Ok(response(request, page));
                }
                SpatialNextPage::State(state) => Some(state.state),
            }
        } else {
            None
        };
        let result = self
            .build_and_publish(&execution, &source, &binding, size, state, token)
            .await;
        if result.is_err() {
            if let Some(token) = token {
                let _ = execution.runtime.results.fail_spatial_next(token, &binding);
            }
        }
        result
    }
    async fn build_and_publish(
        &self,
        execution: &PreparedExecution,
        source: &LayerSource,
        binding: &CursorBinding,
        size: usize,
        state: Option<Value>,
        consumed_token: Option<&str>,
    ) -> ServiceResult<ServiceResponse> {
        let hash = paging::request_hash(source, size, execution.request.operation)?;
        let reservation = execution.runtime.results.reserve_query(
            &execution.connection.id,
            execution.deadline_at_ms.saturating_sub(now_ms()),
        )?;
        let mut built = match source {
            LayerSource::QueryResult { .. } => {
                query_result::build(
                    self,
                    execution,
                    source,
                    binding,
                    size,
                    state.as_ref().map(paging::decode).transpose()?,
                )
                .await?
            }
            LayerSource::Table { .. } => {
                table_query::build(
                    self,
                    execution,
                    source,
                    size,
                    state.as_ref().map(paging::decode).transpose()?,
                )
                .await?
            }
        };
        self.revalidate(execution).await?;
        if let Some(id) = &built.original_result_id {
            execution.runtime.results.snapshot(id, binding)?;
        }
        self.guard(execution)?;
        let mut publication = Publication::new(&execution.runtime.results, binding);
        let result_id = publication.cache(
            paging::snapshot_page(&built.page, source, binding, &built.raw_rows)?,
            &reservation,
        )?;
        if let Some(state) = built.state {
            let token = publication.token(
                paging::cursor_state(binding, state, size),
                built.original_result_id.as_deref(),
                &hash,
            )?;
            built.page.page.next_token = Some(token);
        }
        let page = paging::stored_page(built.page, result_id)?;
        let page = if let Some(token) = consumed_token {
            execution
                .runtime
                .results
                .finish_spatial_next(token, binding, page)?
        } else {
            page
        };
        self.guard(execution)?;
        if let Some(id) = &built.original_result_id {
            execution.runtime.results.snapshot(id, binding)?;
        }
        publication.commit();
        Ok(response(&execution.request, page))
    }
}
#[async_trait]
impl OperationExtension for SpatialDataDomain {
    fn operations(&self) -> &[Operation] {
        OPERATIONS
    }
    async fn execute(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
    ) -> ServiceResult<ServiceResponse> {
        match request.operation {
            Operation::SpatialQueryResult | Operation::SpatialTableQuery => {
                self.query(request, caller, lease).await
            }
            Operation::SpatialColumns => {
                let execution = self.prepare(request, caller, lease, None).await?;
                let raw = self
                    .rpc(
                        &execution,
                        ServiceRpcMethod::SpatialColumns,
                        json!({"table":request.input["table"]}),
                    )
                    .await?;
                let data = normalize_columns(raw)?;
                self.revalidate(&execution).await?;
                Ok(success(request, data))
            }
            Operation::SpatialFeature => feature::execute(self, request, caller, lease).await,
            _ => Err(failure(ErrorCode::UnsupportedOperation)),
        }
    }
}
pub fn authorize_binding(
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
pub(super) async fn prepare_feature(
    domain: &SpatialDataDomain,
    request: &ServiceRequest,
    caller: &CallerContext,
    lease: &JobLease,
    source: Option<&LayerSource>,
) -> ServiceResult<PreparedExecution> {
    domain.prepare(request, caller, lease, source).await
}
fn normalize_columns(mut value: Value) -> ServiceResult<Value> {
    if serde_json::to_vec(&value)
        .map_err(|_| failure(ErrorCode::ResourceLimit))?
        .len()
        > paging::PUBLIC_PAGE_BYTES
    {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    let columns = value["columns"]
        .as_array_mut()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    for column in columns {
        let object = column
            .as_object_mut()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        if let Some(name) = object.remove("column") {
            if !name.is_string() || object.get("name").is_some_and(|existing| *existing != name) {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            object.insert("name".into(), name);
        }
        if !object.get("name").is_some_and(Value::is_string)
            || !object
                .get("column_index")
                .is_some_and(|index| index.as_u64().is_some())
            || !matches!(
                object.get("native_type").and_then(Value::as_str),
                Some("geometry" | "geography")
            )
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
    }
    Ok(value)
}
pub fn success(request: &ServiceRequest, data: Value) -> ServiceResponse {
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
        warnings: vec![],
        error: None,
    }
}
fn response(request: &ServiceRequest, page: StoredPage) -> ServiceResponse {
    ServiceResponse {
        result_id: page.result_id,
        data: page.data,
        page: page.page,
        limits: page.limits,
        metrics: page.metrics,
        warnings: page.warnings,
        ..success(request, Value::Null)
    }
}
