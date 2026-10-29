use super::*;
use crate::driver::azure_cli_auth::fake_cli;
use crate::models::{ConnectionParams, DatabaseSelection};
use crate::settings::PluginSettings;

fn base_params(host: Option<&str>, port: Option<u16>, db: &str) -> ConnectionParams {
    ConnectionParams {
        driver: "sqlserver".into(),
        host: host.map(String::from),
        port,
        username: Some("sa".into()),
        password: Some("Strong!Pass123".into()),
        database: DatabaseSelection::Single(db.into()),
        ..Default::default()
    }
}

#[test]
fn build_config_uses_explicit_host_port() {
    let cfg = build_config(
        &base_params(Some("db.internal"), Some(1445), "master"),
        &PluginSettings::default(),
    )
    .expect("config builds");
    assert_eq!(cfg.get_addr(), "db.internal:1445");
}

#[test]
fn build_config_defaults_host_to_localhost() {
    let cfg = build_config(
        &base_params(None, Some(1433), "master"),
        &PluginSettings::default(),
    )
    .expect("config builds");
    assert_eq!(cfg.get_addr(), "localhost:1433");
}

#[test]
fn build_config_defaults_port_to_1433() {
    let cfg = build_config(
        &base_params(Some("localhost"), None, "master"),
        &PluginSettings::default(),
    )
    .expect("config builds");
    assert_eq!(cfg.get_addr(), "localhost:1433");
}

#[test]
fn build_config_empty_credentials_do_not_panic() {
    let mut params = base_params(Some("localhost"), Some(1433), "master");
    params.username = None;
    params.password = None;
    assert!(build_config(&params, &PluginSettings::default()).is_ok());
}

#[test]
fn build_config_accepts_integrated_auth_without_credentials() {
    let mut params = base_params(Some("localhost"), Some(1433), "master");
    params.username = None;
    params.password = None;
    params.integrated_auth = true;
    assert!(build_config(&params, &PluginSettings::default()).is_ok());
}

#[test]
fn manager_is_clone_send_sync() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    fn assert_clone<T: Clone>() {}
    assert_send::<BridgeManager>();
    assert_sync::<BridgeManager>();
    assert_clone::<BridgeManager>();
}

#[test]
fn manager_new_stores_config() {
    let settings = PluginSettings::default();
    let cfg = build_config(
        &base_params(Some("example.com"), Some(1433), "master"),
        &settings,
    )
    .expect("config builds");
    let mgr = BridgeManager::new(cfg, Some("SET NOCOUNT ON".into()), "prefer", &settings);
    let cloned = mgr.clone();
    let original = format!("{:?}", mgr);
    let cloned_dbg = format!("{:?}", cloned);
    assert_eq!(original, cloned_dbg);
}

#[test]
fn build_config_applies_application_name_and_certificate_override() {
    let settings = PluginSettings {
        application_name: "Tabularis Test".into(),
        trust_server_certificate: true,
        ..PluginSettings::default()
    };
    let mut params = base_params(Some("localhost"), Some(1433), "master");
    params.ssl_mode = Some("verify-full".into());

    let debug = format!(
        "{:?}",
        build_config(&params, &settings).expect("config builds")
    );
    assert!(debug.contains("Tabularis Test"));
    assert!(debug.contains("trust_cert: true"));
}

#[test]
fn manager_snapshots_timeout_settings() {
    let settings = PluginSettings {
        connect_timeout_seconds: 8,
        query_timeout_seconds: 42,
        ..PluginSettings::default()
    };
    let cfg = build_config(
        &base_params(Some("localhost"), Some(1433), "master"),
        &settings,
    )
    .expect("config builds");
    let manager = BridgeManager::new(cfg, None, "prefer", &settings);

    assert_eq!(manager.connect_timeout, Duration::from_secs(8));
    assert_eq!(manager.query_timeout_seconds, Some(42));
}

#[test]
fn build_config_accepts_supported_tls_modes() {
    for mode in [
        "disable",
        "disabled",
        "prefer",
        "preferred",
        "require",
        "required",
        "verify-full",
        "verify_identity",
    ] {
        let mut params = base_params(Some("localhost"), Some(1433), "master");
        params.ssl_mode = Some(mode.into());
        let cfg = build_config(&params, &PluginSettings::default()).expect("config builds");
        assert_eq!(cfg.get_addr(), "localhost:1433");
    }
}

#[test]
fn build_config_rejects_unsupported_tls_inputs() {
    let mut verify_ca = base_params(Some("localhost"), Some(1433), "master");
    verify_ca.ssl_mode = Some("verify_ca".into());
    assert!(build_config(&verify_ca, &PluginSettings::default())
        .unwrap_err()
        .contains("verify-ca"));

    let mut custom_ca = base_params(Some("localhost"), Some(1433), "master");
    custom_ca.ssl_ca = Some("/tmp/ca.pem".into());
    assert!(build_config(&custom_ca, &PluginSettings::default())
        .unwrap_err()
        .contains("custom CA"));

    let mut client_cert = base_params(Some("localhost"), Some(1433), "master");
    client_cert.ssl_cert = Some("/tmp/client.pem".into());
    assert!(build_config(&client_cert, &PluginSettings::default())
        .unwrap_err()
        .contains("client certificates"));
}

