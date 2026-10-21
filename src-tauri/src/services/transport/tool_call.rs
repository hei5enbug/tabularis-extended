//! Running MCP tools: flat arguments become a service request, asynchronous
//! jobs reply as soon as they are registered, and results are presented
//! within the response limit.

use super::super::{
    backend::{CancellationSignal, SavedConnection},
    contracts::{
        ErrorCode, JobStatus, Operation, Outcome, ServiceError, ServiceRequest, ServiceResponse,
    },
    dispatch::{CallerContext, CallerTransport},
    error::{failure, ServiceResult},
    history::{HistoryQuery, MAX_LIST_LIMIT},
    policy::accepts_write_intent,
};
use super::{
    present, request,
    tool_catalog::{ToolKind, ToolSpec, DEFAULT_WAIT_MS, MAX_WAIT_MS, OBJECT_KINDS},
    CatalogTraits, ServiceTransport, SqlFlavor,
};
use serde_json::{json, Map, Value};
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;

const POLL_MS: u64 = 25;

/// Per-call facts the envelope does not carry.
#[derive(Default)]
struct Extras {
    write: Option<bool>,
    wait_ms: Option<u64>,
    kind: Option<String>,
    database: Option<String>,
    schema: Option<String>,
}

enum Waited {
    Terminal(ServiceResponse),
    Pending(ServiceResponse),
}

fn forbidden_name(name: &str) -> bool {
    matches!(
        name.replace('.', "_").as_str(),
        "credential_import" | "connection_delete"
    )
}

fn text<'a>(args: &'a Map<String, Value>, key: &str) -> ServiceResult<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text))
            if !text.is_empty() && !text.chars().any(char::is_control) && text.len() <= 1024 =>
        {
            Ok(Some(text))
        }
        _ => Err(failure(ErrorCode::InvalidArgument)),
    }
}
fn bounded(
    args: &Map<String, Value>,
    key: &str,
    minimum: u64,
    maximum: u64,
) -> ServiceResult<Option<u64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|number| (minimum..=maximum).contains(number))
            .map(Some)
            .ok_or_else(|| failure(ErrorCode::InvalidArgument)),
    }
}

fn error_with_kind(kind: &str) -> ServiceError {
    let mut error = failure(ErrorCode::UnsupportedOperation);
    error.details = json!({"object_kind":kind});
    error
}

