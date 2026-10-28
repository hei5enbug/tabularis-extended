use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio_postgres::Client;

use super::{
    catalog::Catalog,
    catalog_describe::ObjectKind,
    exact::{self, ExactInput, Parameter},
    types::{self, DriverContext, RowIdentity, TableRef},
    SpatialError,
};
use crate::{
    cancel::ScopedGuard,
    models::ConnectionParams,
    utils::identifiers::{qualified, quote_identifier},
};

#[derive(Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum CatalogInput {
    #[serde(rename = "catalog.databases")]
    Databases,
    #[serde(rename = "catalog.schemas")]
    Schemas { database: String },
    #[serde(rename = "catalog.objects")]
    Objects {
        database: String,
        schema: Option<String>,
    },
    #[serde(rename = "catalog.describe")]
    Describe {
        table: TableRef,
        #[serde(default)]
        kind: Option<ObjectKind>,
    },
}
#[derive(Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum RowInput {
    #[serde(rename = "row.insert")]
    Insert {
        table: TableRef,
        values: Map<String, Value>,
    },
    #[serde(rename = "row.update")]
    Update {
        table: TableRef,
        identity: RowIdentity,
        changes: Map<String, Value>,
    },
    #[serde(rename = "row.delete")]
    Delete {
        table: TableRef,
        identity: RowIdentity,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyInput {}

pub fn validate(
    params: &Value,
    method: &str,
    context: &DriverContext,
    connection: &ConnectionParams,
) -> Result<Option<TableRef>, SpatialError> {
    if method == "service_row" && context.read_only {
        return Err(SpatialError::new(
            "WRITE_NOT_ALLOWED",
            "Row mutations require a write context",
        ));
    }
    let (database, table) = match method {
        "service_test" | "service_invalidate_auth" => {
            types::input::<EmptyInput>(params)?;
            (None, None)
        }
        "service_catalog" => match types::input::<CatalogInput>(params)? {
            CatalogInput::Databases => (None, None),
            CatalogInput::Schemas { database } | CatalogInput::Objects { database, .. } => {
                (Some(database), None)
            }
            CatalogInput::Describe { table, .. } => (table.database.clone(), Some(table)),
        },
        "service_row" => {
            let table = match types::input::<RowInput>(params)? {
                RowInput::Insert { table, values } => {
                    validate_values(&values)?;
                    table
                }
                RowInput::Update { table, changes, .. } => {
                    validate_values(&changes)?;
                    table
                }
                RowInput::Delete { table, .. } => table,
            };
            (table.database.clone(), Some(table))
        }
        _ => {
            return Err(SpatialError::new(
                "UNSUPPORTED_OPERATION",
                "Unknown fixed service operation",
            ))
        }
    };
    let configured = crate::client::normalize_service_params(connection)?;
    if database
        .as_deref()
        .is_some_and(|database| configured.database.as_deref() != Some(database))
    {
        return Err(SpatialError::argument(
            "Input database does not match connection settings",
        ));
    }
    if let Some(table) = &table {
        table.connection(&configured)?;
    }
    Ok(table)
}

fn validate_values(values: &Map<String, Value>) -> Result<(), SpatialError> {
    if values.is_empty()
        || values
            .keys()
            .any(|name| name.is_empty() || name.contains('\0'))
    {
        return Err(SpatialError::argument(
            "A nonempty column-to-value object is required",
        ));
    }
    Ok(())
}
pub(super) fn exact_input(text: String, values: Vec<Value>) -> ExactInput {
    ExactInput {
        text,
        parameters: values
            .into_iter()
            .enumerate()
            .map(|(index, value)| Parameter {
                name: format!("${}", index + 1),
                value,
            })
            .collect(),
        max_rows: 10_000,
        max_bytes: 32 * 1024 * 1024,
        schema: None,
        settings: Vec::new(),
    }
}
pub(super) fn row_objects(result: &Value) -> Result<Vec<Value>, SpatialError> {
    let columns = result["columns"]
        .as_array()
        .ok_or_else(|| SpatialError::argument("Missing catalog columns"))?;
    result["rows"]
        .as_array()
        .ok_or_else(|| SpatialError::argument("Missing catalog rows"))?
        .iter()
        .map(|row| {
            let row = row
                .as_array()
                .ok_or_else(|| SpatialError::argument("Invalid catalog row"))?;
            let values = columns
                .iter()
                .zip(row)
                .map(|(name, value)| {
                    Ok((
                        name.as_str()
                            .ok_or_else(|| SpatialError::argument("Invalid catalog column"))?
                            .to_string(),
                        value.clone(),
                    ))
                })
                .collect::<Result<Map<_, _>, SpatialError>>()?;
            Ok(Value::Object(values))
        })
        .collect()
}

pub async fn catalog(
    client: &Client,
    catalog: Option<&Catalog>,
    input: CatalogInput,
    scope: &ScopedGuard,
    reserved: bool,
) -> Result<Value, SpatialError> {
    let (key, text, values, strings) = match input {
        CatalogInput::Databases => ("databases", "SELECT datname::text AS name FROM pg_catalog.pg_database WHERE datallowconn AND pg_catalog.has_database_privilege(oid,'CONNECT') ORDER BY datname".to_string(), vec![], true),
        CatalogInput::Schemas { .. } => ("schemas", "SELECT nspname::text AS name FROM pg_catalog.pg_namespace WHERE pg_catalog.has_schema_privilege(oid,'USAGE') ORDER BY nspname".to_string(), vec![], true),
        CatalogInput::Objects { schema, .. } => ("objects", super::catalog_objects::objects_sql(), vec![schema.map(Value::String).unwrap_or(Value::Null)], false),
        CatalogInput::Describe { table, kind } => {
            return super::catalog_describe::describe(client, catalog, &table, kind, scope, reserved)
                .await
        }
    };
    let result = exact::execute(
        client,
        catalog,
        &exact_input(text, values),
        scope,
        true,
        reserved,
    )
    .await?;
    let objects = row_objects(&result)?;
    let rows = if strings {
        objects
            .into_iter()
            .map(|row| row["name"].clone())
            .collect::<Vec<_>>()
    } else {
        objects
    };
    Ok(json!({key: rows,"limits":result["limits"],"page":result["page"]}))
}

async fn primary_key(client: &Client, table: &TableRef) -> Result<Vec<String>, SpatialError> {
    let rows = client.query("SELECT a.attname::text FROM pg_catalog.pg_constraint p JOIN pg_catalog.pg_class c ON c.oid=p.conrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace JOIN unnest(p.conkey) WITH ORDINALITY k(attnum,ordinality) ON true JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid AND a.attnum=k.attnum WHERE p.contype='p' AND n.nspname=$1 AND c.relname=$2 ORDER BY k.ordinality", &[&table.schema(), &table.table]).await.map_err(SpatialError::database)?;
    Ok(rows.iter().map(|row| row.get(0)).collect())
}
pub fn validate_identity(
    identity: &RowIdentity,
    primary_key: &[String],
) -> Result<(), SpatialError> {
    if primary_key.is_empty() {
        return Err(SpatialError::new(
            "UNSUPPORTED_OPERATION",
            "Update and delete require a primary key",
        ));
    }
    if identity.columns != primary_key
        || identity.values.len() != primary_key.len()
        || identity.values.iter().any(Value::is_null)
    {
        return Err(SpatialError::argument(
            "Identity must contain the complete ordered non-null primary key",
        ));
    }
    Ok(())
}
pub async fn row(
    client: &Client,
    catalog: Option<&Catalog>,
    input: RowInput,
    scope: &ScopedGuard,
    reserved: bool,
) -> Result<Value, SpatialError> {
    let table = match &input {
        RowInput::Insert { table, .. }
        | RowInput::Update { table, .. }
        | RowInput::Delete { table, .. } => table,
    };
    let pk = primary_key(client, table).await?;
    let relation = qualified(table.schema(), &table.table);
    let returning = if pk.is_empty() {
        String::new()
    } else {
        format!(
            " RETURNING {}",
            pk.iter()
                .map(|name| quote_identifier(name))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let (text, values, insert) = match input {
        RowInput::Insert { values, .. } => {
            validate_values(&values)?;
            let columns = values
                .keys()
                .map(|name| quote_identifier(name))
                .collect::<Vec<_>>()
                .join(",");
            let placeholders = (1..=values.len())
                .map(|index| format!("${index}"))
                .collect::<Vec<_>>()
                .join(",");
            (
                format!("INSERT INTO {relation} ({columns}) VALUES ({placeholders}){returning}"),
                values.into_iter().map(|(_, value)| value).collect(),
                true,
            )
        }
        RowInput::Update {
            identity, changes, ..
        } => {
            validate_identity(&identity, &pk)?;
            validate_values(&changes)?;
            let sets = changes
                .keys()
                .enumerate()
                .map(|(index, name)| format!("{}=${}", quote_identifier(name), index + 1))
                .collect::<Vec<_>>()
                .join(",");
            let predicate = identity
                .columns
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    format!("{}=${}", quote_identifier(name), changes.len() + index + 1)
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            let mut values = changes
                .into_iter()
                .map(|(_, value)| value)
                .collect::<Vec<_>>();
            values.extend(identity.values);
            (
                format!("UPDATE {relation} SET {sets} WHERE {predicate}"),
                values,
                false,
            )
        }
        RowInput::Delete { identity, .. } => {
            validate_identity(&identity, &pk)?;
            let predicate = identity
                .columns
                .iter()
                .enumerate()
                .map(|(index, name)| format!("{}=${}", quote_identifier(name), index + 1))
                .collect::<Vec<_>>()
                .join(" AND ");
            (
                format!("DELETE FROM {relation} WHERE {predicate}"),
                identity.values,
                false,
            )
        }
    };
    let result = exact::execute(
        client,
        catalog,
        &exact_input(text, values),
        scope,
        false,
        reserved,
    )
    .await?;
    let mut data = json!({"affected_rows":result["affected_rows"]});
    if insert && !pk.is_empty() {
        data["generated_keys"] = json!(row_objects(&result)?);
    }
    Ok(data)
}

pub async fn invalidate(
    context: &DriverContext,
    scope: &ScopedGuard,
) -> Result<Value, SpatialError> {
    if context.auth.is_some() || context.session_handle.is_some() {
        return Err(SpatialError::argument(
            "Authentication invalidation accepts only an opaque connection context",
        ));
    }
    scope.seal();
    let connection = context.connection_id.clone();
    let cancel = tokio::spawn(async move {
        let requests = crate::cancel::cancel_connection(&connection).await;
        let sessions = crate::session::invalidate_service_connection(&connection).await;
        let cursors = super::table::invalidate_connection(&connection);
        (requests, sessions, cursors)
    });
    let (requests, sessions, cursors) = tokio::time::timeout(
        std::time::Duration::from_millis(context.deadline_ms),
        cancel,
    )
    .await
    .map_err(|_| {
        SpatialError::new(
            "DEADLINE_EXCEEDED",
            "Authentication invalidation deadline exceeded",
        )
    })?
    .map_err(|_| {
        SpatialError::new(
            "DRIVER_EXITED",
            "Authentication invalidation could not be accepted",
        )
    })?;
    Ok(
        json!({"accepted":true,"discarded_sessions":sessions,"cancelled_requests":requests,"invalidated_cursors":cursors}),
    )
}
