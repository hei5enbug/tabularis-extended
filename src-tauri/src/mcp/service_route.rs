//! `run_query` on connections whose driver the common service supports: the
//! statement runs there first, as a read with a deadline the database server
//! enforces. Everything the service does not take as a read stays with the
//! legacy path.

use super::protocol::JsonRpcError;
use crate::{
    drivers::driver_trait::ServiceRpcMethod,
    models::{Pagination, QueryResult},
    service_native::{catalog, NativeCore},
    services::{
        contracts::{ErrorCode, JobStatus, Operation, ServiceError, ServiceResponse},
        dispatch::CallerContext,
        transport::{ReadOutcome, ReadQuery},
    },
};
use serde_json::{json, Map, Value};
use std::{future::Future, sync::Arc};

pub(crate) const DEFAULT_TIMEOUT_MS: u64 = 110_000;
pub(crate) const MAX_TIMEOUT_MS: u64 = 120_000;
pub(crate) const LONGER_READS_HINT: &str = "For a read that needs longer, use query_execute with deadline_ms up to 3600000 and poll job_get and job_result.";
const MAX_LIMIT: u64 = 1000;
const MAX_ADDITIONAL_SETS: usize = 32;
const MAX_NAME_CHARS: usize = 128;

/// What a legacy `run_query` needs to reach the service: the composed
/// service and the caller the MCP request came from.
#[derive(Clone)]
pub(crate) struct Route {
    core: Arc<NativeCore>,
    caller: CallerContext,
}

impl Route {
    pub(crate) fn new(core: Arc<NativeCore>, caller: CallerContext) -> Self {
        Self { core, caller }
    }
}

tokio::task_local! {
    static ROUTE: Route;
}

/// Runs `work` with `route` available to the `run_query` it contains.
pub(crate) async fn scope<F: Future>(route: Route, work: F) -> F::Output {
    ROUTE.scope(route, work).await
}

fn current() -> Option<Route> {
    ROUTE.try_with(Clone::clone).ok()
}

/// A failed `run_query` and the audit status it is recorded with.
pub(super) struct Failure {
    pub error: JsonRpcError,
    pub status: &'static str,
}

fn failure(message: String) -> Failure {
    Failure {
        error: JsonRpcError {
            code: -32000,
            message,
            data: None,
        },
        status: "error",
    }
}

pub(super) struct Executed {
    pub result: QueryResult,
    pub database: Option<String>,
}

/// The `timeout_ms` argument: absent, or an integer from 1 to 120000.
pub(super) fn timeout_ms(args: &Map<String, Value>) -> Result<Option<u64>, JsonRpcError> {
    match args.get("timeout_ms") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|ms| (1..=MAX_TIMEOUT_MS).contains(ms))
            .map(Some)
            .ok_or_else(|| JsonRpcError {
                code: -32602,
                message: format!(
                    "INVALID_ARGUMENT: timeout_ms must be an integer between 1 and {MAX_TIMEOUT_MS}"
                ),
                data: None,
            }),
    }
}

/// Adds `timeout_ms` to the `run_query` schema of a tool list.
pub(super) fn advertise_timeout(mut tools: Value) -> Value {
    let property = json!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_TIMEOUT_MS,
        "description": format!("Maximum time in milliseconds the statement may run. When it runs longer it is cancelled on the database server and the call fails with QUERY_TIMEOUT. Default {DEFAULT_TIMEOUT_MS} on postgresql-plus and sqlserver-plus connections; other connections have no extra limit unless this is set."),
    });
    if let Some(properties) = tools["tools"]
        .as_array_mut()
        .and_then(|tools| tools.iter_mut().find(|tool| tool["name"] == "run_query"))
        .and_then(|tool| tool["inputSchema"]["properties"].as_object_mut())
    {
        properties.insert("timeout_ms".into(), property);
    }
    tools
}

