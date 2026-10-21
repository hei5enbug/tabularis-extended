//! Connection-pool registry.
//!
//! Pools are cached per connection key so repeated RPC calls reuse warm
//! sessions instead of paying a fresh TDS handshake each time.

use std::collections::HashMap;
use std::sync::Arc;

use deadpool::managed::Pool as DeadPool;
use once_cell::sync::Lazy;
use tokio::sync::RwLock;

use crate::connection::resolve_connection_params;
use crate::driver::azure_cli_auth::AzureCliAuth;
use crate::driver::pool::{build_config, BridgeManager};
use crate::models::ConnectionParams;
use crate::settings;

pub type SqlServerPool = DeadPool<BridgeManager>;
type SqlServerPoolMap = Arc<RwLock<HashMap<String, SqlServerPool>>>;

static SQLSERVER_POOLS: Lazy<SqlServerPoolMap> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

pub(crate) static SERVICE_POOLS: Lazy<
    RwLock<HashMap<String, (String, crate::driver::service::pool::NativePool)>>,
> = Lazy::new(|| RwLock::new(HashMap::new()));

/// Stable cache key. Prefers `connection_id`; else host:port:database.
/// Folds in username (bastions route by it), ssl_mode, and auth mode, so
/// none of those can silently reuse a pool built under different ones.
/// Entra user connections also fold in the auth source, tenant, object ID,
/// username, Azure CLI path, host, and port; access tokens never appear.
/// Gap: password isn't in the key, so rotating it still reuses the pool.
fn build_connection_key(params: &ConnectionParams) -> String {
    let ssl_mode = params.ssl_mode.as_deref().unwrap_or("prefer");
    let entra_user_auth = params
        .auth_mode
        .as_deref()
        .or_else(|| params.extra.get("auth_mode").map(String::as_str))
        .is_some_and(|mode| mode.trim() == "entra_user");
    let extra = |key: &str, default: &'static str| {
        key_part(params.extra.get(key).map(String::as_str).unwrap_or(default))
    };
    let auth = if entra_user_auth {
        format!(
            "entra{}{}{}{}{}{}",
            key_part("entra_user"),
            extra("auth_source", "azure_cli"),
            extra("tenant_id", ""),
            extra("entra_object_id", ""),
            key_part(params.username.as_deref().unwrap_or("")),
            extra("azure_cli_path", "az")
        )
    } else if params.integrated_auth {
        "integrated".to_string()
    } else {
        format!("sql:{}", params.username.as_deref().unwrap_or(""))
    };
    let base_key = if let Some(conn_id) = params.connection_id.as_deref() {
        format!("{}:conn:{}:{}", params.driver, conn_id, params.database)
    } else {
        format!(
            "{}:{}:{}:{}",
            params.driver,
            params.host.as_deref().unwrap_or("localhost"),
            params.port.unwrap_or(1433),
            params.database
        )
    };
    let target = if entra_user_auth {
        format!(
            ":host{}:port{}",
            key_part(params.host.as_deref().unwrap_or("localhost")),
            params.port.unwrap_or(1433)
        )
    } else {
        String::new()
    };
    format!("{base_key}:auth:{auth}:ssl:{ssl_mode}{target}")
}

fn key_part(value: &str) -> String {
    format!("{}:{value}", value.len())
}

