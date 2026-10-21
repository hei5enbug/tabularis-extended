use super::{
    auth::{AuthConfig, OAuthGrant},
    backend::CancellationSignal,
    contracts::ErrorCode,
    credentials::Secret,
    error::{failure, ServiceResult},
    storage::now_ms,
};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use oauth2::{
    basic::{
        BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse,
        BasicTokenType,
    },
    AsyncHttpClient, AuthType, Client, ClientId, ClientSecret, DeviceAuthorizationUrl,
    EndpointNotSet, EndpointSet, ExtraTokenFields, HttpRequest, HttpResponse, RefreshToken, Scope,
    StandardDeviceAuthorizationResponse, StandardRevocableToken, StandardTokenResponse,
    TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

#[derive(Debug)]
pub struct OAuthTransportError;
impl fmt::Display for OAuthTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("The OAuth HTTP request failed.")
    }
}
impl std::error::Error for OAuthTransportError {}

#[async_trait]
pub trait OAuthHttpBackend: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, OAuthTransportError>;
}

struct OAuthHttpClient {
    backend: Arc<dyn OAuthHttpBackend>,
}
impl<'a> AsyncHttpClient<'a> for OAuthHttpClient {
    type Error = OAuthTransportError;
    type Future =
        Pin<Box<dyn Future<Output = Result<HttpResponse, OAuthTransportError>> + Send + 'a>>;
    fn call(&'a self, request: HttpRequest) -> Self::Future {
        let backend = self.backend.clone();
        Box::pin(async move { backend.send(request).await })
    }
}

pub struct ReqwestOAuthHttp {
    client: reqwest::Client,
}
impl ReqwestOAuthHttp {
    pub fn new() -> ServiceResult<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|_| failure(ErrorCode::AuthRequired))?,
        })
    }
}
#[async_trait]
impl OAuthHttpBackend for ReqwestOAuthHttp {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, OAuthTransportError> {
        if request.uri().scheme_str() != Some("https")
            || request.uri().host() != Some("login.microsoftonline.com")
        {
            return Err(OAuthTransportError);
        }
        let mut response = self
            .client
            .request(
                reqwest::Method::from_bytes(request.method().as_str().as_bytes())
                    .map_err(|_| OAuthTransportError)?,
                request.uri().to_string(),
            )
            .headers(request.headers().clone())
            .body(request.body().clone())
            .send()
            .await
            .map_err(|_| OAuthTransportError)?;
        let mut builder = oauth2::http::Response::builder().status(response.status());
        if let Some(headers) = builder.headers_mut() {
            *headers = response.headers().clone();
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| OAuthTransportError)? {
            if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
                return Err(OAuthTransportError);
            }
            bytes.extend_from_slice(&chunk);
        }
        builder.body(bytes).map_err(|_| OAuthTransportError)
    }
}

#[async_trait]
pub trait PrincipalResolver: Send + Sync {
    async fn resolve(
        &self,
        config: &AuthConfig,
        id_token: Option<&Secret>,
        previous: Option<&str>,
    ) -> ServiceResult<String>;
}

pub struct MicrosoftPrincipalResolver;
#[async_trait]
impl PrincipalResolver for MicrosoftPrincipalResolver {
    async fn resolve(
        &self,
        config: &AuthConfig,
        id_token: Option<&Secret>,
        previous: Option<&str>,
    ) -> ServiceResult<String> {
        if config.mode == "entra_service_principal" {
            return Ok(config.client_id.clone());
        }
        match id_token {
            Some(token) => validate_id_token(config, token),
            None => previous
                .filter(|principal| uuid::Uuid::parse_str(principal).is_ok())
                .map(str::to_owned)
                .ok_or_else(|| failure(ErrorCode::AuthRequired)),
        }
    }
}

