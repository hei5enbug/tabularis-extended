use super::{
    contracts::ErrorCode,
    dispatch::{CallerContext, CallerTransport},
    error::{failure, ServiceResult},
};
use async_trait::async_trait;
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretPersistence {
    Keychain,
    Session,
}
impl SecretPersistence {
    pub fn parse(value: Option<&str>) -> ServiceResult<Self> {
        match value {
            None | Some("keychain") => Ok(Self::Keychain),
            Some("session") => Ok(Self::Session),
            _ => Err(failure(ErrorCode::InvalidArgument)),
        }
    }
}

#[derive(Clone)]
pub struct Secret(Vec<u8>);
impl Secret {
    pub fn new(bytes: Vec<u8>) -> ServiceResult<Self> {
        if bytes.is_empty() || bytes.len() > 64 * 1024 || std::str::from_utf8(&bytes).is_err() {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        Ok(Self(bytes))
    }
    pub fn expose(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("")
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SecretSlot {
    DbPassword,
    CosmosAccountKey,
    EntraClientSecret,
    EntraRefreshToken,
}
impl SecretSlot {
    pub fn parse(value: &str) -> ServiceResult<Self> {
        match value {
            "db_password" => Ok(Self::DbPassword),
            "cosmos_account_key" => Ok(Self::CosmosAccountKey),
            "entra_client_secret" => Ok(Self::EntraClientSecret),
            "entra_refresh_token" => Ok(Self::EntraRefreshToken),
            _ => Err(failure(ErrorCode::InvalidArgument)),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::DbPassword => "db_password",
            Self::CosmosAccountKey => "cosmos_account_key",
            Self::EntraClientSecret => "entra_client_secret",
            Self::EntraRefreshToken => "entra_refresh_token",
        }
    }
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn get(&self, connection_id: &str, slot: SecretSlot) -> ServiceResult<Option<Secret>>;
    async fn set(&self, connection_id: &str, slot: SecretSlot, secret: Secret)
        -> ServiceResult<()>;
    async fn delete(&self, connection_id: &str, slot: SecretSlot) -> ServiceResult<()>;
}

pub struct CallbackSecretStore {
    pub read: Arc<dyn Fn(&str, SecretSlot) -> ServiceResult<Option<Secret>> + Send + Sync>,
    pub write: Arc<dyn Fn(&str, SecretSlot, Secret) -> ServiceResult<()> + Send + Sync>,
    pub remove: Arc<dyn Fn(&str, SecretSlot) -> ServiceResult<()> + Send + Sync>,
}
#[async_trait]
impl SecretStore for CallbackSecretStore {
    async fn get(&self, connection_id: &str, slot: SecretSlot) -> ServiceResult<Option<Secret>> {
        (self.read)(connection_id, slot)
    }
    async fn set(
        &self,
        connection_id: &str,
        slot: SecretSlot,
        secret: Secret,
    ) -> ServiceResult<()> {
        (self.write)(connection_id, slot, secret)
    }
    async fn delete(&self, connection_id: &str, slot: SecretSlot) -> ServiceResult<()> {
        (self.remove)(connection_id, slot)
    }
}

#[derive(Default)]
pub struct SessionSecretStore {
    secrets: Mutex<HashMap<(String, SecretSlot), Secret>>,
}
#[async_trait]
impl SecretStore for SessionSecretStore {
    async fn get(&self, connection_id: &str, slot: SecretSlot) -> ServiceResult<Option<Secret>> {
        Ok(self
            .secrets
            .lock()
            .map_err(|_| failure(ErrorCode::AuthRequired))?
            .get(&(connection_id.into(), slot))
            .cloned())
    }
    async fn set(
        &self,
        connection_id: &str,
        slot: SecretSlot,
        secret: Secret,
    ) -> ServiceResult<()> {
        let mut secrets = self
            .secrets
            .lock()
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        let key = (connection_id.into(), slot);
        if secrets.len() >= 1024 && !secrets.contains_key(&key) {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        secrets.insert(key, secret);
        Ok(())
    }
    async fn delete(&self, connection_id: &str, slot: SecretSlot) -> ServiceResult<()> {
        self.secrets
            .lock()
            .map_err(|_| failure(ErrorCode::AuthRequired))?
            .remove(&(connection_id.into(), slot));
        Ok(())
    }
}

#[derive(Default)]
pub struct SecretInputs {
    channels: Mutex<HashMap<(String, String, String, String), SecretInputEntry>>,
}
struct SecretInputEntry {
    secret: Secret,
    expires_at_ms: u64,
    connection_id: Option<String>,
}
impl SecretInputs {
    pub fn register(
        &self,
        caller: &CallerContext,
        request_id: &str,
        channel: &Value,
        bytes: Vec<u8>,
    ) -> ServiceResult<()> {
        let key = channel_key(channel)?;
        let mut channels = self
            .channels
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        channels.retain(|_, entry| entry.expires_at_ms > super::storage::now_ms());
        if channels.len() >= 64 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let entry = (
            caller.owner_instance_id.clone(),
            caller.caller_id.clone(),
            request_id.into(),
            key,
        );
        if channels.contains_key(&entry) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        channels.insert(
            entry,
            SecretInputEntry {
                secret: Secret::new(bytes)?,
                expires_at_ms: super::storage::now_ms().saturating_add(120_000),
                connection_id: None,
            },
        );
        Ok(())
    }
    pub fn take(
        &self,
        caller: &CallerContext,
        request_id: &str,
        channel: &Value,
    ) -> ServiceResult<Secret> {
        let entry = self
            .channels
            .lock()
            .map_err(|_| failure(ErrorCode::AuthRequired))?
            .remove(&(
                caller.owner_instance_id.clone(),
                caller.caller_id.clone(),
                request_id.into(),
                channel_key(channel)?,
            ))
            .ok_or_else(|| failure(ErrorCode::PermissionDenied))?;
        if entry.expires_at_ms <= super::storage::now_ms() {
            return Err(failure(ErrorCode::AuthExpired));
        }
        Ok(entry.secret)
    }
    pub fn register_gui(
        &self,
        caller: &CallerContext,
        connection_id: &str,
        request_id: &str,
        bytes: Vec<u8>,
    ) -> ServiceResult<Value> {
        if caller.transport != CallerTransport::Gui {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        for id in [
            connection_id,
            request_id,
            &caller.owner_instance_id,
            &caller.caller_id,
        ] {
            validate_input_id(id)?;
        }
        let secret = Secret::new(bytes)?;
        let mut channels = self
            .channels
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        channels.retain(|_, entry| entry.expires_at_ms > super::storage::now_ms());
        if channels.len() >= 64 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        if channels
            .iter()
            .any(|((owner, identity, request, _), entry)| {
                owner == &caller.owner_instance_id
                    && identity == &caller.caller_id
                    && request == request_id
                    && entry.connection_id.is_some()
            })
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let channel = serde_json::json!({"kind":"gui_buffer","channel_id":id});
        channels.insert(
            (
                caller.owner_instance_id.clone(),
                caller.caller_id.clone(),
                request_id.into(),
                format!("gui:{id}"),
            ),
            SecretInputEntry {
                secret,
                expires_at_ms: super::storage::now_ms().saturating_add(120_000),
                connection_id: Some(connection_id.into()),
            },
        );
        Ok(channel)
    }
    pub fn take_for_connection(
        &self,
        caller: &CallerContext,
        request_id: &str,
        channel: &Value,
        connection_id: &str,
    ) -> ServiceResult<Secret> {
        if channel["kind"] != "gui_buffer" {
            return self.take(caller, request_id, channel);
        }
        if caller.transport != CallerTransport::Gui {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        validate_input_id(connection_id)?;
        validate_input_id(request_id)?;
        let id = gui_channel_id(channel)?;
        let key = (
            caller.owner_instance_id.clone(),
            caller.caller_id.clone(),
            request_id.into(),
            format!("gui:{id}"),
        );
        let mut channels = self
            .channels
            .lock()
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        let entry = channels
            .get(&key)
            .ok_or_else(|| failure(ErrorCode::PermissionDenied))?;
        if entry.connection_id.as_deref() != Some(connection_id) {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let entry = channels
            .remove(&key)
            .ok_or_else(|| failure(ErrorCode::PermissionDenied))?;
        if entry.expires_at_ms <= super::storage::now_ms() {
            return Err(failure(ErrorCode::AuthExpired));
        }
        Ok(entry.secret)
    }
    pub fn discard(&self, caller: &CallerContext, request_id: &str) {
        if let Ok(mut channels) = self.channels.lock() {
            channels.retain(|(owner, identity, request, _), _| {
                owner != &caller.owner_instance_id
                    || identity != &caller.caller_id
                    || request != request_id
            });
        }
    }
}

fn validate_input_id(id: &str) -> ServiceResult<()> {
    if id.is_empty() || id.chars().count() > 128 {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(())
}

fn gui_channel_id(channel: &Value) -> ServiceResult<&str> {
    let id = channel["channel_id"]
        .as_str()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    if channel.as_object().is_none_or(|object| object.len() != 2)
        || id.len() != 36
        || parsed.get_version_num() != 4
        || parsed.get_variant() != uuid::Variant::RFC4122
        || parsed.to_string() != id
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(id)
}

fn channel_key(channel: &Value) -> ServiceResult<String> {
    match channel["kind"].as_str() {
        Some("protected_stdin") if channel.as_object().is_some_and(|object| object.len() == 1) => {
            Ok("stdin".into())
        }
        Some("file_descriptor") if channel.as_object().is_some_and(|object| object.len() == 2) => {
            match channel["descriptor"].as_u64() {
                Some(descriptor) if (3..=255).contains(&descriptor) => {
                    Ok(format!("fd:{descriptor}"))
                }
                _ => Err(failure(ErrorCode::InvalidArgument)),
            }
        }
        _ => Err(failure(ErrorCode::InvalidArgument)),
    }
}

#[cfg(test)]
#[path = "tests/credentials.rs"]
mod tests;
