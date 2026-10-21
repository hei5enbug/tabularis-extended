use super::{
    super::{
        auth::{AuthConfig, AuthManager},
        backend::TransientAuth,
        contracts::ErrorCode,
        credentials::{Secret, SecretSlot, SecretStore, SessionSecretStore},
        dispatch::CallerTransport,
        oauth::{
            AzureOAuthProvider, MicrosoftPrincipalResolver, OAuthHttpBackend, OAuthProvider,
            OAuthTransportError,
        },
        storage::now_ms,
    },
    support::*,
};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{atomic::Ordering, Arc, Mutex},
};

struct MockHttp {
    responses: Mutex<VecDeque<(u16, Value)>>,
    requests: Mutex<Vec<(String, String)>>,
}
impl MockHttp {
    fn new(responses: Vec<(u16, Value)>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(vec![]),
        }
    }
}
#[async_trait]
impl OAuthHttpBackend for MockHttp {
    async fn send(
        &self,
        request: oauth2::HttpRequest,
    ) -> Result<oauth2::HttpResponse, OAuthTransportError> {
        self.requests.lock().unwrap().push((
            request.uri().to_string(),
            String::from_utf8(request.body().clone()).unwrap(),
        ));
        let (status, body) = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(OAuthTransportError)?;
        oauth2::http::Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&body).unwrap())
            .map_err(|_| OAuthTransportError)
    }
}
fn token(config: &AuthConfig, altered: Option<(&str, Value)>, algorithm: &str) -> String {
    let now = now_ms() / 1000;
    let mut claims = json!({"iss":format!("https://login.microsoftonline.com/{}/v2.0",config.tenant_id),"aud":config.client_id,"tid":config.tenant_id,"oid":"11111111-1111-4111-8111-111111111111","sub":"subject","exp":now+3600,"nbf":now-1,"iat":now-1});
    if let Some((key, value)) = altered {
        claims[key] = value;
    }
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"alg":algorithm})).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
        URL_SAFE_NO_PAD.encode(vec![1; 256])
    )
}
fn provider(http: Arc<MockHttp>) -> AzureOAuthProvider {
    AzureOAuthProvider::new(http, Arc::new(MicrosoftPrincipalResolver))
}

#[tokio::test]
async fn 앱_인증은_고정_tenant_endpoint와_resource_scope를_사용한다() {
    // given
    let config = AuthConfig::from_connection(&entra_connection("entra_service_principal"))
        .unwrap()
        .unwrap();
    let http = Arc::new(MockHttp::new(vec![(
        200,
        json!({"access_token":"synthetic-access","token_type":"Bearer","expires_in":3600}),
    )]));
    let oauth = provider(http.clone());
    let secret = Secret::new(b"synthetic-client-secret".to_vec()).unwrap();

    // when
    let actual = oauth.client_credentials(&config, &secret).await.unwrap();

    // then
    assert_eq!(actual.principal_id, config.client_id);
    assert!(http.requests.lock().unwrap()[0].0.starts_with(&format!(
        "https://login.microsoftonline.com/{}/",
        config.tenant_id
    )));
    assert!(http.requests.lock().unwrap()[0]
        .1
        .contains("grant_type=client_credentials"));
    assert!(http.requests.lock().unwrap()[0]
        .1
        .contains("database.windows.net"));
}

#[tokio::test]
async fn device_flow는_id_token으로_안정적인_주체를_확인한다() {
    // given
    let config = AuthConfig::from_connection(&entra_connection("entra_user"))
        .unwrap()
        .unwrap();
    let id_token = token(&config, None, "RS256");
    let http = Arc::new(MockHttp::new(vec![(
        200,
        json!({"access_token":"opaque-not-a-jwt","token_type":"Bearer","expires_in":3600,"refresh_token":"synthetic-refresh","id_token":id_token}),
    )]));
    let oauth = provider(http);
    let device = serde_json::from_value(json!({"device_code":"device","user_code":"code","verification_uri":"https://microsoft.com/devicelogin","expires_in":900,"interval":1})).unwrap();

    // when
    let actual = oauth
        .device_finish(&config, device, Default::default())
        .await
        .unwrap();

    // then
    assert_eq!(actual.principal_id, "11111111-1111-4111-8111-111111111111");
    assert_eq!(actual.access_token.expose(), "opaque-not-a-jwt");
    assert!(actual.refresh_token.is_some());
}