fn validate_id_token(config: &AuthConfig, token: &Secret) -> ServiceResult<String> {
    let parts: Vec<_> = token.expose().split('.').collect();
    if token.expose().len() > 64 * 1024
        || parts.len() != 3
        || parts.iter().any(|part| part.is_empty())
    {
        return Err(failure(ErrorCode::AuthRequired));
    }
    let header: serde_json::Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(parts[0])
            .map_err(|_| failure(ErrorCode::AuthRequired))?,
    )
    .map_err(|_| failure(ErrorCode::AuthRequired))?;
    if header["alg"] != "RS256"
        || URL_SAFE_NO_PAD
            .decode(parts[2])
            .map_err(|_| failure(ErrorCode::AuthRequired))?
            .len()
            < 128
    {
        return Err(failure(ErrorCode::AuthRequired));
    }
    let claims: serde_json::Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| failure(ErrorCode::AuthRequired))?,
    )
    .map_err(|_| failure(ErrorCode::AuthRequired))?;
    let now = now_ms() / 1000;
    let issuer = format!(
        "https://login.microsoftonline.com/{}/v2.0",
        config.tenant_id
    );
    let expiry = claims["exp"]
        .as_u64()
        .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
    let not_before = claims["nbf"]
        .as_u64()
        .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
    let issued = claims["iat"]
        .as_u64()
        .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
    let principal = claims["oid"]
        .as_str()
        .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
    if claims["iss"] != issuer
        || claims["aud"] != config.client_id
        || claims["tid"] != config.tenant_id
        || expiry <= now
        || not_before > now.saturating_add(60)
        || issued > now.saturating_add(60)
        || issued >= expiry
        || not_before >= expiry
        || claims["sub"].as_str().map_or(true, str::is_empty)
        || uuid::Uuid::parse_str(principal).is_err()
    {
        return Err(failure(ErrorCode::AuthRequired));
    }
    Ok(principal.into())
}

#[async_trait]
pub trait OAuthProvider: Send + Sync {
    async fn azure_cli(&self, _config: &AuthConfig) -> ServiceResult<OAuthGrant> {
        Err(failure(ErrorCode::CapabilityUnavailable))
    }
    async fn device_begin(
        &self,
        config: &AuthConfig,
    ) -> ServiceResult<StandardDeviceAuthorizationResponse>;
    async fn device_finish(
        &self,
        config: &AuthConfig,
        device: StandardDeviceAuthorizationResponse,
        signal: CancellationSignal,
    ) -> ServiceResult<OAuthGrant>;
    async fn client_credentials(
        &self,
        config: &AuthConfig,
        secret: &Secret,
    ) -> ServiceResult<OAuthGrant>;
    async fn refresh(
        &self,
        config: &AuthConfig,
        refresh: &Secret,
        previous_principal: Option<&str>,
    ) -> ServiceResult<OAuthGrant>;
}

pub struct AzureOAuthProvider {
    http: Arc<dyn OAuthHttpBackend>,
    principal: Arc<dyn PrincipalResolver>,
}
impl AzureOAuthProvider {
    pub fn new(http: Arc<dyn OAuthHttpBackend>, principal: Arc<dyn PrincipalResolver>) -> Self {
        Self { http, principal }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct EntraExtraFields {
    #[serde(default)]
    id_token: Option<String>,
}
impl fmt::Debug for EntraExtraFields {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EntraExtraFields([redacted])")
    }
}
impl ExtraTokenFields for EntraExtraFields {}
type EntraTokenResponse = StandardTokenResponse<EntraExtraFields, BasicTokenType>;
type EntraClient = Client<
    BasicErrorResponse,
    EntraTokenResponse,
    BasicTokenIntrospectionResponse,
    StandardRevocableToken,
    BasicRevocationErrorResponse,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
>;

fn client(config: &AuthConfig) -> ServiceResult<EntraClient> {
    Ok(Client::<
        BasicErrorResponse,
        EntraTokenResponse,
        BasicTokenIntrospectionResponse,
        StandardRevocableToken,
        BasicRevocationErrorResponse,
    >::new(ClientId::new(config.client_id.clone()))
    .set_auth_type(AuthType::RequestBody)
    .set_token_uri(
        TokenUrl::new(format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            config.tenant_id
        ))
        .map_err(|_| failure(ErrorCode::InvalidArgument))?,
    ))
}

