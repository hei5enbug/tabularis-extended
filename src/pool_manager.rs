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

/// Stable cache key for a target and its non-secret authentication identity.
/// Passwords stay out so SQL password rotation preserves existing behavior.
fn build_connection_key(
    params: &ConnectionParams,
    entra_auth: Option<&AzureCliAuth>,
    trust_server_certificate: bool,
) -> String {
    let ssl_mode = params.ssl_mode.as_deref().unwrap_or("prefer");
    let auth = if let Some(entra_auth) = entra_auth {
        entra_auth.identity_key()
    } else if params.integrated_auth {
        "integrated".to_owned()
    } else {
        format!("sql:{}", params.username.as_deref().unwrap_or(""))
    };
    let port = params.port.unwrap_or(1433).to_string();
    let database = params.database.to_string();
    let trust_server_certificate = trust_server_certificate.to_string();
    let mut key = String::new();
    for (name, value) in [
        ("driver", params.driver.as_str()),
        (
            "connection_id",
            params.connection_id.as_deref().unwrap_or(""),
        ),
        ("host", params.host.as_deref().unwrap_or("localhost")),
        ("port", port.as_str()),
        ("database", database.as_str()),
        ("auth", auth.as_str()),
        ("ssl_mode", ssl_mode),
        (
            "trust_server_certificate",
            trust_server_certificate.as_str(),
        ),
        ("ssl_ca", params.ssl_ca.as_deref().unwrap_or("")),
        ("ssl_cert", params.ssl_cert.as_deref().unwrap_or("")),
        ("ssl_key", params.ssl_key.as_deref().unwrap_or("")),
    ] {
        use std::fmt::Write;
        let _ = write!(key, "{name}:{}:{value};", value.len());
    }
    key
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
    let entra_auth = AzureCliAuth::from_params(&params)?;
    let settings = settings::current();
    let key = build_connection_key(
        &params,
        entra_auth.as_ref(),
        settings.trust_server_certificate,
    );
    let mut pools = SQLSERVER_POOLS.write().await;
    if let Some(pool) = pools.get(&key).cloned() {
        return Ok(pool);
    }

    // A pool snapshots process settings. The host initializes the process
    // before opening connections, and live pools are never mutated in place.
    let manager = BridgeManager::new(
        build_config(&params, &settings)?,
        entra_auth,
        startup_script(&params),
        params.ssl_mode.as_deref().unwrap_or("prefer"),
        &settings,
    );
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
    let Ok(entra_auth) = AzureCliAuth::from_params(&params) else {
        return;
    };
    let settings = settings::current();
    SQLSERVER_POOLS.write().await.remove(&build_connection_key(
        &params,
        entra_auth.as_ref(),
        settings.trust_server_certificate,
    ));
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

    fn entra_params(connection_id: Option<&str>) -> ConnectionParams {
        let mut params = params(connection_id);
        params.host = Some("sample.database.windows.net".into());
        params.username = Some("person@example.test".into());
        params.password = None;
        params.ssl_mode = Some("verify-full".into());
        params.extra.insert("auth_mode".into(), "entra_user".into());
        params
            .extra
            .insert("auth_source".into(), "azure_cli".into());
        params
            .extra
            .insert("tenant_id".into(), "tenant-guid".into());
        params
            .extra
            .insert("entra_object_id".into(), "object-guid".into());
        params
    }

    /// 저장된 연결 ID가 풀 키에 포함된다.
    #[test]
    fn key_includes_connection_id() {
        // given
        let params = params(Some("abc"));

        // when
        let key = build_connection_key(&params, None, false);

        // then
        assert!(key.contains("connection_id:3:abc;"));
    }

    /// 호스트와 인증 사용자, TLS 설정이 풀 키에 포함된다.
    #[test]
    fn key_includes_user_and_ssl_mode_for_adhoc_connections() {
        // given
        let mut p = params(None);
        p.ssl_mode = Some("require".into());

        // when
        let key = build_connection_key(&p, None, false);

        // then
        assert!(key.contains("host:9:localhost;"));
        assert!(key.contains("port:4:1433;"));
        assert!(key.contains("database:6:master;"));
        assert!(key.contains("auth:6:sql:sa;"));
        assert!(key.contains("ssl_mode:7:require;"));
    }

    /// 저장된 연결 ID가 같아도 인증 방식이 다르면 풀이 분리된다.
    #[test]
    fn integrated_auth_is_part_of_the_key_even_with_a_shared_connection_id() {
        // given
        let sql_auth = params(Some("ss045-key"));
        let mut integrated = sql_auth.clone();
        integrated.username = None;
        integrated.integrated_auth = true;

        // when
        let sql_key = build_connection_key(&sql_auth, None, false);
        let integrated_key = build_connection_key(&integrated, None, false);

        // then
        assert_ne!(
            sql_key, integrated_key,
            "editing a saved connection between SQL and Windows auth must not reuse a stale pool"
        );
    }

    /// 데이터베이스가 풀 키에 포함되고 같은 설정은 같은 키를 만든다.
    #[test]
    fn database_is_part_of_the_key_and_identical_params_are_stable() {
        // given
        let master = params(Some("ss045-key"));
        let identical = master.clone();
        let mut other_database = master.clone();
        other_database.database = crate::models::DatabaseSelection::Single("tempdb".into());

        // when
        let master_key = build_connection_key(&master, None, false);
        let identical_key = build_connection_key(&identical, None, false);
        let other_database_key = build_connection_key(&other_database, None, false);

        // then
        assert_eq!(master_key, identical_key);
        assert_ne!(master_key, other_database_key);
    }

    /// 동일한 연결 정보는 연결 문자열 입력과 개별 필드 입력에서 같은 풀 키를 만든다.
    #[test]
    fn equivalent_discrete_and_connection_string_params_share_a_key() {
        // given
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

        // when
        let discrete_key = build_connection_key(&discrete, None, false);
        let string_key = build_connection_key(&from_string, None, false);

        // then
        assert_eq!(discrete_key, string_key);
    }

    /// 같은 연결 ID를 사용해도 서버 대상과 TLS 설정이 다르면 풀이 분리된다.
    #[test]
    fn shared_connection_id_still_includes_target_and_tls_identity() {
        // given
        let base = params(Some("shared"));
        let mut other_host = base.clone();
        other_host.host = Some("other.internal".into());
        let mut other_port = base.clone();
        other_port.port = Some(1444);
        let mut other_tls = base.clone();
        other_tls.ssl_mode = Some("require".into());

        // when
        let base_key = build_connection_key(&base, None, false);

        // then
        assert_ne!(base_key, build_connection_key(&other_host, None, false));
        assert_ne!(base_key, build_connection_key(&other_port, None, false));
        assert_ne!(base_key, build_connection_key(&other_tls, None, false));
        assert_ne!(
            base_key,
            build_connection_key(&base, None, true),
            "certificate trust changes must not share a pool"
        );
    }

    /// Entra 풀 키는 사용자 신원과 CLI 경로를 구분하며 비밀번호를 담지 않는다.
    #[test]
    fn entra_pool_key_contains_nonsecret_identity_and_excludes_password() {
        // given
        let mut first = entra_params(Some("shared"));
        let cli_path = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        first
            .extra
            .insert("azure_cli_path".into(), cli_path.clone());
        let mut second = first.clone();
        second
            .extra
            .insert("entra_object_id".into(), "other-object".into());
        let auth = AzureCliAuth::from_params(&first).unwrap();
        let other_auth = AzureCliAuth::from_params(&second).unwrap();

        // when
        let first_key = build_connection_key(&first, auth.as_ref(), false);
        let second_key = build_connection_key(&second, other_auth.as_ref(), false);

        // then
        assert_ne!(first_key, second_key);
        assert!(first_key.contains("tenant=11:tenant-guid"));
        assert!(first_key.contains("object_id=11:object-guid"));
        assert!(first_key.contains("username=19:person@example.test"));
        assert!(first_key.contains(&format!("cli={}:{}", cli_path.len(), cli_path)));
    }

    /// SQL 비밀번호는 풀 키에 저장되지 않는다.
    #[test]
    fn sql_password_is_excluded_from_pool_key() {
        // given
        let mut params = params(Some("shared"));
        params.password = Some("password-secret".into());

        // when
        let key = build_connection_key(&params, None, false);

        // then
        assert!(!key.contains("password-secret"));
    }
}
