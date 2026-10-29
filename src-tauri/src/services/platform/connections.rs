use crate::services::{
    backend::{matching_ids, ConnectionBackend, SavedConnection},
    contracts::ErrorCode,
    credentials::SecretSlot,
    error::{failure, ServiceResult},
    runtime::validate_connection_configuration,
    security::{sanitized_settings, value_hash},
};
use crate::{
    drivers::registry,
    models::{ConnectionParams, SavedConnection as LegacyConnection},
    persistence,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[async_trait]
pub trait ConnectionObservation: Send + Sync {
    async fn observe(
        &self,
        connection: &SavedConnection,
        externally_changed: bool,
    ) -> ServiceResult<bool>;
}

pub struct ProfileConnections {
    path: PathBuf,
    observation: Mutex<Option<Arc<dyn ConnectionObservation>>>,
    fences: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    #[cfg(test)]
    preparation_hook: Mutex<Option<Arc<dyn PreparationHook>>>,
}
#[cfg(test)]
#[async_trait]
pub(super) trait PreparationHook: Send + Sync {
    async fn before_validation(&self);
}

#[derive(Clone, Serialize, Deserialize)]
struct ServiceMetadata {
    #[serde(default)]
    allow_writes: bool,
    auth_revision: String,
    settings_fingerprint: String,
    #[serde(default)]
    credential_ref: Option<String>,
}

impl ProfileConnections {
    pub fn new(profile: &std::path::Path) -> Self {
        Self {
            path: crate::paths::resolve_connections_path(profile),
            observation: Mutex::new(None),
            fences: Mutex::new(HashMap::new()),
            #[cfg(test)]
            preparation_hook: Mutex::new(None),
        }
    }
    #[cfg(test)]
    pub(super) fn set_preparation_hook(&self, hook: Arc<dyn PreparationHook>) {
        *self.preparation_hook.lock().unwrap() = Some(hook);
    }
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub fn set_observation(
        &self,
        observation: Arc<dyn ConnectionObservation>,
    ) -> ServiceResult<()> {
        *self
            .observation
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))? = Some(observation);
        Ok(())
    }
    pub fn private_params(&self, id: &str) -> ServiceResult<ConnectionParams> {
        persistence::transact_connections(&self.path, |file, _| {
            file.connections
                .iter()
                .find(|connection| connection.id == id)
                .map(|connection| connection.params.clone())
                .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))
        })
    }
    /// The stored params of a connection read at `revision`; a connection
    /// edited since then is a conflict, whatever the edit was.
    pub fn private_params_at(&self, id: &str, revision: u64) -> ServiceResult<ConnectionParams> {
        persistence::transact_connections(&self.path, |file, _| {
            let connection = file
                .connections
                .iter()
                .find(|connection| connection.id == id)
                .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))?;
            require_version(connection.revision, revision)?;
            Ok(connection.params.clone())
        })
    }
    fn fresh(&self, id: &str, detect_changes: bool) -> ServiceResult<(SavedConnection, bool)> {
        persistence::transact_connections(&self.path, |file, raw| {
            let connection = file
                .connections
                .iter()
                .find(|connection| connection.id == id)
                .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))?;
            let record = raw_connection(raw, id)?;
            let fingerprint = settings_fingerprint(&merged_params(
                &record["params"],
                &connection.params,
                &connection.params,
            )?);
            let mut metadata = metadata(record, &fingerprint)?;
            let changed = detect_changes && metadata.settings_fingerprint != fingerprint;
            if changed {
                metadata.auth_revision = uuid::Uuid::new_v4().to_string();
                metadata.settings_fingerprint = fingerprint;
            }
            write_metadata(record, &metadata)?;
            Ok((public(connection, &metadata)?, changed))
        })
    }
    async fn create_prepared(
        &self,
        name: &str,
        driver: &str,
        settings: Value,
        credential_ref: Option<String>,
        lease: Option<&crate::services::jobs::JobLease>,
    ) -> ServiceResult<SavedConnection> {
        if credential_ref.is_some() {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        valid_name(name)?;
        self.validate_settings(driver, &settings).await?;
        let registered = registry::get_driver(driver)
            .await
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
        let params = checked_params(driver, &settings, &registered.manifest().settings)?;
        persistence::transact_connections(&self.path, |file, raw| {
            let id = uuid::Uuid::new_v4().to_string();
            let connection = LegacyConnection {
                id,
                name: name.into(),
                params,
                plugin_secret_keys: Vec::new(),
                group_id: None,
                sort_order: None,
                detect_json_in_text_columns: None,
                appearance: None,
                tag_ids: None,
                environment: None,
                revision: crate::models::INITIAL_REVISION,
            };
            let mut record = serde_json::to_value(&connection)
                .map_err(|_| failure(ErrorCode::InvalidArgument))?;
            let metadata = metadata(&record, &settings_fingerprint(&record["params"]))?;
            if let Some(lease) = lease {
                lease.mark_write_dispatched()?;
            }
            write_metadata(&mut record, &metadata)?;
            let result = public(&connection, &metadata)?;
            raw_connections(raw)?.push(record);
            file.connections.push(connection);
            Ok(result)
        })
    }
    async fn update_prepared(
        &self,
        id: &str,
        expected_version: u64,
        patch: Value,
        lease: Option<&crate::services::jobs::JobLease>,
    ) -> ServiceResult<SavedConnection> {
        let object = patch
            .as_object()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        if object.is_empty()
            || object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "name" | "settings" | "credential_ref" | "allow_writes"
                )
            })
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let name = patch
            .get("name")
            .map(|value| {
                let name = value
                    .as_str()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
                valid_name(name)?;
                Ok::<_, crate::services::contracts::ServiceError>(name.to_owned())
            })
            .transpose()?;
        let allow_writes = patch
            .get("allow_writes")
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))
            })
            .transpose()?;
        let credential_ref = patch
            .get("credential_ref")
            .map(|value| checked_ref(id, value))
            .transpose()?;
        let driver = self.private_params(id)?.driver;
        let new_params = if let Some(settings) = patch.get("settings") {
            self.validate_settings(&driver, settings).await?;
            let registered = registry::get_driver(&driver)
                .await
                .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
            Some(checked_params(
                &driver,
                settings,
                &registered.manifest().settings,
            )?)
        } else {
            None
        };
        persistence::transact_connections(&self.path, |file, raw| {
            let connection = file
                .connections
                .iter_mut()
                .find(|connection| connection.id == id)
                .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))?;
            if connection.params.driver != driver {
                return Err(connection_conflict(connection.revision));
            }
            let record = raw_connection(raw, id)?;
            let fingerprint = settings_fingerprint(&merged_params(
                &record["params"],
                &connection.params,
                &connection.params,
            )?);
            let mut metadata = metadata(record, &fingerprint)?;
            require_observed_settings(connection.revision, &metadata, &fingerprint)?;
            require_version(connection.revision, expected_version)?;
            let version = next_version(connection.revision)?;
            if let Some(lease) = lease {
                lease.mark_write_dispatched()?;
            }
            if let Some(name) = name {
                connection.name = name;
            }
            if let Some(mut params) = new_params {
                carry_protected_params(&connection.params, &mut params);
                record["params"] = merged_params(&record["params"], &connection.params, &params)?;
                connection.params = params;
                metadata.settings_fingerprint = settings_fingerprint(&record["params"]);
            }
            if let Some(value) = allow_writes {
                metadata.allow_writes = value;
            }
            if let Some(value) = credential_ref {
                metadata.credential_ref = value;
            }
            connection.revision = version;
            if patch.get("settings").is_some()
                || patch.get("allow_writes").is_some()
                || patch.get("credential_ref").is_some()
            {
                metadata.auth_revision = uuid::Uuid::new_v4().to_string();
            }
            write_metadata(record, &metadata)?;
            public(connection, &metadata)
        })
    }
    fn fence(&self, id: &str) -> ServiceResult<Arc<tokio::sync::Mutex<()>>> {
        let mut fences = self
            .fences
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        if !fences.contains_key(id) && fences.len() >= 16_384 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        Ok(fences
            .entry(id.into())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }
}

