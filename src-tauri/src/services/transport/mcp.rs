use super::super::{
    backend::CancellationSignal,
    contracts::{ErrorCode, Operation, ServiceResponse, REQUEST_SCHEMA},
    dispatch::{CallerContext, CallerTransport},
    error::{failure, redact_error, ServiceResult},
};
use super::{request, ServiceTransport};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const MAX_PRESENTATION: usize = 64 * 1024;
const DATA_BUDGET: usize = 12 * 1024;

#[async_trait]
pub trait LegacyMcpBackend: Send + Sync {
    fn tools(&self) -> Value;
    async fn call(&self, name: &str, arguments: Value, caller: &CallerContext) -> Option<Value>;
    async fn read(&self, uri: &str, caller: &CallerContext) -> Option<Value>;
    async fn list_resources(&self, _caller: &CallerContext) -> Option<Value> {
        None
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ArtifactResource {
    Metadata {
        id: String,
    },
    Chunk {
        id: String,
        offset: u64,
        limit: usize,
    },
}
pub enum ResourceRead {
    NotHandled,
    Content(Value),
}

impl ArtifactResource {
    pub fn parse(uri: &str) -> ServiceResult<Option<Self>> {
        let Some(rest) = uri.strip_prefix("tabularis://artifact/") else {
            if uri.starts_with("tabularis://artifact") {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            return Ok(None);
        };
        if uri.len() > 512 || rest.contains(['#', '%', '/', '\\', '@']) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let (id, query) = rest
            .split_once('?')
            .map_or((rest, None), |(id, query)| (id, Some(query)));
        artifact_id(id)?;
        let Some(query) = query else {
            return Ok(Some(Self::Metadata { id: id.into() }));
        };
        let mut offset = None;
        let mut limit = None;
        for field in query.split('&') {
            let (name, value) = field
                .split_once('=')
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            match name {
                "offset" if offset.is_none() => {
                    offset = Some(
                        value
                            .parse::<u64>()
                            .map_err(|_| failure(ErrorCode::InvalidArgument))?,
                    )
                }
                "limit" if limit.is_none() => {
                    limit = Some(
                        value
                            .parse::<usize>()
                            .map_err(|_| failure(ErrorCode::InvalidArgument))?,
                    )
                }
                _ => return Err(failure(ErrorCode::InvalidArgument)),
            }
        }
        let limit = limit
            .filter(|limit| (1..=65536).contains(limit))
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        Ok(Some(Self::Chunk {
            id: id.into(),
            offset: offset.ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
            limit,
        }))
    }
}

fn artifact_id(id: &str) -> ServiceResult<()> {
    let uuid = id
        .strip_prefix("artifact-")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    if uuid.get_version_num() != 4
        || uuid.get_variant() != uuid::Variant::RFC4122
        || id != format!("artifact-{uuid}")
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    artifact_id: String,
    offset: u64,
    bytes_base64: String,
    eof: bool,
    checksum: String,
    chunk_checksum: String,
}

impl ServiceTransport {
    pub fn tools(&self) -> ServiceResult<Value> {
        let source: Value = serde_json::from_str(REQUEST_SCHEMA)
            .map_err(|_| failure(ErrorCode::CapabilityUnavailable))?;
        let mut tools = Vec::new();
        for name in self
            .validator
            .operation_names()
            .into_iter()
            .filter(|name| {
                serde_json::from_value::<super::super::contracts::Operation>(json!(name))
                    .map_or(true, |operation| !super::mcp_forbidden(operation))
            })
            .chain(std::iter::once("query.execute_write".into()))
        {
            let operation = if name == "query.execute_write" {
                "query.execute"
            } else {
                &name
            };
            let schema = operation_schema(&source, operation)?;
            tools.push(json!({"name":name,"description":format!("Common service {name}"),"inputSchema":schema}));
        }
        if let Some(legacy) = self.legacy.get() {
            let additional = legacy.tools();
            let additional = additional["tools"]
                .as_array()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
            let mut names: std::collections::HashSet<String> = tools
                .iter()
                .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
                .collect();
            for tool in additional {
                let name = tool["name"]
                    .as_str()
                    .filter(|name| {
                        !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
                    })
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
                if !names.insert(name.into()) {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
                tools.push(tool.clone());
            }
        }
        let value = json!({"tools":tools});
        if serde_json::to_vec(&value)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len()
            > request::MAX_FRAME_BYTES
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        Ok(value)
    }

    pub async fn call_tool(
        self: &Arc<Self>,
        name: &str,
        arguments: Value,
        mut caller: CallerContext,
        signal: Option<CancellationSignal>,
    ) -> ServiceResult<Value> {
        caller.transport = CallerTransport::Mcp;
        request::validate_caller(&caller)?;
        let alias = name == "query.execute_write";
        let operation = if alias { "query.execute" } else { name };
        if serde_json::from_value::<Operation>(json!(operation)).is_ok_and(super::mcp_forbidden) {
            return Err(failure(ErrorCode::OperationNotAllowed));
        }
        if !self
            .validator
            .operation_names()
            .iter()
            .any(|name| name == operation)
        {
            if let Some(legacy) = self.legacy.get() {
                if let Some(value) = legacy.call(name, arguments, &caller).await {
                    return Ok(value);
                }
            }
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        let decoded = request::decode(&self.validator, &arguments)?;
        if serde_json::to_value(decoded.operation)
            .ok()
            .as_ref()
            .and_then(Value::as_str)
            != Some(operation)
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if super::mcp_forbidden(decoded.operation) {
            return Err(failure(ErrorCode::OperationNotAllowed));
        }
        let response = match self.run_one(arguments, caller, alias, signal).await {
            Ok(response) => response,
            Err(error) => request::error_response(&decoded, error),
        };
        Ok(tool_result(decoded.operation, &response))
    }

    pub async fn read_resource(
        &self,
        uri: &str,
        caller: &CallerContext,
    ) -> ServiceResult<ResourceRead> {
        request::validate_caller(caller)?;
        let Some(resource) = ArtifactResource::parse(uri)? else {
            return Ok(ResourceRead::NotHandled);
        };
        match resource {
            ArtifactResource::Metadata { id } => {
                let metadata = self
                    .artifacts
                    .metadata(&id, caller)
                    .await
                    .map_err(redact_error)?;
                let text = serde_json::to_string(&metadata)
                    .map_err(|_| failure(ErrorCode::ResourceLimit))?;
                if metadata["artifact_id"] != id
                    || !metadata.is_object()
                    || text.len() > MAX_PRESENTATION
                    || metadata.get("bytes_base64").is_some()
                    || metadata.get("features").is_some()
                {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
                Ok(ResourceRead::Content(
                    json!({"contents":[{"uri":uri,"mimeType":"application/json","text":text}]}),
                ))
            }
            ArtifactResource::Chunk { id, offset, limit } => {
                let raw = self
                    .artifacts
                    .read_chunk(&id, caller, offset, limit)
                    .await
                    .map_err(redact_error)?;
                if serde_json::to_vec(&raw)
                    .map_err(|_| failure(ErrorCode::ResourceLimit))?
                    .len()
                    > 96 * 1024
                {
                    return Err(failure(ErrorCode::ResourceLimit));
                }
                let chunk: Chunk =
                    serde_json::from_value(raw).map_err(|_| failure(ErrorCode::InvalidArgument))?;
                let bytes = STANDARD
                    .decode(&chunk.bytes_base64)
                    .map_err(|_| failure(ErrorCode::InvalidArgument))?;
                let hex = |value: &str| {
                    value.len() == 64
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                };
                if chunk.artifact_id != id
                    || chunk.offset != offset
                    || bytes.len() > limit
                    || !hex(&chunk.checksum)
                    || !hex(&chunk.chunk_checksum)
                    || chunk.chunk_checksum != format!("{:x}", Sha256::digest(&bytes))
                    || STANDARD.encode(&bytes) != chunk.bytes_base64
                {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
                Ok(ResourceRead::Content(
                    json!({"contents":[{"uri":uri,"mimeType":"application/octet-stream","blob":chunk.bytes_base64}],"artifact_id":id,"offset":offset,"eof":chunk.eof,"checksum":chunk.checksum,"chunk_checksum":chunk.chunk_checksum}),
                ))
            }
        }
    }

    pub(crate) async fn resource_with_legacy(
        &self,
        uri: &str,
        caller: &CallerContext,
    ) -> ServiceResult<Value> {
        match self.read_resource(uri, caller).await? {
            ResourceRead::Content(value) => Ok(value),
            ResourceRead::NotHandled => {
                if let Some(legacy) = self.legacy.get() {
                    if let Some(value) = legacy.read(uri, caller).await {
                        return Ok(value);
                    }
                }
                Err(failure(ErrorCode::UnsupportedOperation))
            }
        }
    }
}

fn schema_references(
    value: &Value,
    names: &mut std::collections::BTreeSet<String>,
) -> ServiceResult<()> {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref") {
                let name = reference
                    .as_str()
                    .and_then(|reference| reference.strip_prefix("#/definitions/"))
                    .filter(|name| !name.is_empty() && !name.contains('/'))
                    .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
                names.insert(name.replace("~1", "/").replace("~0", "~"));
            }
            for value in object.values() {
                schema_references(value, names)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                schema_references(value, names)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn operation_schema(source: &Value, operation: &str) -> ServiceResult<Value> {
    let branch = source["oneOf"]
        .as_array()
        .and_then(|branches| {
            branches
                .iter()
                .find(|branch| branch["properties"]["operation"]["const"] == operation)
        })
        .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
    let mut schema = source.clone();
    let object = schema
        .as_object_mut()
        .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
    object.remove("$id");
    object.remove("definitions");
    object.insert("oneOf".into(), json!([branch]));
    schema["properties"]["operation"] = json!({"const":operation});
    let mut pending = std::collections::BTreeSet::new();
    let mut definitions = Map::new();
    schema_references(&schema, &mut pending)?;
    while let Some(name) = pending.pop_first() {
        if definitions.contains_key(&name) {
            continue;
        }
        let definition = source["definitions"]
            .get(&name)
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
        schema_references(definition, &mut pending)?;
        definitions.insert(name, definition.clone());
    }
    schema["definitions"] = Value::Object(definitions);
    Ok(schema)
}

struct Presentation {
    budget: usize,
    nodes: usize,
    truncated: bool,
}
impl Presentation {
    fn value(&mut self, value: &Value, depth: usize) -> Value {
        if depth >= 32 || self.nodes >= 2048 || self.budget < 16 {
            self.truncated = true;
            return Value::Null;
        }
        self.nodes += 1;
        match value {
            Value::String(text) => {
                let maximum = self.budget.min(1024);
                let mut end = text.len().min(maximum);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                self.truncated |= end < text.len();
                self.budget = self.budget.saturating_sub(end + 8);
                json!(&text[..end])
            }
            Value::Array(items) => {
                self.truncated |= items.len() > 5;
                self.budget = self.budget.saturating_sub(16);
                Value::Array(
                    items
                        .iter()
                        .take(5)
                        .map(|value| self.value(value, depth + 1))
                        .collect(),
                )
            }
            Value::Object(object) => {
                self.budget = self.budget.saturating_sub(16);
                let mut result = Map::new();
                for (key, value) in object.iter().take(64) {
                    if self.budget < key.len() + 16 || key.len() > 256 {
                        self.truncated = true;
                        continue;
                    }
                    self.budget -= key.len() + 8;
                    if key == "coordinates" && object.contains_key("type") {
                        self.truncated = true;
                        continue;
                    }
                    if matches!(key.as_str(), "native_state" | "tail") {
                        self.truncated = true;
                        continue;
                    }
                    if key == "geometry" && value.is_object() {
                        result.insert(
                            key.clone(),
                            json!({"type":self.value(&value["type"],depth+1)}),
                        );
                        self.truncated |= value.get("coordinates").is_some();
                        continue;
                    }
                    if matches!(key.as_str(), "rows" | "documents" | "values" | "features")
                        && value.is_array()
                    {
                        let count = value.as_array().map_or(0, Vec::len);
                        result.insert(format!("{key}_count"), json!(count));
                        result.insert(format!("{key}_sample"), self.value(value, depth + 1));
                    } else {
                        result.insert(key.clone(), self.value(value, depth + 1));
                    }
                }
                self.truncated |= object.len() > 64;
                Value::Object(result)
            }
            _ => {
                self.budget = self.budget.saturating_sub(32);
                value.clone()
            }
        }
    }
}

pub(crate) fn tool_result(operation: Operation, response: &ServiceResponse) -> Value {
    let mut presentation = Presentation {
        budget: DATA_BUDGET,
        nodes: 0,
        truncated: false,
    };
    let data = if matches!(
        operation,
        Operation::AuthBegin | Operation::AuthStatus | Operation::AuthCancel
    ) {
        let mut safe = Map::new();
        for key in ["auth_job_id", "state", "user_action_required", "error_code"] {
            if let Some(value) = response.data.get(key) {
                safe.insert(key.into(), presentation.value(value, 0));
            }
        }
        Value::Object(safe)
    } else {
        presentation.value(&response.data, 0)
    };
    let mut summary = json!({"protocol_version":response.protocol_version,"request_id":response.request_id,"connection_id":response.connection_id,"status":response.status,"job_id":response.job_id,"result_id":response.result_id,"data":data,"page":response.page,"limits":response.limits,"metrics":response.metrics,"warnings":response.warnings.iter().take(16).map(|warning| { let mut end=warning.len().min(256); while !warning.is_char_boundary(end) {end-=1;} warning[..end].to_owned() }).collect::<Vec<_>>(),"error":response.error.clone().map(redact_error),"presentation_truncated":presentation.truncated});
    let render = |summary: &Value| json!({"content":[{"type":"text","text":summary.to_string()}],"structuredContent":summary,"isError":response.error.is_some()});
    let mut output = render(&summary);
    if serde_json::to_vec(&output).map_or(true, |bytes| bytes.len() > MAX_PRESENTATION) {
        summary["data"] = json!({"omitted":true});
        summary["presentation_truncated"] = json!(true);
        summary["warnings"] = json!([]);
        output = render(&summary);
        if serde_json::to_vec(&output).map_or(true, |bytes| bytes.len() > MAX_PRESENTATION) {
            let error = redact_error(failure(ErrorCode::ResourceLimit));
            let safe = json!({"request_id":response.request_id,"connection_id":response.connection_id,"status":response.status,"job_id":response.job_id,"result_id":response.result_id,"presentation_truncated":true,"presentation_error":error});
            output = json!({"content":[{"type":"text","text":safe.to_string()}],"structuredContent":safe,"isError":true});
        }
    }
    output
}
