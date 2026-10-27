use super::contracts::{ErrorCode, Outcome, ServiceError};
use serde_json::Value;

pub type ServiceResult<T> = Result<T, ServiceError>;

pub fn failure(code: ErrorCode) -> ServiceError {
    let message = match code {
        ErrorCode::InvalidArgument => "The service input is invalid.",
        ErrorCode::ConnectionNotFound => "The saved connection was not found.",
        ErrorCode::PermissionDenied => "The caller cannot access this resource.",
        ErrorCode::WriteNotAllowed => {
            "This write requires connection permission and request approval."
        }
        ErrorCode::WriteIntentRequired => {
            "This statement is not a proven read; resend it with write set to true."
        }
        ErrorCode::SecretNotAccepted => {
            "Secrets are not accepted here; enter them in the Tabularis window."
        }
        ErrorCode::OperationNotAllowed => "This operation is not available through MCP.",
        ErrorCode::VersionConflict => "The saved version has changed.",
        ErrorCode::ConnectionConflict => "The connection has changed since it was read.",
        ErrorCode::AuthRequired => "Authentication is required.",
        ErrorCode::AuthExpired => "The authentication has expired.",
        ErrorCode::InteractionRequired => "Authentication requires user interaction.",
        ErrorCode::ResourceLimit => "The service resource limit was reached.",
        ErrorCode::RateLimited => "The service queue is full.",
        ErrorCode::InvalidPageToken => "The page token is invalid for this resource.",
        ErrorCode::CursorExpired => "The cursor has expired.",
        ErrorCode::ResultExpired => "The result has expired or was released.",
        ErrorCode::DeadlineExceeded => "The service deadline was exceeded.",
        ErrorCode::Cancelled => "The driver confirmed cancellation.",
        ErrorCode::CancelNotConfirmed => {
            "The driver did not confirm the cancellation; the call may still finish."
        }
        ErrorCode::OutcomeUnknown => "The write outcome could not be confirmed.",
        ErrorCode::UnsupportedOperation => "The connection does not support this operation.",
        ErrorCode::TlsValidationFailed => "TLS certificate validation failed.",
        _ => "The service backend could not complete this operation.",
    };
    ServiceError {
        code,
        message: message.into(),
        retryable: false,
        outcome: Outcome::NotStarted,
        details: Value::Null,
    }
}

const SERVER_TOOLS: [&str; 2] = ["server_activity", "server_cancel"];

/// The refusal for a server tool called on a connection whose driver is not
/// one of the two the tool supports. The driver is named in the message.
pub fn unsupported_server_tool(tool: &str, driver: &str) -> ServiceError {
    ServiceError {
        message: unsupported_server_tool_message(tool, driver),
        details: serde_json::json!({"server_tool":tool,"driver":driver}),
        ..failure(ErrorCode::UnsupportedOperation)
    }
}

fn unsupported_server_tool_message(tool: &str, driver: &str) -> String {
    format!(
        "{tool} supports only postgresql-plus and sqlserver-plus connections; this connection uses driver '{driver}'"
    )
}

const MAX_DATABASE_MESSAGE_BYTES: usize = 4000;
const MAX_DATABASE_FIELD_BYTES: usize = 1000;
const MAX_DATABASE_ERRORS: usize = 8;

fn clean_text(text: &str, max_bytes: usize) -> String {
    let mut cleaned: String = text
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect();
    let mut end = cleaned.len().min(max_bytes);
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned.truncate(end);
    cleaned
}

fn json_integer(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .filter(|value| *value <= 9_007_199_254_740_991)
}

