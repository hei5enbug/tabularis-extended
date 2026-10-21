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
    ServiceError {
        outcome: error.outcome,
        retryable: error.retryable,
        details: if details.is_empty() {
            Value::Null
        } else {
            Value::Object(details)
        },
        ..failure(error.code)
    }
}