#[async_trait]
impl ConnectionBackend for ProfileConnections {
    async fn validate_settings(&self, driver: &str, settings: &Value) -> ServiceResult<()> {
        #[cfg(test)]
        {
            let hook = self
                .preparation_hook
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                .clone();
            if let Some(hook) = hook {
                hook.before_validation().await;
            }
        }
        crate::services::security::validate_settings(settings)?;
        let registered = registry::get_driver(driver)
            .await
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
        checked_params(driver, settings, &registered.manifest().settings)?;
        validate_connection_configuration(driver, settings)?;
        Ok(())
    }
    async fn rotate_auth_revision(&self, id: &str) -> ServiceResult<String> {
        persistence::transact_connections(&self.path, |file, raw| {
            let connection = file
                .connections
                .iter()
                .find(|connection| connection.id == id)
                .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))?;
            let record = raw_connection(raw, id)?;
            let mut metadata = metadata(
                record,
                &settings_fingerprint(&merged_params(
                    &record["params"],
                    &connection.params,
                    &connection.params,
                )?),
            )?;
            metadata.auth_revision = uuid::Uuid::new_v4().to_string();
            write_metadata(record, &metadata)?;
            Ok(metadata.auth_revision)
        })
    }
    async fn list(&self) -> ServiceResult<Vec<SavedConnection>> {
        let ids = persistence::transact_connections(&self.path, |file, _| {
            Ok(file
                .connections
                .iter()
                .map(|connection| connection.id.clone())
                .collect::<Vec<_>>())
        })?;
        let mut connections = Vec::with_capacity(ids.len());
        for id in ids {
            connections.push(self.get(&id).await?);
        }
        Ok(connections)
    }
    async fn find_ids(&self, given: &str) -> ServiceResult<Vec<String>> {
        persistence::transact_connections(&self.path, |file, _| {
            Ok(matching_ids(
                file.connections
                    .iter()
                    .map(|connection| (connection.id.as_str(), connection.name.as_str())),
                given,
            ))
        })
    }
    async fn get(&self, id: &str) -> ServiceResult<SavedConnection> {
        let fence = self.fence(id)?;
        let _fence = fence.lock().await;
        let (connection, changed) = self.fresh(id, true)?;
        let observation = self
            .observation
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .clone();
        if let Some(observation) = observation {
            if observation.observe(&connection, changed).await? {
                return self.fresh(id, false).map(|(connection, _)| connection);
            }
        }
        Ok(connection)
    }
    async fn has_stored_secrets(&self, id: &str) -> ServiceResult<bool> {
        let params = self.private_params(id)?;
        let filled = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.is_empty());
        Ok(self.fresh(id, false)?.0.credential_ref.is_some()
            || params.save_in_keychain == Some(true)
            || params.connection_uri_in_keychain == Some(true)
            || filled(&params.password)
            || filled(&params.connection_uri)
            || filled(&params.ssh_password)
            || filled(&params.ssh_key_passphrase))
    }
    async fn create(
        &self,
        name: &str,
        driver: &str,
        settings: Value,
        credential_ref: Option<String>,
    ) -> ServiceResult<SavedConnection> {
        self.create_prepared(name, driver, settings, credential_ref, None)
            .await
    }
    async fn create_guarded(
        &self,
        name: &str,
        driver: &str,
        settings: Value,
        credential_ref: Option<String>,
        lease: &crate::services::jobs::JobLease,
    ) -> ServiceResult<SavedConnection> {
        self.create_prepared(name, driver, settings, credential_ref, Some(lease))
            .await
    }
    async fn update(
        &self,
        id: &str,
        expected_version: u64,
        patch: Value,
    ) -> ServiceResult<SavedConnection> {
        self.update_prepared(id, expected_version, patch, None)
            .await
    }
    async fn update_guarded(
        &self,
        id: &str,
        expected_version: u64,
        patch: Value,
        lease: &crate::services::jobs::JobLease,
    ) -> ServiceResult<SavedConnection> {
        self.update_prepared(id, expected_version, patch, Some(lease))
            .await
    }
    async fn delete(&self, id: &str, expected_version: u64) -> ServiceResult<()> {
        persistence::transact_connections(&self.path, |file, raw| {
            let connection = file
                .connections
                .iter()
                .find(|connection| connection.id == id)
                .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))?;
            let record = raw_connection(raw, id)?;
            let fingerprint = settings_fingerprint(&merged_params(
                &record["params"],
                &connection.params,
                &connection.params,
            )?);
            let metadata = metadata(record, &fingerprint)?;
            require_observed_settings(connection.revision, &metadata, &fingerprint)?;
            require_version(connection.revision, expected_version)?;
            file.connections.retain(|connection| connection.id != id);
            raw_connections(raw)?.retain(|connection| connection["id"] != id);
            Ok(())
        })
    }
}