fn quote(flavor: SqlFlavor, name: &str) -> String {
    match flavor {
        SqlFlavor::Mssql => format!("[{}]", name.replace(']', "]]")),
        SqlFlavor::Mysql => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}
fn identifier(name: &str) -> ServiceResult<&str> {
    if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(name)
}

/// The preview statement, with every identifier quoted for the dialect.
pub(crate) fn preview_sql(
    flavor: SqlFlavor,
    schema: Option<&str>,
    table: &str,
    limit: u64,
) -> ServiceResult<String> {
    let mut target = String::new();
    if let Some(schema) = schema {
        target.push_str(&quote(flavor, identifier(schema)?));
        target.push('.');
    }
    target.push_str(&quote(flavor, identifier(table)?));
    Ok(match flavor {
        SqlFlavor::Mssql => format!("SELECT TOP ({limit}) * FROM {target}"),
        _ => format!("SELECT * FROM {target} LIMIT {limit}"),
    })
}

impl ServiceTransport {
    pub fn tools(&self) -> ServiceResult<Value> {
        let mut tools = Vec::new();
        let mut names = std::collections::HashSet::new();
        if let Some(legacy) = self.legacy.get() {
            let additional = legacy.tools();
            let additional = additional["tools"]
                .as_array()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
            for tool in additional {
                let name = tool["name"]
                    .as_str()
                    .filter(|name| {
                        !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
                    })
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
                if !names.insert(name.to_owned()) {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
                tools.push(tool.clone());
            }
        }
        for spec in &self.catalog {
            if !names.insert(spec.name.clone()) {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            tools.push(
                json!({"name":spec.name,"description":spec.description,"inputSchema":spec.schema}),
            );
        }
        let value = json!({"tools":tools});
        if serde_json::to_vec(&value)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len()
            > request::MAX_FRAME_BYTES
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        Ok(value)
    }

    fn spec(&self, name: &str) -> Option<&ToolSpec> {
        self.catalog.iter().find(|spec| spec.name == name)
    }

    /// One of the five legacy tools, answered exactly as the legacy server
    /// answers it, including its JSON-RPC errors.
    pub(crate) async fn legacy_call(
        &self,
        name: &str,
        arguments: &Value,
        caller: &CallerContext,
    ) -> Option<Result<Value, super::LegacyFailure>> {
        if self.spec(name).is_some() {
            return None;
        }
        self.legacy
            .get()?
            .call(name, arguments.clone(), caller)
            .await
    }

    pub async fn call_tool(
        self: &Arc<Self>,
        name: &str,
        arguments: Value,
        mut caller: CallerContext,
        signal: Option<CancellationSignal>,
    ) -> ServiceResult<Value> {
        caller.transport = CallerTransport::Mcp;
        request::validate_caller(&caller)?;
        if forbidden_name(name) {
            return Err(failure(ErrorCode::OperationNotAllowed));
        }
        let Some(spec) = self.spec(name) else {
            return match self.legacy_call(name, &arguments, &caller).await {
                Some(Ok(value)) => Ok(value),
                Some(Err(_)) => Err(failure(ErrorCode::InvalidArgument)),
                None => Err(failure(ErrorCode::UnsupportedOperation)),
            };
        };
        let arguments = match arguments {
            Value::Null => Map::new(),
            Value::Object(arguments) => arguments,
            _ => return Err(failure(ErrorCode::InvalidArgument)),
        };
        if arguments.keys().any(|key| !spec.arguments.contains(key)) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        match spec.kind {
            ToolKind::Service(operation) => {
                self.service_tool(operation, arguments, caller, signal)
                    .await
            }
            ToolKind::TablePreview => self.table_preview(arguments, caller, signal).await,
            ToolKind::JobList => self.job_list(arguments, caller).await,
        }
    }

    fn envelope(
        operation: Operation,
        arguments: &mut Map<String, Value>,
    ) -> ServiceResult<(Value, Extras)> {
        let mut extras = Extras::default();
        extras.write = match arguments.remove("write") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(write)) => Some(write),
            Some(_) => return Err(failure(ErrorCode::InvalidArgument)),
        };
        extras.wait_ms = bounded(arguments, "wait_ms", 0, MAX_WAIT_MS)?;
        arguments.remove("wait_ms");
        extras.kind = match arguments.remove("kind") {
            None | Some(Value::Null) => None,
            Some(Value::String(kind)) if OBJECT_KINDS.contains(&kind.as_str()) => Some(kind),
            Some(_) => return Err(failure(ErrorCode::InvalidArgument)),
        };
        let mut envelope = Map::new();
        for key in [
            "connection_id",
            "session_id",
            "map_id",
            "expected_version",
            "deadline_ms",
        ] {
            if let Some(value) = arguments.remove(key) {
                envelope.insert(key.into(), value);
            }
        }
        let mut input = std::mem::take(arguments);
        match operation {
            Operation::CatalogDescribe => {
                extras.database = text(&input, "database")?.map(str::to_owned);
                extras.schema = text(&input, "schema")?.map(str::to_owned);
                input.remove("database");
                input.remove("schema");
                match input.get("table").cloned() {
                    Some(Value::String(table)) => {
                        input.insert(
                            "table".into(),
                            json!({"database":extras.database,"schema":extras.schema,"table":table}),
                        );
                    }
                    Some(Value::Object(table)) => {
                        extras.database = table
                            .get("database")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .or(extras.database.take());
                        extras.schema = table
                            .get("schema")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .or(extras.schema.take());
                    }
                    _ => return Err(failure(ErrorCode::InvalidArgument)),
                }
            }
            Operation::CatalogObjects => {
                extras.database = text(&input, "database")?.map(str::to_owned);
                extras.schema = text(&input, "schema")?.map(str::to_owned);
                input.insert("schema".into(), json!(extras.schema));
            }
            Operation::CatalogSchemas => {
                extras.database = text(&input, "database")?.map(str::to_owned);
            }
            Operation::QueryExecute => {
                extras.database = text(&input, "database")?.map(str::to_owned);
                extras.schema = text(&input, "schema")?.map(str::to_owned);
                input.entry("language").or_insert(json!("sql"));
                input.entry("parameters").or_insert(json!([]));
                input.entry("page_size").or_insert(json!(100));
            }
            Operation::ResultGet => {
                input.entry("result_set_index").or_insert(json!(0));
                input.entry("offset").or_insert(json!(0));
                input.entry("limit").or_insert(json!(100));
                if bounded(&input, "limit", 1, 1000)?.is_none() {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
            }
            _ => {}
        }
        let name =
            serde_json::to_value(operation).map_err(|_| failure(ErrorCode::InvalidArgument))?;
        let mut raw = json!({
            "protocol_version": 1,
            "operation": name,
            "request_id": uuid::Uuid::new_v4().to_string(),
            "input": input,
        });
        for (key, value) in envelope {
            raw[key] = value;
        }
        Ok((raw, extras))
    }

    async fn traits(&self, connection: &SavedConnection) -> ServiceResult<CatalogTraits> {
        self.metadata
            .get()
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?
            .traits(connection)
            .await
    }

    fn connection_database(connection: &SavedConnection) -> Option<String> {
        match &connection.settings["database"] {
            Value::String(name) if !name.is_empty() => Some(name.clone()),
            Value::Array(names) if names.len() == 1 => names[0]
                .as_str()
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            _ => None,
        }
    }

    /// The names the driver lists as databases, read through the service.
    async fn listed_databases(
        self: &Arc<Self>,
        decoded: &ServiceRequest,
        caller: &CallerContext,
        signal: &Option<CancellationSignal>,
    ) -> ServiceResult<Vec<String>> {
        let raw = json!({
            "protocol_version": 1,
            "operation": "catalog.databases",
            "request_id": uuid::Uuid::new_v4().to_string(),
            "connection_id": decoded.connection_id,
            "deadline_ms": decoded.deadline_ms.min(30_000),
            "input": {},
        });
        let response = self.call(raw, caller.clone(), signal.clone()).await?;
        if let Some(error) = response.error {
            return Err(error);
        }
        let list = response
            .data
            .as_array()
            .or_else(|| response.data["databases"].as_array())
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        Ok(list
            .iter()
            .filter_map(|item| item.as_str().or_else(|| item["name"].as_str()))
            .map(str::to_owned)
            .collect())
    }

    /// A database the caller chose must be one the driver lists.
    async fn check_database(
        self: &Arc<Self>,
        decoded: &ServiceRequest,
        extras: &Extras,
        caller: &CallerContext,
        signal: &Option<CancellationSignal>,
    ) -> ServiceResult<()> {
        if let Some(database) = &extras.database {
            if !self
                .listed_databases(decoded, caller, signal)
                .await?
                .iter()
                .any(|name| name == database)
            {
                return Err(failure(ErrorCode::InvalidArgument));
            }
        }
        Ok(())
    }

    async fn check_kind(
        &self,
        operation: Operation,
        extras: &Extras,
        connection: &SavedConnection,
    ) -> ServiceResult<()> {
        let Some(kind) = extras.kind.as_deref() else {
            return Ok(());
        };
        let traits = self.traits(connection).await?;
        if !traits.kinds.iter().any(|supported| supported == kind) {
            return Err(error_with_kind(kind));
        }
        // The service describes tables and views only.
        if operation == Operation::CatalogDescribe && !matches!(kind, "table" | "view") {
            return Err(error_with_kind(kind));
        }
        Ok(())
    }

    async fn effective(
        &self,
        decoded: &ServiceRequest,
        extras: &Extras,
        connection: Option<&SavedConnection>,
    ) -> Value {
        let input = &decoded.input;
        let explicit = |key: &str| {
            input[key]
                .as_str()
                .or_else(|| input["table"][key].as_str())
                .map(str::to_owned)
        };
        let database = explicit("database")
            .or_else(|| extras.database.clone())
            .or_else(|| connection.and_then(Self::connection_database));
        let mut schema = explicit("schema").or_else(|| extras.schema.clone());
        if schema.is_none()
            && matches!(
                decoded.operation,
                Operation::CatalogObjects
                    | Operation::CatalogDescribe
                    | Operation::QueryExecute
                    | Operation::RowInsert
                    | Operation::RowUpdate
                    | Operation::RowDelete
            )
        {
            if let Some(connection) = connection {
                schema = self
                    .traits(connection)
                    .await
                    .ok()
                    .and_then(|traits| traits.default_schema);
            }
        }
        json!({"database":database,"schema":schema})
    }

    fn failed(decoded: &ServiceRequest, error: ServiceError, effective: Option<&Value>) -> Value {
        present::tool_result(
            decoded.operation,
            &request::error_response(decoded, error),
            effective,
        )
    }

    async fn service_tool(
        self: &Arc<Self>,
        operation: Operation,
        mut arguments: Map<String, Value>,
        caller: CallerContext,
        signal: Option<CancellationSignal>,
    ) -> ServiceResult<Value> {
        let (mut raw, extras) = Self::envelope(operation, &mut arguments)?;
        if extras.write.is_some() && !accepts_write_intent(operation) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        // The decode below checks the shape; the connection is needed first
        // to fill a default database.
        let connection = match raw["connection_id"].as_str() {
            Some(id) => self.runtime.connections.get(id).await.ok(),
            None => None,
        };
        if let Some(connection) = &connection {
            if matches!(
                operation,
                Operation::CatalogSchemas | Operation::CatalogObjects
            ) && extras.database.is_none()
            {
                if let Some(database) = Self::connection_database(connection) {
                    raw["input"]["database"] = json!(database);
                }
            }
        }
        let decoded = request::decode(&self.validator, &raw)?;
        let effective = self.effective(&decoded, &extras, connection.as_ref()).await;
        if let Some(connection) = &connection {
            let checked = async {
                self.runtime.policy.permission(connection, &caller).await?;
                if matches!(
                    operation,
                    Operation::CatalogObjects | Operation::CatalogDescribe
                ) {
                    self.check_kind(operation, &extras, connection).await?;
                }
                if extras.database.is_some()
                    && matches!(
                        operation,
                        Operation::QueryExecute
                            | Operation::CatalogSchemas
                            | Operation::CatalogObjects
                            | Operation::CatalogDescribe
                    )
                {
                    self.check_database(&decoded, &extras, &caller, &signal)
                        .await?;
                }
                Ok::<(), ServiceError>(())
            }
            .await;
            if let Err(error) = checked {
                return Ok(Self::failed(&decoded, error, Some(&effective)));
            }
        }
        if extras.write == Some(true) {
            self.register_query_write(raw.clone(), &caller)?;
        }
        match operation {
            Operation::QueryExecute => {
                let wait = extras.wait_ms.unwrap_or(DEFAULT_WAIT_MS);
                let response = self
                    .run_asynchronously(raw, decoded.clone(), caller, wait, signal)
                    .await;
                Ok(self.finish(&decoded, response, &effective, &extras))
            }
            Operation::JobCancel => {
                let wait = extras.wait_ms.unwrap_or(0);
                let response = self.cancel_job(raw, decoded.clone(), caller, wait).await;
                Ok(self.finish(&decoded, response, &effective, &extras))
            }
            _ => {
                let response = self.call(raw, caller, signal).await;
                Ok(self.finish(&decoded, response, &effective, &extras))
            }
        }
    }

    fn finish(
        &self,
        decoded: &ServiceRequest,
        response: ServiceResult<ServiceResponse>,
        effective: &Value,
        extras: &Extras,
    ) -> Value {
        let mut response = match response {
            Ok(response) => response,
            Err(error) => request::error_response(decoded, error),
        };
        if decoded.operation == Operation::CatalogObjects && response.error.is_none() {
            if let Some(kind) = extras.kind.as_deref() {
                if let Some(objects) = response.data["objects"].as_array() {
                    let kept: Vec<Value> = objects
                        .iter()
                        .filter(|object| object["kind"].as_str().unwrap_or("table") == kind)
                        .cloned()
                        .collect();
                    response.data["objects"] = Value::Array(kept);
                }
            }
        }
        present::tool_result(decoded.operation, &response, Some(effective))
    }

    /// Registers the job and replies once it finishes within `wait_ms`, or
    /// with the job's identity and status when it does not. An approval that
    /// is still pending never holds the reply.
    async fn run_asynchronously(
        self: &Arc<Self>,
        raw: Value,
        decoded: ServiceRequest,
        caller: CallerContext,
        wait_ms: u64,
        signal: Option<CancellationSignal>,
    ) -> ServiceResult<ServiceResponse> {
        let _ = raw;
        let queued = self.submit_request(decoded.clone(), caller.clone()).await?;
        if request::terminal(queued.status) {
            return request::checked_response(&self.validator, &decoded, queued, true);
        }
        let (job_id, owner) = Self::job_identity(&queued)?;
        match self
            .wait_for_job(
                &decoded,
                &caller,
                &job_id,
                &owner,
                Duration::from_millis(wait_ms),
                signal.unwrap_or_default(),
            )
            .await?
        {
            Waited::Terminal(response) | Waited::Pending(response) => Ok(response),
        }
    }

    fn job_identity(queued: &ServiceResponse) -> ServiceResult<(String, String)> {
        let job_id = queued
            .job_id
            .clone()
            .ok_or_else(|| failure(ErrorCode::OutcomeUnknown))?;
        let owner = queued.data["owner_instance_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| failure(ErrorCode::OutcomeUnknown))?;
        if queued.data["job_id"] != job_id.as_str()
            || super::super::security::opaque_id(&owner).is_err()
            || job_id.is_empty()
            || job_id.len() > 128
        {
            return Err(failure(ErrorCode::OutcomeUnknown));
        }
        Ok((job_id, owner))
    }

    async fn wait_for_job(
        &self,
        decoded: &ServiceRequest,
        caller: &CallerContext,
        job_id: &str,
        owner: &str,
        wait: Duration,
        signal: CancellationSignal,
    ) -> ServiceResult<Waited> {
        let until = Instant::now() + wait;
        let mut first = true;
        loop {
            if !first && Instant::now() >= until {
                break;
            }
            if wait.is_zero() {
                break;
            }
            first = false;
            let get = request::control_request(decoded, Operation::JobGet, job_id, owner, 5_000);
            let state = self.submit_request(get, caller.clone()).await?;
            if state.error.is_some() || state.data["job_id"] != job_id {
                return Err(failure(ErrorCode::OutcomeUnknown));
            }
            let status: JobStatus = serde_json::from_value(state.data["status"].clone())
                .map_err(|_| failure(ErrorCode::OutcomeUnknown))?;
            if request::terminal(status) {
                let result =
                    request::control_request(decoded, Operation::JobResult, job_id, owner, 5_000);
                let stored = self.submit_request(result, caller.clone()).await?;
                if stored.error.is_some() {
                    return Err(failure(ErrorCode::OutcomeUnknown));
                }
                let terminal: ServiceResponse =
                    serde_json::from_value(stored.data["response"].clone())
                        .map_err(|_| failure(ErrorCode::OutcomeUnknown))?;
                if terminal.job_id.as_deref() != Some(job_id) || terminal.status != status {
                    return Err(failure(ErrorCode::OutcomeUnknown));
                }
                return Ok(Waited::Terminal(request::checked_response(
                    &self.validator,
                    decoded,
                    terminal,
                    true,
                )?));
            }
            if signal.is_cancelled() {
                break;
            }
            tokio::time::sleep(
                Duration::from_millis(POLL_MS).min(until.saturating_duration_since(Instant::now())),
            )
            .await;
        }
        let get = request::control_request(decoded, Operation::JobGet, job_id, owner, 5_000);
        let state = self.submit_request(get, caller.clone()).await?;
        let status: JobStatus = serde_json::from_value(state.data["status"].clone())
            .map_err(|_| failure(ErrorCode::OutcomeUnknown))?;
        if request::terminal(status) {
            let result =
                request::control_request(decoded, Operation::JobResult, job_id, owner, 5_000);
            let stored = self.submit_request(result, caller.clone()).await?;
            let terminal: ServiceResponse = serde_json::from_value(stored.data["response"].clone())
                .map_err(|_| failure(ErrorCode::OutcomeUnknown))?;
            return Ok(Waited::Terminal(request::checked_response(
                &self.validator,
                decoded,
                terminal,
                true,
            )?));
        }
        let mut pending = ServiceResponse {
            protocol_version: 1,
            request_id: decoded.request_id.clone(),
            connection_id: decoded.connection_id.clone(),
            status,
            job_id: Some(job_id.to_owned()),
            result_id: None,
            data: json!({"job_id":job_id,"owner_instance_id":owner,"awaiting_approval":state.data["awaiting_approval"]}),
            page: Default::default(),
            limits: Default::default(),
            metrics: Default::default(),
            warnings: vec![],
            error: None,
        };
        if state.data["awaiting_approval"] != true {
            pending.data["awaiting_approval"] = json!(false);
        }
        Ok(Waited::Pending(pending))
    }

    /// Asks the driver to cancel and, when `wait_ms` is set, waits that long
    /// for the job to end. A job that does not end in time stays recorded as
    /// not confirmed: it may still finish on its own.
    async fn cancel_job(
        self: &Arc<Self>,
        raw: Value,
        decoded: ServiceRequest,
        caller: CallerContext,
        wait_ms: u64,
    ) -> ServiceResult<ServiceResponse> {
        let _ = raw;
        let response = self.submit_request(decoded.clone(), caller.clone()).await?;
        let response = request::checked_response(&self.validator, &decoded, response, false)?;
        if wait_ms == 0 || response.error.is_some() || response.data["cancel_state"] != "requested"
        {
            return Ok(response);
        }
        let job_id = decoded.input["job_id"].as_str().unwrap_or_default();
        let owner = decoded.input["owner_instance_id"]
            .as_str()
            .unwrap_or_default();
        let until = Instant::now() + Duration::from_millis(wait_ms);
        loop {
            let get = request::control_request(&decoded, Operation::JobGet, job_id, owner, 5_000);
            let mut state = self.submit_request(get, caller.clone()).await?;
            let status: JobStatus = serde_json::from_value(state.data["status"].clone())
                .map_err(|_| failure(ErrorCode::OutcomeUnknown))?;
            if request::terminal(status) {
                state.request_id = decoded.request_id.clone();
                return Ok(state);
            }
            if Instant::now() >= until {
                if owner == self.runtime.jobs.instance_id() {
                    self.runtime.jobs.mark_cancel_not_confirmed(job_id);
                }
                state.request_id = decoded.request_id.clone();
                state.data["cancel_state"] = json!("not_confirmed");
                state.data["cancel_code"] = json!("CANCEL_NOT_CONFIRMED");
                state.status = JobStatus::Failed;
                let mut error = failure(ErrorCode::CancelNotConfirmed);
                error.outcome = Outcome::NotStarted;
                state.error = Some(error);
                return Ok(state);
            }
            tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
        }
    }

    async fn job_list(
        self: &Arc<Self>,
        arguments: Map<String, Value>,
        caller: CallerContext,
    ) -> ServiceResult<Value> {
        let limit = bounded(&arguments, "limit", 1, MAX_LIST_LIMIT as u64)?
            .map_or(50, |limit| limit as usize);
        let connection_id = text(&arguments, "connection_id")?.map(str::to_owned);
        let status = match arguments.get("status") {
            None | Some(Value::Null) => None,
            Some(status) => Some(
                serde_json::from_value::<JobStatus>(status.clone())
                    .map_err(|_| failure(ErrorCode::InvalidArgument))?,
            ),
        };
        let since_ms = bounded(&arguments, "since_ms", 0, u64::MAX / 2)?;
        let until_ms = bounded(&arguments, "until_ms", 0, u64::MAX / 2)?;
        let cursor = text(&arguments, "cursor")?.map(str::to_owned);
        let mut permitted = std::collections::HashMap::new();
        if let Some(id) = &connection_id {
            let allowed = async {
                let connection = self.runtime.connections.get(id).await?;
                self.runtime.policy.permission(&connection, &caller).await
            }
            .await;
            if let Err(error) = allowed {
                return Ok(present::error_result(error));
            }
            permitted.insert(id.clone(), true);
        }
        let query = |size: usize| HistoryQuery {
            connection_id: connection_id.clone(),
            status,
            since_ms,
            until_ms,
            limit: Some(size),
            cursor: cursor.clone(),
        };
        // Running jobs of this process stay visible when no history is
        // written. They carry no SQL text then.
        let mut extra = Vec::new();
        if cursor.is_none() {
            let known: std::collections::HashSet<String> = self
                .runtime
                .job_history(&query(MAX_LIST_LIMIT))?
                .entries
                .into_iter()
                .map(|entry| entry.job_id)
                .collect();
            let mut live = self.runtime.jobs.live_details();
            live.retain(|job| {
                !known.contains(&job.job_id)
                    && connection_id
                        .as_ref()
                        .is_none_or(|id| job.connection_id.as_ref() == Some(id))
                    && status.is_none_or(|status| job.status == status)
                    && since_ms.is_none_or(|since| job.started_ms >= since)
                    && until_ms.is_none_or(|until| job.started_ms < until)
            });
            live.sort_by(|a, b| (b.started_ms, &b.job_id).cmp(&(a.started_ms, &a.job_id)));
            for job in live {
                if let Some(id) = &job.connection_id {
                    if !permitted.contains_key(id) {
                        let allowed = match self.runtime.connections.get(id).await {
                            Ok(connection) => self
                                .runtime
                                .policy
                                .permission(&connection, &caller)
                                .await
                                .is_ok(),
                            Err(_) => false,
                        };
                        permitted.insert(id.clone(), allowed);
                    }
                    if !permitted[id] {
                        continue;
                    }
                }
                extra.push(json!({
                    "job_id":job.job_id,
                    "owner_instance_id":self.runtime.jobs.instance_id(),
                    "operation":job.operation,
                    "connection_id":job.connection_id,
                    "query":null,
                    "query_kind":null,
                    "status":job.status,
                    "started_at_ms":job.started_ms,
                    "finished_at_ms":null,
                    "elapsed_ms":null,
                    "row_count":null,
                    "error":null,
                    "legacy":false,
                    "cancellable":true,
                }));
            }
        }
        extra.truncate(limit);
        let mut size = limit - extra.len();
        loop {
            let page = if size == 0 {
                super::super::history::HistoryPage {
                    entries: Vec::new(),
                    next_cursor: None,
                }
            } else {
                self.runtime.job_history(&query(size))?
            };
            let mut entries = extra.clone();
            for entry in &page.entries {
                if let Some(id) = &entry.connection_id {
                    if !permitted.contains_key(id) {
                        let allowed = match self.runtime.connections.get(id).await {
                            Ok(connection) => self
                                .runtime
                                .policy
                                .permission(&connection, &caller)
                                .await
                                .is_ok(),
                            Err(_) => false,
                        };
                        permitted.insert(id.clone(), allowed);
                    }
                    if !permitted[id] {
                        continue;
                    }
                }
                let mut value =
                    serde_json::to_value(entry).map_err(|_| failure(ErrorCode::ResourceLimit))?;
                if let Some(query) = entry.query.as_deref() {
                    let mut end = query.len().min(2048);
                    while !query.is_char_boundary(end) {
                        end -= 1;
                    }
                    value["query"] = json!(&query[..end]);
                    value["query_truncated"] = json!(end < query.len());
                }
                entries.push(value);
            }
            let summary = json!({
                "status":"succeeded",
                "effective":{"database":null,"schema":null},
                "data":{"entries":entries,"next_cursor":page.next_cursor,"returned":entries.len()},
                "error":null,
                "presentation_truncated":false,
            });
            let output = present::raw_result(&summary);
            if present::fits(&output) || (size <= 1 && extra.is_empty()) {
                return Ok(output);
            }
            if size == 0 {
                extra.truncate(extra.len() / 2);
                size = limit - extra.len();
            } else {
                size = (size / 2).max(if extra.is_empty() { 1 } else { 0 });
            }
        }
    }

    async fn table_preview(
        self: &Arc<Self>,
        arguments: Map<String, Value>,
        caller: CallerContext,
        signal: Option<CancellationSignal>,
    ) -> ServiceResult<Value> {
        let connection_id = text(&arguments, "connection_id")?
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
            .to_owned();
        let table = identifier(
            text(&arguments, "table")?.ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
        )?
        .to_owned();
        let database = text(&arguments, "database")?.map(str::to_owned);
        let schema = text(&arguments, "schema")?.map(str::to_owned);
        let limit = bounded(&arguments, "limit", 1, 1000)?.unwrap_or(10);
        let wait = bounded(&arguments, "wait_ms", 0, MAX_WAIT_MS)?.unwrap_or(DEFAULT_WAIT_MS);
        let deadline = bounded(&arguments, "deadline_ms", 1, 120_000)?.unwrap_or(30_000);
        let connection = self.runtime.connections.get(&connection_id).await?;
        self.runtime.policy.permission(&connection, &caller).await?;
        let traits = self.traits(&connection).await?;
        let extras = Extras {
            database: database.clone(),
            schema: schema.clone(),
            ..Extras::default()
        };
        let effective = json!({
            "database":database.clone().or_else(|| Self::connection_database(&connection)),
            "schema":schema.clone().or(traits.default_schema.clone()),
        });
        let describe = ServiceRequest {
            protocol_version: 1,
            operation: Operation::CatalogDescribe,
            request_id: uuid::Uuid::new_v4().to_string(),
            connection_id: Some(connection_id.clone()),
            session_id: None,
            map_id: None,
            expected_version: None,
            deadline_ms: deadline.min(30_000),
            input: json!({"table":{"database":database,"schema":schema,"table":table}}),
        };
        if let Err(error) = self
            .check_database(&describe, &extras, &caller, &signal)
            .await
        {
            return Ok(Self::failed(&describe, error, Some(&effective)));
        }
        // The catalog must know the table before any statement is built.
        let described = self
            .call(
                serde_json::to_value(&describe).map_err(|_| failure(ErrorCode::InvalidArgument))?,
                caller.clone(),
                signal.clone(),
            )
            .await?;
        if described.error.is_some() {
            return Ok(present::tool_result(
                Operation::CatalogDescribe,
                &described,
                Some(&effective),
            ));
        }
        let text = preview_sql(traits.flavor, schema.as_deref(), &table, limit)?;
        let query = ServiceRequest {
            protocol_version: 1,
            operation: Operation::QueryExecute,
            request_id: uuid::Uuid::new_v4().to_string(),
            connection_id: Some(connection_id),
            session_id: None,
            map_id: None,
            expected_version: None,
            deadline_ms: deadline,
            input: {
                let mut input =
                    json!({"language":"sql","text":text,"parameters":[],"page_size":limit});
                if let Some(database) = &extras.database {
                    input["database"] = json!(database);
                }
                input
            },
        };
        let raw = serde_json::to_value(&query).map_err(|_| failure(ErrorCode::InvalidArgument))?;
        request::decode(&self.validator, &raw)?;
        let response = self
            .run_asynchronously(raw, query.clone(), caller, wait, signal)
            .await;
        Ok(self.finish(&query, response, &effective, &extras))
    }
}
