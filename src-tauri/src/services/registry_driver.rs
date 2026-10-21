use crate::services::{
    backend::{
        CancellationSignal, DriverBackend, DriverContext, DriverOutput, ReadOnlyGuarantee,
        SavedConnection, SessionFinish, TransientAuth,
    },
    contracts::{ErrorCode, Limits, Metrics, Operation, Outcome, ServiceRequest},
    error::{failure, ServiceResult},
    results::{
        CursorBinding, CursorState, ResultKind, ResultSet, ResultSnapshot, MAX_RESULT_BYTES,
        MAX_RESULT_ROWS,
    },
    security::value_hash,
};
use crate::{
    drivers::{
        driver_trait::{
            DatabaseDriver, DriverServiceCapabilities, DriverServiceReadOnly, ServiceRpcMethod,
        },
        registry,
    },
    models::{ConnectionParams, DatabaseSelection},
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[async_trait]
pub trait ServiceConnectionResolver: Send + Sync {
    async fn params(&self, connection: &SavedConnection) -> ServiceResult<ConnectionParams>;
}

struct ReservedSession {
    driver: Arc<dyn DatabaseDriver>,
    params: ConnectionParams,
    connection: SavedConnection,
    context: DriverContext,
    database: String,
    auth_identity: String,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
type CachedDriver = (String, Arc<dyn DatabaseDriver>);

pub struct RegistryDriverBackend {
    resolver: Arc<dyn ServiceConnectionResolver>,
    active: Mutex<HashMap<(String, String), Arc<dyn DatabaseDriver>>>,
    sessions: Mutex<HashMap<(String, String), ReservedSession>>,
    connections: Mutex<HashMap<String, CachedDriver>>,
    session_slots: Arc<tokio::sync::Semaphore>,
    auth_epochs: Mutex<HashMap<String, u64>>,
}

struct ActiveCall<'a> {
    backend: &'a RegistryDriverBackend,
    key: (String, String),
}
impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.backend.active.lock() {
            active.remove(&self.key);
        }
    }
}

