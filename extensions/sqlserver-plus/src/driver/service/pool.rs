use super::{
    exact, readonly,
    registry::{Scope, State},
    types::{self, DriverContext, TransientAuth},
    ServiceError,
};
use crate::{
    driver::azure_cli_auth::AzureCliAuth, models::ConnectionParams, settings::PluginSettings,
};
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
    entra: Option<AzureCliAuth>,
}
impl ServiceManager {
    fn new(
        params: ConnectionParams,
        settings: PluginSettings,
        read_only: bool,
    ) -> Result<Self, ServiceError> {
        let entra = types::entra_auth(&params)?;
        Ok(Self {
            params,
            settings,
            read_only,
            entra,
        })
    }
    /// Login config for one new physical session. An `entra_user` token is
    /// acquired here through the driver's Azure CLI module, within the request
    /// deadline and cancellable, and lives only in the returned config.
    async fn login_config(
        &self,
        context: &DriverContext,
        deadline: tokio::time::Instant,
        state: Option<&State>,
    ) -> Result<Config, ServiceError> {
        let mut cfg = config(&self.params, &self.settings, context)?;
        if let Some(auth) = &self.entra {
            let cancelled = async {
                match state {
                    Some(state) => state.wait_cancelled().await,
                    None => std::future::pending().await,
                }
            };
            let token = tokio::select! {
                result = tokio::time::timeout_at(deadline, auth.acquire_access_token()) => {
                    result
                        .map_err(|_| ServiceError::new(
                            "DEADLINE_EXCEEDED",
                            "The physical login deadline was exceeded",
                        ))?
                        .map_err(|message| ServiceError::new("AUTH_REQUIRED", message))?
                }
                _ = cancelled => {
                    return Err(ServiceError::new("CANCELLED", "The service request was cancelled"));
                }
            };
            cfg.authentication(AuthMethod::aad_token(token.expose()));
        }
        Ok(cfg)
    }
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
    let params_for_identity = params.clone();
    let mut params = params.clone();
    params.password = None;
    params.connection_string = None;
    params.connection_id = None;
    let mut settings = serde_json::to_value(params).unwrap();
    let script = settings["startup_script"].take();
    settings["startup_script"] = json!(digest(&script));
    digest(
        &json!({"connection_id":context.connection_id,"settings":settings,"identity":context.identity(&params_for_identity),"read_only":context.read_only,
        "process":{"pool":process.max_pool_size,"connect":process.connect_timeout_seconds,"query":process.query_timeout_seconds,"application":process.application_name,"trust":process.trust_server_certificate}}),
    )
}
pub fn config(
    params: &ConnectionParams,
    settings: &PluginSettings,
    context: &DriverContext,
) -> Result<Config, ServiceError> {
    context.validate_auth(params)?;
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
        None if params.auth_mode.as_deref() == Some("entra_user") => {
            // Placeholder only: `login_config` installs the driver-acquired
            // token, and an unreplaced empty token fails instead of falling
            // back to another authentication method.
            cfg.authentication(AuthMethod::aad_token(""));
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
        let (context, deadline, cancel, state) = LOGIN
            .try_with(|login| {
                (
                    login.context.clone(),
                    login.deadline,
                    login.cancel.child_handle(),
                    login.state.clone(),
                )
            })
            .map_err(|_| {
                ServiceError::new(
                    "AUTH_REQUIRED",
                    "A physical login requires the current request context",
                )
            })?;
        if deadline <= tokio::time::Instant::now() {
            return Err(ServiceError::new(
                "DEADLINE_EXCEEDED",
                "The physical login deadline was exceeded",
            ));
        }
        let cfg = self.login_config(&context, deadline, Some(&state)).await?;
        let mut login = cfg.to_client_context();
        login.connect_retry_count = 0;
        login.connect_timeout = self.settings.connect_timeout_seconds;
        let mut client = TdsConnectionProvider
            .create_client(login, &cfg.datasource_string(), Some(&cancel))
            .await
            .map_err(|error| match error {
                mssql_tds::error::Error::OperationCancelledError(_) => {
                    ServiceError::new("CANCELLED", "The service request was cancelled")
                }
                _ => ServiceError::new("AUTH_REQUIRED", "SQL Server physical login failed"),
            })?;
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
    scope.remaining()?;
    context.validate_auth(params)?;
    let pool = {
        let mut pools = POOLS.write().await;
        if let Some((_, pool)) = pools.get(&key) {
            pool.clone()
        } else {
            let mut sanitized = params.clone();
            sanitized.password = None;
            sanitized.connection_string = None;
            let settings = crate::settings::current();
            let manager = ServiceManager::new(sanitized, settings.clone(), context.read_only)?;
            let pool = Pool::builder(manager)
                .max_size(settings.max_pool_size.min(4))
                .build()
                .map_err(|_| {
                    ServiceError::new("RESOURCE_LIMIT", "Cannot create a service connection pool")
                })?;
            pools.insert(key, (context.connection_id.clone(), pool.clone()));
            pool
        }
    };
    scope.remaining()?;
    let login = Login {
        context: context.clone(),
        deadline: scope.state.deadline,
        cancel: scope.cancel_handle(),
        state: scope.state.clone(),
    };
    let checkout = LOGIN.scope(login, pool.get());
    tokio::select! {
        result = checkout => result.map_err(|error| match error {
            deadpool::managed::PoolError::Backend(error) => error,
            _ => ServiceError::new(
                "CONNECTION_NOT_FOUND",
                "Cannot acquire a service connection",
            ),
        }),
        _ = scope.wait_cancelled() => Err(scope.remaining().err().unwrap_or_else(|| {
            ServiceError::new("CANCELLED", "The service request was cancelled")
        })),
    }
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
    use crate::driver::azure_cli_auth::fake_cli::{self, HOST, OBJECT, TENANT, USER};
    use crate::driver::service::registry;
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
    fn entra_fixture(cli_path: Option<&str>) -> (ConnectionParams, DriverContext) {
        let mut params = ConnectionParams {
            host: Some(HOST.into()),
            username: Some(USER.into()),
            database: crate::models::DatabaseSelection::Single("fixture".into()),
            auth_mode: Some("entra_user".into()),
            ssl_mode: Some("verify-full".into()),
            extra: [
                ("auth_source".into(), "azure_cli".into()),
                ("tenant_id".into(), TENANT.into()),
                ("entra_object_id".into(), OBJECT.into()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        if let Some(path) = cli_path {
            params
                .extra
                .insert("azure_cli_path".into(), path.to_owned());
        }
        let context = serde_json::from_value(
            json!({"protocol_version":1,"connection_id":"fixture","request_id":"request"}),
        )
        .unwrap();
        (params, context)
    }
    #[test]
    fn entra_pool_identity는_tenant_object_user_cli_경로를_분리한다() {
        // given
        let (base, context) = entra_fixture(Some("/opt/azure/az"));
        let settings = PluginSettings::default();
        let original = fingerprint_with_settings(&base, &context, &settings);
        let variants = [
            ("tenant_id", "00000000-0000-0000-0000-0000000000a9"),
            ("entra_object_id", "00000000-0000-0000-0000-0000000000b9"),
            ("azure_cli_path", "/opt/other/az"),
        ]
        .map(|(key, value)| {
            let mut params = base.clone();
            params.extra.insert(key.into(), value.into());
            params
        });
        let mut other_user = base.clone();
        other_user.username = Some("other@example.com".into());

        // when
        let separated = variants
            .iter()
            .chain([&other_user])
            .map(|params| {
                (
                    fingerprint_with_settings(params, &context, &settings),
                    context.identity(params),
                )
            })
            .collect::<Vec<_>>();

        // then
        for (fingerprint, identity) in separated {
            assert_ne!(fingerprint, original);
            assert_ne!(identity, context.identity(&base));
        }
        assert_eq!(context.identity(&base)["kind"], json!("entra_user"));
        assert!(!original.contains("eyJ"));
    }
    #[test]
    fn entra_user는_sql과_integrated와_다른_identity를_가진다() {
        // given
        let (entra, context) = entra_fixture(None);
        let mut integrated = entra.clone();
        integrated.auth_mode = Some("integrated".into());
        integrated.extra.clear();
        integrated.username = None;
        let settings = PluginSettings::default();

        // when
        let first = fingerprint_with_settings(&entra, &context, &settings);
        let second = fingerprint_with_settings(&integrated, &context, &settings);

        // then
        assert_ne!(first, second);
        assert_eq!(context.identity(&integrated)["kind"], json!("integrated"));
    }
    #[test]
    fn entra_manager는_잘못된_설정으로_만들_수_없다() {
        // given
        let (mut params, _) = entra_fixture(None);
        params.extra.remove("tenant_id");

        // when
        let actual = ServiceManager::new(params, PluginSettings::default(), true);

        // then
        assert_eq!(actual.err().unwrap().code, "INVALID_ARGUMENT");
    }
    #[test]
    fn entra가_아닌_manager는_token을_조회하지_않는다() {
        // given
        let params = ConnectionParams {
            host: Some("localhost".into()),
            ..Default::default()
        };

        // when
        let actual = ServiceManager::new(params, PluginSettings::default(), true).unwrap();

        // then
        assert!(actual.entra.is_none());
    }

    mod loopback {
        use super::*;
        use std::{
            io::Read,
            net::TcpListener,
            sync::{
                atomic::{AtomicBool, Ordering},
                Arc,
            },
            time::Duration,
        };

        struct Silent {
            port: u16,
            accepted: Arc<AtomicBool>,
            closed: Arc<AtomicBool>,
        }
        fn silent_server() -> Silent {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let accepted = Arc::new(AtomicBool::new(false));
            let closed = Arc::new(AtomicBool::new(false));
            let (a, c) = (accepted.clone(), closed.clone());
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                a.store(true, Ordering::SeqCst);
                let mut buffer = [0_u8; 4096];
                while stream.read(&mut buffer).is_ok_and(|read| read > 0) {}
                c.store(true, Ordering::SeqCst);
            });
            Silent {
                port,
                accepted,
                closed,
            }
        }
        fn fixture(port: u16) -> (ConnectionParams, DriverContext) {
            let params = ConnectionParams {
                host: Some("127.0.0.1".into()),
                port: Some(port),
                username: Some("user".into()),
                database: crate::models::DatabaseSelection::Single("fixture".into()),
                ssl_mode: Some("disable".into()),
                ..Default::default()
            };
            let context = serde_json::from_value(json!({"protocol_version":1,"connection_id":uuid::Uuid::new_v4().to_string(),"request_id":"login","deadline_ms":60000,"read_only":false,"auth":{"kind":"db_password","password":"synthetic-key","identity":"principal"}})).unwrap();
            (params, context)
        }
        fn on_worker_stack<F, T>(future: F) -> T
        where
            F: std::future::Future<Output = T> + Send + 'static,
            T: Send + 'static,
        {
            let _gate = registry::SHUTDOWN_GATE.read().unwrap();
            std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(future)
                })
                .unwrap()
                .join()
                .unwrap()
        }
        async fn until(flag: &AtomicBool) {
            while !flag.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }

        #[test]
        fn 취소_요청은_physical_login의_tds_cancel_handle을_깨우고_소켓을_닫는다() {
            // given
            let server = silent_server();
            let (params, context) = fixture(server.port);
            let (accepted, closed) = (server.accepted.clone(), server.closed.clone());

            // when
            let (error, closed_by_cancel) = on_worker_stack(async move {
                let manager =
                    ServiceManager::new(params, PluginSettings::default(), false).unwrap();
                let scope = registry::Scope::reserve(&context).unwrap();
                let login = Login {
                    context: context.clone(),
                    deadline: scope.state.deadline,
                    cancel: scope.cancel_handle(),
                    state: scope.state.clone(),
                };
                let canceller = async {
                    until(&accepted).await;
                    registry::cancel(&context.connection_id, &context.request_id)
                };
                let (created, signalled) =
                    tokio::join!(LOGIN.scope(login, manager.create()), canceller);
                assert!(signalled);
                tokio::time::timeout(Duration::from_secs(10), until(&closed))
                    .await
                    .unwrap();
                (created.err().unwrap(), closed.load(Ordering::SeqCst))
            });

            // then
            assert_eq!(error.code, "CANCELLED");
            assert!(closed_by_cancel);
        }

        #[test]
        fn 이미_취소된_요청은_checkout_전에_네트워크_없이_끝난다() {
            // given
            let (params, context) = fixture(1);
            // when
            let error = on_worker_stack(async move {
                let scope = registry::Scope::reserve(&context).unwrap();
                registry::cancel(&context.connection_id, &context.request_id);
                acquire(&params, &context, &scope).await.err().unwrap()
            });
            // then
            assert_eq!(error.code, "CANCELLED");
        }

        #[test]
        fn acquire_도중_취소는_유예_시간_없이_즉시_끝난다() {
            // given
            let server = silent_server();
            let (params, context) = fixture(server.port);
            let accepted = server.accepted.clone();
            // when
            let (error, elapsed) = on_worker_stack(async move {
                let scope = registry::Scope::reserve(&context).unwrap();
                let canceller = async {
                    until(&accepted).await;
                    registry::cancel(&context.connection_id, &context.request_id)
                };
                let started = std::time::Instant::now();
                let (acquired, _) = tokio::join!(acquire(&params, &context, &scope), canceller);
                (acquired.err().unwrap(), started.elapsed())
            });
            // then
            assert_eq!(error.code, "CANCELLED");
            assert!(elapsed < Duration::from_secs(5));
        }
    }

    #[cfg(unix)]
    mod cli {
        use super::*;
        use crate::driver::azure_cli_auth::fake_cli::FakeCli;
        use std::time::Duration;

        fn manager(cli: &FakeCli) -> (ServiceManager, DriverContext) {
            let (params, context) = entra_fixture(Some(&cli.path()));
            let params = crate::connection::resolve_connection_params(&params).unwrap();
            (
                ServiceManager::new(params, PluginSettings::default(), true).unwrap(),
                context,
            )
        }
        fn soon() -> tokio::time::Instant {
            tokio::time::Instant::now() + Duration::from_secs(30)
        }

        #[tokio::test]
        async fn entra_user_service_session은_매_login마다_driver_module로_token을_받는다() {
            // given
            let secret = fake_cli::valid_token();
            let cli = FakeCli::responding_with(&secret);
            let (manager, context) = manager(&cli);

            // when
            let first = manager.login_config(&context, soon(), None).await.unwrap();
            let second = manager.login_config(&context, soon(), None).await.unwrap();

            // then
            assert_eq!(cli.invocations(), 2);
            for config in [first, second] {
                let login = config.to_client_context();
                assert_eq!(login.access_token.as_deref(), Some(secret.as_str()));
                assert!(login.password.is_empty());
                assert!(!login.encryption_options.trust_server_certificate);
            }
            assert!(cli.args().contains(&TENANT.to_owned()));
            assert!(!format!("{:?}", manager.params).contains(&secret));
        }

        #[tokio::test]
        async fn token_실패는_정제된_인증_오류이고_cli_출력을_담지_않는다() {
            // given
            let cli = FakeCli::script("echo 'stderr-canary' >&2; echo 'stdout-canary'; exit 1");
            let (manager, context) = manager(&cli);

            // when
            let error = manager
                .login_config(&context, soon(), None)
                .await
                .err()
                .unwrap();

            // then
            assert_eq!(error.code, "AUTH_REQUIRED");
            assert_eq!(error.message, "Azure CLI token request failed");
            assert!(!format!("{error:?}").contains("canary"));
        }

        #[tokio::test]
        async fn 다른_주체의_token은_사용하지_않고_본문을_노출하지_않는다() {
            // given
            let other = fake_cli::token(fake_cli::claims_with(
                TENANT,
                "00000000-0000-0000-0000-0000000000ff",
                USER,
                fake_cli::unix_now() + 3_600,
            ));
            let cli = FakeCli::responding_with(&other);
            let (manager, context) = manager(&cli);

            // when
            let error = manager
                .login_config(&context, soon(), None)
                .await
                .err()
                .unwrap();

            // then
            assert_eq!(error.code, "AUTH_REQUIRED");
            assert!(!format!("{error:?}").contains(&other));
        }

        #[tokio::test]
        async fn deadline이_지나면_cli_token_조회를_중단한다() {
            // given
            let cli = FakeCli::script("exec sleep 60");
            let (manager, context) = manager(&cli);
            let deadline = tokio::time::Instant::now() + Duration::from_millis(300);

            // when
            let started = std::time::Instant::now();
            let error = manager
                .login_config(&context, deadline, None)
                .await
                .err()
                .unwrap();

            // then
            assert_eq!(error.code, "DEADLINE_EXCEEDED");
            assert!(started.elapsed() < Duration::from_secs(10));
        }

        #[tokio::test]
        async fn 요청을_취소하면_cli_token_조회를_즉시_중단한다() {
            // given
            let cli = FakeCli::script("exec sleep 60");
            let (manager, context) = manager(&cli);
            let scope = registry::Scope::reserve(&context).unwrap();
            let state = scope.state.clone();
            let canceller = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                scope.stop();
                scope
            });

            // when
            let started = std::time::Instant::now();
            let error = manager
                .login_config(&context, soon(), Some(&state))
                .await
                .err()
                .unwrap();
            drop(canceller.await.unwrap());

            // then
            assert_eq!(error.code, "CANCELLED");
            assert!(started.elapsed() < Duration::from_secs(10));
        }
    }
}
