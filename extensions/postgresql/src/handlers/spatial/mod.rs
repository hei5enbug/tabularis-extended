pub mod catalog;
pub mod catalog_objects;
pub mod connection;
pub mod ewkb;
pub mod exact;
pub mod service;
pub mod sessions;
pub mod sql;
pub mod table;
pub mod transform;
pub mod types;

use serde::Deserialize;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize)]
pub struct SpatialError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub outcome: String,
    pub details: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ColumnsInput {
    table: types::TableRef,
}

pub async fn dispatch(id: Value, params: &Value, method: &str) -> Value {
    dispatch_with_scope(id, params, method, None).await
}
pub async fn dispatch_with_scope(
    id: Value,
    params: &Value,
    method: &str,
    scope: Option<crate::cancel::ScopedGuard>,
) -> Value {
    match run(params, method, scope).await {
        Ok(value) => crate::rpc::ok_response(id, value),
        Err(error) => error.response(id),
    }
}

async fn run(
    params: &Value,
    method: &str,
    scope: Option<crate::cancel::ScopedGuard>,
) -> Result<Value, SpatialError> {
    let read_only = !matches!(
        method,
        "execute_query_exact" | "service_row" | "service_invalidate_auth"
    );
    let (context, mut connection) = types::DriverContext::parse(params, read_only)?;
    let scope = match scope {
        Some(scope) => scope,
        None => crate::cancel::ScopedGuard::reserve(&context.connection_id, &context.request_id)?,
    };
    if method == "service_invalidate_auth" {
        types::input::<service::EmptyInput>(params)?;
        return service::invalidate(&context, &scope).await;
    }
    let table = match method {
        "spatial_columns" => Some(types::input::<ColumnsInput>(params)?.table),
        "spatial_query_table" => {
            let input = types::input::<table::TableInput>(params)?;
            input.validate()?;
            Some(input.table)
        }
        "spatial_read_feature" => Some(types::input::<table::ReadInput>(params)?.table),
        "execute_query_exact" => {
            let input = types::input::<exact::ExactInput>(params)?;
            exact::validate_text(&input.text)?;
            exact::search_path_statement(input.schema.as_deref())?;
            None
        }
        "spatial_transform_values" => {
            types::input::<transform::TransformInput>(params)?;
            None
        }
        "service_test" | "service_catalog" | "service_row" => {
            service::validate(params, method, &context, &connection)?
        }
        _ => {
            return Err(SpatialError::new(
                "UNSUPPORTED_OPERATION",
                "Unknown spatial driver method",
            ))
        }
    };
    if let Some(table) = &table {
        if context.session_handle.is_none() {
            connection = table.connection(&connection)?;
        } else {
            table.connection(&connection)?;
        }
    }
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(context.deadline_ms);
    let mut lease = tokio::time::timeout(
        std::time::Duration::from_millis(context.deadline_ms),
        connection::Lease::acquire(&context, &connection, &scope),
    )
    .await
    .map_err(|_| {
        SpatialError::new(
            "DEADLINE_EXCEEDED",
            "Connection acquisition deadline exceeded",
        )
    })??;
    let result = async {
        if let Some(database) = table.as_ref().and_then(|t| t.database.as_deref()) {
            let actual: String = lease
                .client()
                .query_one("SELECT current_database()", &[])
                .await
                .map_err(SpatialError::database)?
                .get(0);
            if actual != database {
                return Err(SpatialError::argument(
                    "Table database does not match the reserved connection",
                ));
            }
        }
        let catalog = catalog::Catalog::load(lease.client()).await?;
        if method == "execute_query_exact" {
            return exact::execute(
                lease.client(),
                catalog.as_ref(),
                &types::input(params)?,
                &scope,
                context.read_only,
                context.session_handle.is_some(),
            )
            .await;
        }
        match method {
            "service_test" => {
                lease
                    .client()
                    .query_one("SELECT 1", &[])
                    .await
                    .map_err(SpatialError::database)?;
                return Ok(json!({"connected":true}));
            }
            "service_catalog" => {
                return service::catalog(
                    lease.client(),
                    catalog.as_ref(),
                    types::input(params)?,
                    &scope,
                    context.session_handle.is_some(),
                )
                .await
            }
            "service_row" => {
                return service::row(
                    lease.client(),
                    catalog.as_ref(),
                    types::input(params)?,
                    &scope,
                    context.session_handle.is_some(),
                )
                .await
            }
            _ => {}
        }
        let catalog = catalog.ok_or_else(|| {
            SpatialError::new(
                "CAPABILITY_UNAVAILABLE",
                "PostGIS extension is not installed",
            )
        })?;
        match method {
            "spatial_columns" => {
                let input: ColumnsInput = types::input(params)?;
                let metadata = catalog::table_columns(
                    lease.client(),
                    &catalog,
                    input.table.schema(),
                    &input.table.table,
                )
                .await?;
                Ok(json!({"columns":metadata.columns}))
            }
            "spatial_transform_values" => {
                transform::transform_values(lease.client(), &catalog, &types::input(params)?).await
            }
            "spatial_query_table" => {
                table::query_table(
                    lease.client(),
                    &catalog,
                    &context.connection_id,
                    &types::input(params)?,
                )
                .await
            }
            "spatial_read_feature" => {
                table::read_feature(lease.client(), &catalog, &types::input(params)?).await
            }
            _ => unreachable!(),
        }
    };
    let result = match tokio::time::timeout_at(deadline, result).await {
        Ok(result) => result,
        Err(_) => {
            scope.stop_backend().await;
            Err(SpatialError::new(
                "DEADLINE_EXCEEDED",
                "Driver operation deadline exceeded",
            ))
        }
    };
    let successful = result.is_ok();
    let finish = lease.finish(&scope, successful, context.read_only).await;
    if let Err(mut error) = finish {
        if !context.read_only {
            error.code = "OUTCOME_UNKNOWN".into();
            error.outcome = "unknown".into();
        }
        if context.session_handle.is_some() {
            error.details = json!({"session_closed":true});
        }
        return Err(error);
    }
    if scope.cancelled() && result.is_ok() {
        let mut error = SpatialError::new(
            "CANCELLED",
            "Request was cancelled; transaction rollback was confirmed",
        );
        if context.session_handle.is_some() {
            error.details = json!({"session_closed":true});
        }
        return Err(error);
    }
    result.map_err(|mut error| {
        if error.code == "CANCELLED" && !scope.cancelled() {
            error.code = "DEADLINE_EXCEEDED".into();
        }
        if context.session_handle.is_some() {
            error.details = json!({"session_closed":true});
        }
        error
    })
}