impl RegistryDriverBackend {
    pub fn new(resolver: Arc<dyn ServiceConnectionResolver>) -> Self {
        Self {
            resolver,
            active: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            session_slots: Arc::new(tokio::sync::Semaphore::new(64)),
            auth_epochs: Mutex::new(HashMap::new()),
        }
    }
    async fn driver(&self, connection: &SavedConnection) -> ServiceResult<Arc<dyn DatabaseDriver>> {
        registry::get_driver(&connection.driver)
            .await
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))
    }
    fn track(
        &self,
        connection: &SavedConnection,
        context: &DriverContext,
        driver: Arc<dyn DatabaseDriver>,
    ) -> ServiceResult<ActiveCall<'_>> {
        if context.connection_id != connection.id {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let key = (connection.id.clone(), context.request_id.clone());
        let mut active = self
            .active
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        if active.contains_key(&key) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if active.len() >= 64 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        active.insert(key.clone(), driver.clone());
        self.connections
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .insert(connection.id.clone(), (connection.driver.clone(), driver));
        Ok(ActiveCall { backend: self, key })
    }
    fn session(
        &self,
        connection: &SavedConnection,
        context: &DriverContext,
        database: Option<&str>,
    ) -> ServiceResult<Option<(Arc<dyn DatabaseDriver>, ConnectionParams)>> {
        let Some(handle) = &context.session_handle else {
            return Ok(None);
        };
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        let session = sessions
            .get(&(connection.id.clone(), handle.clone()))
            .ok_or_else(|| failure(ErrorCode::PermissionDenied))?;
        if session.connection.driver != connection.driver
            || session.connection.auth_revision != connection.auth_revision
            || session.auth_identity != auth_identity(context)
            || session.context.read_only != context.read_only
            || database.is_some_and(|database| database != session.database)
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        Ok(Some((session.driver.clone(), session.params.clone())))
    }
    async fn resolved(
        &self,
        connection: &SavedConnection,
        context: &DriverContext,
        database: Option<&str>,
    ) -> ServiceResult<ConnectionParams> {
        if let Some((_, params)) = self.session(connection, context, database)? {
            return Ok(params);
        }
        let mut params = self.resolver.params(connection).await?;
        if params.driver != connection.driver
            || params
                .connection_id
                .as_deref()
                .is_some_and(|id| id != connection.id)
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        params.connection_id = Some(connection.id.clone());
        if let Some(database) = database {
            params.database = DatabaseSelection::Single(database.into());
        }
        Ok(params)
    }
    async fn invoke(
        &self,
        connection: &SavedConnection,
        method: ServiceRpcMethod,
        input: Value,
        database: Option<&str>,
        mut context: DriverContext,
    ) -> ServiceResult<Value> {
        let deadline = context_deadline(&context)?;
        let driver = match self.session(connection, &context, database)? {
            Some((driver, _)) => driver,
            None => self.driver(connection).await?,
        };
        let _active = self.track(connection, &context, driver.clone())?;
        let params = tokio::select! {
            result = self.resolved(connection, &context, database) => result?,
            _ = context.signal.cancelled() => return Err(failure(ErrorCode::Cancelled)),
            _ = tokio::time::sleep_until(deadline) => return Err(failure(ErrorCode::DeadlineExceeded)),
        };
        context.deadline_ms = remaining_ms(deadline)?;
        driver.service_call(method, &params, &context, input).await
    }
    async fn bounded_capabilities(
        &self,
        connection: &SavedConnection,
        context: &mut DriverContext,
    ) -> ServiceResult<DriverServiceCapabilities> {
        let deadline = context_deadline(context)?;
        let capabilities = tokio::select! {
            result = self.capabilities(connection) => result?,
            _ = context.signal.cancelled() => return Err(failure(ErrorCode::Cancelled)),
            _ = tokio::time::sleep_until(deadline) => return Err(failure(ErrorCode::DeadlineExceeded)),
        };
        context.deadline_ms = remaining_ms(deadline)?;
        Ok(capabilities)
    }
    async fn capabilities(
        &self,
        connection: &SavedConnection,
    ) -> ServiceResult<DriverServiceCapabilities> {
        self.driver(connection).await?.service_capabilities().await
    }
    pub async fn call_spatial(
        &self,
        connection: &SavedConnection,
        method: ServiceRpcMethod,
        input: Value,
        mut context: DriverContext,
    ) -> ServiceResult<Value> {
        if !matches!(
            method,
            ServiceRpcMethod::SpatialColumns
                | ServiceRpcMethod::SpatialTransformValues
                | ServiceRpcMethod::SpatialQueryTable
                | ServiceRpcMethod::SpatialReadFeature
        ) {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        if !context.read_only || context.session_handle.is_some() {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        if !self
            .bounded_capabilities(connection, &mut context)
            .await?
            .spatial
        {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
        let database = explicit_database(&input).map(str::to_owned);
        self.invoke(connection, method, input, database.as_deref(), context)
            .await
    }
}

#[async_trait]
impl DriverBackend for RegistryDriverBackend {
    async fn supports(
        &self,
        connection: &SavedConnection,
        operation: Operation,
    ) -> ServiceResult<bool> {
        let capabilities = match self.capabilities(connection).await {
            Ok(capabilities) => capabilities,
            Err(error) if error.code == ErrorCode::CapabilityUnavailable => return Ok(false),
            Err(error) => return Err(error),
        };
        if matches!(
            operation,
            Operation::SessionOpen
                | Operation::SessionCommit
                | Operation::SessionRollback
                | Operation::SessionClose
        ) {
            return Ok(capabilities.sessions);
        }
        Ok(operation_method(operation, capabilities.documents)
            .is_some_and(|method| capabilities.supports(method)))
    }
    async fn read_only_guarantee(
        &self,
        connection: &SavedConnection,
    ) -> ServiceResult<ReadOnlyGuarantee> {
        Ok(match self.capabilities(connection).await?.read_only {
            DriverServiceReadOnly::ReadOnlyTransaction => ReadOnlyGuarantee::ReadOnlyTransaction,
            DriverServiceReadOnly::RestrictedDatabasePrincipal => {
                ReadOnlyGuarantee::RestrictedDatabasePrincipal
            }
            DriverServiceReadOnly::ReadOnlySdkOperations => {
                ReadOnlyGuarantee::ReadOnlySdkOperations
            }
            DriverServiceReadOnly::Unavailable => ReadOnlyGuarantee::Unavailable,
        })
    }
    async fn execute(
        &self,
        connection: &SavedConnection,
        request: &ServiceRequest,
        mut context: DriverContext,
    ) -> ServiceResult<DriverOutput> {
        let capabilities = self.bounded_capabilities(connection, &mut context).await?;
        let method = operation_method(request.operation, capabilities.documents)
            .ok_or_else(|| failure(ErrorCode::UnsupportedOperation))?;
        if !capabilities.supports(method) {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
        if request.operation == Operation::QueryExecute
            && request.input["language"]
                != if capabilities.documents {
                    "cosmos_sql"
                } else {
                    "sql"
                }
        {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        if context.read_only
            && matches!(
                request.operation,
                Operation::RowInsert
                    | Operation::RowUpdate
                    | Operation::RowDelete
                    | Operation::DocumentCreate
                    | Operation::DocumentReplace
                    | Operation::DocumentDelete
            )
        {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        let input = match method {
            ServiceRpcMethod::ExactQuery => {
                let mut input = json!({"text":request.input["text"],"parameters":request.input["parameters"],"max_rows":10000,"max_bytes":16*1024*1024-64*1024});
                if let Some(schema) = request.input["schema"].as_str() {
                    input["schema"] = json!(schema);
                }
                input
            }
            ServiceRpcMethod::Catalog | ServiceRpcMethod::Row => {
                let mut input = request.input.clone();
                input["operation"] = serde_json::to_value(request.operation)
                    .map_err(|_| failure(ErrorCode::InvalidArgument))?;
                input
            }
            ServiceRpcMethod::Test | ServiceRpcMethod::CosmosTest => json!({}),
            _ => request.input.clone(),
        };
        let value = self
            .invoke(
                connection,
                method,
                input,
                explicit_database(&request.input),
                context.clone(),
            )
            .await?;
        let parsed = if method == ServiceRpcMethod::ExactQuery {
            sql_output(value)
        } else {
            native_output(value, connection, &request.input)
        };
        let mut output = parsed.map_err(|mut error| {
            if !context.read_only {
                error.outcome = Outcome::Applied;
            }
            error
        })?;
        if method == ServiceRpcMethod::ExactQuery && output.limits.truncated {
            if let Some(handle) = &context.session_handle {
                self.discard_session(&connection.id, handle).await;
                let mut error = failure(ErrorCode::ResourceLimit);
                error.outcome = Outcome::Unknown;
                error.details = json!({"session_closed":true});
                return Err(error);
            }
        }
        if method == ServiceRpcMethod::CosmosColumns {
            output.data = cosmos_columns(output.data)?;
        }
        if matches!(
            method,
            ServiceRpcMethod::CosmosQuery | ServiceRpcMethod::CosmosQueryPage
        ) && output.snapshot.is_none()
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if capabilities.documents && context.read_only {
            output
                .warnings
                .push("cosmos_read_only_database_permissions_unverified".into());
        }
        validate_output_metadata(&output).map_err(|mut error| {
            if !context.read_only {
                error.outcome = Outcome::Applied;
            }
            error
        })?;
        Ok(output)
    }
    async fn continue_cursor(
        &self,
        connection: &SavedConnection,
        state: &CursorState,
        mut context: DriverContext,
    ) -> ServiceResult<DriverOutput> {
        if !self
            .bounded_capabilities(connection, &mut context)
            .await?
            .query_page
        {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
        let query = state
            .native_state
            .get("query")
            .ok_or_else(|| failure(ErrorCode::InvalidPageToken))?;
        let cursor = json!({"binding":{"sdk_version":state.binding.sdk_version,"container_rid":state.binding.container_rid,"endpoint_identity":state.binding.endpoint_identity,"partition_hash":state.binding.partition_hash},"native_state":state.native_state,"tail":state.tail,"columns":state.columns,"page_size":state.page_size,"expires_at_ms":state.expires_at_ms});
        let value = self
            .invoke(
                connection,
                ServiceRpcMethod::CosmosQueryPage,
                json!({"query":query,"cursor_state":cursor}),
                explicit_database(query),
                context,
            )
            .await?;
        let mut output = native_output(value, connection, query)?;
        if output.snapshot.is_none() {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        output
            .warnings
            .push("cosmos_read_only_database_permissions_unverified".into());
        validate_output_metadata(&output)?;
        Ok(output)
    }
    async fn cancel(&self, connection_id: &str, request_id: &str) -> ServiceResult<()> {
        let driver = self
            .active
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .get(&(connection_id.into(), request_id.into()))
            .cloned();
        if let Some(driver) = driver {
            driver.service_cancel(connection_id, request_id).await?;
        }
        Ok(())
    }
    async fn reserve_session(
        &self,
        connection: &SavedConnection,
        database: &str,
        mut context: DriverContext,
    ) -> ServiceResult<String> {
        if context.session_handle.is_some() {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let permit = self
            .session_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        if !self
            .bounded_capabilities(connection, &mut context)
            .await?
            .sessions
        {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        let deadline = context_deadline(&context)?;
        let driver = self.driver(connection).await?;
        let epoch = *self
            .auth_epochs
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .entry(connection.id.clone())
            .or_default();
        let _active = self.track(connection, &context, driver.clone())?;
        let params = tokio::select! {
            result = self.resolved(connection, &context, Some(database)) => result?,
            _ = context.signal.cancelled() => return Err(failure(ErrorCode::Cancelled)),
            _ = tokio::time::sleep_until(deadline) => return Err(failure(ErrorCode::DeadlineExceeded)),
        };
        context.deadline_ms = remaining_ms(deadline)?;
        let response = driver.service_call(ServiceRpcMethod::SessionOpen, &params, &context, json!({"database":database,"mode":if context.read_only {"read_only"} else {"write"}})).await?;
        let handle = response
            .get("session_handle")
            .and_then(Value::as_str)
            .filter(|handle| !handle.is_empty() && handle.len() <= 128)
            .ok_or_else(|| {
                let mut error = failure(ErrorCode::OutcomeUnknown);
                error.outcome = Outcome::Unknown;
                error
            })?
            .to_owned();
        let inserted = {
            let epochs = self
                .auth_epochs
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            let key = (connection.id.clone(), handle.clone());
            if response["in_transaction"] != true || response["rollback_confirmed"] != false {
                let mut error = failure(ErrorCode::OutcomeUnknown);
                error.outcome = Outcome::Unknown;
                Err(error)
            } else if epochs.get(&connection.id).copied().unwrap_or_default() != epoch {
                Err(failure(ErrorCode::AuthRequired))
            } else if let std::collections::hash_map::Entry::Vacant(entry) = sessions.entry(key) {
                entry.insert(ReservedSession {
                    driver: driver.clone(),
                    params: params.clone(),
                    connection: connection.clone(),
                    auth_identity: auth_identity(&context),
                    context: context.clone(),
                    database: database.into(),
                    _permit: permit,
                });
                Ok(())
            } else {
                Err(failure(ErrorCode::ResourceLimit))
            }
        };
        if let Err(error) = inserted {
            context.request_id = uuid::Uuid::new_v4().to_string();
            context.session_handle = Some(handle);
            context.deadline_ms = 2000;
            context.signal = CancellationSignal::default();
            let _ = driver
                .service_call(
                    ServiceRpcMethod::SessionDiscard,
                    &params,
                    &context,
                    json!({}),
                )
                .await;
            return Err(error);
        }
        Ok(handle)
    }
    async fn finish_session(
        &self,
        connection: &SavedConnection,
        handle: &str,
        finish: SessionFinish,
        mut context: DriverContext,
    ) -> ServiceResult<()> {
        context.session_handle = Some(handle.into());
        let value = self
            .invoke(
                connection,
                ServiceRpcMethod::SessionFinish,
                json!({"action":if finish == SessionFinish::Commit {"commit"} else {"rollback"}}),
                None,
                context,
            )
            .await?;
        if value["session_handle"] != handle
            || value["in_transaction"] != false
            || !value["rollback_confirmed"].is_boolean()
            || (finish == SessionFinish::Rollback && value["rollback_confirmed"] != true)
        {
            let mut error = failure(ErrorCode::OutcomeUnknown);
            error.outcome = Outcome::Unknown;
            return Err(error);
        }
        self.sessions
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .remove(&(connection.id.clone(), handle.into()));
        Ok(())
    }
    async fn discard_session(&self, connection_id: &str, handle: &str) {
        let session = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&(connection_id.into(), handle.into())));
        if let Some(mut session) = session {
            session.context.request_id = uuid::Uuid::new_v4().to_string();
            session.context.deadline_ms = 2000;
            session.context.signal = CancellationSignal::default();
            session.context.session_handle = Some(handle.into());
            let _ = session
                .driver
                .service_call(
                    ServiceRpcMethod::SessionDiscard,
                    &session.params,
                    &session.context,
                    json!({}),
                )
                .await;
        }
    }
    async fn invalidate_auth(&self, connection_id: &str, _previous_principal: Option<&str>) {
        if let Ok(mut epochs) = self.auth_epochs.lock() {
            let epoch = epochs.entry(connection_id.into()).or_default();
            *epoch = epoch.wrapping_add(1);
        }
        let active: Vec<_> = self
            .active
            .lock()
            .map(|active| {
                active
                    .iter()
                    .filter(|((connection, _), _)| connection == connection_id)
                    .map(|((_, request), driver)| (request.clone(), driver.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (request, driver) in active {
            let _ = driver.service_cancel(connection_id, &request).await;
        }
        let handles: Vec<_> = self
            .sessions
            .lock()
            .map(|sessions| {
                sessions
                    .keys()
                    .filter(|(connection, _)| connection == connection_id)
                    .map(|(_, handle)| handle.clone())
                    .collect()
            })
            .unwrap_or_default();
        for handle in handles {
            self.discard_session(connection_id, &handle).await;
        }
        let connection = self
            .connections
            .lock()
            .ok()
            .and_then(|mut connections| connections.remove(connection_id));
        if let Some((driver_id, driver)) = connection {
            let context = DriverContext {
                connection_id: connection_id.into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                deadline_ms: 2000,
                signal: CancellationSignal::default(),
                transient_auth: None,
                read_only: true,
                session_handle: None,
            };
            let params = ConnectionParams {
                driver: driver_id,
                connection_id: Some(connection_id.into()),
                ..ConnectionParams::default()
            };
            let _ = driver
                .service_call(
                    ServiceRpcMethod::InvalidateAuth,
                    &params,
                    &context,
                    json!({}),
                )
                .await;
            driver
                .invalidate_connection_metadata(Some(connection_id))
                .await;
        }
    }
}

fn context_deadline(context: &DriverContext) -> ServiceResult<tokio::time::Instant> {
    if context.deadline_ms == 0 || context.deadline_ms > 120_000 {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    if context.signal.is_cancelled() {
        return Err(failure(ErrorCode::Cancelled));
    }
    Ok(tokio::time::Instant::now() + Duration::from_millis(context.deadline_ms))
}
fn remaining_ms(deadline: tokio::time::Instant) -> ServiceResult<u64> {
    let remaining = deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .as_millis() as u64;
    if remaining == 0 {
        Err(failure(ErrorCode::DeadlineExceeded))
    } else {
        Ok(remaining)
    }
}
fn explicit_database(input: &Value) -> Option<&str> {
    input
        .get("database")
        .and_then(Value::as_str)
        .or_else(|| {
            input
                .get("table")
                .and_then(|table| table.get("database"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            input
                .get("query")
                .and_then(|query| query.get("database"))
                .and_then(Value::as_str)
        })
}
fn auth_identity(context: &DriverContext) -> String {
    match &context.transient_auth {
        Some(
            TransientAuth::AccountKey { identity, .. } | TransientAuth::DbPassword { identity, .. },
        ) => identity.clone(),
        Some(TransientAuth::EntraToken {
            tenant_id,
            client_id,
            principal_id,
            scope,
            ..
        }) => value_hash(
            &json!({"tenant_id":tenant_id,"client_id":client_id,"principal_id":principal_id,"scope":scope}),
        ),
        None => String::new(),
    }
}
fn operation_method(operation: Operation, documents: bool) -> Option<ServiceRpcMethod> {
    Some(match operation {
        Operation::ConnectionTest => {
            if documents {
                ServiceRpcMethod::CosmosTest
            } else {
                ServiceRpcMethod::Test
            }
        }
        Operation::QueryExecute => {
            if documents {
                ServiceRpcMethod::CosmosQuery
            } else {
                ServiceRpcMethod::ExactQuery
            }
        }
        Operation::QueryNext if documents => ServiceRpcMethod::CosmosQueryPage,
        Operation::CatalogDatabases if documents => ServiceRpcMethod::CosmosDatabases,
        Operation::CatalogObjects if documents => ServiceRpcMethod::CosmosTables,
        Operation::CatalogDescribe if documents => ServiceRpcMethod::CosmosColumns,
        Operation::CatalogDatabases
        | Operation::CatalogSchemas
        | Operation::CatalogObjects
        | Operation::CatalogDescribe
            if !documents =>
        {
            ServiceRpcMethod::Catalog
        }
        Operation::RowInsert | Operation::RowUpdate | Operation::RowDelete if !documents => {
            ServiceRpcMethod::Row
        }
        Operation::DocumentRead if documents => ServiceRpcMethod::DocumentRead,
        Operation::DocumentCreate if documents => ServiceRpcMethod::DocumentCreate,
        Operation::DocumentReplace if documents => ServiceRpcMethod::DocumentReplace,
        Operation::DocumentDelete if documents => ServiceRpcMethod::DocumentDelete,
        _ => return None,
    })
}

fn empty_output() -> DriverOutput {
    DriverOutput {
        data: Value::Null,
        snapshot: None,
        cursor: None,
        confirmed_cancelled: false,
        session_closed: false,
        metrics: Metrics::default(),
        limits: Limits::default(),
        warnings: vec![],
    }
}
fn warnings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|warnings| {
            warnings
                .iter()
                .filter_map(Value::as_str)
                .filter(|warning| {
                    !warning.is_empty()
                        && warning.len() <= 128
                        && warning.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                        })
                })
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
fn validate_output_metadata(output: &DriverOutput) -> ServiceResult<()> {
    let safe_integer = |value: u64| value <= 9_007_199_254_740_991;
    if !safe_integer(output.metrics.elapsed_ms)
        || !safe_integer(output.metrics.retry_count)
        || output
            .metrics
            .request_charge
            .is_some_and(|charge| !charge.is_finite() || charge < 0.0)
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let reason = |value: &String| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    };
    if !output.limits.reasons.iter().all(reason)
        || output
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.limits.reasons.iter().all(reason))
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let snapshot = output
        .snapshot
        .as_ref()
        .map(|snapshot| json!({"limits":snapshot.limits,"warnings":snapshot.warnings}));
    let envelope = json!({"metrics":output.metrics,"limits":output.limits,"warnings":output.warnings,"snapshot_metadata":snapshot});
    if serde_json::to_vec(&envelope)
        .map_err(|_| failure(ErrorCode::ResourceLimit))?
        .len()
        > 32 * 1024
    {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    Ok(())
}
fn sql_output(value: Value) -> ServiceResult<DriverOutput> {
    let mut pending = vec![&value];
    let mut sets = Vec::new();
    let mut rows_total = 0usize;
    let mut limits = Limits::default();
    let mut all_warnings = Vec::new();
    while let Some(set) = pending.pop() {
        let source_limits: Limits = match set.get("limits") {
            Some(source) => serde_json::from_value(source.clone())
                .map_err(|_| failure(ErrorCode::InvalidArgument))?,
            None => Limits {
                truncated: set["truncated"].as_bool().unwrap_or(false),
                reasons: vec![],
            },
        };
        limits.truncated |= source_limits.truncated;
        for reason in source_limits.reasons {
            if !limits.reasons.contains(&reason) {
                limits.reasons.push(reason);
            }
        }
        all_warnings.extend(warnings(&set["warnings"]));
        let names = set["columns"]
            .as_array()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let rows = set["rows"]
            .as_array()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let metadata = set.get("column_metadata").and_then(Value::as_array);
        if metadata.is_some_and(|metadata| metadata.len() != names.len())
            || rows
                .iter()
                .any(|row| row.as_array().map_or(true, |row| row.len() != names.len()))
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        rows_total = rows_total.saturating_add(rows.len());
        if rows_total > MAX_RESULT_ROWS {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let columns = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let name = name
                    .as_str()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
                let mut column = metadata
                    .and_then(|metadata| metadata.get(index))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if !column.is_object() {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
                column["name"] = name.into();
                column["column_index"] = index.into();
                Ok(column)
            })
            .collect::<ServiceResult<Vec<_>>>()?;
        sets.push(ResultSet {
            kind: ResultKind::Tabular,
            columns,
            rows: rows.clone(),
        });
        if let Some(additional) = set
            .get("additional_results")
            .filter(|value| !value.is_null())
        {
            for extra in additional
                .as_array()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
                .iter()
                .rev()
            {
                pending.push(extra);
            }
        }
    }
    let mut output = empty_output();
    output.limits = limits.clone();
    output.warnings = all_warnings;
    let snapshot = ResultSnapshot {
        sets,
        revision: uuid::Uuid::new_v4().to_string(),
        limits,
        allow_truncation: false,
        warnings: output.warnings.clone(),
    };
    if serde_json::to_vec(&snapshot)
        .map_err(|_| failure(ErrorCode::ResourceLimit))?
        .len()
        > MAX_RESULT_BYTES
    {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    output.snapshot = Some(snapshot);
    if let Some(affected) = value.get("affected_rows").filter(|value| {
        value.is_null()
            || value
                .as_u64()
                .is_some_and(|count| count <= 9_007_199_254_740_991)
    }) {
        output.data = json!({"affected_rows":affected});
    }
    validate_output_metadata(&output)?;
    Ok(output)
}
fn cosmos_columns(value: Value) -> ServiceResult<Value> {
    if value.is_array() {
        return Ok(json!({"columns":value}));
    }
    if !value.is_object() || !value["columns"].is_array() {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let mut normalized = json!({"columns":value["columns"]});
    for field in [
        "partition_key_paths",
        "partition_key_kind",
        "partition_key_version",
        "system_key",
    ] {
        if let Some(metadata) = value.get(field) {
            let valid = match field {
                "partition_key_paths" => metadata
                    .as_array()
                    .is_some_and(|paths| paths.iter().all(Value::is_string)),
                "partition_key_kind" => metadata.is_string(),
                "partition_key_version" => metadata.as_u64().is_some(),
                "system_key" => metadata.is_boolean(),
                _ => false,
            };
            if !valid {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            normalized[field] = metadata.clone();
        }
    }
    Ok(normalized)
}
fn native_output(
    mut value: Value,
    connection: &SavedConnection,
    input: &Value,
) -> ServiceResult<DriverOutput> {
    let mut output = empty_output();
    if value.get("snapshot").is_none()
        && value
            .get("data")
            .is_some_and(|data| data.get("snapshot").is_some())
    {
        let envelope = value;
        value = envelope["data"].clone();
        for field in ["metrics", "limits"] {
            if let Some(metadata) = envelope.get(field) {
                value[field] = metadata.clone();
            }
        }
        let mut combined = warnings(&value["warnings"]);
        combined.extend(warnings(&envelope["warnings"]));
        value["warnings"] = json!(combined);
    }
    if value.get("snapshot").is_none() && value.get("cursor").is_some() {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    output.metrics = match value.get("metrics") {
        Some(metrics) => serde_json::from_value(metrics.clone())
            .map_err(|_| failure(ErrorCode::InvalidArgument))?,
        None => Metrics::default(),
    };
    output.limits = match value.get("limits") {
        Some(limits) => serde_json::from_value(limits.clone())
            .map_err(|_| failure(ErrorCode::InvalidArgument))?,
        None => Limits::default(),
    };
    output.warnings = warnings(&value["warnings"]);
    output.session_closed = value["session_closed"].as_bool().unwrap_or(false);
    output.confirmed_cancelled = value["confirmed_cancelled"].as_bool().unwrap_or(false);
    if let Some(snapshot) = value.get("snapshot") {
        let mut snapshot: ResultSnapshot = serde_json::from_value(snapshot.clone())
            .map_err(|_| failure(ErrorCode::InvalidArgument))?;
        snapshot.warnings = warnings(&json!(snapshot.warnings));
        if snapshot.sets.len() != 1
            || snapshot
                .sets
                .iter()
                .any(|set| !matches!(set.kind, ResultKind::Documents | ResultKind::JsonValues))
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if let Some(cursor) = value.get("cursor").filter(|cursor| !cursor.is_null()) {
            let binding = &cursor["binding"];
            let string = |field: &str| -> ServiceResult<Option<String>> {
                binding[field]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(|value| Some(value.into()))
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))
            };
            output.cursor = Some(CursorState {
                binding: CursorBinding {
                    connection_id: connection.id.clone(),
                    permission_scope: String::new(),
                    auth_revision: connection.auth_revision.clone(),
                    query_hash: value_hash(input),
                    result_revision: snapshot.revision.clone(),
                    sdk_version: string("sdk_version")?,
                    container_rid: string("container_rid")?,
                    endpoint_identity: string("endpoint_identity")?,
                    partition_hash: string("partition_hash")?,
                },
                native_state: cursor
                    .get("native_state")
                    .cloned()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
                tail: cursor["tail"]
                    .as_array()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
                    .clone(),
                columns: cursor["columns"]
                    .as_array()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
                    .clone(),
                page_size: cursor["page_size"]
                    .as_u64()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
                    .try_into()
                    .map_err(|_| failure(ErrorCode::InvalidArgument))?,
                expires_at_ms: cursor["expires_at_ms"]
                    .as_u64()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
            });
        }
        output.snapshot = Some(snapshot);
        output.data = value.get("data").cloned().unwrap_or(Value::Null);
    } else {
        output.data = value.get("data").cloned().unwrap_or(value);
    }
    validate_output_metadata(&output)?;
    Ok(output)
}

#[cfg(test)]
#[path = "tests/registry_driver.rs"]
mod tests;
