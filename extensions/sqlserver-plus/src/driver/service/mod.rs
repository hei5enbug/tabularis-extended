pub mod catalog;
pub mod exact;
pub mod explain;
pub mod pool;
pub mod registry;
pub mod row;
pub mod sessions;
pub mod supervise;
#[cfg(test)]
mod tests;
pub mod types;

use serde_json::{json, Value};
#[derive(Clone, Debug)]
pub struct ServiceError {
    pub code: &'static str,
    pub message: &'static str,
    pub unknown: bool,
    pub session_closed: bool,
    pub database_error: Option<Value>,
}
impl ServiceError {
    pub fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            unknown: false,
            session_closed: false,
            database_error: None,
        }
    }
    pub fn argument(message: &'static str) -> Self {
        Self::new("INVALID_ARGUMENT", message)
    }
    pub fn database(error: mssql_tds::error::Error) -> Self {
        let database_error = crate::driver::error::statement_error_details(&error);
        let mut result = Self::from_database(error);
        result.database_error = database_error;
        result
    }
    fn from_database(error: mssql_tds::error::Error) -> Self {
        match error {
            mssql_tds::error::Error::SqlServerError { errors, .. }
                if errors
                    .iter()
                    .any(|error| matches!(error.number, 229 | 230 | 262 | 297 | 15151)) =>
            {
                Self::new(
                    "WRITE_NOT_ALLOWED",
                    "SQL Server denied the requested database permission",
                )
            }
            mssql_tds::error::Error::OperationCancelledError(_) => {
                Self::new("CANCELLED", "SQL Server execution was cancelled")
            }
            mssql_tds::error::Error::TimeoutError(_) => Self::new(
                "DEADLINE_EXCEEDED",
                "SQL Server execution exceeded its deadline",
            ),
            _ => Self::new("DRIVER_EXITED", "SQL Server execution failed"),
        }
    }
    pub fn response(self, id: Value) -> Value {
        let mut details = serde_json::Map::new();
        if self.session_closed {
            details.insert("session_closed".into(), json!(true));
        }
        if let Some(database_error) = self.database_error {
            details.insert("database_error".into(), database_error);
        }
        let details = if details.is_empty() {
            Value::Null
        } else {
            Value::Object(details)
        };
        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":self.message,"data":{"code":self.code,"message":self.message,"retryable":false,"outcome":if self.unknown {"unknown"} else {"not_applied"},"details":details}}})
    }
}