fn entra_params(cli_path: Option<&str>) -> ConnectionParams {
    let mut params = base_params(Some(fake_cli::HOST), Some(1433), "fixture");
    params.username = Some(fake_cli::USER.into());
    params.password = None;
    params.ssl_mode = Some("verify-full".into());
    params.extra.insert("auth_mode".into(), "entra_user".into());
    params
        .extra
        .insert("auth_source".into(), "azure_cli".into());
    params
        .extra
        .insert("tenant_id".into(), fake_cli::TENANT.into());
    params
        .extra
        .insert("entra_object_id".into(), fake_cli::OBJECT.into());
    if let Some(path) = cli_path {
        params
            .extra
            .insert("azure_cli_path".into(), path.to_owned());
    }
    params
}

fn entra_manager(cli_path: &str) -> BridgeManager {
    let settings = PluginSettings::default();
    let params = entra_params(Some(cli_path));
    let auth = AzureCliAuth::from_connection_params(&params, &settings)
        .expect("valid entra settings")
        .expect("entra enabled");
    BridgeManager::new(
        build_config(&params, &settings).expect("config builds"),
        None,
        "verify-full",
        &settings,
    )
    .with_azure_cli_auth(auth)
}

#[test]
fn default_manager_keeps_the_existing_sql_auth_config() {
    // given
    let settings = PluginSettings::default();
    let cfg = build_config(
        &base_params(Some("example.com"), Some(1433), "master"),
        &settings,
    )
    .expect("config builds");
    let expected = format!("{cfg:?}");

    // when
    let manager = BridgeManager::new(cfg, None, "prefer", &settings);

    // then
    assert!(manager.azure_cli_auth.is_none());
    assert_eq!(format!("{:?}", manager.config), expected);
}

#[test]
fn entra_config_never_carries_sql_credentials_or_a_token() {
    // given
    let mut params = entra_params(None);
    params.username = Some("user@example.com".into());

    // when
    let context = build_config(&params, &PluginSettings::default())
        .expect("config builds")
        .to_client_context();

    // then
    assert!(context.user_name.is_empty());
    assert!(context.password.is_empty());
    assert_eq!(context.access_token.as_deref(), Some(""));
    assert!(!context.encryption_options.trust_server_certificate);
}

#[test]
fn entra_config_rejects_unverified_tls_and_credential_combinations() {
    // given
    let mut with_password = entra_params(None);
    with_password.password = Some("synthetic-password".into());
    let mut integrated = entra_params(None);
    integrated.integrated_auth = true;
    let mut downgraded = entra_params(None);
    downgraded.ssl_mode = Some("require".into());
    let mut non_azure = entra_params(None);
    non_azure.host = Some("sql.example.com".into());
    let trusting = PluginSettings {
        trust_server_certificate: true,
        ..PluginSettings::default()
    };

    // when
    let results = [with_password, integrated, downgraded, non_azure]
        .map(|params| build_config(&params, &PluginSettings::default()));
    let trusted = build_config(&entra_params(None), &trusting);

    // then
    assert!(results.iter().all(Result::is_err));
    assert!(trusted.is_err());
}

/// An absolute Azure CLI path on every platform, written without backslashes
/// so it appears verbatim in Debug output.
const CLI_PATH: &str = if cfg!(windows) {
    "C:/azure/az"
} else {
    "/opt/azure/az"
};

#[test]
fn entra_manager_debug_output_has_no_token() {
    // given
    let manager = entra_manager(CLI_PATH);

    // when
    let debug = format!("{manager:?}");

    // then
    assert!(debug.contains(CLI_PATH));
    assert!(!debug.contains("eyJ"));
}

#[cfg(unix)]
mod cli {
    use super::*;
    use crate::driver::azure_cli_auth::fake_cli::FakeCli;

    #[tokio::test]
    async fn each_new_physical_connection_acquires_a_token_from_the_driver_module() {
        // given
        let secret = fake_cli::valid_token();
        let cli = FakeCli::responding_with(&secret);
        let manager = entra_manager(&cli.path());

        // when
        let first = manager.connection_config().await.expect("first config");
        let second = manager.connection_config().await.expect("second config");

        // then
        assert_eq!(cli.invocations(), 2);
        assert_eq!(
            first.to_client_context().access_token.as_deref(),
            Some(secret.as_str())
        );
        assert_eq!(
            second.to_client_context().access_token.as_deref(),
            Some(secret.as_str())
        );
        assert!(!format!("{manager:?}").contains(&secret));
        assert_eq!(
            manager.config.to_client_context().access_token.as_deref(),
            Some("")
        );
    }

    #[tokio::test]
    async fn token_failure_fails_the_connection_without_sql_authentication() {
        // given
        let cli = FakeCli::script("echo 'stderr-canary' >&2; exit 1");
        let manager = entra_manager(&cli.path());

        // when
        let error = manager.create().await.err().expect("connection fails");

        // then
        let message = error.to_string();
        assert_eq!(cli.invocations(), 1);
        assert!(
            message.contains("Azure CLI token request failed"),
            "{message}"
        );
        assert!(!message.contains("stderr-canary"));
    }

    #[tokio::test]
    async fn token_acquisition_is_inside_the_connection_deadline() {
        // given
        let cli = FakeCli::script("exec sleep 60");
        let settings = PluginSettings {
            connect_timeout_seconds: 1,
            ..PluginSettings::default()
        };
        let params = entra_params(Some(&cli.path()));
        let auth = AzureCliAuth::from_connection_params(&params, &settings)
            .unwrap()
            .unwrap();
        let manager = BridgeManager::new(
            build_config(&params, &settings).unwrap(),
            None,
            "verify-full",
            &settings,
        )
        .with_azure_cli_auth(auth);

        // when
        let started = std::time::Instant::now();
        let error = manager.create().await.err().expect("connection fails");

        // then
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(error.to_string().contains("timed out after 1 seconds"));
    }
}
