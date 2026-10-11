use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use mssql_tiberius_bridge::{AuthMethod, Config};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::{timeout_at, Instant};

use crate::models::ConnectionParams;

const SQL_RESOURCE: &str = "https://database.windows.net/";
const MAX_STDOUT_BYTES: usize = 1024 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct AzureCliAuth {
    username: String,
    tenant_id: String,
    object_id: String,
    executable: PathBuf,
}

impl fmt::Debug for AzureCliAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AzureCliAuth")
            .field("identity", &"[redacted]")
            .field("executable", &self.executable)
            .finish()
    }
}

impl AzureCliAuth {
    pub fn from_params(params: &ConnectionParams) -> Result<Option<Self>, String> {
        let mode = params.extra.get("auth_mode").map(String::as_str);
        match mode {
            None => {
                if params.extra.contains_key("auth_source") {
                    return Err("SQL Server auth_source requires auth_mode".into());
                }
                Ok(None)
            }
            Some("entra_user") => {
                let source = params
                    .extra
                    .get("auth_source")
                    .map(String::as_str)
                    .ok_or_else(|| "Entra user authentication requires auth_source".to_owned())?;
                if source != "azure_cli" {
                    return Err("unsupported Entra authentication source".into());
                }

                if params.integrated_auth {
                    return Err(
                        "Entra user authentication cannot use integrated authentication".into(),
                    );
                }
                if params
                    .password
                    .as_deref()
                    .is_some_and(|password| !password.is_empty())
                {
                    return Err("Entra user authentication cannot include a password".into());
                }

                let username = required_identity_field(params.username.as_deref(), "username")?;
                let tenant_id = required_extra_field(params, "tenant_id")?;
                let object_id = required_extra_field(params, "entra_object_id")?;
                let executable = match params.extra.get("azure_cli_path") {
                    Some(path) if !path.trim().is_empty() => {
                        let path = PathBuf::from(path.trim());
                        if !path.is_absolute() {
                            return Err("azure_cli_path must be an absolute path".into());
                        }
                        path
                    }
                    _ => PathBuf::from("az"),
                };

                Ok(Some(Self {
                    username,
                    tenant_id,
                    object_id,
                    executable,
                }))
            }
            Some(_) => Err("unsupported SQL Server authentication mode".into()),
        }
    }

    pub fn validate_connection(
        &self,
        params: &ConnectionParams,
        trust_server_certificate: bool,
    ) -> Result<(), String> {
        if !params.host.as_deref().is_some_and(is_public_azure_sql_host) {
            return Err("Entra user authentication requires a public Azure SQL host".into());
        }
        if params.ssl_mode.as_deref() != Some("verify-full") {
            return Err("Entra user authentication requires ssl_mode 'verify-full'".into());
        }
        if trust_server_certificate {
            return Err("Entra user authentication requires trust_server_certificate=false".into());
        }
        Ok(())
    }

    pub fn identity_key(&self) -> String {
        format!(
            "entra_user:azure_cli:tenant={}:{}:object_id={}:{}:username={}:{}:cli={}:{}",
            self.tenant_id.len(),
            self.tenant_id,
            self.object_id.len(),
            self.object_id,
            self.username.len(),
            self.username,
            self.executable.to_string_lossy().len(),
            self.executable.display()
        )
    }

    pub async fn acquire_access_token(&self, deadline: Instant) -> Result<String, AcquireError> {
        let mut child = Command::new(&self.executable)
            .args([
                "account",
                "get-access-token",
                "--resource",
                SQL_RESOURCE,
                "--tenant",
                self.tenant_id.as_str(),
                "--output",
                "json",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| AcquireError::Failed("Azure CLI could not be started"))?;

        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let output = async {
            let stdout = read_bounded(stdout, MAX_STDOUT_BYTES);
            let stderr = read_bounded(stderr, MAX_STDERR_BYTES);
            let status = child.wait();
            tokio::join!(stdout, stderr, status)
        };

        let (stdout, stderr, status) = match timeout_at(deadline, output).await {
            Ok((Ok(stdout), Ok(stderr), Ok(status))) => (stdout, stderr, status),
            Ok(_) => return Err(AcquireError::Failed("Azure CLI process failed")),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(AcquireError::Timeout);
            }
        };

        if !status.success() {
            return Err(AcquireError::Failed("Azure CLI token acquisition failed"));
        }
        if stdout.1 || stderr.1 {
            return Err(AcquireError::Failed(
                "Azure CLI output exceeded the allowed size",
            ));
        }

        let output: CliTokenOutput = serde_json::from_slice(&stdout.0)
            .map_err(|_| AcquireError::Failed("Azure CLI returned invalid token data"))?;
        validate_access_token(
            &output.access_token,
            &self.tenant_id,
            &self.object_id,
            &self.username,
            chrono::Utc::now().timestamp(),
        )
        .map_err(AcquireError::Failed)?;
        Ok(output.access_token)
    }

