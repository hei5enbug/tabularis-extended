use super::{
    catalog::TableRef,
    exact::{self, Parameter},
    ServiceError,
};
use crate::{
    driver::helpers::{bracket_quote, wrap_dml_with_rowcount},
    models::ConnectionParams,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub columns: Vec<String>,
    pub values: Vec<Value>,
}
#[derive(Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum Input {
    #[serde(rename = "row.insert")]
    Insert {
        table: TableRef,
        values: BTreeMap<String, Value>,
    },
    #[serde(rename = "row.update")]
    Update {
        table: TableRef,
        identity: Identity,
        changes: BTreeMap<String, Value>,
    },
    #[serde(rename = "row.delete")]
    Delete { table: TableRef, identity: Identity },
}
pub struct Query {
    pub text: String,
    pub parameters: Vec<Parameter>,
}
pub async fn validate_primary_key(
    client: &mut mssql_tds::connection::tds_client::TdsClient,
    input: &Input,
    cancel: &mssql_tds::core::CancelHandle,
) -> Result<(), ServiceError> {
    let (table, identity) = match input {
        Input::Insert { .. } => return Ok(()),
        Input::Update {
            table, identity, ..
        }
        | Input::Delete { table, identity } => (table, identity),
    };
    let text = "SELECT c.name FROM sys.tables t JOIN sys.indexes i ON i.object_id=t.object_id AND i.is_primary_key=1 JOIN sys.index_columns ic ON ic.object_id=i.object_id AND ic.index_id=i.index_id JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id WHERE t.object_id=OBJECT_ID(@table) AND ic.key_ordinal>0 ORDER BY ic.key_ordinal";
    let parameters = exact::bind(&[Parameter {
        name: "@table".into(),
        value: json!(table.qualified()),
    }])?;
    let data = exact::collect(
        client,
        text,
        parameters,
        Some(cancel),
        128,
        super::types::FRAME_LIMIT,
        false,
    )
    .await?;
    let keys = data["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row[0].as_str().map(str::to_owned).ok_or_else(|| {
                ServiceError::new(
                    "CAPABILITY_UNAVAILABLE",
                    "Primary key metadata is unavailable",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if keys.is_empty() {
        return Err(ServiceError::new(
            "UNSUPPORTED_OPERATION",
            "Update and delete require a table primary key",
        ));
    }
    if identity.columns != keys
        || identity.values.len() != keys.len()
        || identity.values.iter().any(Value::is_null)
    {
        return Err(ServiceError::argument(
            "Identity must contain the complete ordered non-null primary key",
        ));
    }
    Ok(())
}
fn identifier(value: &str) -> Result<(), ServiceError> {
    if value.is_empty() || value.contains('\0') || value.chars().count() > 128 {
        Err(ServiceError::argument("Invalid row column identifier"))
    } else {
        Ok(())
    }
}
fn add(parameters: &mut Vec<Parameter>, value: &Value) -> Result<String, ServiceError> {
    exact::value_type(value)?;
    let name = format!("@P{}", parameters.len() + 1);
    parameters.push(Parameter {
        name: name.clone(),
        value: value.clone(),
    });
    Ok(name)
}
fn predicate(identity: &Identity, parameters: &mut Vec<Parameter>) -> Result<String, ServiceError> {
    if identity.columns.is_empty() || identity.columns.len() != identity.values.len() {
        return Err(ServiceError::argument(
            "A complete row identity is required",
        ));
    }
    let mut names = std::collections::HashSet::new();
    identity
        .columns
        .iter()
        .zip(&identity.values)
        .map(|(column, value)| {
            identifier(column)?;
            if !names.insert(column.to_ascii_lowercase()) {
                return Err(ServiceError::argument("Duplicate row identity columns"));
            }
            Ok(if value.is_null() {
                format!("{} IS NULL", bracket_quote(column))
            } else {
                format!("{}={}", bracket_quote(column), add(parameters, value)?)
            })
        })
        .collect::<Result<Vec<_>, ServiceError>>()
        .map(|parts| parts.join(" AND "))
}
pub fn query(input: &Input, params: &ConnectionParams) -> Result<Query, ServiceError> {
    let mut parameters = Vec::new();
    let text = match input {
        Input::Insert { table, values } => {
            table.validate(params)?;
            if values.is_empty() {
                return Err(ServiceError::argument("Insert values are required"));
            }
            let columns = values.keys().cloned().collect::<Vec<_>>();
            for column in &columns {
                identifier(column)?;
            }
            for value in values.values() {
                add(&mut parameters, value)?;
            }
            crate::driver::helpers::build_insert_sql(
                table.schema.as_deref(),
                &table.table,
                &columns,
                false,
            )
        }
        Input::Update {
            table,
            identity,
            changes,
        } => {
            table.validate(params)?;
            if changes.is_empty() {
                return Err(ServiceError::argument("Row changes are required"));
            }
            let mut assignments = Vec::new();
            for (column, value) in changes {
                identifier(column)?;
                assignments.push(format!(
                    "{}={}",
                    bracket_quote(column),
                    add(&mut parameters, value)?
                ));
            }
            let filter = predicate(identity, &mut parameters)?;
            wrap_dml_with_rowcount(&format!(
                "UPDATE {} SET {} WHERE {}",
                table.qualified(),
                assignments.join(","),
                filter
            ))
        }
        Input::Delete { table, identity } => {
            table.validate(params)?;
            let filter = predicate(identity, &mut parameters)?;
            wrap_dml_with_rowcount(&format!(
                "DELETE FROM {} WHERE {}",
                table.qualified(),
                filter
            ))
        }
    };
    Ok(Query { text, parameters })
}
