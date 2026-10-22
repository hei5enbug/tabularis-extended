use serde::Deserialize;
use serde_json::{json, Value};
use tokio_postgres::Client;

use super::{
    exact::{search_path_statement, validate_text},
    SpatialError,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplainInput {
    pub sql: String,
    #[serde(default)]
    pub schema: Option<String>,
}

/// The planner-only statement. It carries no `ANALYZE` or `BUFFERS` option,
/// so the planned statement is never run.
pub fn statement(sql: &str) -> Result<String, SpatialError> {
    validate_text(sql)?;
    Ok(format!("EXPLAIN (FORMAT JSON) {sql}"))
}

/// The estimated plan of one statement, read inside the caller's read-only
/// transaction, which the lease rolls back afterwards.
pub async fn explain(client: &Client, input: &ExplainInput) -> Result<Value, SpatialError> {
    let text = statement(&input.sql)?;
    if let Some(set_local) = search_path_statement(input.schema.as_deref())? {
        client
            .batch_execute(&set_local)
            .await
            .map_err(SpatialError::database)?;
    }
    let row = client
        .query_one(&text, &[])
        .await
        .map_err(SpatialError::database)?;
    let plan: Value = row.try_get(0).map_err(SpatialError::database)?;
    Ok(json!({"engine":"postgres","format":"postgres-json","payload":plan}))
}

#[cfg(test)]
#[path = "explain_tests.rs"]
mod explain_tests;
