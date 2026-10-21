use super::{output::ToolOutputFormat, protocol::JsonRpcRequest};
use crate::{
    service_native::{catalog, NativeCore},
    services::{
        contracts::{ErrorCode, Operation, ServiceResponse},
        dispatch::CallerContext,
        error::{failure, redact_error, ServiceResult},
        transport::LegacyMcpBackend,
    },
};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Weak;

pub struct NativeLegacyMcp {
    core: Weak<NativeCore>,
}
impl NativeLegacyMcp {
    pub fn new(core: Weak<NativeCore>) -> Self {
        Self { core }
    }
    fn core(&self) -> ServiceResult<std::sync::Arc<NativeCore>> {
        self.core
            .upgrade()
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))
    }
    async fn connection(
        &self,
        name: &str,
    ) -> ServiceResult<crate::services::backend::SavedConnection> {
        let core = self.core()?;
        let connections = core.platform.runtime.connections.list().await?;
        let matched = connections
            .iter()
            .filter(|connection| connection.id == name || connection.name == name)
            .collect::<Vec<_>>();
        if matched.len() != 1 {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        Ok(matched[0].clone())
    }
    async fn common(
        &self,
        name: &str,
        args: &Map<String, Value>,
        caller: &CallerContext,
    ) -> ServiceResult<Option<Value>> {
        let core = self.core()?;
        if name == "list_connections" {
            let response = catalog::call(
                &core,
                caller.clone(),
                Operation::ConnectionList,
                None,
                json!({}),
                false,
            )
            .await?;
            return Ok(Some(summary(&response)));
        }
        let name_or_id = args["connection_id"]
            .as_str()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let connection = self.connection(name_or_id).await?;
        core.platform
            .runtime
            .policy
            .permission(&connection, caller)
            .await?;
        let Some((_, caps)) = catalog::driver_capabilities(&connection.driver).await? else {
            return Ok(None);
        };
        let schema = args.get("schema").and_then(Value::as_str);
        let operation = match name {
            "list_databases" => Operation::CatalogDatabases,
            "list_tables" => Operation::CatalogObjects,
            "describe_table" => Operation::CatalogDescribe,
            "run_query" => Operation::QueryExecute,
            _ => return Err(failure(ErrorCode::UnsupportedOperation)),
        };
        if !core
            .platform
            .runtime
            .driver
            .supports(&connection, operation)
            .await?
        {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        let database = || {
            catalog::database(
                &core,
                &connection.id,
                args.get("database")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            )
        };
        let mut input = match operation {
            Operation::CatalogDatabases => json!({}),
            Operation::CatalogObjects => json!({"database":database()?,"schema":schema}),
            Operation::CatalogDescribe => {
                json!({"table":{"database":database()?,"schema":schema,"table":args["table_name"].as_str().ok_or_else(||failure(ErrorCode::InvalidArgument))?}})
            }
            Operation::QueryExecute => {
                let text = args["query"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
                let size = match args.get("limit") {
                    None => 100,
                    Some(value) => value
                        .as_u64()
                        .filter(|size| *size >= 1)
                        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
                        .min(1000),
                };
                let mut query = json!({"language":if caps.documents{"cosmos_sql"}else{"sql"},"text":text,"parameters":[],"page_size":size,"mode":"exact"});
                if caps.documents {
                    for field in ["database", "container"] {
                        query[field] = json!(args[field]
                            .as_str()
                            .filter(|value| !value.is_empty())
                            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?);
                    }
                } else {
                    if args.contains_key("container") {
                        return Err(failure(ErrorCode::InvalidArgument));
                    }
                    query["database"] = json!(database()?);
                }
                query
            }
            _ => return Err(failure(ErrorCode::UnsupportedOperation)),
        };
        let write = match args.get("write") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
        };
        let mut response = catalog::call(
            &core,
            caller.clone(),
            operation,
            Some(connection.id),
            input.take(),
            write,
        )
        .await?;
        if operation == Operation::CatalogDescribe {
            response.data["indexes"] = Value::Null;
            response.data["foreign_keys"] = Value::Null;
            response.data["unsupported_fields"] = json!(["indexes", "foreign_keys"]);
        }
        Ok(Some(summary(&response)))
    }
    fn format(&self, args: &Map<String, Value>) -> ServiceResult<ToolOutputFormat> {
        let core = self.core()?;
        let path = core.platform.approvals.profile().join("config.json");
        let config = match crate::persistence::service_read_file(&path, 16 * 1024 * 1024, false)? {
            Some(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| failure(ErrorCode::PermissionDenied))?
            }
            None => crate::config::AppConfig::default(),
        };
        ToolOutputFormat::from_arguments(Some(args), ToolOutputFormat::from_config(&config))
            .map_err(|_| failure(ErrorCode::InvalidArgument))
    }
    async fn legacy(&self, method: &str, params: Value) -> Value {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!("legacy-adapter")),
            method: method.into(),
            params: Some(params),
        };
        match super::handle_request(request).await {Some(response)=>response.result.unwrap_or_else(||json!({"isError":true,"content":[{"type":"text","text":"Legacy service failed"}]})),None=>json!({"isError":true,"content":[{"type":"text","text":"Legacy service failed"}]})}
    }
}