/// The database statement error a plugin reported, reduced to the keys the
/// host knows and bounded. `None` when it is not well formed.
fn database_error(value: &Value) -> Option<Value> {
    let engine = value.get("engine")?.as_str()?;
    let message = clean_text(value.get("message")?.as_str()?, MAX_DATABASE_MESSAGE_BYTES);
    if message.trim().is_empty() {
        return None;
    }
    let mut cleaned = serde_json::Map::new();
    match engine {
        "sqlserver" => {
            let errors: Vec<Value> = value
                .get("errors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(MAX_DATABASE_ERRORS)
                .filter_map(|error| {
                    let fields: serde_json::Map<String, Value> =
                        ["number", "severity", "state", "line"]
                            .into_iter()
                            .filter_map(|key| {
                                Some((key.to_owned(), json_integer(error.get(key)?)?.into()))
                            })
                            .collect();
                    (!fields.is_empty()).then_some(Value::Object(fields))
                })
                .collect();
            cleaned.insert("errors".into(), Value::Array(errors));
        }
        "postgresql" => {
            if let Some(sqlstate) = value
                .get("sqlstate")
                .and_then(Value::as_str)
                .filter(|state| {
                    state.len() == 5 && state.chars().all(|c| c.is_ascii_alphanumeric())
                })
            {
                cleaned.insert("sqlstate".into(), sqlstate.into());
            }
            for key in ["detail", "hint"] {
                if let Some(text) = value.get(key).and_then(Value::as_str) {
                    cleaned.insert(
                        key.into(),
                        clean_text(text, MAX_DATABASE_FIELD_BYTES).into(),
                    );
                }
            }
            if let Some(position) = value.get("position").and_then(json_integer) {
                cleaned.insert("position".into(), position.into());
            }
        }
        _ => return None,
    }
    cleaned.insert("engine".into(), engine.into());
    cleaned.insert("message".into(), message.into());
    Some(Value::Object(cleaned))
}

pub fn redact_error(error: ServiceError) -> ServiceError {
    let mut details = serde_json::Map::new();
    if let Some(closed) = error.details.get("session_closed").and_then(Value::as_bool) {
        details.insert("session_closed".into(), Value::Bool(closed));
    }
    let integer = |value: &Value| {
        value
            .as_u64()
            .filter(|value| *value <= 9_007_199_254_740_991)
    };
    if matches!(
        error.code,
        ErrorCode::VersionConflict | ErrorCode::ConnectionConflict
    ) {
        if let Some(version) = error.details.get("current_version").and_then(integer) {
            details.insert("current_version".into(), version.into());
        }
    }
    if error.code == ErrorCode::ApplyTimeout {
        for key in ["state_version", "rendered_version"] {
            if let Some(value) = error.details.get(key) {
                if value.is_null() {
                    details.insert(key.into(), Value::Null);
                } else if let Some(version) = integer(value) {
                    details.insert(key.into(), version.into());
                }
            }
        }
        if error.details["gui_applied"] == false {
            details.insert("gui_applied".into(), Value::Bool(false));
        }
    }
    if error.code == ErrorCode::UnsupportedOperation {
        if let Some(kind) = error
            .details
            .get("object_kind")
            .and_then(Value::as_str)
            .filter(|kind| {
                matches!(
                    *kind,
                    "table" | "view" | "materialized_view" | "routine" | "trigger"
                )
            })
        {
            details.insert("object_kind".into(), Value::String(kind.into()));
        }
    }
    let mut message = None;
    if let Some(database_error) = error.details.get("database_error").and_then(database_error) {
        message = database_error["message"].as_str().map(str::to_owned);
        details.insert("database_error".into(), database_error);
    }
    if error.code == ErrorCode::UnsupportedOperation {
        if let (Some(tool), Some(driver)) = (
            error
                .details
                .get("server_tool")
                .and_then(Value::as_str)
                .filter(|tool| SERVER_TOOLS.contains(tool)),
            error
                .details
                .get("driver")
                .and_then(Value::as_str)
                .filter(|driver| super::security::opaque_id(driver).is_ok()),
        ) {
            message = Some(unsupported_server_tool_message(tool, driver));
            details.insert("server_tool".into(), Value::String(tool.into()));
            details.insert("driver".into(), Value::String(driver.into()));
        }
    }
    if error.code == ErrorCode::GuiUnavailable {
        if let Some(instances) = error.details.get("instances").and_then(Value::as_array) {
            let safe: Vec<Value> = instances
                .iter()
                .take(64)
                .filter_map(|instance| {
                    let id = instance["instance_id"]
                        .as_str()
                        .filter(|id| super::security::opaque_id(id).is_ok())?;
                    let process = instance["process_id"]
                        .as_u64()
                        .filter(|process| *process > 0 && *process <= u32::MAX as u64)?;
                    if instance["kind"] != "gui" {
                        return None;
                    }
                    Some(serde_json::json!({"instance_id":id,"process_id":process,"kind":"gui"}))
                })
                .collect();
            details.insert("instances".into(), Value::Array(safe));
        }
    }
    let redacted = failure(error.code);
    ServiceError {
        outcome: error.outcome,
        retryable: error.retryable,
        message: message.unwrap_or(redacted.message),
        details: if details.is_empty() {
            Value::Null
        } else {
            Value::Object(details)
        },
        ..redacted
    }
}
