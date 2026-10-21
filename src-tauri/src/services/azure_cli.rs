use super::{
    auth::{AuthConfig, OAuthGrant},
    contracts::ErrorCode,
    credentials::Secret,
    error::{failure, ServiceResult},
    storage::now_ms,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

pub const CLIENT_ID: &str = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";
const COSMOS_AUDIENCE: &str = "a232010e-820c-4083-83bb-3ace5fc29d0b";
const MAX_OUTPUT: usize = 64 * 1024;

pub async fn acquire(config: &AuthConfig) -> ServiceResult<OAuthGrant> {
    config.validate()?;
    let executable = which::which("az").map_err(|_| failure(ErrorCode::CapabilityUnavailable))?;
    let mut command = Command::new(executable);
    command
        .args([
            "account",
            "get-access-token",
            "--tenant",
            &config.tenant_id,
            "--scope",
            &config.scope,
            "--output",
            "json",
            "--only-show-errors",
        ])
        .env("AZURE_LOGGING_ENABLE_LOG_FILE", "false")
        .env("AZURE_CORE_COLLECT_TELEMETRY", "false");
    execute(config, command, Duration::from_secs(30)).await
}

pub(super) async fn execute(
    config: &AuthConfig,
    mut command: Command,
    timeout: Duration,
) -> ServiceResult<OAuthGrant> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| failure(ErrorCode::CapabilityUnavailable))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| failure(ErrorCode::AuthRequired))?
        .take((MAX_OUTPUT + 1) as u64);
    let result = tokio::time::timeout(timeout, async {
        let mut bytes = Vec::new();
        stdout
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| failure(ErrorCode::AuthRequired))?;
        if bytes.len() > MAX_OUTPUT {
            for byte in &mut bytes {
                unsafe {
                    std::ptr::write_volatile(byte, 0);
                }
            }
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let output = Secret::new(bytes).map_err(|_| failure(ErrorCode::AuthRequired))?;
        if !child
            .wait()
            .await
            .map_err(|_| failure(ErrorCode::AuthRequired))?
            .success()
        {
            return Err(failure(ErrorCode::AuthRequired));
        }
        parse(config, &output, now_ms())
    })
    .await
    .map_err(|_| failure(ErrorCode::DeadlineExceeded))?;
    result
}

#[derive(Deserialize)]
struct TokenOutput {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "tokenType")]
    token_type: String,
    tenant: String,
    expires_on: u64,
}

pub(super) fn parse(config: &AuthConfig, output: &Secret, now: u64) -> ServiceResult<OAuthGrant> {
    config.validate()?;
    let token: TokenOutput =
        serde_json::from_str(output.expose()).map_err(|_| failure(ErrorCode::AuthRequired))?;
    let access_token = Secret::new(token.access_token.into_bytes())
        .map_err(|_| failure(ErrorCode::AuthRequired))?;
    if token.token_type != "Bearer" || token.tenant != config.tenant_id {
        return Err(failure(ErrorCode::AuthRequired));
    }
    let parts: Vec<_> = access_token.expose().split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(failure(ErrorCode::AuthRequired));
    }
    let claims: serde_json::Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| failure(ErrorCode::AuthRequired))?,
    )
    .map_err(|_| failure(ErrorCode::AuthRequired))?;
    let principal = claims["oid"]
        .as_str()
        .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
    let audience = config
        .scope
        .strip_suffix("/.default")
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    let expiry = claims["exp"]
        .as_u64()
        .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
    let actual_audience = claims["aud"]
        .as_str()
        .map(|value| value.trim_end_matches('/'));
    let audience_matches = actual_audience == Some(audience)
        || (config.scope == "https://cosmos.azure.com/.default"
            && actual_audience == Some(COSMOS_AUDIENCE));
    if claims["tid"] != config.tenant_id
        || claims["appid"].as_str().or_else(|| claims["azp"].as_str()) != Some(CLIENT_ID)
        || !audience_matches
        || uuid::Uuid::parse_str(principal).is_err()
    {
        return Err(failure(ErrorCode::AuthRequired));
    }
    let expires_at_ms = expiry
        .min(token.expires_on)
        .checked_mul(1000)
        .ok_or_else(|| failure(ErrorCode::AuthExpired))?;
    if expires_at_ms <= now
        || claims["nbf"]
            .as_u64()
            .is_some_and(|value| value > now / 1000 + 60)
    {
        return Err(failure(ErrorCode::AuthExpired));
    }
    Ok(OAuthGrant {
        access_token,
        refresh_token: None,
        expires_at_ms,
        principal_id: principal.into(),
    })
}