#[tokio::test]
async fn 첫_사용자_응답에_id_token이_없으면_인증_성공을_가장하지_않는다() {
    // given
    let config = AuthConfig::from_connection(&entra_connection("entra_user"))
        .unwrap()
        .unwrap();
    let http = Arc::new(MockHttp::new(vec![(
        200,
        json!({"access_token":"opaque","token_type":"Bearer","expires_in":3600}),
    )]));
    let oauth = provider(http);
    let device = serde_json::from_value(json!({"device_code":"device","user_code":"code","verification_uri":"https://microsoft.com/devicelogin","expires_in":900,"interval":1})).unwrap();

    // when
    let actual = oauth
        .device_finish(&config, device, Default::default())
        .await;

    // then
    assert!(matches!(actual,Err(error) if error.code==ErrorCode::AuthRequired));
}

async fn invalid_token_cases(config: &AuthConfig) -> Vec<ErrorCode> {
    let cases = vec![
        (Some(("aud", json!("wrong"))), "RS256"),
        (Some(("iss", json!("https://wrong.invalid"))), "RS256"),
        (Some(("tid", json!("wrong"))), "RS256"),
        (Some(("exp", json!(0))), "RS256"),
        (Some(("oid", json!("not-a-uuid"))), "RS256"),
        (None, "none"),
    ];
    let mut errors = Vec::new();
    for (altered, algorithm) in cases {
        let http = Arc::new(MockHttp::new(vec![(
            200,
            json!({"access_token":"opaque","token_type":"Bearer","expires_in":3600,"id_token":token(config,altered,algorithm)}),
        )]));
        let device=serde_json::from_value(json!({"device_code":"device","user_code":"code","verification_uri":"https://microsoft.com/devicelogin","expires_in":900,"interval":1})).unwrap();
        errors.push(
            provider(http)
                .device_finish(config, device, Default::default())
                .await
                .err()
                .unwrap()
                .code,
        );
    }
    errors
}

#[tokio::test]
async fn issuer와_audience와_tenant와_만료와_주체와_alg를_검증한다() {
    // given
    let config = AuthConfig::from_connection(&entra_connection("entra_user"))
        .unwrap()
        .unwrap();

    // when
    let actual = invalid_token_cases(&config).await;

    // then
    assert_eq!(actual.len(), 6);
    assert!(actual.iter().all(|code| *code == ErrorCode::AuthRequired));
}

#[tokio::test]
async fn refresh_응답에_id_token이_없으면_보관된_주체_바인딩을_유지한다() {
    // given
    let config = AuthConfig::from_connection(&entra_connection("entra_user"))
        .unwrap()
        .unwrap();
    let http = Arc::new(MockHttp::new(vec![(
        200,
        json!({"access_token":"new-access","token_type":"Bearer","expires_in":3600,"refresh_token":"rotated-refresh"}),
    )]));
    let oauth = provider(http);
    let refresh = Secret::new(b"old-refresh".to_vec()).unwrap();

    // when
    let actual = oauth
        .refresh(
            &config,
            &refresh,
            Some("11111111-1111-4111-8111-111111111111"),
        )
        .await
        .unwrap();

    // then
    assert_eq!(actual.principal_id, "11111111-1111-4111-8111-111111111111");
    assert_eq!(actual.refresh_token.unwrap().expose(), "rotated-refresh");
}