/// The error for a statement the host stopped waiting for without a server
/// cancellation to report.
pub(super) fn stopped_waiting(timeout_ms: u64) -> JsonRpcError {
    JsonRpcError {
        code: -32000,
        message: format!("QUERY_TIMEOUT: the query ran longer than timeout_ms={timeout_ms}; the host stopped waiting and asked the driver to cancel, but this driver cannot confirm that the server stopped the statement. {LONGER_READS_HINT}"),
        data: None,
    }
}

fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && !name.chars().any(char::is_control)
}

/// Codes with which the service refuses a statement before running it and
/// that leave the decision to the legacy gates.
fn refused_before_running(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::WriteNotAllowed
            | ErrorCode::WriteIntentRequired
            | ErrorCode::UnsupportedOperation
            | ErrorCode::CapabilityUnavailable
            | ErrorCode::OperationNotAllowed
    )
}

fn code_name(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|name| name.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{code:?}"))
}

/// The text the database server gave for the statement, which the legacy
/// path answers with unchanged.
fn database_message(error: &ServiceError) -> Option<&str> {
    error
        .details
        .get("database_error")?
        .get("message")?
        .as_str()
        .filter(|message| !message.is_empty())
}

fn service_failure(error: &ServiceError) -> Failure {
    match database_message(error) {
        Some(message) => failure(message.to_owned()),
        None => failure(format!("{}: {}", code_name(error.code), error.message)),
    }
}

fn deadline_failure(timeout_ms: u64, cancel_state: Option<&str>) -> Failure {
    let base = format!("QUERY_TIMEOUT: the query ran longer than timeout_ms={timeout_ms}");
    let detail = match cancel_state {
        Some("confirmed") => " and was cancelled on the database server".to_owned(),
        Some(state) => format!(
            "; the cancellation was asked of the database server but is not confirmed (cancel_state={state}), so the statement may still be running"
        ),
        None => " and was stopped before it finished on the database server".to_owned(),
    };
    Failure {
        error: JsonRpcError {
            code: -32000,
            message: format!("{base}{detail}. {LONGER_READS_HINT}"),
            data: None,
        },
        status: "timeout",
    }
}