    pub fn apply_token(&self, config: &mut Config, token: String) {
        config.authentication(AuthMethod::aad_token(token));
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AcquireError {
    Failed(&'static str),
    Timeout,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliTokenOutput {
    access_token: String,
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut overflow = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let available = limit.saturating_sub(output.len());
        let retained = count.min(available);
        output.extend_from_slice(&buffer[..retained]);
        overflow |= retained < count;
    }
    Ok((output, overflow))
}

pub fn validate_access_token(
    token: &str,
    expected_tenant: &str,
    expected_object_id: &str,
    expected_username: &str,
    now_epoch_seconds: i64,
) -> Result<(), &'static str> {
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("Azure CLI returned an invalid access token");
    };
    if header.is_empty() || payload.is_empty() || signature.is_empty() {
        return Err("Azure CLI returned an invalid access token");
    }

    let header_bytes = URL_SAFE_NO_PAD
        .decode(header)
        .map_err(|_| "Azure CLI returned an invalid access token")?;
    let header: Value = serde_json::from_slice(&header_bytes)
        .map_err(|_| "Azure CLI returned an invalid access token")?;
    let algorithm = header.get("alg").and_then(Value::as_str);
    if !header.is_object()
        || !algorithm.is_some_and(|algorithm| {
            !algorithm.is_empty() && !algorithm.eq_ignore_ascii_case("none")
        })
    {
        return Err("Azure CLI returned an invalid access token");
    }
    URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| "Azure CLI returned an invalid access token")?;

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| "Azure CLI returned an invalid access token")?;
    let claims: Value = serde_json::from_slice(&payload_bytes)
        .map_err(|_| "Azure CLI returned an invalid access token")?;
    let claims = claims
        .as_object()
        .ok_or("Azure CLI returned an invalid access token")?;

    if !claim_matches(claims.get("tid"), expected_tenant) {
        return Err("Azure CLI token tenant does not match the configured identity");
    }
    if !claim_matches(claims.get("oid"), expected_object_id) {
        return Err("Azure CLI token principal does not match the configured identity");
    }
    for name in ["upn", "unique_name", "preferred_username"] {
        if let Some(value) = claims.get(name) {
            if !claim_matches(Some(value), expected_username) {
                return Err("Azure CLI token username does not match the configured identity");
            }
        }
    }
    if !audience_matches(claims.get("aud")) {
        return Err("Azure CLI token audience is not Azure SQL");
    }

    let expiry = claims
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or("Azure CLI token has no valid expiry")?;
    if expiry <= now_epoch_seconds.saturating_add(120) {
        return Err("Azure CLI token expires too soon");
    }
    Ok(())
}

fn claim_matches(value: Option<&Value>, expected: &str) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
}

fn audience_matches(value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(audience)) => audience == SQL_RESOURCE,
        Some(Value::Array(audiences)) => audiences
            .iter()
            .any(|audience| audience.as_str() == Some(SQL_RESOURCE)),
        _ => false,
    }
}

fn required_identity_field(value: Option<&str>, name: &str) -> Result<String, String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("Entra user authentication requires {name}"))
}

fn required_extra_field(params: &ConnectionParams, name: &str) -> Result<String, String> {
    required_identity_field(params.extra.get(name).map(String::as_str), name)
}

fn is_public_azure_sql_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.');
    host.len() > ".database.windows.net".len()
        && host.to_ascii_lowercase().ends_with(".database.windows.net")
}

pub async fn connect_with_auth<T, E, F, Fut>(
    config: &Config,
    auth: Option<&AzureCliAuth>,
    connect_timeout: Duration,
    connector: F,
) -> Result<T, ConnectError<E>>
where
    F: FnOnce(Config) -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let deadline = Instant::now() + connect_timeout;
    let mut connection_config = config.clone();
    if let Some(auth) = auth {
        let token = auth
            .acquire_access_token(deadline)
            .await
            .map_err(|error| match error {
                AcquireError::Failed(message) => ConnectError::Authentication(message),
                AcquireError::Timeout => ConnectError::Timeout,
            })?;
        auth.apply_token(&mut connection_config, token);
    }

    timeout_at(deadline, connector(connection_config))
        .await
        .map_err(|_| ConnectError::Timeout)?
        .map_err(ConnectError::Connect)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConnectError<E> {
    Authentication(&'static str),
    Timeout,
    Connect(E),
}

#[cfg(test)]
mod tests;