fn valid_name(name: &str) -> ServiceResult<()> {
    if name.trim().is_empty() || name.chars().count() > 256 || name.chars().any(char::is_control) {
        Err(failure(ErrorCode::InvalidArgument))
    } else {
        Ok(())
    }
}
fn settings_fingerprint(value: &Value) -> String {
    let mut canonical = value.clone();
    canonical.sort_all_objects();
    value_hash(&canonical)
}
fn next_version(version: u64) -> ServiceResult<u64> {
    version
        .checked_add(1)
        .filter(|version| *version <= 9_007_199_254_740_991)
        .ok_or_else(|| failure(ErrorCode::ResourceLimit))
}
fn connection_conflict(current: u64) -> crate::services::contracts::ServiceError {
    let mut error = failure(ErrorCode::ConnectionConflict);
    error.details = json!({"current_version":current});
    error
}
fn require_version(current: u64, expected: u64) -> ServiceResult<()> {
    if current == expected {
        Ok(())
    } else {
        Err(connection_conflict(current))
    }
}
fn require_observed_settings(
    revision: u64,
    metadata: &ServiceMetadata,
    fingerprint: &str,
) -> ServiceResult<()> {
    if metadata.settings_fingerprint == fingerprint {
        Ok(())
    } else {
        Err(connection_conflict(revision))
    }
}
fn checked_ref(id: &str, value: &Value) -> ServiceResult<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    let reference = value
        .as_str()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    let (owner, slot) = reference
        .split_once(':')
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    if owner != id {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    SecretSlot::parse(slot)?;
    Ok(Some(reference.into()))
}
fn raw_connections(raw: &mut Value) -> ServiceResult<&mut Vec<Value>> {
    if raw.is_array() {
        *raw = json!({"groups":[],"connections":raw.take()});
    }
    raw.get_mut("connections")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))
}
fn raw_connection<'a>(raw: &'a mut Value, id: &str) -> ServiceResult<&'a mut Value> {
    raw_connections(raw)?
        .iter_mut()
        .find(|record| record["id"] == id)
        .ok_or_else(|| failure(ErrorCode::ConnectionNotFound))
}
fn metadata(record: &Value, fingerprint: &str) -> ServiceResult<ServiceMetadata> {
    let metadata: ServiceMetadata = match record.get("service_v1") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| failure(ErrorCode::InvalidArgument))?,
        None => ServiceMetadata {
            allow_writes: false,
            auth_revision: uuid::Uuid::new_v4().to_string(),
            settings_fingerprint: fingerprint.into(),
            credential_ref: None,
        },
    };
    if uuid::Uuid::parse_str(&metadata.auth_revision).is_err() {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    if let Some(reference) = &metadata.credential_ref {
        checked_ref(
            record["id"]
                .as_str()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
            &json!(reference),
        )?;
    }
    Ok(metadata)
}
fn write_metadata(record: &mut Value, metadata: &ServiceMetadata) -> ServiceResult<()> {
    if record.get("service_v1").is_none() {
        record["service_v1"] = json!({});
    }
    let object = record["service_v1"]
        .as_object_mut()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    for (key, value) in serde_json::to_value(metadata)
        .map_err(|_| failure(ErrorCode::InvalidArgument))?
        .as_object()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
    {
        object.insert(key.clone(), value.clone());
    }
    Ok(())
}
fn public(
    connection: &LegacyConnection,
    metadata: &ServiceMetadata,
) -> ServiceResult<SavedConnection> {
    let mut settings = sanitized_settings(
        &serde_json::to_value(&connection.params)
            .map_err(|_| failure(ErrorCode::InvalidArgument))?,
    );
    if let Some(object) = settings.as_object_mut() {
        object.remove("connection_id");
    }
    Ok(SavedConnection {
        id: connection.id.clone(),
        name: connection.name.clone(),
        driver: connection.params.driver.clone(),
        settings,
        credential_ref: metadata.credential_ref.clone(),
        version: connection.revision,
        allow_writes: metadata.allow_writes,
        auth_revision: metadata.auth_revision.clone(),
    })
}
/// Settings updates are rebuilt from non-secret input, so every credential and
/// credential-storage marker of the stored record is carried over unchanged.
fn carry_protected_params(previous: &ConnectionParams, next: &mut ConnectionParams) {
    next.password = previous.password.clone();
    next.save_in_keychain = previous.save_in_keychain;
    next.connection_uri = previous.connection_uri.clone();
    next.connection_uri_in_keychain = previous.connection_uri_in_keychain;
    next.ssh_password = previous.ssh_password.clone();
    next.ssh_key_passphrase = previous.ssh_key_passphrase.clone();
    for (key, value) in &previous.extra {
        if crate::services::security::is_secret_extra_key(key) {
            next.extra.insert(key.clone(), value.clone());
        }
    }
}

