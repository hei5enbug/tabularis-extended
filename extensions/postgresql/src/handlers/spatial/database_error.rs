//! Statement errors the database server raised, carried to the host with the
//! text the legacy query path shows for them.

use serde_json::{json, Value};

const MAX_MESSAGE_BYTES: usize = 4000;
const MAX_FIELD_BYTES: usize = 1000;

/// Errors a server raises outside a running statement: connection and
/// authorization failures, and the cancellation the driver itself requests.
fn is_statement_state(sqlstate: &str) -> bool {
    !(sqlstate.starts_with("08")
        || sqlstate.starts_with("28")
        || matches!(
            sqlstate,
            "3D000" | "53300" | "57014" | "57P01" | "57P02" | "57P03"
        ))
}

fn bounded(text: &str, max_bytes: usize) -> String {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

pub(super) fn value(
    message: &str,
    sqlstate: &str,
    detail: Option<&str>,
    hint: Option<&str>,
    position: Option<u32>,
) -> Option<Value> {
    if !is_statement_state(sqlstate) {
        return None;
    }
    let mut error = json!({
        "engine": "postgresql",
        "message": bounded(message, MAX_MESSAGE_BYTES),
        "sqlstate": sqlstate,
    });
    if let Some(detail) = detail {
        error["detail"] = json!(bounded(detail, MAX_FIELD_BYTES));
    }
    if let Some(hint) = hint {
        error["hint"] = json!(bounded(hint, MAX_FIELD_BYTES));
    }
    if let Some(position) = position {
        error["position"] = json!(position);
    }
    Some(error)
}

/// The server's own wording, as psql prints it, without the client's debug
/// dump of the error.
pub(super) fn brief(
    severity: &str,
    message: &str,
    sqlstate: &str,
    detail: Option<&str>,
    hint: Option<&str>,
    position: Option<u32>,
) -> String {
    let mut text = format!("{severity}: {message} (SQLSTATE {sqlstate})");
    if let Some(detail) = detail {
        text.push_str(&format!("\nDETAIL: {detail}"));
    }
    if let Some(hint) = hint {
        text.push_str(&format!("\nHINT: {hint}"));
    }
    if let Some(position) = position {
        text.push_str(&format!("\nPOSITION: {position}"));
    }
    text
}

/// `None` unless the server itself raised the error while running a statement.
pub(super) fn details(error: &tokio_postgres::Error) -> Option<Value> {
    let database = error.as_db_error()?;
    let position = match database.position() {
        Some(tokio_postgres::error::ErrorPosition::Original(position)) => Some(*position),
        _ => None,
    };
    value(
        &brief(
            database.severity(),
            database.message(),
            database.code().code(),
            database.detail(),
            database.hint(),
            position,
        ),
        database.code().code(),
        database.detail(),
        database.hint(),
        position,
    )
}

#[cfg(test)]
#[path = "database_error_tests.rs"]
mod database_error_tests;
