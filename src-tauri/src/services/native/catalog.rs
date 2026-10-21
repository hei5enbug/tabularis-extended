use super::NativeCore;
use crate::{
    drivers::driver_trait::{DatabaseDriver, DriverServiceCapabilities},
    models::{ConnectionParams, TableColumn, TestConnectionRequest},
    services::{
        contracts::{ErrorCode, Operation, ServiceRequest, ServiceResponse},
        dispatch::{CallerContext, CallerTransport},
        error::{failure, ServiceResult},
    },
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tauri::Manager;

#[derive(Serialize)]
pub struct ServiceTableColumn {
    #[serde(flatten)]
    pub column: TableColumn,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_key_position: Option<u32>,
}
impl From<TableColumn> for ServiceTableColumn {
    fn from(column: TableColumn) -> Self {
        Self {
            column,
            primary_key_position: None,
        }
    }
}

pub fn request(
    operation: Operation,
    connection_id: Option<String>,
    input: Value,
) -> ServiceRequest {
    ServiceRequest {
        protocol_version: 1,
        operation,
        request_id: uuid::Uuid::new_v4().to_string(),
        connection_id,
        session_id: None,
        map_id: None,
        expected_version: None,
        deadline_ms: 30_000,
        input,
    }
}
pub fn command_error(error: crate::services::contracts::ServiceError) -> String {
    let error = crate::services::error::redact_error(error);
    format!("{:?}: {}", error.code, error.message)
}

pub async fn driver_capabilities(
    driver_id: &str,
) -> ServiceResult<Option<(Arc<dyn DatabaseDriver>, DriverServiceCapabilities)>> {
    let driver = crate::drivers::registry::get_driver(driver_id)
        .await
        .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
    match tokio::time::timeout(Duration::from_secs(30), driver.service_capabilities())
        .await
        .map_err(|_| failure(ErrorCode::DeadlineExceeded))?
    {
        Ok(capabilities) if capabilities.service_protocol == 1 => Ok(Some((driver, capabilities))),
        Ok(_) => Err(failure(ErrorCode::ProtocolMismatch)),
        Err(error)
            if matches!(
                error.code,
                ErrorCode::CapabilityUnavailable | ErrorCode::UnsupportedOperation
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
pub async fn for_connection<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: &str,
) -> Result<Option<Arc<NativeCore>>, String> {
    let core = app
        .try_state::<Arc<NativeCore>>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| command_error(failure(ErrorCode::CapabilityUnavailable)))?;
    let connection = core
        .platform
        .runtime
        .connections
        .get(id)
        .await
        .map_err(command_error)?;
    Ok(driver_capabilities(&connection.driver)
        .await
        .map_err(command_error)?
        .map(|_| core))
}
pub fn database(core: &NativeCore, id: &str, explicit: Option<String>) -> ServiceResult<String> {
    if let Some(database) = explicit {
        if database.is_empty() || database.chars().any(char::is_control) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        return Ok(database);
    }
    let databases = core
        .platform
        .connections
        .private_params(id)?
        .database
        .as_vec();
    if databases.len() != 1 {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(databases[0].clone())
}
pub async fn call(
    core: &Arc<NativeCore>,
    caller: CallerContext,
    operation: Operation,
    id: Option<String>,
    input: Value,
    write: bool,
) -> ServiceResult<ServiceResponse> {
    let raw = serde_json::to_value(request(operation, id, input))
        .map_err(|_| failure(ErrorCode::InvalidArgument))?;
    let response = core.transport.run_one(raw, caller, write, None).await?;
    if let Some(error) = response.error.clone() {
        return Err(error);
    }
    Ok(response)
}
pub async fn gui_call(
    core: &Arc<NativeCore>,
    operation: Operation,
    id: &str,
    input: Value,
) -> Result<Value, String> {
    Ok(call(
        core,
        core.caller(CallerTransport::Gui).map_err(command_error)?,
        operation,
        Some(id.into()),
        input,
        false,
    )
    .await
    .map_err(command_error)?
    .data)
}
pub fn names(data: &Value, key: &str) -> ServiceResult<Vec<String>> {
    data.as_array()
        .or_else(|| data.get(key).and_then(Value::as_array))
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .or_else(|| value.get("name").and_then(Value::as_str))
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))
        })
        .collect()
}
pub fn columns(data: &Value) -> ServiceResult<Vec<ServiceTableColumn>> {
    data["columns"]
        .as_array()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
        .iter()
        .map(|value| {
            let name = value["name"]
                .as_str()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
                .to_owned();
            let data_type = value["data_type"]
                .as_str()
                .or_else(|| value["native_type"].as_str())
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
                .to_owned();
            let primary_key_position = match value.get("primary_key_position") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .filter(|position| *position > 0 && *position <= u32::MAX as u64)
                        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
                        as u32,
                ),
            };
            let boolean = |first: &str, second: &str, default: bool| {
                value
                    .get(first)
                    .or_else(|| value.get(second))
                    .and_then(Value::as_bool)
                    .unwrap_or(default)
            };
            Ok(ServiceTableColumn {
                column: TableColumn {
                    name,
                    data_type,
                    is_pk: primary_key_position.is_some()
                        || boolean("is_pk", "is_primary_key", false),
                    is_nullable: boolean("is_nullable", "nullable", true),
                    is_auto_increment: boolean("is_auto_increment", "is_identity", false),
                    is_generated: boolean("is_generated", "generated", false),
                    default_value: value["default_value"].as_str().map(str::to_owned),
                    character_maximum_length: value["character_maximum_length"].as_u64(),
                    comment: value["comment"].as_str().map(str::to_owned),
                },
                primary_key_position,
            })
        })
        .collect()
}
pub fn identity(
    columns: &[ServiceTableColumn],
    keys: &HashMap<String, Value>,
) -> ServiceResult<Value> {
    let mut ordered = columns
        .iter()
        .filter(|column| column.column.is_pk)
        .collect::<Vec<_>>();
    ordered.sort_by_key(|column| column.primary_key_position.unwrap_or(u32::MAX));
    if ordered.is_empty()
        || ordered.len() != keys.len()
        || ordered.iter().enumerate().any(|(index, column)| {
            column.primary_key_position != Some(index as u32 + 1)
                || !keys.contains_key(&column.column.name)
        })
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let values = ordered
        .iter()
        .map(|column| keys[&column.column.name].clone())
        .collect::<Vec<_>>();
    if values.iter().any(Value::is_object)
        || values.iter().any(Value::is_array)
        || values.iter().any(Value::is_null)
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(
        json!({"columns":ordered.iter().map(|column|&column.column.name).collect::<Vec<_>>(),"values":values}),
    )
}
pub fn values(values: &HashMap<String, Value>) -> ServiceResult<Value> {
    if values.is_empty()
        || values
            .keys()
            .any(|key| key.is_empty() || key.chars().any(char::is_control))
        || values.values().any(|value| {
            value.as_str().is_some_and(|text| {
                text == "__USE_DEFAULT__"
                    || (text.trim().to_ascii_uppercase().starts_with("ST_") && text.contains('('))
            })
        })
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    serde_json::to_value(values).map_err(|_| failure(ErrorCode::InvalidArgument))
}
pub struct NativeRowInput {
    pub table: String,
    pub schema: Option<String>,
    pub database: Option<String>,
    pub keys: HashMap<String, Value>,
    pub changes: HashMap<String, Value>,
}
pub async fn row(
    core: &Arc<NativeCore>,
    id: &str,
    operation: Operation,
    row: NativeRowInput,
) -> ServiceResult<u64> {
    let NativeRowInput {
        table,
        schema,
        database: explicit_database,
        keys,
        changes,
    } = row;
    let table =
        json!({"database":database(core,id,explicit_database)?,"schema":schema,"table":table});
    let mut input = json!({"table":table});
    if operation != Operation::RowInsert {
        let description = call(
            core,
            core.caller(CallerTransport::Gui)?,
            Operation::CatalogDescribe,
            Some(id.into()),
            json!({"table":table}),
            false,
        )
        .await?
        .data;
        input["identity"] = identity(&columns(&description)?, &keys)?;
    }
    match operation {
        Operation::RowInsert => input["values"] = values(&changes)?,
        Operation::RowUpdate => input["changes"] = values(&changes)?,
        Operation::RowDelete => {}
        _ => return Err(failure(ErrorCode::InvalidArgument)),
    }
    let response = call(
        core,
        core.caller(CallerTransport::Gui)?,
        operation,
        Some(id.into()),
        input,
        false,
    )
    .await?;
    response.data["affected_rows"].as_u64().ok_or_else(|| {
        let mut error = failure(ErrorCode::CapabilityUnavailable);
        error.outcome = crate::services::contracts::Outcome::Applied;
        error
    })
}
pub fn unchanged_test(core: &NativeCore, request: &TestConnectionRequest) -> ServiceResult<String> {
    let id = request
        .connection_id
        .as_deref()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    let saved = core.platform.connections.private_params(id)?;
    let canonical = |params: &ConnectionParams| -> ServiceResult<Value> {
        let mut value =
            serde_json::to_value(params).map_err(|_| failure(ErrorCode::InvalidArgument))?;
        value
            .as_object_mut()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
            .remove("connection_id");
        Ok(value)
    };
    if canonical(&saved)? != canonical(&request.params)? {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(id.into())
}