fn merged_params(
    raw: &Value,
    previous: &ConnectionParams,
    params: &ConnectionParams,
) -> ServiceResult<Value> {
    let known = serde_json::to_value(previous).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    let mut next = serde_json::to_value(params).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    if let (Some(raw), Some(known), Some(next)) =
        (raw.as_object(), known.as_object(), next.as_object_mut())
    {
        for (key, value) in raw {
            if !known.contains_key(key)
                && !matches!(key.as_str(), "connectionUri" | "connectionUriInKeychain")
            {
                next.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }
    Ok(next)
}

fn checked_params(
    driver: &str,
    settings: &Value,
    definitions: &[crate::drivers::driver_trait::PluginSettingDefinition],
) -> ServiceResult<ConnectionParams> {
    let settings = settings
        .as_object()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    if settings
        .get("driver")
        .is_some_and(|value| value.as_str() != Some(driver))
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let mut normalized = serde_json::to_value(ConnectionParams::default())
        .map_err(|_| failure(ErrorCode::InvalidArgument))?;
    normalized["driver"] = driver.into();
    let driver_owns_entra = driver == "sqlserver-plus";
    let mut extra = serde_json::Map::new();
    for (key, value) in settings {
        if key == "extra" {
            let fields = value
                .as_object()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
            for (key, value) in fields {
                extra.insert(key.clone(), value.clone());
            }
        } else if matches!(
            key.as_str(),
            "endpoint" | "auth_mode" | "auth_source" | "tenant_id" | "client_id"
        ) || (driver_owns_entra && matches!(key.as_str(), "entra_object_id" | "azure_cli_path"))
        {
            extra.insert(key.clone(), value.clone());
        } else {
            if !NONSECRET_PARAMS.contains(&key.as_str()) {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            normalized[key] = value.clone();
        }
    }
    for (key, value) in &extra {
        let fixed = matches!(
            key.as_str(),
            "endpoint"
                | "auth_mode"
                | "auth_source"
                | "tenant_id"
                | "client_id"
                | "trust_server_certificate"
                | "integrated"
        ) || (driver_owns_entra && matches!(key.as_str(), "entra_object_id" | "azure_cli_path"));
        let definition = definitions.iter().find(|definition| definition.key == *key);
        if !fixed && definition.is_none() {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if fixed
            && (if matches!(key.as_str(), "trust_server_certificate" | "integrated") {
                !value.is_boolean() && !matches!(value.as_str(), Some("true" | "false"))
            } else {
                !value.is_string()
            })
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if !value.is_string() && !value.is_boolean() && !value.is_number() {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if let Some(definition) = definition {
            let valid = match definition.setting_type.as_str() {
                "boolean" => value.is_boolean() || matches!(value.as_str(), Some("true" | "false")),
                "number" => {
                    value.as_f64().is_some_and(f64::is_finite)
                        || value
                            .as_str()
                            .filter(|value| value.trim() == *value)
                            .and_then(|value| serde_json::from_str::<Value>(value).ok())
                            .and_then(|value| value.as_f64())
                            .is_some_and(f64::is_finite)
                }
                "string" | "select" => value.is_string(),
                _ => false,
            };
            if !valid
                || (!definition.options.is_empty()
                    && value.as_str().map_or(true, |value| {
                        !definition.options.iter().any(|option| option == value)
                    }))
            {
                return Err(failure(ErrorCode::InvalidArgument));
            }
        }
    }
    let auth_mode = extra.get("auth_mode").and_then(Value::as_str);
    if !driver_owns_entra
        && matches!(auth_mode, Some("entra_user" | "entra_service_principal"))
        && (normalized
            .get("username")
            .is_some_and(|value| !value.is_null())
            || extra
                .get("integrated")
                .is_some_and(|value| value == true || value == "true"))
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    if normalized["host"]
        .as_str()
        .is_some_and(|host| host.ends_with(".database.windows.net"))
        && extra
            .get("trust_server_certificate")
            .is_some_and(|value| value == true || value == "true")
    {
        return Err(failure(ErrorCode::TlsValidationFailed));
    }
    normalized["extra"] = Value::Object(
        extra
            .into_iter()
            .map(|(key, value)| {
                (
                    key,
                    Value::String(
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string()),
                    ),
                )
            })
            .collect(),
    );
    let mut params: ConnectionParams =
        serde_json::from_value(normalized).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    params.connection_id = None;
    Ok(params)
}

#[cfg(test)]
pub fn checked_params_for_tests(driver: &str, settings: &Value) -> ServiceResult<ConnectionParams> {
    checked_params(driver, settings, &[])
}

const NONSECRET_PARAMS: &[&str] = &[
    "driver",
    "host",
    "port",
    "username",
    "database",
    "ssl_mode",
    "ssl_ca",
    "ssl_cert",
    "ssl_key",
    "enable_cleartext_plugin",
    "pipes_as_concat",
    "use_iam_auth",
    "ssh_enabled",
    "ssh_connection_id",
    "ssh_host",
    "ssh_port",
    "ssh_user",
    "ssh_key_file",
    "ssh_allow_passphrase_prompt",
    "save_in_keychain",
    "k8s_enabled",
    "k8s_connection_id",
    "k8s_context",
    "k8s_namespace",
    "k8s_resource_type",
    "k8s_resource_name",
    "k8s_port",
    "k8s_kubectl_path",
    "k8s_kubeconfig_path",
    "ssm_enabled",
    "ssm_target",
    "ssm_profile",
    "ssm_region",
    "startup_script",
    "proxy",
];
