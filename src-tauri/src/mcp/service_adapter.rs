use super::protocol::JsonRpcRequest;
use crate::{
    service_native::{catalog, NativeCore},
    services::{
        contracts::{ErrorCode, Operation, ServiceResponse},
        dispatch::CallerContext,
        error::{failure, redact_error, ServiceResult},
        transport::{LegacyFailure, LegacyMcpBackend},
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
    /// Resource reads for service-capable drivers: the connection list and
    /// one connection's tables, through the common service.
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
        if catalog::driver_capabilities(&connection.driver)
            .await?
            .is_none()
        {
            return Ok(None);
        }
        if name != "list_tables"
            || !core
                .platform
                .runtime
                .driver
                .supports(&connection, Operation::CatalogObjects)
                .await?
        {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        let input = json!({"database":catalog::database(&core, &connection.id, None)?,"schema":args.get("schema").and_then(Value::as_str)});
        let response = catalog::call(
            &core,
            caller.clone(),
            Operation::CatalogObjects,
            Some(connection.id),
            input,
            false,
        )
        .await?;
        Ok(Some(summary(&response)))
    }
    /// Runs the request on the legacy server's own handler and returns its
    /// result or its JSON-RPC error untouched.
    async fn legacy(&self, method: &str, params: Value) -> Result<Value, LegacyFailure> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!("legacy-adapter")),
            method: method.into(),
            params: Some(params),
        };
        let failed = || LegacyFailure {
            code: -32603,
            message: "Legacy service failed".into(),
            data: None,
        };
        let response = super::handle_request(request).await.ok_or_else(failed)?;
        match (response.result, response.error) {
            (_, Some(error)) => Err(LegacyFailure {
                code: error.code,
                message: error.message,
                data: error.data,
            }),
            (Some(result), None) => Ok(result),
            (None, None) => Err(failed()),
        }
    }
}

#[async_trait]
impl LegacyMcpBackend for NativeLegacyMcp {
    fn tools(&self) -> Value {
        super::handle_list_tools(&crate::config::AppConfig::default())
            .map(super::service_route::advertise_timeout)
            .unwrap_or_else(|_| json!({"tools":[]}))
    }
    async fn call(
        &self,
        name: &str,
        arguments: Value,
        caller: &CallerContext,
    ) -> Option<Result<Value, LegacyFailure>> {
        if !matches!(
            name,
            "list_connections" | "list_databases" | "list_tables" | "describe_table" | "run_query"
        ) {
            return None;
        }
        let call = self.legacy("tools/call", json!({"name":name,"arguments":arguments}));
        let route = self
            .core()
            .ok()
            .filter(|_| name == "run_query")
            .map(|core| super::service_route::Route::new(core, caller.clone()));
        Some(match route {
            Some(route) => super::service_route::scope(route, call).await,
            None => call.await,
        })
    }
    fn client_connected(&self, name: &str) {
        crate::ai_activity::set_client_hint(Some(name.to_string()));
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
            Ok(None) => self
                .legacy("resources/read", json!({"uri":uri}))
                .await
                .unwrap_or_else(|_| json!({"contents":[]})),
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
