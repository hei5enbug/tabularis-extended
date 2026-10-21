use super::super::{
    contracts::ErrorCode,
    dispatch::CallerContext,
    error::{failure, redact_error, ServiceResult},
};
use super::{request, ServiceTransport};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MAX_PRESENTATION: usize = 64 * 1024;

/// A JSON-RPC error the legacy server answers with, passed through unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyFailure {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

#[async_trait]
pub trait LegacyMcpBackend: Send + Sync {
    fn tools(&self) -> Value;
    /// `None` when the name is not a legacy tool.
    async fn call(
        &self,
        name: &str,
        arguments: Value,
        caller: &CallerContext,
    ) -> Option<Result<Value, LegacyFailure>>;
    /// The client named in the `initialize` request, for audit records.
    fn client_connected(&self, _name: &str) {}
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
