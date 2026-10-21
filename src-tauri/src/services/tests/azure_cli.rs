use super::{
    super::{
        auth::{AuthConfig, AuthManager, OAuthGrant},
        azure_cli,
        backend::{CancellationSignal, TransientAuth},
        contracts::ErrorCode,
        credentials::{Secret, SecretSlot, SecretStore, SessionSecretStore},
        error::{failure, ServiceResult},
        oauth::OAuthProvider,
        storage::now_ms,
    },
    support::*,
};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

fn config() -> AuthConfig {
    let mut saved = entra_connection("entra_user");
    saved.settings["extra"]["auth_source"] = json!("azure_cli");
    saved.settings["extra"]["client_id"] = json!(azure_cli::CLIENT_ID);
    AuthConfig::from_connection(&saved).unwrap().unwrap()
}
fn output(config: &AuthConfig, change: Option<(&str, Value)>) -> Secret {
    let expiry = now_ms() / 1000 + 3600;
    let mut claims = json!({"tid":config.tenant_id,"appid":azure_cli::CLIENT_ID,"aud":config.scope.strip_suffix("/.default").unwrap(),"oid":"11111111-1111-4111-8111-111111111111","exp":expiry,"nbf":0});
    if let Some((key, value)) = change {
        claims[key] = value;
    }
    let token = format!(
        "synthetic.{}.synthetic",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    Secret::new(serde_json::to_vec(&json!({"accessToken":token,"tokenType":"Bearer","tenant":config.tenant_id,"expires_on":expiry})).unwrap()).unwrap()
}

#[test]
fn cli_토큰은_테넌트와_대상에_묶이고_갱신_토큰을_저장하지_않는다() {
    // given
    let config = config();
    let output = output(&config, None);
    // when
    let grant = azure_cli::parse(&config, &output, now_ms()).unwrap();
    // then
    assert_eq!(grant.principal_id, "11111111-1111-4111-8111-111111111111");
    assert!(grant.refresh_token.is_none());
    assert!(!format!("{:?}", grant.access_token).contains("synthetic"));
}

#[test]
fn 다른_테넌트_대상_클라이언트와_잘못된_사용자_토큰은_거부한다() {
    // given
    let config = config();
    let outputs = [
        output(
            &config,
            Some(("tid", json!("44444444-4444-4444-8444-444444444444"))),
        ),
        output(&config, Some(("aud", json!("https://graph.microsoft.com")))),
        output(
            &config,
            Some(("appid", json!("22222222-2222-4222-8222-222222222222"))),
        ),
        output(&config, Some(("oid", json!("invalid")))),
    ];
    // when
    let errors: Vec<_> = outputs
        .iter()
        .map(|output| azure_cli::parse(&config, output, now_ms()).err().unwrap())
        .collect();
    // then
    assert!(errors
        .iter()
        .all(|error| error.code == ErrorCode::AuthRequired));
    assert!(errors
        .iter()
        .all(|error| !error.message.contains("synthetic")));
}

#[test]
fn 만료되거나_아직_유효하지_않은_cli_토큰은_거부한다() {
    // given
    let config = config();
    let now = now_ms();
    let outputs = [
        output(&config, Some(("exp", json!(now / 1000 - 1)))),
        output(&config, Some(("nbf", json!(now / 1000 + 300)))),
    ];
    // when
    let errors: Vec<_> = outputs
        .iter()
        .map(|output| azure_cli::parse(&config, output, now).err().unwrap().code)
        .collect();
    // then
    assert!(errors.iter().all(|code| *code == ErrorCode::AuthExpired));
}

#[test]
fn cli_소스는_사용자_인증과_공식_cli_클라이언트만_허용한다() {
    // given
    let mut wrong_mode = config();
    wrong_mode.mode = "entra_service_principal".into();
    let mut wrong_client = config();
    wrong_client.client_id = "22222222-2222-4222-8222-222222222222".into();
    // when
    let errors = [wrong_mode.validate(), wrong_client.validate()];
    // then
    assert!(errors
        .iter()
        .all(|result| result.as_ref().unwrap_err().code == ErrorCode::InvalidArgument));
}

#[test]
fn cli_연결은_지원_드라이버와_sql의_인증서_검증을_요구한다() {
    // given
    let config = config();
    let mut saved = entra_connection("entra_user");
    saved.settings["extra"]["auth_source"] = json!(config.source);
    saved.settings["extra"]["client_id"] = json!(config.client_id);
    let mut insecure = saved.settings.clone();
    insecure["ssl_mode"] = json!("require");
    // when
    let supported =
        super::super::runtime::validate_connection_configuration("sqlserver", &saved.settings);
    let wrong_driver =
        super::super::runtime::validate_connection_configuration("postgresql", &saved.settings);
    let wrong_tls =
        super::super::runtime::validate_connection_configuration("sqlserver", &insecure);
    // then
    assert!(supported.is_ok());
    assert_eq!(wrong_driver.err().unwrap().code, ErrorCode::InvalidArgument);
    assert_eq!(
        wrong_tls.err().unwrap().code,
        ErrorCode::TlsValidationFailed
    );
}

#[test]
fn cosmos의_공식_리소스_id는_cosmos_토큰에만_허용한다() {
    // given
    let mut cosmos = config();
    cosmos.scope = "https://cosmos.azure.com/.default".into();
    let sql = config();
    let audience = "a232010e-820c-4083-83bb-3ace5fc29d0b";
    let outputs = [
        (
            cosmos.clone(),
            output(&cosmos, Some(("aud", json!(audience)))),
        ),
        (sql.clone(), output(&sql, Some(("aud", json!(audience))))),
    ];
    // when
    let results: Vec<_> = outputs
        .iter()
        .map(|(config, output)| azure_cli::parse(config, output, now_ms()))
        .collect();
    // then
    assert!(results[0].is_ok());
    assert_eq!(
        results[1].as_ref().err().unwrap().code,
        ErrorCode::AuthRequired
    );
}

struct CliOAuth {
    principal: Mutex<String>,
}
#[async_trait]
impl OAuthProvider for CliOAuth {
    async fn azure_cli(&self, config: &AuthConfig) -> ServiceResult<OAuthGrant> {
        azure_cli::parse(
            config,
            &output(
                config,
                Some(("oid", json!(*self.principal.lock().unwrap()))),
            ),
            now_ms(),
        )
    }
    async fn device_begin(
        &self,
        _: &AuthConfig,
    ) -> ServiceResult<oauth2::StandardDeviceAuthorizationResponse> {
        Err(failure(ErrorCode::InteractionRequired))
    }
    async fn device_finish(
        &self,
        _: &AuthConfig,
        _: oauth2::StandardDeviceAuthorizationResponse,
        _: CancellationSignal,
    ) -> ServiceResult<OAuthGrant> {
        Err(failure(ErrorCode::InteractionRequired))
    }
    async fn client_credentials(&self, _: &AuthConfig, _: &Secret) -> ServiceResult<OAuthGrant> {
        Err(failure(ErrorCode::AuthRequired))
    }
    async fn refresh(
        &self,
        _: &AuthConfig,
        _: &Secret,
        _: Option<&str>,
    ) -> ServiceResult<OAuthGrant> {
        Err(failure(ErrorCode::AuthRequired))
    }
}

#[tokio::test]
async fn cli_계정이_바뀌면_이전_연결_인증을_무효화한다() {
    // given
    let config = config();
    let mut saved = entra_connection("entra_user");
    saved.settings["extra"]["auth_source"] = json!(config.source);
    saved.settings["extra"]["client_id"] = json!(config.client_id);
    let provider = Arc::new(CliOAuth {
        principal: Mutex::new("11111111-1111-4111-8111-111111111111".into()),
    });
    let persistent = Arc::new(SessionSecretStore::default());
    let session = Arc::new(SessionSecretStore::default());
    let lifecycle = Arc::new(TestLifecycle(Default::default()));
    let manager = AuthManager::new(
        provider.clone(),
        persistent.clone(),
        session.clone(),
        lifecycle.clone(),
    );
    manager.context(&saved, 30_000).await.unwrap();
    *provider.principal.lock().unwrap() = "22222222-2222-4222-8222-222222222222".into();
    // when
    let result = manager.context(&saved, 30_000).await;
    // then
    assert_eq!(result.err().unwrap().code, ErrorCode::AuthRequired);
    assert_eq!(lifecycle.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(persistent
        .get(&saved.id, SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
    assert!(session
        .get(&saved.id, SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn cli_인증_시작은_기기_코드_없이_공통_인증_작업으로_완료한다() {
    // given
    let config = config();
    let mut saved = entra_connection("entra_user");
    saved.settings["extra"]["auth_source"] = json!(config.source);
    saved.settings["extra"]["client_id"] = json!(config.client_id);
    let manager = AuthManager::new(
        Arc::new(CliOAuth {
            principal: Mutex::new("11111111-1111-4111-8111-111111111111".into()),
        }),
        Arc::new(SessionSecretStore::default()),
        Arc::new(SessionSecretStore::default()),
        Arc::new(TestLifecycle(Default::default())),
    );
    // when
    let started = manager
        .begin(
            &saved,
            "entra_user",
            &caller(),
            super::super::credentials::SecretPersistence::Session,
        )
        .await
        .unwrap();
    tokio::task::yield_now().await;
    let status = manager
        .status(
            &saved.id,
            started["auth_job_id"].as_str().unwrap(),
            &caller(),
        )
        .unwrap();
    // then
    assert_eq!(status["state"], "authenticated");
    assert!(status.get("challenge").is_none());
    assert!(matches!(
        manager.context(&saved, 30_000).await.unwrap(),
        Some(TransientAuth::EntraToken { .. })
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn 실패한_cli의_표준_오류에_있는_비밀은_응답에_노출하지_않는다() {
    // given
    let config = config();
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args(["-c", "printf 'synthetic-secret' >&2; printf '{}'; exit 1"]);
    // when
    let error = azure_cli::execute(&config, command, Duration::from_secs(1))
        .await
        .err()
        .unwrap();
    // then
    assert_eq!(error.code, ErrorCode::AuthRequired);
    assert!(!error.message.contains("synthetic-secret"));
}

#[cfg(unix)]
#[tokio::test]
async fn 응답하지_않는_cli는_제한_시간에_종료한다() {
    // given
    let config = config();
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args(["-c", "exec sleep 30"]);
    // when
    let error = azure_cli::execute(&config, command, Duration::from_millis(50))
        .await
        .err()
        .unwrap();
    // then
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
}

#[cfg(unix)]
#[tokio::test]
async fn 지나치게_큰_cli_응답은_메모리_상한에서_중단한다() {
    // given
    let config = config();
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args(["-c", "exec head -c 70000 /dev/zero"]);
    // when
    let error = azure_cli::execute(&config, command, Duration::from_secs(1))
        .await
        .err()
        .unwrap();
    // then
    assert_eq!(error.code, ErrorCode::ResourceLimit);
}

#[tokio::test]
#[ignore]
async fn 실제_cli에서_sql과_cosmos_토큰을_원문_출력_없이_획득한다() {
    // given
    let mut config = config();
    config.tenant_id = std::env::var("TABULARIS_VERIFY_AZURE_CLI_TENANT_ID").unwrap();
    // when
    let sql = azure_cli::acquire(&config).await;
    config.scope = "https://cosmos.azure.com/.default".into();
    let cosmos = azure_cli::acquire(&config).await;
    // then
    assert!(sql.is_ok());
    assert!(cosmos.is_ok());
    assert!(sql.unwrap().refresh_token.is_none());
    assert!(cosmos.unwrap().refresh_token.is_none());
}