#[tokio::test]
async fn oauth_redirect나_정책_오류는_일반_인증_오류로_반환한다() {
    // given
    let config = AuthConfig::from_connection(&entra_connection("entra_service_principal"))
        .unwrap()
        .unwrap();
    let http = Arc::new(MockHttp::new(vec![(
        302,
        json!({"error":"canary-client-secret-415","error_description":"canary-client-secret-415"}),
    )]));
    let oauth = provider(http.clone());
    let secret = Secret::new(b"synthetic-secret".to_vec()).unwrap();

    // when
    let actual = oauth.client_credentials(&config, &secret).await;

    // then
    assert!(
        matches!(actual,Err(error) if error.code==ErrorCode::AuthRequired && !error.message.contains("canary-client-secret-415"))
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

async fn concurrent_refresh(
    manager: Arc<AuthManager>,
    saved: super::super::backend::SavedConnection,
) -> Vec<Option<TransientAuth>> {
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let manager = manager.clone();
        let saved = saved.clone();
        tasks.spawn(async move { manager.context(&saved, 30000).await.unwrap() });
    }
    let mut results = vec![];
    while let Some(result) = tasks.join_next().await {
        results.push(result.unwrap());
    }
    results
}

#[tokio::test]
async fn 같은_자격_증명의_동시_갱신은_한번만_수행한다() {
    // given
    let saved = entra_connection("entra_user");
    let persistent = Arc::new(SessionSecretStore::default());
    let oauth = Arc::new(TestOAuth::new());
    bound_refresh(
        persistent.as_ref(),
        &saved,
        "11111111-1111-4111-8111-111111111111",
    )
    .await;
    let manager = AuthManager::new(
        oauth.clone(),
        persistent.clone(),
        Arc::new(SessionSecretStore::default()),
        Arc::new(TestLifecycle(Default::default())),
    );

    // when
    let actual = concurrent_refresh(manager, saved).await;

    // then
    assert_eq!(actual.len(), 8);
    assert_eq!(oauth.refreshes.load(Ordering::SeqCst), 1);
    assert!(actual
        .iter()
        .all(|auth| matches!(auth, Some(TransientAuth::EntraToken { .. }))));
    assert!(persistent
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .unwrap()
        .expose()
        .contains("synthetic-refresh-canary-582"));
}

#[tokio::test]
async fn 요청_deadline과_5분_여유보다_짧은_토큰으로_실행하지_않는다() {
    // given
    let saved = entra_connection("entra_service_principal");
    let persistent = Arc::new(SessionSecretStore::default());
    persistent
        .set(
            "conn-a",
            SecretSlot::EntraClientSecret,
            Secret::new(b"synthetic".to_vec()).unwrap(),
        )
        .await
        .unwrap();
    let oauth = Arc::new(TestOAuth::new());
    oauth.short_expiry.store(true, Ordering::SeqCst);
    let manager = AuthManager::new(
        oauth,
        persistent,
        Arc::new(SessionSecretStore::default()),
        Arc::new(TestLifecycle(Default::default())),
    );

    // when
    let actual = manager.context(&saved, 30000).await;

    // then
    assert!(matches!(actual,Err(error) if error.code==ErrorCode::AuthExpired));
}

#[tokio::test]
async fn refresh가_다른_주체를_반환하면_이전_자원을_무효화하고_인증을_거부한다() {
    // given
    let saved = entra_connection("entra_user");
    let persistent = Arc::new(SessionSecretStore::default());
    bound_refresh(
        persistent.as_ref(),
        &saved,
        "44444444-4444-4444-8444-444444444444",
    )
    .await;
    let oauth = Arc::new(TestOAuth::new());
    let lifecycle = Arc::new(TestLifecycle(Default::default()));
    let manager = AuthManager::new(
        oauth,
        persistent,
        Arc::new(SessionSecretStore::default()),
        lifecycle.clone(),
    );

    // when
    let actual = manager.context(&saved, 30000).await;

    // then
    assert!(matches!(actual,Err(error) if error.code==ErrorCode::AuthRequired));
    assert_eq!(lifecycle.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mcp_인증_응답에는_사용자_code와_verification_uri가_없다() {
    // given
    let saved = entra_connection("entra_user");
    let manager = AuthManager::new(
        Arc::new(TestOAuth::new()),
        Arc::new(SessionSecretStore::default()),
        Arc::new(SessionSecretStore::default()),
        Arc::new(TestLifecycle(Default::default())),
    );
    let mut caller = caller();
    caller.transport = CallerTransport::Mcp;

    // when
    let actual = manager
        .begin(
            &saved,
            "entra_user",
            &caller,
            super::super::credentials::SecretPersistence::Keychain,
        )
        .await
        .unwrap();

    // then
    assert!(actual["challenge"].is_null());
    assert!(actual["user_action_required"].as_bool().unwrap());
    assert!(!actual.to_string().contains("TEST-CODE"));
}

#[tokio::test]
async fn 직접_cli_인증_화면에만_사용자_challenge를_표시한다() {
    // given
    let saved = entra_connection("entra_user");
    let manager = AuthManager::new(
        Arc::new(TestOAuth::new()),
        Arc::new(SessionSecretStore::default()),
        Arc::new(SessionSecretStore::default()),
        Arc::new(TestLifecycle(Default::default())),
    );

    // when
    let actual = manager
        .begin(
            &saved,
            "entra_user",
            &caller(),
            super::super::credentials::SecretPersistence::Keychain,
        )
        .await
        .unwrap();

    // then
    assert_eq!(actual["challenge"]["user_code"], "TEST-CODE");
    assert_eq!(
        actual["challenge"]["verification_uri"],
        "https://microsoft.com/devicelogin"
    );
}

#[tokio::test]
async fn 로그아웃은_캐시와_보호된_slot과_pool을_정리한다() {
    // given
    let saved = entra_connection("entra_service_principal");
    let persistent = Arc::new(SessionSecretStore::default());
    persistent
        .set(
            "conn-a",
            SecretSlot::EntraClientSecret,
            Secret::new(b"synthetic".to_vec()).unwrap(),
        )
        .await
        .unwrap();
    let lifecycle = Arc::new(TestLifecycle(Default::default()));
    let manager = AuthManager::new(
        Arc::new(TestOAuth::new()),
        persistent.clone(),
        Arc::new(SessionSecretStore::default()),
        lifecycle.clone(),
    );
    manager.context(&saved, 30000).await.unwrap();

    // when
    manager.logout("conn-a").await.unwrap();

    // then
    assert!(persistent
        .get("conn-a", SecretSlot::EntraClientSecret)
        .await
        .unwrap()
        .is_none());
    assert!(persistent
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
    assert_eq!(lifecycle.0.load(Ordering::SeqCst), 1);
}

#[test]
fn 암묵적인_tenant나_client_id나_다른_resource_scope는_거부한다() {
    // given
    let mut config = AuthConfig::from_connection(&entra_connection("entra_user"))
        .unwrap()
        .unwrap();
    config.tenant_id = "common".into();
    config.scope = "https://graph.microsoft.com/.default".into();

    // when
    let actual = config.validate();

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn 보호된_refresh_import는_issuer_id_token으로_주체를_확인하고_host_바인딩을_저장한다() {
    // given
    let mut saved = entra_connection("entra_user");
    saved.auth_revision = "rotated-import-revision".into();
    let config = AuthConfig::from_connection(&saved).unwrap().unwrap();
    let http = Arc::new(MockHttp::new(vec![(
        200,
        json!({"access_token":"opaque-import-access","token_type":"Bearer","expires_in":3600,"refresh_token":"rotated-import-refresh","id_token":token(&config,None,"RS256")}),
    )]));
    let persistent = Arc::new(SessionSecretStore::default());
    let session = Arc::new(SessionSecretStore::default());
    let manager = AuthManager::new(
        Arc::new(provider(http.clone())),
        persistent.clone(),
        session.clone(),
        Arc::new(TestLifecycle(Default::default())),
    );

    // when
    let actual = manager
        .import_refresh(
            &saved,
            Secret::new(b"raw-protected-refresh".to_vec()).unwrap(),
            super::super::credentials::SecretPersistence::Session,
            30000,
        )
        .await;

    // then
    assert!(actual.is_ok());
    assert!(persistent
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
    assert!(session
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_some_and(
            |secret| serde_json::from_str::<Value>(secret.expose()).is_ok_and(|record| record
                ["credential_identity"]
                == config.credential_identity
                && record["principal_id"] == "11111111-1111-4111-8111-111111111111"
                && record["refresh_token"] == "rotated-import-refresh")
        ));
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    assert!(http.requests.lock().unwrap()[0]
        .1
        .contains("grant_type=refresh_token"));
}

#[tokio::test]
async fn 첫_refresh_import에_id_token이_없으면_아무_slot도_저장하지_않는다() {
    // given
    let saved = entra_connection("entra_user");
    let http = Arc::new(MockHttp::new(vec![(
        200,
        json!({"access_token":"opaque","token_type":"Bearer","expires_in":3600}),
    )]));
    let persistent = Arc::new(SessionSecretStore::default());
    let session = Arc::new(SessionSecretStore::default());
    let manager = AuthManager::new(
        Arc::new(provider(http)),
        persistent.clone(),
        session.clone(),
        Arc::new(TestLifecycle(Default::default())),
    );

    // when
    let actual = manager
        .import_refresh(
            &saved,
            Secret::new(b"raw-protected-refresh".to_vec()).unwrap(),
            super::super::credentials::SecretPersistence::Session,
            30000,
        )
        .await;

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::AuthRequired);
    assert!(persistent
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
    assert!(session
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn 사용자_refresh_binding_json은_issuer에_보내거나_저장하지_않는다() {
    // given
    let saved = entra_connection("entra_user");
    let http = Arc::new(MockHttp::new(vec![]));
    let persistent = Arc::new(SessionSecretStore::default());
    let session = Arc::new(SessionSecretStore::default());
    let manager = AuthManager::new(
        Arc::new(provider(http.clone())),
        persistent.clone(),
        session.clone(),
        Arc::new(TestLifecycle(Default::default())),
    );
    let forged = Secret::new(
        serde_json::to_vec(
            &json!({"refresh_token":"raw","principal_id":"11111111-1111-4111-8111-111111111111"}),
        )
        .unwrap(),
    )
    .unwrap();

    // when
    let actual = manager
        .import_refresh(
            &saved,
            forged,
            super::super::credentials::SecretPersistence::Keychain,
            30000,
        )
        .await;

    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(http.requests.lock().unwrap().len(), 0);
    assert!(persistent
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
    assert!(session
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
}

async fn device_session_login(
    manager: &Arc<AuthManager>,
    saved: &super::super::backend::SavedConnection,
) -> Value {
    let started = manager
        .begin(
            saved,
            "entra_user",
            &caller(),
            super::super::credentials::SecretPersistence::Session,
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status = manager
                .status(
                    &saved.id,
                    started["auth_job_id"].as_str().unwrap(),
                    &caller(),
                )
                .unwrap();
            if status["state"] == "authenticated" || status["state"] == "failed" {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn device_login의_session_선택은_keychain에_비밀을_저장하지_않는다() {
    // given
    let saved = entra_connection("entra_user");
    let persistent = Arc::new(SessionSecretStore::default());
    let session = Arc::new(SessionSecretStore::default());
    let manager = AuthManager::new(
        Arc::new(TestOAuth::new()),
        persistent.clone(),
        session.clone(),
        Arc::new(TestLifecycle(Default::default())),
    );

    // when
    let actual = device_session_login(&manager, &saved).await;

    // then
    assert_eq!(actual["state"], "authenticated");
    assert!(persistent
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_none());
    assert!(session
        .get("conn-a", SecretSlot::EntraRefreshToken)
        .await
        .unwrap()
        .is_some());
}
