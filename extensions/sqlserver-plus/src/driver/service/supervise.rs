use super::{
    registry::{self, Cause, Scope},
    sessions, types, ServiceError,
};
use serde_json::{json, Value};
use std::{future::Future, time::Duration};

/// How long an interrupted execution may take to finish its ATTENTION
/// handshake before the driver stops waiting and discards the connection.
pub const SETTLE: Duration = Duration::from_secs(5);

/// Answers a `cancel_request` immediately. The acknowledgement only reports
/// whether a live in-flight entry was signalled; it never claims termination.
pub fn acknowledge(id: Value, params: &Value) -> Value {
    if types::only_keys(params, &["connection_id", "request_id"]).is_err() {
        return ServiceError::argument("Invalid cancellation input").response(id);
    }
    let Some(connection) = params["connection_id"]
        .as_str()
        .filter(|v| !v.is_empty() && v.chars().count() <= 128)
    else {
        return ServiceError::argument("connection_id is required").response(id);
    };
    let Some(request) = params["request_id"]
        .as_str()
        .filter(|v| !v.is_empty() && v.chars().count() <= 128)
    else {
        return ServiceError::argument("request_id is required").response(id);
    };
    crate::rpc::ok_response(
        id,
        json!({"acknowledged":true,"cancellation_requested":registry::cancel(connection, request)}),
    )
}

fn cancellation(cause: Cause, error: ServiceError) -> ServiceError {
    let (code, message) = match cause {
        Cause::Deadline => (
            "DEADLINE_EXCEEDED",
            "The service execution deadline was exceeded",
        ),
        Cause::Cancelled => ("CANCELLED", "The service request was cancelled"),
    };
    if error.code == code {
        error
    } else {
        ServiceError {
            code,
            message,
            ..error
        }
    }
}

/// Maps a failed execution to its terminal error. SQL failures keep their own
/// code and text; only an interruption by this driver becomes a cancellation
/// or deadline outcome. `None` means the execution did not finish within the
/// settle window, so no cancellation is confirmed.
fn classify(cause: Option<Cause>, finished: Option<ServiceError>) -> ServiceError {
    match (cause, finished) {
        (Some(cause), Some(error)) if matches!(error.code, "CANCELLED" | "DEADLINE_EXCEEDED") => {
            cancellation(cause, error)
        }
        (_, Some(error)) => error,
        (Some(Cause::Deadline), None) => ServiceError::new(
            "DEADLINE_EXCEEDED",
            "The service execution deadline was exceeded",
        ),
        (_, None) => {
            ServiceError::new("DRIVER_EXITED", "SQL Server cancellation was not confirmed")
        }
    }
}

/// Runs one admitted execution under its registry scope: it races the
/// execution against cancellation and the deadline, seals the scope as soon
/// as the outcome is decided, and builds the original request's response.
/// A completion that wins the race stays a success.
pub async fn supervise<F>(
    id: Value,
    session: Option<&str>,
    scope: &Scope,
    action: F,
    settle: Duration,
) -> Value
where
    F: Future<Output = Result<Value, ServiceError>>,
{
    tokio::pin!(action);
    let finished = tokio::select! {
        result = &mut action => Some(result),
        _ = tokio::time::sleep_until(scope.state.deadline) => {
            scope.expire();
            tokio::time::timeout(settle, &mut action).await.ok()
        }
        _ = scope.wait_cancelled() => tokio::time::timeout(settle, &mut action).await.ok(),
    };
    scope.seal();
    let response = match finished {
        Some(Ok(value)) => crate::rpc::ok_response(id.clone(), value),
        other => {
            let mut error = classify(scope.cause(), other.map(|result| result.unwrap_err()));
            if scope.write_dispatched() {
                error.code = "OUTCOME_UNKNOWN";
                error.unknown = true;
            }
            if scope.session_touched() {
                if let Some(handle) = session {
                    sessions::forget(handle).await;
                    error.session_closed = true;
                }
            }
            if let Some(handle) = scope.opened_session() {
                sessions::forget(&handle).await;
                error.session_closed = true;
            }
            error.response(id.clone())
        }
    };
    if serde_json::to_vec(&response).unwrap().len() > types::FRAME_LIMIT {
        let mut error = ServiceError::new(
            "RESOURCE_LIMIT",
            "The actual driver response exceeds its byte limit",
        );
        if scope.write_dispatched() {
            error.code = "OUTCOME_UNKNOWN";
            error.unknown = true;
        }
        error.response(id)
    } else {
        response
    }
}

#[cfg(test)]
mod tests;