impl SpatialError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            outcome: if code == "OUTCOME_UNKNOWN" {
                "unknown"
            } else {
                "not_applied"
            }
            .into(),
            details: Value::Null,
        }
    }
    pub fn argument(message: &str) -> Self {
        Self::new("INVALID_ARGUMENT", message)
    }
    pub fn geometry(message: &str) -> Self {
        Self::new("INVALID_GEOMETRY", message)
    }
    pub fn unsupported(message: &str) -> Self {
        Self::new("UNSUPPORTED_TYPE", message)
    }
    pub fn database(error: tokio_postgres::Error) -> Self {
        let code = match error.code().map(|c| c.code()) {
            Some("57014") => "CANCELLED",
            Some("25006") => "WRITE_NOT_ALLOWED",
            Some("42501") => "PERMISSION_DENIED",
            Some("XX000" | "22023" | "22003") => "INVALID_GEOMETRY",
            Some("42601" | "42P18" | "42703" | "42P01" | "42883") => "INVALID_ARGUMENT",
            _ => "DRIVER_EXITED",
        };
        let message = match code {
            "CANCELLED" => "Database statement was cancelled",
            "WRITE_NOT_ALLOWED" => "Database rejected a write in a read-only transaction",
            "PERMISSION_DENIED" => "Database permission denied",
            "INVALID_GEOMETRY" => "PostGIS rejected the geometry or coordinate transformation",
            _ => "Database operation failed",
        };
        Self::new(code, message)
    }
    pub fn response(&self, id: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000,
            "message": "Driver operation failed", "data": self } })
    }
}

impl std::fmt::Display for SpatialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SpatialError {}
