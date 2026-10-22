//! The estimated plan of one statement for the service protocol: the plan is
//! read through `SET SHOWPLAN_XML`, so the statement is never run. The plan
//! connection comes from the same service pool and login as exact queries and
//! is thrown away afterwards, so no SHOWPLAN state can reach another request.

use super::{exact, pool, registry::Scope, types::DriverContext, ServiceError};
use crate::models::ConnectionParams;
use mssql_tds::{
    connection::tds_client::{ResultSet, ResultSetClient},
    core::CancelHandle,
};
use mssql_tiberius_bridge::{row::RowSchema, Row};
use serde::Deserialize;
use serde_json::{json, Value};

pub(super) const SHOWPLAN_ON: &str = "SET SHOWPLAN_XML ON";
pub(super) const SHOWPLAN_OFF: &str = "SET SHOWPLAN_XML OFF";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub sql: String,
    /// Accepted for symmetry with exact queries; T-SQL has no search path.
    #[serde(default)]
    #[allow(dead_code)]
    pub schema: Option<String>,
}

/// A connection that runs one batch at a time and returns the text cells of
/// the results that hold a `ShowPlanXML` document.
pub(super) trait PlanConnection {
    async fn batch(&mut self, sql: &str) -> Result<Vec<String>, ServiceError>;
    /// Closes the physical connection instead of returning it to a pool.
    fn detach(self);
}

struct NativePlan {
    client: pool::NativeConnection,
    cancel: CancelHandle,
}

impl PlanConnection for NativePlan {
    async fn batch(&mut self, sql: &str) -> Result<Vec<String>, ServiceError> {
        let client = &mut *self.client;
        client.close_query().await.map_err(ServiceError::database)?;
        client
            .execute(sql.to_owned(), None, Some(&self.cancel))
            .await
            .map_err(ServiceError::database)?;
        let mut documents = Vec::new();
        while let Some(result) = client.get_current_resultset() {
            let metadata = result.get_metadata().clone();
            let schema = RowSchema::from_metadata(&metadata);
            while let Some(values) = result.next_row().await.map_err(ServiceError::database)? {
                let row = Row::from_schema(schema.clone(), values);
                for (index, column) in metadata.iter().enumerate() {
                    let column_type = crate::driver::extract::normalized_column_type(
                        column.data_type,
                        column.type_info.length,
                    );
                    if let Ok(Value::String(text)) =
                        crate::driver::extract::extract_value_as(&row, index, column_type)
                    {
                        if text.contains("ShowPlanXML") {
                            documents.push(text);
                        }
                    }
                }
            }
            if !client
                .move_to_next()
                .await
                .map_err(ServiceError::database)?
            {
                break;
            }
        }
        Ok(documents)
    }

    fn detach(self) {
        pool::discard(self.client);
    }
}

fn stopped(scope: &Scope) -> ServiceError {
    scope
        .remaining()
        .err()
        .unwrap_or_else(|| ServiceError::new("CANCELLED", "The service request was cancelled"))
}

fn no_plan() -> ServiceError {
    ServiceError::new(
        "DRIVER_EXITED",
        "SQL Server could not produce an estimated plan",
    )
}

pub fn validate(input: &Input) -> Result<(), ServiceError> {
    exact::validate_text(&input.sql)
}

/// Plans `sql` on `connection` in three batches: SHOWPLAN on, the statement,
/// SHOWPLAN off. The connection is detached however the run ends.
pub(super) async fn run_plan<C: PlanConnection>(
    mut connection: C,
    sql: &str,
    scope: &Scope,
) -> Result<Value, ServiceError> {
    let plan = tokio::select! {
        plan = async {
            connection.batch(SHOWPLAN_ON).await?;
            let documents = connection.batch(sql).await?;
            connection.batch(SHOWPLAN_OFF).await?;
            Ok::<_, ServiceError>(documents)
        } => Some(plan),
        _ = scope.wait_cancelled() => None,
        _ = tokio::time::sleep_until(scope.state.deadline) => None,
    };
    connection.detach();
    match plan {
        Some(Ok(mut documents)) if documents.len() == 1 => Ok(
            json!({"engine":"sqlserver","format":"sqlserver-showplan-xml","payload":documents.remove(0)}),
        ),
        Some(Ok(_)) => Err(no_plan()),
        Some(Err(error)) => Err(error),
        None => Err(stopped(scope)),
    }
}

pub async fn estimated_plan(
    input: Input,
    context: &DriverContext,
    connection: &ConnectionParams,
    scope: &Scope,
) -> Result<Value, ServiceError> {
    validate(&input)?;
    if !context.read_only {
        return Err(ServiceError::new(
            "WRITE_NOT_ALLOWED",
            "An estimated plan needs a read-only context",
        ));
    }
    if context.session_handle.is_some() {
        return Err(ServiceError::argument(
            "An estimated plan cannot run inside a session",
        ));
    }
    scope.remaining()?;
    let client = pool::acquire(connection, context, scope).await?;
    let plan = NativePlan {
        client,
        cancel: scope.cancel_handle(),
    };
    run_plan(plan, &input.sql, scope).await
}
