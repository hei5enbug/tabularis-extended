use super::{
    exact, readonly,
    registry::Scope,
    types::{DriverContext, TransientAuth},
    ServiceError,
};
use crate::{models::ConnectionParams, settings::PluginSettings};
use deadpool::managed::{Manager, Metrics, Pool, RecycleError, RecycleResult};
use mssql_tds::{
    connection::tds_client::TdsClient,
    connection_provider::tds_connection_provider::TdsConnectionProvider,
};
use mssql_tiberius_bridge::{AuthMethod, Config, EncryptionLevel};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub type NativePool = Pool<ServiceManager>;
pub type NativeConnection = deadpool::managed::Object<ServiceManager>;
pub struct Login {
    pub context: DriverContext,
    pub deadline: tokio::time::Instant,
    pub cancel: mssql_tds::core::CancelHandle,
    pub state: std::sync::Arc<super::registry::State>,
}
tokio::task_local! { static LOGIN: Login; }
use crate::pool_manager::SERVICE_POOLS as POOLS;
pub struct ServiceManager {
    params: ConnectionParams,
    settings: PluginSettings,
    read_only: bool,
}
fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}
pub fn fingerprint(params: &ConnectionParams, context: &DriverContext) -> String {
    fingerprint_with_settings(params, context, &crate::settings::current())
}
fn fingerprint_with_settings(
    params: &ConnectionParams,
    context: &DriverContext,
    process: &PluginSettings,
) -> String {
    let mut params = params.clone();
    params.password = None;
    params.connection_string = None;
    params.connection_id = None;
    let mut settings = serde_json::to_value(params).unwrap();
    let script = settings["startup_script"].take();
    settings["startup_script"] = json!(digest(&script));
    digest(
        &json!({"connection_id":context.connection_id,"settings":settings,"identity":context.identity(),"read_only":context.read_only,
        "process":{"pool":process.max_pool_size,"connect":process.connect_timeout_seconds,"query":process.query_timeout_seconds,"application":process.application_name,"trust":process.trust_server_certificate}}),
    )
}
pub fn config(
    params: &ConnectionParams,
    settings: &PluginSettings,
    context: &DriverContext,
    remaining: u64,
) -> Result<Config, ServiceError> {
    context.validate_auth(params, remaining)?;
    if crate::connection::is_azure_sql(params) && settings.trust_server_certificate {
        return Err(ServiceError::argument("Azure SQL requires verified TLS"));
    }
    let mut cfg = Config::new();
    cfg.host(params.host.as_deref().unwrap_or("localhost"))
        .port(params.port.unwrap_or(1433))
        .database(params.database.primary())
        .application_name(&settings.application_name);
    match &context.auth {
        Some(TransientAuth::DbPassword { password, .. }) => {
            cfg.authentication(AuthMethod::sql_server(
                params.username.as_deref().unwrap_or(""),
                password,
            ));
        }
        Some(TransientAuth::EntraToken { access_token, .. }) => {
            cfg.authentication(AuthMethod::aad_token(access_token));
        }
        None => {
            cfg.authentication(AuthMethod::Integrated);
        }
    }
    match params.ssl_mode.as_deref() {
        Some("verify-full") => {
            cfg.encryption(EncryptionLevel::Required);
        }
        Some("disable") => {
            cfg.encryption(EncryptionLevel::NotSupported);
        }
        Some("require") => {
            cfg.encryption(EncryptionLevel::Required).trust_cert();
        }
        Some("verify-ca") => {
            return Err(ServiceError::argument(
                "SQL Server requires verify-full for certificate verification",
            ))
        }
        _ => {
            cfg.encryption(EncryptionLevel::On).trust_cert();
        }
    }
    if settings.trust_server_certificate {
        cfg.trust_cert();
    }
    if [&params.ssl_ca, &params.ssl_cert, &params.ssl_key]
        .iter()
        .any(|value| value.as_ref().is_some_and(|v| !v.is_empty()))
    {
        return Err(ServiceError::argument(
            "Custom TLS credential files are unsupported",
        ));
    }
    Ok(cfg)
}
impl Manager for ServiceManager {
    type Type = TdsClient;
    type Error = ServiceError;
    async fn create(&self) -> Result<Self::Type, Self::Error> {
        let (context, deadline, cancel) = LOGIN
            .try_with(|login| {
                (
                    login.context.clone(),
                    login.deadline,
                    login.cancel.child_handle(),
                )
            })
            .map_err(|_| {
                ServiceError::new(
                    "AUTH_REQUIRED",
                    "A physical login requires the current request context",
                )
            })?;
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis() as u64;
        if remaining == 0 {
            return Err(ServiceError::new(
                "DEADLINE_EXCEEDED",
                "The physical login deadline was exceeded",
            ));
        }
        let cfg = config(&self.params, &self.settings, &context, remaining)?;
        let mut login = cfg.to_client_context();
        login.connect_retry_count = 0;
        login.connect_timeout = self.settings.connect_timeout_seconds;
        let mut client = TdsConnectionProvider
            .create_client(login, &cfg.datasource_string(), Some(&cancel))
            .await
            .map_err(|_| ServiceError::new("AUTH_REQUIRED", "SQL Server physical login failed"))?;
        if self.read_only {
            readonly::probe(&mut client, Some(&cancel)).await?;
        }
        if let Some(script) = self
            .params
            .startup_script
            .as_deref()
            .filter(|v| !v.trim().is_empty())
        {
            if !self.read_only {
                LOGIN.with(|login| login.state.mark_write());
            }
            exact::collect(
                &mut client,
                script,
                Vec::new(),
                Some(&cancel),
                10_000,
                super::types::FRAME_LIMIT,
                false,
            )
            .await?;
        }
        Ok(client)
    }
    async fn recycle(&self, _: &mut Self::Type, _: &Metrics) -> RecycleResult<Self::Error> {
        Err(RecycleError::message(
            "Native connections require a fresh login because a safe reset API is unavailable",
        ))
    }
}
pub async fn acquire(
    params: &ConnectionParams,
    context: &DriverContext,
    scope: &Scope,
) -> Result<NativeConnection, ServiceError> {
    let key = fingerprint(params, context);
    let pool = {
        let mut pools = POOLS.write().await;
        if let Some((_, pool)) = pools.get(&key) {
            pool.clone()
        } else {
            let mut sanitized = params.clone();
            sanitized.password = None;
            sanitized.connection_string = None;
            let settings = crate::settings::current();
            let pool = Pool::builder(ServiceManager {
                params: sanitized,
                settings: settings.clone(),
                read_only: context.read_only,
            })
            .max_size(settings.max_pool_size.min(4))
            .build()
            .map_err(|_| {
                ServiceError::new("RESOURCE_LIMIT", "Cannot create a service connection pool")
            })?;
            pools.insert(key, (context.connection_id.clone(), pool.clone()));
            pool
        }
    };
    let remaining = scope.remaining()?;
    context.validate_auth(params, remaining)?;
    let login = Login {
        context: context.clone(),
        deadline: scope.state.deadline,
        cancel: scope.cancel_handle(),
        state: scope.state.clone(),
    };
    LOGIN
        .scope(login, pool.get())
        .await
        .map_err(|error| match error {
            deadpool::managed::PoolError::Backend(error) => error,
            _ => ServiceError::new(
                "CONNECTION_NOT_FOUND",
                "Cannot acquire a service connection",
            ),
        })
}
pub async fn invalidate(connection: &str) {
    let mut pools = POOLS.write().await;
    pools.retain(|_, (id, pool)| {
        if id == connection {
            pool.close();
            false
        } else {
            true
        }
    });
}
pub async fn shutdown() {
    for (_, pool) in POOLS.write().await.drain().map(|(_, value)| value) {
        pool.close();
    }
}
pub async fn cleanup() {
    POOLS.write().await.retain(|_, (_, pool)| {
        let status = pool.status();
        if status.size == status.available {
            pool.close();
            false
        } else {
            true
        }
    });
}
pub fn discard(connection: NativeConnection) {
    drop(deadpool::managed::Object::take(connection));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::service::types;
    #[test]
    fn 계정과_설정과_startup_script를_바꾸면_별도_pool을_선택한다() {
        // given
        let mut params = ConnectionParams {
            host: Some("localhost".into()),
            username: Some("user".into()),
            database: crate::models::DatabaseSelection::Single("fixture".into()),
            ..Default::default()
        };
        let context: DriverContext = serde_json::from_value(json!({"protocol_version":1,"connection_id":"fixture","request_id":"request","deadline_ms":1000,"read_only":false,"auth":{"kind":"db_password","password":"synthetic-key","identity":"principal"}})).unwrap();
        let settings = PluginSettings::default();
        let original = fingerprint_with_settings(&params, &context, &settings);
        params.startup_script = Some("SET ANSI_NULLS ON".into());
        // when
        let actual = fingerprint_with_settings(&params, &context, &settings);
        // then
        assert_ne!(actual, original);
        assert_eq!(actual.len(), 64);
        assert!(!actual.contains("SET"));
    }
    #[test]
    fn 같은_주체의_token_refresh는_pool에_token을_남기지_않고_binding을_유지한다() {
        // given
        let params = ConnectionParams::default();
        let mut context: DriverContext = serde_json::from_value(json!({"protocol_version":1,"connection_id":"fixture","request_id":"request","auth":{"kind":"entra_token","access_token":"synthetic-old","expires_at_ms":types::unix_ms()+1_000_000,"identity":"principal","tenant_id":"tenant","client_id":"client","principal_id":"principal","scope":types::SQL_SCOPE}})).unwrap();
        let settings = PluginSettings::default();
        let original = fingerprint_with_settings(&params, &context, &settings);
        if let Some(TransientAuth::EntraToken {
            access_token,
            expires_at_ms,
            ..
        }) = &mut context.auth
        {
            *access_token = "synthetic-new".into();
            *expires_at_ms += 1000;
        }
        // when
        let actual = fingerprint_with_settings(&params, &context, &settings);
        // then
        assert_eq!(actual, original);
        assert!(!actual.contains("synthetic"));
    }
}