fn column_names(columns: &Value) -> Vec<String> {
    columns
        .as_array()
        .map(|columns| {
            columns
                .iter()
                .map(|column| {
                    column
                        .as_str()
                        .or_else(|| column["name"].as_str())
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A tabular service page in the shape the legacy tool answers with.
pub(super) fn legacy_result(data: &Value, has_more: bool, limit: u32) -> QueryResult {
    QueryResult {
        columns: column_names(&data["columns"]),
        rows: data["rows"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| row.as_array().cloned().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default(),
        // The route runs proven reads only; the legacy server reported 0 for them.
        affected_rows: 0,
        truncated: has_more,
        pagination: Some(Pagination {
            page: 1,
            page_size: limit,
            total_rows: None,
            has_more,
        }),
        additional_results: None,
        server_time_ms: None,
        fetch_time_ms: None,
    }
}

enum Judged {
    Done(Box<ServiceResponse>),
    Fallback,
    Failed(Failure),
}

fn judge(outcome: ReadOutcome, timeout_ms: u64) -> Judged {
    let ReadOutcome {
        response,
        cancel_state,
    } = outcome;
    match &response.error {
        None if response.status == JobStatus::Succeeded => Judged::Done(Box::new(response)),
        None => Judged::Failed(failure(format!(
            "{}: the query did not finish with a result",
            code_name(ErrorCode::OutcomeUnknown)
        ))),
        Some(error) if refused_before_running(error.code) => Judged::Fallback,
        Some(error) if error.code == ErrorCode::DeadlineExceeded => {
            Judged::Failed(deadline_failure(timeout_ms, cancel_state.as_deref()))
        }
        Some(error) => Judged::Failed(service_failure(error)),
    }
}

/// Everything about the call the route needs, taken from its arguments.
pub(super) struct Request<'a> {
    pub connection: &'a str,
    pub query: &'a str,
    pub limit: u64,
    pub timeout_ms: Option<u64>,
    pub args: &'a Map<String, Value>,
}

/// Runs the statement through the service when a route is present and the
/// connection's driver is one the service supports. `Ok(None)` leaves the
/// statement to the legacy path.
pub(super) async fn run(request: &Request<'_>) -> Result<Option<Executed>, Failure> {
    let Some(route) = current() else {
        return Ok(None);
    };
    if !(1..=MAX_LIMIT).contains(&request.limit) {
        return Ok(None);
    }
    let (Some(database), Some(schema)) = (
        optional_name(request.args, "database"),
        optional_name(request.args, "schema"),
    ) else {
        return Ok(None);
    };
    let Ok(connection) = route
        .core
        .platform
        .runtime
        .connections
        .get(request.connection)
        .await
    else {
        return Ok(None);
    };
    match catalog::driver_capabilities(&connection.driver).await {
        Ok(Some((_, capabilities)))
            if capabilities.supports(ServiceRpcMethod::ExactQuery) && !capabilities.documents => {}
        _ => return Ok(None),
    }
    let timeout_ms = request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let limit = request.limit as u32;
    let query = ReadQuery {
        connection_id: connection.id.clone(),
        text: request.query.to_owned(),
        page_size: request.limit,
        deadline_ms: timeout_ms,
        database: database.clone(),
        schema,
    };
    let outcome = match route
        .core
        .transport
        .run_read_query(query, route.caller.clone())
        .await
    {
        Ok(outcome) => outcome,
        Err(error) if refused_before_running(error.code) => return Ok(None),
        Err(error) => return Err(service_failure(&error)),
    };
    let response = match judge(outcome, timeout_ms) {
        Judged::Done(response) => *response,
        Judged::Fallback => return Ok(None),
        Judged::Failed(failed) => return Err(failed),
    };
    let mut result = legacy_result(&response.data, response.page.has_more, limit);
    let sets = response.data["result_set_count"].as_u64().unwrap_or(1) as usize;
    if sets > 1 {
        result.additional_results = Some(
            additional_sets(&route, &connection.id, &response, sets, limit)
                .await
                .map_err(|error| service_failure(&error))?,
        );
    }
    Ok(Some(Executed { result, database }))
}

/// `Some(None)` when the argument is absent, `Some(Some(name))` when it is a
/// usable name, and `None` when it is present but not one, which the legacy
/// path reports.
fn optional_name(args: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match args.get(key) {
        None => Some(None),
        Some(Value::String(name)) if plain(name) => Some(Some(name.clone())),
        Some(value) if key == "schema" && !value.is_string() => Some(None),
        Some(_) => None,
    }
}

async fn additional_sets(
    route: &Route,
    connection_id: &str,
    first: &ServiceResponse,
    sets: usize,
    limit: u32,
) -> Result<Vec<QueryResult>, ServiceError> {
    let result_id = first
        .result_id
        .clone()
        .ok_or_else(|| crate::services::error::failure(ErrorCode::OutcomeUnknown))?;
    let mut results = Vec::new();
    for index in 1..sets.min(MAX_ADDITIONAL_SETS + 1) {
        let mut request = catalog::request(
            Operation::ResultGet,
            Some(connection_id.to_owned()),
            json!({"result_id":result_id,"result_set_index":index,"offset":0,"limit":limit}),
        );
        request.deadline_ms = 30_000;
        let raw = serde_json::to_value(request)
            .map_err(|_| crate::services::error::failure(ErrorCode::InvalidArgument))?;
        let page = route
            .core
            .transport
            .call(raw, route.caller.clone(), None)
            .await?;
        if let Some(error) = page.error {
            return Err(error);
        }
        results.push(legacy_result(&page.data, page.page.has_more, limit));
    }
    Ok(results)
}

#[cfg(test)]
#[path = "service_route_tests.rs"]
mod tests;