impl AzureOAuthProvider {
    async fn grant(
        &self,
        config: &AuthConfig,
        token: EntraTokenResponse,
        previous: Option<&str>,
    ) -> ServiceResult<OAuthGrant> {
        let expiry = token
            .expires_in()
            .ok_or_else(|| failure(ErrorCode::AuthExpired))?;
        let access_token = Secret::new(token.access_token().secret().as_bytes().to_vec())?;
        let id_token = token
            .extra_fields()
            .id_token
            .as_ref()
            .map(|token| Secret::new(token.as_bytes().to_vec()))
            .transpose()?;
        let principal_id = self
            .principal
            .resolve(config, id_token.as_ref(), previous)
            .await?;
        if principal_id.is_empty() || principal_id.len() > 256 {
            return Err(failure(ErrorCode::AuthRequired));
        }
        Ok(OAuthGrant {
            access_token,
            refresh_token: token
                .refresh_token()
                .map(|token| Secret::new(token.secret().as_bytes().to_vec()))
                .transpose()?,
            expires_at_ms: now_ms().saturating_add(expiry.as_millis().min(u64::MAX as u128) as u64),
            principal_id,
        })
    }
}

#[async_trait]
impl OAuthProvider for AzureOAuthProvider {
    async fn azure_cli(&self, config: &AuthConfig) -> ServiceResult<OAuthGrant> {
        super::azure_cli::acquire(config).await
    }
    async fn device_begin(
        &self,
        config: &AuthConfig,
    ) -> ServiceResult<StandardDeviceAuthorizationResponse> {
        config.validate()?;
        let client = client(config)?.set_device_authorization_url(
            DeviceAuthorizationUrl::new(format!(
                "https://login.microsoftonline.com/{}/oauth2/v2.0/devicecode",
                config.tenant_id
            ))
            .map_err(|_| failure(ErrorCode::InvalidArgument))?,
        );
        let http = OAuthHttpClient {
            backend: self.http.clone(),
        };
        client
            .exchange_device_code()
            .add_scope(Scope::new(config.scope.clone()))
            .add_scope(Scope::new("openid".into()))
            .add_scope(Scope::new("profile".into()))
            .add_scope(Scope::new("offline_access".into()))
            .request_async(&http)
            .await
            .map_err(|_| failure(ErrorCode::InteractionRequired))
    }
    async fn device_finish(
        &self,
        config: &AuthConfig,
        device: StandardDeviceAuthorizationResponse,
        signal: CancellationSignal,
    ) -> ServiceResult<OAuthGrant> {
        config.validate()?;
        let client = client(config)?;
        let http = OAuthHttpClient {
            backend: self.http.clone(),
        };
        let token = tokio::select! {
            _ = signal.cancelled() => return Err(failure(ErrorCode::Cancelled)),
            token = client.exchange_device_access_token(&device).request_async(&http, tokio::time::sleep, Some(Duration::from_secs(15 * 60))) => token.map_err(|_| failure(ErrorCode::InteractionRequired))?,
        };
        self.grant(config, token, None).await
    }
    async fn client_credentials(
        &self,
        config: &AuthConfig,
        secret: &Secret,
    ) -> ServiceResult<OAuthGrant> {
        config.validate()?;
        let client = client(config)?.set_client_secret(ClientSecret::new(secret.expose().into()));
        let http = OAuthHttpClient {
            backend: self.http.clone(),
        };
        let token = client
            .exchange_client_credentials()
            .add_scope(Scope::new(config.scope.clone()))
            .request_async(&http)
            .await
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        self.grant(config, token, None).await
    }
    async fn refresh(
        &self,
        config: &AuthConfig,
        refresh: &Secret,
        previous_principal: Option<&str>,
    ) -> ServiceResult<OAuthGrant> {
        config.validate()?;
        let client = client(config)?;
        let http = OAuthHttpClient {
            backend: self.http.clone(),
        };
        let refresh = RefreshToken::new(refresh.expose().into());
        let token = client
            .exchange_refresh_token(&refresh)
            .add_scope(Scope::new(config.scope.clone()))
            .add_scope(Scope::new("openid".into()))
            .add_scope(Scope::new("profile".into()))
            .add_scope(Scope::new("offline_access".into()))
            .request_async(&http)
            .await
            .map_err(|_| failure(ErrorCode::AuthExpired))?;
        self.grant(config, token, previous_principal).await
    }
}
