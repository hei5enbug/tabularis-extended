use super::{
    super::{
        auth::{AuthConfig, AuthManager, OAuthGrant},
        azure_cli,
        backend::{CancellationSignal, TransientAuth},
        contracts::ErrorCode,
        credentials::{Secret, SessionSecretStore},
        error::{failure, ServiceResult},
        oauth::OAuthProvider,
        runtime::validate_connection_configuration,
    },
    support::*,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const TENANT: &str = "22222222-2222-4222-8222-222222222222";
const OBJECT: &str = "44444444-4444-4444-8444-444444444444";

fn plus_settings() -> Value {
    json!({
        "host": "fixture.database.windows.net",
        "database": "fixture",
        "ssl_mode": "verify-full",
        "username": "user@example.invalid",
        "extra": {
            "auth_mode": "entra_user",
            "auth_source": "azure_cli",
            "tenant_id": TENANT,
            "entra_object_id": OBJECT,
            "azure_cli_path": "/opt/azure/az"
        }
    })
}

fn without(key: &str) -> Value {
    let mut settings = plus_settings();
    settings["extra"].as_object_mut().unwrap().remove(key);
    settings
}

#[derive(Default)]
struct CountingOAuth(AtomicUsize);

#[async_trait]
impl OAuthProvider for CountingOAuth {
    async fn azure_cli(&self, _: &AuthConfig) -> ServiceResult<OAuthGrant> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn device_begin(
        &self,
        _: &AuthConfig,
    ) -> ServiceResult<oauth2::StandardDeviceAuthorizationResponse> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn device_finish(
        &self,
        _: &AuthConfig,
        _: oauth2::StandardDeviceAuthorizationResponse,
        _: CancellationSignal,
    ) -> ServiceResult<OAuthGrant> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn client_credentials(&self, _: &AuthConfig, _: &Secret) -> ServiceResult<OAuthGrant> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn refresh(
        &self,
        _: &AuthConfig,
        _: &Secret,
        _: Option<&str>,
    ) -> ServiceResult<OAuthGrant> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(failure(ErrorCode::AuthRequired))
    }
}

#[test]
fn sqlserver_plus는_client_id_없이_드라이버가_소유한_entra_필드를_허용한다() {
    // given
    let settings = plus_settings();
    let without_path = without("azure_cli_path");
    // when
    let accepted = validate_connection_configuration("sqlserver-plus", &settings);
    let optional_path = validate_connection_configuration("sqlserver-plus", &without_path);
    let host_owned = validate_connection_configuration("sqlserver", &settings);
    // then
    assert!(accepted.is_ok());
    assert!(optional_path.is_ok());
    assert_eq!(host_owned.err().unwrap().code, ErrorCode::InvalidArgument);
}

#[test]
fn sqlserver_plus는_빠졌거나_잘못된_entra_필드를_거부한다() {
    // given
    let mut relative_path = plus_settings();
    relative_path["extra"]["azure_cli_path"] = json!("az");
    let mut oauth_source = plus_settings();
    oauth_source["extra"]["auth_source"] = json!("oauth");
    let mut service_principal = plus_settings();
    service_principal["extra"]["auth_mode"] = json!("entra_service_principal");
    let mut no_user = plus_settings();
    no_user.as_object_mut().unwrap().remove("username");
    let mut bad_tenant = plus_settings();
    bad_tenant["extra"]["tenant_id"] = json!("--tenant");
    let mut orphan = plus_settings();
    orphan["extra"]["auth_mode"] = json!("sql_password");
    let cases = [
        without("auth_source"),
        without("tenant_id"),
        without("entra_object_id"),
        relative_path,
        oauth_source,
        service_principal,
        no_user,
        bad_tenant,
        orphan,
    ];
    // when
    let results: Vec<_> = cases
        .iter()
        .map(|settings| validate_connection_configuration("sqlserver-plus", settings))
        .collect();
    // then
    assert!(results
        .iter()
        .all(|result| result.as_ref().err().unwrap().code == ErrorCode::InvalidArgument));
}

#[test]
fn sqlserver_plus의_azure_sql_연결도_인증서_검증을_요구한다() {
    // given
    let mut insecure = plus_settings();
    insecure["ssl_mode"] = json!("require");
    // when
    let actual = validate_connection_configuration("sqlserver-plus", &insecure);
    // then
    assert_eq!(actual.err().unwrap().code, ErrorCode::TlsValidationFailed);
}

#[test]
fn cosmos의_entra_cli_검증은_client_id를_계속_요구한다() {
    // given
    let cosmos = |client: Option<&str>| {
        let mut extra = json!({
            "auth_mode": "entra_user",
            "auth_source": "azure_cli",
            "tenant_id": TENANT,
            "endpoint": "https://account.documents.azure.com/"
        });
        if let Some(client) = client {
            extra["client_id"] = json!(client);
        }
        json!({ "extra": extra })
    };
    // when
    let accepted =
        validate_connection_configuration("cosmos-nosql", &cosmos(Some(azure_cli::CLIENT_ID)));
    let missing = validate_connection_configuration("cosmos-nosql", &cosmos(None));
    let foreign = validate_connection_configuration(
        "cosmos-nosql",
        &cosmos(Some("33333333-3333-4333-8333-333333333333")),
    );
    // then
    assert!(accepted.is_ok());
    assert_eq!(missing.err().unwrap().code, ErrorCode::InvalidArgument);
    assert_eq!(foreign.err().unwrap().code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn 호스트는_sqlserver_plus에_entra_토큰을_얻거나_전달하지_않는다() {
    // given
    let mut saved = connection("conn-plus");
    saved.driver = "sqlserver-plus".into();
    saved.settings = plus_settings();
    let provider = Arc::new(CountingOAuth::default());
    let manager = AuthManager::new(
        provider.clone(),
        Arc::new(SessionSecretStore::default()),
        Arc::new(SessionSecretStore::default()),
        Arc::new(TestLifecycle(Default::default())),
    );
    // when
    let config = AuthConfig::from_connection(&saved).unwrap();
    let context = manager.context(&saved, 30_000).await.unwrap();
    let begin = manager
        .begin(
            &saved,
            "entra_user",
            &caller(),
            super::super::credentials::SecretPersistence::Session,
        )
        .await;
    // then
    assert!(config.is_none());
    assert!(!matches!(context, Some(TransientAuth::EntraToken { .. })));
    assert!(context.is_none());
    assert_eq!(begin.err().unwrap().code, ErrorCode::UnsupportedOperation);
    assert_eq!(provider.0.load(Ordering::SeqCst), 0);
}

#[test]
fn sqlserver_plus_설정은_driver_extra_키를_client_id_없이_정규화한다() {
    // given
    let settings = plus_settings();
    // when
    let secure = super::super::security::validate_settings(&settings);
    let params = crate::service_platform::checked_params_for_tests("sqlserver-plus", &settings);
    // then
    assert!(secure.is_ok());
    let params = params.unwrap();
    assert_eq!(params.username.as_deref(), Some("user@example.invalid"));
    assert_eq!(params.extra["entra_object_id"], OBJECT);
    assert_eq!(params.extra["azure_cli_path"], "/opt/azure/az");
    assert_eq!(params.extra["auth_source"], "azure_cli");
    assert!(!params.extra.contains_key("client_id"));
}
