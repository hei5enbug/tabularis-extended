use super::{
    backend::{CancellationSignal, SavedConnection, TransientAuth},
    contracts::ErrorCode,
    credentials::{Secret, SecretSlot, SecretStore},
    dispatch::{CallerContext, CallerTransport},
    error::{failure, ServiceResult},
    oauth::OAuthProvider,
    storage::now_ms,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;

/// Driver that signs in with Entra itself and advertises `entra_token: false`;
/// the host never acquires a token for it.
pub const DRIVER_OWNED_ENTRA_DRIVER: &str = "sqlserver-plus";
pub const AUTH_TTL_MS: u64 = 15 * 60 * 1000;
pub const REFRESH_MARGIN_MS: u64 = 5 * 60 * 1000;
const MAX_AUTH_IDENTITIES: usize = 256;
const MAX_AUTH_JOBS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AuthConfig {
    pub connection_id: String,
    pub mode: String,
    pub source: String,
    pub tenant_id: String,
    pub client_id: String,
    pub scope: String,
    pub credential_identity: String,
}

impl AuthConfig {
    pub fn from_connection(connection: &SavedConnection) -> ServiceResult<Option<Self>> {
        if connection.driver == DRIVER_OWNED_ENTRA_DRIVER {
            return Ok(None);
        }
        let settings = connection
            .settings
            .get("extra")
            .unwrap_or(&connection.settings);
        let mode = settings["auth_mode"].as_str().unwrap_or("");
        if !matches!(mode, "entra_user" | "entra_service_principal") {
            return Ok(None);
        }
        let config = Self {
            connection_id: connection.id.clone(),
            mode: mode.into(),
            source: settings["auth_source"].as_str().unwrap_or("oauth").into(),
            tenant_id: uuid::Uuid::parse_str(settings["tenant_id"].as_str().unwrap_or(""))
                .map_err(|_| failure(ErrorCode::InvalidArgument))?
                .to_string(),
            client_id: uuid::Uuid::parse_str(settings["client_id"].as_str().unwrap_or(""))
                .map_err(|_| failure(ErrorCode::InvalidArgument))?
                .to_string(),
            scope: if connection.driver == "cosmos-nosql" {
                "https://cosmos.azure.com/.default"
            } else {
                "https://database.windows.net/.default"
            }
            .into(),
            credential_identity: format!(
                "{}:{}",
                connection
                    .credential_ref
                    .as_deref()
                    .unwrap_or(&connection.id),
                connection.auth_revision
            ),
        };
        config.validate()?;
        Ok(Some(config))
    }
    pub fn validate(&self) -> ServiceResult<()> {
        if !matches!(self.mode.as_str(), "entra_user" | "entra_service_principal")
            || uuid::Uuid::parse_str(&self.tenant_id).is_err()
            || uuid::Uuid::parse_str(&self.client_id).is_err()
            || !matches!(
                self.scope.as_str(),
                "https://database.windows.net/.default" | "https://cosmos.azure.com/.default"
            )
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if !matches!(self.source.as_str(), "oauth" | "azure_cli")
            || (self.source == "azure_cli"
                && (self.mode != "entra_user" || self.client_id != super::azure_cli::CLIENT_ID))
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct OAuthGrant {
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    pub expires_at_ms: u64,
    pub principal_id: String,
}

#[async_trait]
pub trait AuthLifecycle: Send + Sync {
    async fn principal_changed(
        &self,
        connection_id: &str,
        previous: Option<&str>,
        current: &str,
    ) -> ServiceResult<()>;
    async fn logout(&self, connection_id: &str, principal: Option<&str>) -> ServiceResult<()>;
}

struct AuthJob {
    connection_id: String,
    caller_id: String,
    owner: String,
    signal: CancellationSignal,
    state: String,
    expires_at_ms: u64,
    challenge: Option<(String, String)>,
    error: Option<ErrorCode>,
}
struct AuthState {
    grants: HashMap<AuthConfig, OAuthGrant>,
    locks: HashMap<AuthConfig, Arc<AsyncMutex<()>>>,
    connection_locks: HashMap<String, Arc<AsyncMutex<()>>>,
    jobs: HashMap<String, AuthJob>,
    generations: HashMap<String, u64>,
}

pub struct AuthManager {
    provider: Arc<dyn OAuthProvider>,
    persistent: Arc<dyn SecretStore>,
    session: Arc<dyn SecretStore>,
    lifecycle: Arc<dyn AuthLifecycle>,
    state: Mutex<AuthState>,
}

impl AuthManager {
    pub fn new(
        provider: Arc<dyn OAuthProvider>,
        persistent: Arc<dyn SecretStore>,
        session: Arc<dyn SecretStore>,
        lifecycle: Arc<dyn AuthLifecycle>,
    ) -> Arc<Self> {
        Arc::new(Self {
            provider,
            persistent,
            session,
            lifecycle,
            state: Mutex::new(AuthState {
                grants: HashMap::new(),
                locks: HashMap::new(),
                connection_locks: HashMap::new(),
                jobs: HashMap::new(),
                generations: HashMap::new(),
            }),
        })
    }
    async fn secret(&self, connection_id: &str, slot: SecretSlot) -> ServiceResult<(Secret, bool)> {
        if let Some(secret) = self.session.get(connection_id, slot).await? {
            return Ok((secret, false));
        }
        self.persistent
            .get(connection_id, slot)
            .await?
            .map(|secret| (secret, true))
            .ok_or_else(|| failure(ErrorCode::AuthRequired))
    }
    fn generation(&self, id: &str) -> ServiceResult<u64> {
        Ok(*self
            .state
            .lock()
            .map_err(|_| failure(ErrorCode::AuthRequired))?
            .generations
            .get(id)
            .unwrap_or(&0))
    }
    fn connection_lock(&self, id: &str) -> ServiceResult<Arc<AsyncMutex<()>>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        if !state.connection_locks.contains_key(id)
            && state.connection_locks.len() >= MAX_AUTH_IDENTITIES
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        Ok(state
            .connection_locks
            .entry(id.into())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone())
    }
    async fn install(
        &self,
        config: &AuthConfig,
        grant: OAuthGrant,
        generation: u64,
        persistent: bool,
    ) -> ServiceResult<()> {
        let lock = self.connection_lock(&config.connection_id)?;
        let _guard = lock.lock().await;
        if grant.expires_at_ms <= now_ms() || grant.principal_id.is_empty() {
            return Err(failure(ErrorCode::AuthExpired));
        }
        let previous = {
            let state = self
                .state
                .lock()
                .map_err(|_| failure(ErrorCode::AuthRequired))?;
            if *state.generations.get(&config.connection_id).unwrap_or(&0) != generation {
                return Err(failure(ErrorCode::Cancelled));
            }
            state
                .grants
                .iter()
                .find(|(key, _)| key.connection_id == config.connection_id)
                .map(|(_, grant)| grant.principal_id.clone())
        };
        if previous
            .as_deref()
            .is_some_and(|previous| previous != grant.principal_id)
        {
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| failure(ErrorCode::AuthRequired))?;
                state
                    .grants
                    .retain(|key, _| key.connection_id != config.connection_id);
                *state
                    .generations
                    .entry(config.connection_id.clone())
                    .or_default() += 1;
            }
            self.lifecycle
                .principal_changed(
                    &config.connection_id,
                    previous.as_deref(),
                    &grant.principal_id,
                )
                .await?;
            return Err(failure(ErrorCode::AuthRequired));
        }
        if let Some(refresh) = grant.refresh_token.clone() {
            let bound = Secret::new(serde_json::to_vec(&json!({"refresh_token":refresh.expose(),"tenant_id":config.tenant_id,"client_id":config.client_id,"scope":config.scope,"principal_id":grant.principal_id,"credential_identity":config.credential_identity})).map_err(|_| failure(ErrorCode::AuthRequired))?)?;
            if persistent {
                self.persistent
                    .set(&config.connection_id, SecretSlot::EntraRefreshToken, bound)
                    .await?;
            } else {
                self.session
                    .set(&config.connection_id, SecretSlot::EntraRefreshToken, bound)
                    .await?;
            }
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        if *state.generations.get(&config.connection_id).unwrap_or(&0) != generation {
            return Err(failure(ErrorCode::Cancelled));
        }
        state
            .grants
            .retain(|key, _| key.connection_id != config.connection_id || key == config);
        state.grants.insert(config.clone(), grant);
        Ok(())
    }
    pub async fn context(
        &self,
        connection: &SavedConnection,
        deadline_ms: u64,
    ) -> ServiceResult<Option<TransientAuth>> {
        let config = match AuthConfig::from_connection(connection)? {
            Some(config) => config,
            None => {
                let settings = connection
                    .settings
                    .get("extra")
                    .unwrap_or(&connection.settings);
                return match settings["auth_mode"].as_str() {
                    Some("account_key") => {
                        let (key, _) = self
                            .secret(&connection.id, SecretSlot::CosmosAccountKey)
                            .await?;
                        Ok(Some(TransientAuth::AccountKey {
                            key,
                            identity: connection.auth_revision.clone(),
                        }))
                    }
                    Some("sql_password") => {
                        let (password, _) =
                            self.secret(&connection.id, SecretSlot::DbPassword).await?;
                        Ok(Some(TransientAuth::DbPassword {
                            password,
                            identity: connection.auth_revision.clone(),
                        }))
                    }
                    _ => Ok(None),
                };
            }
        };
        let lock = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failure(ErrorCode::AuthRequired))?;
            if !state.locks.contains_key(&config) && state.locks.len() >= MAX_AUTH_IDENTITIES {
                return Err(failure(ErrorCode::ResourceLimit));
            }
            state
                .locks
                .entry(config.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let _guard = tokio::time::timeout(Duration::from_millis(deadline_ms), lock.lock())
            .await
            .map_err(|_| failure(ErrorCode::DeadlineExceeded))?;
        let minimum_expiry = now_ms()
            .saturating_add(deadline_ms)
            .saturating_add(REFRESH_MARGIN_MS);
        let cached = self
            .state
            .lock()
            .map_err(|_| failure(ErrorCode::AuthRequired))?
            .grants
            .get(&config)
            .cloned();
        let grant = if config.source == "azure_cli" {
            let generation = self.generation(&connection.id)?;
            let grant = tokio::time::timeout(
                Duration::from_millis(deadline_ms),
                self.provider.azure_cli(&config),
            )
            .await
            .map_err(|_| failure(ErrorCode::DeadlineExceeded))??;
            if grant.expires_at_ms < minimum_expiry {
                return Err(failure(ErrorCode::AuthExpired));
            }
            self.install(&config, grant.clone(), generation, false)
                .await?;
            grant
        } else {
            match cached {
                Some(grant) if grant.expires_at_ms >= minimum_expiry => grant,
                _ => {
                    let generation = self.generation(&connection.id)?;
                    let (grant, persistent) = if config.mode == "entra_service_principal" {
                        let (secret, persistent) = self
                            .secret(&connection.id, SecretSlot::EntraClientSecret)
                            .await?;
                        (
                            tokio::time::timeout(
                                Duration::from_millis(deadline_ms),
                                self.provider.client_credentials(&config, &secret),
                            )
                            .await
                            .map_err(|_| failure(ErrorCode::DeadlineExceeded))??,
                            persistent,
                        )
                    } else {
                        let (bound, persistent) = self
                            .secret(&connection.id, SecretSlot::EntraRefreshToken)
                            .await?;
                        let binding: Value = serde_json::from_str(bound.expose())
                            .map_err(|_| failure(ErrorCode::AuthRequired))?;
                        if binding["tenant_id"] != config.tenant_id
                            || binding["client_id"] != config.client_id
                            || binding["scope"] != config.scope
                            || binding["credential_identity"] != config.credential_identity
                        {
                            return Err(failure(ErrorCode::AuthRequired));
                        }
                        let principal = binding["principal_id"]
                            .as_str()
                            .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
                        let refresh = Secret::new(
                            binding["refresh_token"]
                                .as_str()
                                .ok_or_else(|| failure(ErrorCode::AuthRequired))?
                                .as_bytes()
                                .to_vec(),
                        )?;
                        let grant = tokio::time::timeout(
                            Duration::from_millis(deadline_ms),
                            self.provider.refresh(&config, &refresh, Some(principal)),
                        )
                        .await
                        .map_err(|_| failure(ErrorCode::DeadlineExceeded))??;
                        if grant.principal_id != principal {
                            let connection_lock = self.connection_lock(&connection.id)?;
                            let _guard = connection_lock.lock().await;
                            {
                                let mut state = self
                                    .state
                                    .lock()
                                    .map_err(|_| failure(ErrorCode::AuthRequired))?;
                                state
                                    .grants
                                    .retain(|key, _| key.connection_id != connection.id);
                                *state.generations.entry(connection.id.clone()).or_default() += 1;
                            }
                            self.lifecycle
                                .principal_changed(
                                    &connection.id,
                                    Some(principal),
                                    &grant.principal_id,
                                )
                                .await?;
                            return Err(failure(ErrorCode::AuthRequired));
                        }
                        (grant, persistent)
                    };
                    if grant.expires_at_ms < minimum_expiry {
                        return Err(failure(ErrorCode::AuthExpired));
                    }
                    self.install(&config, grant.clone(), generation, persistent)
                        .await?;
                    grant
                }
            }
        };
        Ok(Some(TransientAuth::EntraToken {
            access_token: grant.access_token,
            expires_at_ms: grant.expires_at_ms,
            tenant_id: config.tenant_id,
            client_id: config.client_id,
            principal_id: grant.principal_id,
            scope: config.scope,
        }))
    }
    pub async fn begin(
        self: &Arc<Self>,
        connection: &SavedConnection,
        requested_mode: &str,
        caller: &CallerContext,
        persistence: super::credentials::SecretPersistence,
    ) -> ServiceResult<Value> {
        let settings = connection
            .settings
            .get("extra")
            .unwrap_or(&connection.settings);
        if settings["auth_mode"].as_str() != Some(requested_mode) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if connection.driver == DRIVER_OWNED_ENTRA_DRIVER && requested_mode.starts_with("entra_") {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        let config = AuthConfig::from_connection(connection)?;
        let id = format!("a-{}", uuid::Uuid::new_v4());
        let signal = CancellationSignal::default();
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failure(ErrorCode::AuthRequired))?;
            state.jobs.retain(|_, job| job.expires_at_ms > now_ms());
            if state.jobs.len() >= MAX_AUTH_JOBS {
                return Err(failure(ErrorCode::ResourceLimit));
            }
            if state.jobs.values().any(|job| {
                job.connection_id == connection.id
                    && matches!(job.state.as_str(), "pending" | "interaction_required")
            }) {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            state.jobs.insert(
                id.clone(),
                AuthJob {
                    connection_id: connection.id.clone(),
                    caller_id: caller.caller_id.clone(),
                    owner: caller.owner_instance_id.clone(),
                    signal: signal.clone(),
                    state: "pending".into(),
                    expires_at_ms: now_ms().saturating_add(AUTH_TTL_MS),
                    challenge: None,
                    error: None,
                },
            );
        }
        if let Some(config) = config {
            if config.mode == "entra_user" && config.source == "oauth" {
                let device = match self.provider.device_begin(&config).await {
                    Ok(device) => device,
                    Err(error) => {
                        self.set_job(&id, "failed", Some(error.code));
                        return Err(error);
                    }
                };
                {
                    let mut state = self
                        .state
                        .lock()
                        .map_err(|_| failure(ErrorCode::AuthRequired))?;
                    let job = state
                        .jobs
                        .get_mut(&id)
                        .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
                    job.state = "interaction_required".into();
                    job.expires_at_ms = job
                        .expires_at_ms
                        .min(now_ms().saturating_add(device.expires_in().as_millis() as u64));
                    job.challenge = Some((
                        device.verification_uri().to_string(),
                        device.user_code().secret().clone(),
                    ));
                }
                let manager = self.clone();
                let task_id = id.clone();
                let generation = self.generation(&connection.id)?;
                let timeout = device.expires_in().min(Duration::from_millis(AUTH_TTL_MS));
                tokio::spawn(async move {
                    let result = tokio::select! { _ = signal.cancelled() => Err(failure(ErrorCode::Cancelled)), result = tokio::time::timeout(timeout, manager.provider.device_finish(&config, device, signal.clone())) => result.unwrap_or_else(|_| Err(failure(ErrorCode::AuthExpired))) };
                    let result = match result {
                        Ok(grant) if !signal.is_cancelled() => {
                            manager
                                .install(
                                    &config,
                                    grant,
                                    generation,
                                    persistence == super::credentials::SecretPersistence::Keychain,
                                )
                                .await
                        }
                        Ok(_) => Err(failure(ErrorCode::Cancelled)),
                        Err(error) => Err(error),
                    };
                    manager.set_job(
                        &task_id,
                        if result.is_ok() {
                            "authenticated"
                        } else if signal.is_cancelled() {
                            "cancelled"
                        } else {
                            "failed"
                        },
                        result.err().map(|error| error.code),
                    );
                });
            } else {
                let manager = self.clone();
                let connection = connection.clone();
                let task_id = id.clone();
                tokio::spawn(async move {
                    let result = tokio::select! { _ = signal.cancelled() => Err(failure(ErrorCode::Cancelled)), result = manager.context(&connection, 120_000) => result.map(|_| ()) };
                    manager.set_job(
                        &task_id,
                        if result.is_ok() {
                            "authenticated"
                        } else if signal.is_cancelled() {
                            "cancelled"
                        } else {
                            "failed"
                        },
                        result.err().map(|error| error.code),
                    );
                });
            }
        } else {
            let result = self.context(connection, 30_000).await;
            self.set_job(
                &id,
                if result.is_ok() {
                    "authenticated"
                } else {
                    "failed"
                },
                result.err().map(|error| error.code),
            );
        }
        self.status(&connection.id, &id, caller)
    }
    fn set_job(&self, id: &str, status: &str, error: Option<ErrorCode>) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(job) = state.jobs.get_mut(id) {
                if job.signal.is_cancelled() {
                    job.state = "cancelled".into();
                    job.error = Some(ErrorCode::Cancelled);
                } else {
                    job.state = status.into();
                    job.error = error;
                }
                if job.state != "interaction_required" {
                    job.challenge = None;
                }
            }
        }
    }
    pub fn status(
        &self,
        connection_id: &str,
        id: &str,
        caller: &CallerContext,
    ) -> ServiceResult<Value> {
        let state = self
            .state
            .lock()
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        let job = state
            .jobs
            .get(id)
            .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
        if job.connection_id != connection_id
            || job.caller_id != caller.caller_id
            || job.owner != caller.owner_instance_id
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        if job.expires_at_ms <= now_ms() {
            job.signal.cancel();
            return Err(failure(ErrorCode::AuthExpired));
        }
        let mut value = json!({"auth_job_id":id,"state":job.state,"user_action_required":job.state=="interaction_required","error_code":job.error});
        if matches!(
            caller.transport,
            CallerTransport::Gui | CallerTransport::Cli
        ) {
            if let Some((uri, code)) = &job.challenge {
                value["challenge"] = json!({"verification_uri":uri,"user_code":code});
            }
        }
        Ok(value)
    }
    pub async fn cancel(
        &self,
        connection_id: &str,
        id: &str,
        caller: &CallerContext,
    ) -> ServiceResult<Value> {
        self.status(connection_id, id, caller)?;
        let lock = self.connection_lock(connection_id)?;
        let _guard = lock.lock().await;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failure(ErrorCode::AuthRequired))?;
            if let Some(job) = state.jobs.get_mut(id) {
                if matches!(job.state.as_str(), "pending" | "interaction_required") {
                    job.signal.cancel();
                    job.challenge = None;
                    job.state = "cancelled".into();
                    *state.generations.entry(connection_id.into()).or_default() += 1;
                }
            }
        }
        self.status(connection_id, id, caller)
    }
    pub async fn logout(&self, connection_id: &str) -> ServiceResult<()> {
        self.invalidate(connection_id, true).await
    }
    pub async fn invalidate(&self, connection_id: &str, clear_secrets: bool) -> ServiceResult<()> {
        let lock = self.connection_lock(connection_id)?;
        let _guard = lock.lock().await;
        let previous = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failure(ErrorCode::AuthRequired))?;
            let previous = state
                .grants
                .iter()
                .find(|(config, _)| config.connection_id == connection_id)
                .map(|(_, grant)| grant.principal_id.clone());
            state
                .grants
                .retain(|config, _| config.connection_id != connection_id);
            *state.generations.entry(connection_id.into()).or_default() += 1;
            for job in state
                .jobs
                .values_mut()
                .filter(|job| job.connection_id == connection_id)
            {
                job.signal.cancel();
                job.challenge = None;
                job.state = "cancelled".into();
            }
            previous
        };
        self.lifecycle
            .logout(connection_id, previous.as_deref())
            .await?;
        if clear_secrets {
            for slot in [
                SecretSlot::EntraRefreshToken,
                SecretSlot::EntraClientSecret,
                SecretSlot::CosmosAccountKey,
                SecretSlot::DbPassword,
            ] {
                self.session.delete(connection_id, slot).await?;
                self.persistent.delete(connection_id, slot).await?;
            }
        }
        Ok(())
    }
    pub fn persistent_store(&self) -> &Arc<dyn SecretStore> {
        &self.persistent
    }
    pub fn session_store(&self) -> &Arc<dyn SecretStore> {
        &self.session
    }
    pub async fn import_refresh(
        &self,
        connection: &SavedConnection,
        refresh: Secret,
        persistence: super::credentials::SecretPersistence,
        deadline_ms: u64,
    ) -> ServiceResult<()> {
        if serde_json::from_str::<Value>(refresh.expose()).is_ok() {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let config = AuthConfig::from_connection(connection)?
            .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
        if config.mode != "entra_user" || config.source != "oauth" {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let generation = self.generation(&connection.id)?;
        let mut grant = tokio::time::timeout(
            Duration::from_millis(deadline_ms),
            self.provider.refresh(&config, &refresh, None),
        )
        .await
        .map_err(|_| failure(ErrorCode::DeadlineExceeded))??;
        if grant.expires_at_ms
            < now_ms()
                .saturating_add(deadline_ms)
                .saturating_add(REFRESH_MARGIN_MS)
        {
            return Err(failure(ErrorCode::AuthExpired));
        }
        if grant.refresh_token.is_none() {
            grant.refresh_token = Some(refresh);
        }
        self.install(
            &config,
            grant,
            generation,
            persistence == super::credentials::SecretPersistence::Keychain,
        )
        .await
    }
    pub async fn teardown(&self) -> ServiceResult<()> {
        let connections: std::collections::HashSet<String> = {
            let state = self
                .state
                .lock()
                .map_err(|_| failure(ErrorCode::AuthRequired))?;
            state
                .connection_locks
                .keys()
                .cloned()
                .chain(
                    state
                        .grants
                        .keys()
                        .map(|config| config.connection_id.clone()),
                )
                .chain(state.jobs.values().map(|job| job.connection_id.clone()))
                .collect()
        };
        for connection in connections {
            let lock = self.connection_lock(&connection)?;
            let _guard = lock.lock().await;
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| failure(ErrorCode::AuthRequired))?;
                *state.generations.entry(connection.clone()).or_default() += 1;
                state
                    .grants
                    .retain(|config, _| config.connection_id != connection);
                for job in state
                    .jobs
                    .values_mut()
                    .filter(|job| job.connection_id == connection)
                {
                    job.signal.cancel();
                    job.state = "cancelled".into();
                    job.challenge = None;
                }
            }
            for slot in [
                SecretSlot::EntraRefreshToken,
                SecretSlot::EntraClientSecret,
                SecretSlot::CosmosAccountKey,
                SecretSlot::DbPassword,
            ] {
                self.session.delete(&connection, slot).await?;
            }
        }
        Ok(())
    }
}