fn startup_script(params: &ConnectionParams) -> Option<String> {
    params
        .startup_script
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub async fn get_sqlserver_pool(params: &ConnectionParams) -> Result<SqlServerPool, String> {
    let params = resolve_connection_params(params)?;
    let key = build_connection_key(&params);
    let mut pools = SQLSERVER_POOLS.write().await;
    if let Some(pool) = pools.get(&key).cloned() {
        return Ok(pool);
    }

    // A pool snapshots process settings. The host initializes the process
    // before opening connections, and live pools are never mutated in place.
    let settings = settings::current();
    let azure_cli_auth = AzureCliAuth::from_connection_params(&params, &settings)?;
    let mut manager = BridgeManager::new(
        build_config(&params, &settings)?,
        startup_script(&params),
        params.ssl_mode.as_deref().unwrap_or("prefer"),
        &settings,
    );
    if let Some(auth) = azure_cli_auth {
        manager = manager.with_azure_cli_auth(auth);
    }
    let pool = DeadPool::builder(manager)
        .max_size(settings.max_pool_size)
        .build()
        .map_err(|error| error.to_string())?;
    pools.insert(key, pool.clone());
    Ok(pool)
}

/// Remove a cached pool after checkout failed. This lets corrected connection
/// parameters replace a failed lazy manager under the same connection id.
pub async fn remove_sqlserver_pool(params: &ConnectionParams) {
    let Ok(params) = resolve_connection_params(params) else {
        return;
    };
    SQLSERVER_POOLS
        .write()
        .await
        .remove(&build_connection_key(&params));
}

/// Drop pools that currently have no checked-out connections. Called
/// periodically so long-idle sessions don't linger for the plugin's lifetime.
pub async fn cleanup_idle_pools() {
    let mut pools = SQLSERVER_POOLS.write().await;
    pools.retain(|_, pool| {
        let status = pool.status();
        let has_checked_out_connection = status.size > status.available;
        if !has_checked_out_connection {
            // Explicit close makes the server-side sessions disappear now;
            // removing the last registry handle alone would rely on drop
            // timing inside deadpool.
            pool.close();
        }
        has_checked_out_connection
    });
}

/// Close every cached pool and remove it from the process-wide registry.
pub async fn shutdown() {
    crate::driver::service::pool::shutdown().await;
    let pools: Vec<_> = SQLSERVER_POOLS
        .write()
        .await
        .drain()
        .map(|(_, pool)| pool)
        .collect();
    for pool in pools {
        pool.close();
    }
}

#[cfg(test)]
pub async fn pool_count() -> usize {
    SQLSERVER_POOLS.read().await.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(connection_id: Option<&str>) -> ConnectionParams {
        ConnectionParams {
            driver: "sqlserver".into(),
            host: Some("localhost".into()),
            port: Some(1433),
            username: Some("sa".into()),
            database: crate::models::DatabaseSelection::Single("master".into()),
            connection_id: connection_id.map(str::to_owned),
            ..Default::default()
        }
    }

    fn entra_params() -> ConnectionParams {
        let mut params = params(Some("azure-sql"));
        params.host = Some("synthetic.database.windows.net".into());
        params.username = Some("user@example.com".into());
        params.ssl_mode = Some("verify-full".into());
        params.extra.insert("auth_mode".into(), "entra_user".into());
        params
            .extra
            .insert("auth_source".into(), "azure_cli".into());
        params.extra.insert(
            "tenant_id".into(),
            "00000000-0000-0000-0000-0000000000a1".into(),
        );
        params.extra.insert(
            "entra_object_id".into(),
            "00000000-0000-0000-0000-0000000000b2".into(),
        );
        params
    }

    #[test]
    fn key_prefers_connection_id() {
        let key = build_connection_key(&params(Some("abc")));
        assert!(key.contains(":conn:abc:"));
    }

    #[test]
    fn key_includes_user_and_ssl_mode_for_adhoc_connections() {
        let mut p = params(None);
        p.ssl_mode = Some("require".into());
        let key = build_connection_key(&p);
        assert_eq!(
            key,
            "sqlserver:localhost:1433:master:auth:sql:sa:ssl:require"
        );
    }

    #[test]
    fn integrated_auth_is_part_of_the_key_even_with_a_shared_connection_id() {
        let sql_auth = params(Some("ss045-key"));
        let mut integrated = sql_auth.clone();
        integrated.username = None;
        integrated.integrated_auth = true;

        assert_ne!(
            build_connection_key(&sql_auth),
            build_connection_key(&integrated),
            "editing a saved connection between SQL and Windows auth must not reuse a stale pool"
        );
    }

    #[test]
    fn database_is_part_of_the_key_and_identical_params_are_stable() {
        let master = params(Some("ss045-key"));
        let identical = master.clone();
        let mut other_database = master.clone();
        other_database.database = crate::models::DatabaseSelection::Single("tempdb".into());

        assert_eq!(
            build_connection_key(&master),
            build_connection_key(&identical)
        );
        assert_ne!(
            build_connection_key(&master),
            build_connection_key(&other_database)
        );
    }

    #[test]
    fn equivalent_discrete_and_connection_string_params_share_a_key() {
        let mut discrete = params(None);
        discrete.ssl_mode = Some("require".into());
        let discrete = resolve_connection_params(&discrete).unwrap();
        let from_string = resolve_connection_params(&ConnectionParams {
            connection_string: Some(
                "sqlserver://sa@localhost/master?Encrypt=true&TrustServerCertificate=true".into(),
            ),
            password: Some(String::new()),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(
            build_connection_key(&discrete),
            build_connection_key(&from_string)
        );
    }

    #[test]
    fn entra_pool_identity_includes_tenant_object_user_source_and_cli_path() {
        // given
        let base = entra_params();
        let base_key = build_connection_key(&base);
        let variants = [
            ("tenant_id", "00000000-0000-0000-0000-0000000000a9"),
            ("entra_object_id", "00000000-0000-0000-0000-0000000000b9"),
            ("azure_cli_path", "/opt/azure/az"),
            ("auth_source", "other_source"),
        ]
        .map(|(key, value)| {
            let mut changed = base.clone();
            changed.extra.insert(key.into(), value.into());
            changed
        });
        let mut other_user = base.clone();
        other_user.username = Some("other@example.com".into());

        // when
        let keys = variants
            .iter()
            .map(build_connection_key)
            .collect::<Vec<_>>();
        let user_key = build_connection_key(&other_user);

        // then
        assert!(keys.iter().all(|key| *key != base_key));
        assert_ne!(user_key, base_key);
        assert!(base_key.contains("azure_cli"));
    }

    #[test]
    fn entra_pool_identity_includes_host_and_port_with_connection_id() {
        // given
        let base = entra_params();
        let base_key = build_connection_key(&base);
        let mut other_host = base.clone();
        other_host.host = Some("other.database.windows.net".into());
        let mut other_port = base.clone();
        other_port.port = Some(1444);

        // when
        let host_key = build_connection_key(&other_host);
        let port_key = build_connection_key(&other_port);

        // then
        assert_ne!(base_key, host_key);
        assert_ne!(base_key, port_key);
    }

    #[test]
    fn entra_pool_identity_is_unambiguous_across_field_boundaries() {
        // given
        let mut first = entra_params();
        first.username = Some("a".into());
        first.extra.insert("tenant_id".into(), "bc".into());
        let mut second = entra_params();
        second.username = Some("ab".into());
        second.extra.insert("tenant_id".into(), "c".into());

        // when
        let first_key = build_connection_key(&first);
        let second_key = build_connection_key(&second);

        // then
        assert_ne!(first_key, second_key);
    }

    #[test]
    fn entra_pool_identity_keeps_sql_and_integrated_keys_unchanged() {
        // given
        let mut sql = params(None);
        sql.auth_mode = Some("sql_password".into());

        // when
        let key = build_connection_key(&sql);

        // then
        assert_eq!(
            key,
            "sqlserver:localhost:1433:master:auth:sql:sa:ssl:prefer"
        );
    }

    #[tokio::test]
    async fn entra_pool_creation_rejects_incomplete_identity_settings() {
        // given
        let mut invalid = entra_params();
        invalid.extra.remove("entra_object_id");
        invalid.connection_id = Some(uuid::Uuid::new_v4().to_string());

        // when
        let actual = get_sqlserver_pool(&invalid).await;

        // then
        assert!(actual.is_err());
    }
}