#[async_trait]
impl LegacyMcpBackend for NativeLegacyMcp {
    fn tools(&self) -> Value {
        super::handle_list_tools(&crate::config::AppConfig::default())
            .unwrap_or_else(|_| json!({"tools":[]}))
    }
    async fn call(&self, name: &str, arguments: Value, caller: &CallerContext) -> Option<Value> {
        if !matches!(
            name,
            "list_connections" | "list_databases" | "list_tables" | "describe_table" | "run_query"
        ) {
            return None;
        }
        let result = async {
            let args = arguments
                .as_object()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
            let format = self.format(args)?;
            match self.common(name, args, caller).await? {
                Some(summary) => {
                    let mut output = super::tool_result(&summary, format)
                        .map_err(|_| failure(ErrorCode::ResourceLimit))?;
                    output["structuredContent"] = summary;
                    if serde_json::to_vec(&output)
                        .map_err(|_| failure(ErrorCode::ResourceLimit))?
                        .len()
                        > 64 * 1024
                    {
                        return Err(failure(ErrorCode::ResourceLimit));
                    }
                    Ok(output)
                }
                None => Ok(self
                    .legacy("tools/call", json!({"name":name,"arguments":arguments}))
                    .await),
            }
        }
        .await;
        Some(match result {
            Ok(value) => value,
            Err(error) => {
                json!({"isError":true,"content":[{"type":"text","text":"Service request failed"}],"structuredContent":{"error":redact_error(error)}})
            }
        })
    }
    async fn read(&self, uri: &str, caller: &CallerContext) -> Option<Value> {
        let name = if uri == "tabularis://connections" {
            Some(("list_connections", json!({})))
        } else {
            uri.strip_prefix("tabularis://")
                .and_then(|rest| rest.strip_suffix("/schema"))
                .filter(|id| !id.contains('/') && !id.is_empty())
                .map(|id| ("list_tables", json!({"connection_id":id})))
        };
        let (name, args) = name?;
        let result = self
            .common(name, args.as_object().expect("resource arguments"), caller)
            .await;
        Some(match result {
            Ok(Some(summary)) => {
                json!({"contents":[{"uri":uri,"mimeType":"application/json","text":summary.to_string()}]})
            }
            Ok(None) => self.legacy("resources/read", json!({"uri":uri})).await,
            Err(error) => {
                json!({"contents":[{"uri":uri,"mimeType":"application/json","text":json!({"error":redact_error(error)}).to_string()}]})
            }
        })
    }
    async fn list_resources(&self, caller: &CallerContext) -> Option<Value> {
        let result=async {let core=self.core()?;let connections=core.platform.runtime.connections.list().await?;let mut resources=vec![json!({"uri":"tabularis://connections","name":"Connections List","mimeType":"application/json"})];for connection in connections {if core.platform.runtime.policy.permission(&connection,caller).await.is_ok(){resources.push(json!({"uri":format!("tabularis://{}/schema",connection.id),"name":connection.name,"mimeType":"application/json"}));}}Ok::<_,crate::services::contracts::ServiceError>(json!({"resources":resources}))}.await;
        Some(result.unwrap_or_else(|error| json!({"resources":[],"error":redact_error(error)})))
    }
}
fn sample(value: &Value, budget: &mut usize, depth: usize, truncated: &mut bool) -> Value {
    if depth > 16 || *budget < 64 {
        *truncated = true;
        return Value::Null;
    }
    *budget -= 32;
    match value {
        Value::String(text) => {
            let mut end = text.len().min((*budget).min(1024));
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            *budget = budget.saturating_sub(end);
            *truncated |= end < text.len();
            json!(&text[..end])
        }
        Value::Array(values) => {
            *truncated |= values.len() > 5;
            Value::Array(
                values
                    .iter()
                    .take(5)
                    .map(|value| sample(value, budget, depth + 1, truncated))
                    .collect(),
            )
        }
        Value::Object(object) => {
            let mut output = Map::new();
            for (key, value) in object.iter().take(64) {
                if matches!(key.as_str(), "coordinates" | "native_state" | "tail") {
                    *truncated = true;
                    continue;
                }
                if *budget < key.len() + 64 {
                    *truncated = true;
                    break;
                }
                *budget -= key.len();
                output.insert(key.clone(), sample(value, budget, depth + 1, truncated));
            }
            *truncated |= object.len() > 64;
            Value::Object(output)
        }
        _ => value.clone(),
    }
}
fn summary(response: &ServiceResponse) -> Value {
    let mut budget = 12 * 1024;
    let mut truncated = false;
    let data = sample(&response.data, &mut budget, 0, &mut truncated);
    json!({"request_id":response.request_id,"connection_id":response.connection_id,"status":response.status,"job_id":response.job_id,"result_id":response.result_id,"data":data,"page":response.page,"limits":response.limits,"warnings":response.warnings.iter().take(16).collect::<Vec<_>>(),"presentation_truncated":truncated})
}

#[cfg(test)]
#[path = "service_adapter_tests.rs"]
mod tests;
