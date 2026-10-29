//! Driver-owned Microsoft Entra user authentication through Azure CLI.
//!
//! The driver requests a tenant-scoped Azure SQL access token from the
//! locally signed-in Azure CLI for every new physical connection, for both
//! the legacy pool and the service pool. The host never supplies a token.
//! The payload checks below validate JWT shape and identity metadata only;
//! Azure SQL performs the cryptographic signature validation.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::Value;
use tokio::process::Command;

use crate::connection::resolve_connection_params;
use crate::models::ConnectionParams;
use crate::settings::PluginSettings;

const RESOURCE: &str = "https://database.windows.net/";
const DEFAULT_AZURE_CLI_PATH: &str = "az";
const EXPIRY_SAFETY_SECONDS: u64 = 120;
const MAX_CLI_OUTPUT_BYTES: usize = 64 * 1024;
const TOKEN_REQUEST_FAILED: &str = "Azure CLI token request failed";
const INVALID_RESPONSE: &str = "Azure CLI returned an invalid token response";
const INVALID_TOKEN: &str = "Azure CLI returned an invalid access token";
const DIFFERENT_IDENTITY: &str = "Azure CLI returned a token for a different identity";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Program and leading arguments that launch Azure CLI without a shell.
#[derive(Debug, PartialEq, Eq)]
struct CliCommand {
    program: PathBuf,
    prefix_args: Vec<OsString>,
}

impl CliCommand {
    fn direct(program: PathBuf) -> Self {
        Self {
            program,
            prefix_args: Vec::new(),
        }
    }
}

/// Expected Entra principal and the CLI used to obtain its token.
/// It never holds a token, so its Debug output is safe to log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AzureCliAuth {
    pub tenant_id: String,
    pub entra_object_id: String,
    pub username: String,
    pub azure_cli_path: String,
}

/// A validated access token. Debug output is redacted and the value is only
/// reachable through `expose`, which callers hand to the TDS login.
pub struct AccessToken(String);

impl AccessToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

impl AzureCliAuth {
    /// Returns `Ok(None)` for non-Entra connections and the validated identity
    /// for `auth_mode=entra_user`.
    pub fn from_connection_params(
        params: &ConnectionParams,
        settings: &PluginSettings,
    ) -> Result<Option<Self>, String> {
        let params = resolve_connection_params(params)?;
        if params.auth_mode.as_deref() != Some("entra_user") {
            return Ok(None);
        }
        let source = params.extra.get("auth_source").map(String::as_str);
        if source.map(str::trim) != Some("azure_cli") {
            return Err("unsupported SQL Server auth_source".into());
        }
        if params.integrated_auth {
            return Err(
                "SQL Server Entra authentication cannot be combined with integrated authentication"
                    .into(),
            );
        }
        if params
            .password
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(
                "SQL Server Entra authentication cannot be combined with a password".into(),
            );
        }

        let username = params
            .username
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "SQL Server Entra authentication requires a username".to_string())?;
        let tenant_id = required_extra(&params, "tenant_id")?;
        if !valid_tenant(&tenant_id) {
            return Err("SQL Server Entra authentication has an invalid tenant_id".into());
        }
        let entra_object_id = required_extra(&params, "entra_object_id")?;
        if !is_public_azure_sql_host(params.host.as_deref()) {
            return Err(
                "SQL Server Entra authentication requires an Azure SQL database.windows.net host"
                    .into(),
            );
        }
        if params.ssl_mode.as_deref() != Some("verify-full") {
            return Err("SQL Server Entra authentication requires ssl_mode=verify-full".into());
        }
        if settings.trust_server_certificate {
            return Err(
                "SQL Server Entra authentication requires trust_server_certificate=false".into(),
            );
        }

        let azure_cli_path = match params.extra.get("azure_cli_path") {
            Some(value) if !value.is_empty() && value.trim() == value && is_absolute(value) => {
                value.clone()
            }
            Some(_) => {
                return Err("azure_cli_path must be a nonempty absolute path".into());
            }
            None => DEFAULT_AZURE_CLI_PATH.to_owned(),
        };

        Ok(Some(Self {
            tenant_id,
            entra_object_id,
            username: username.to_owned(),
            azure_cli_path,
        }))
    }

    /// Run Azure CLI without a shell and return a validated token. Output is
    /// captured privately and never surfaced; dropping the future (deadline,
    /// cancellation) kills the child process.
    pub async fn acquire_access_token(&self) -> Result<AccessToken, &'static str> {
        let explicit = (self.azure_cli_path != DEFAULT_AZURE_CLI_PATH)
            .then(|| Path::new(&self.azure_cli_path));
        let cli = resolve_cli_command(explicit).map_err(|_| TOKEN_REQUEST_FAILED)?;
        let output = build_cli_command(&cli)
            .args([
                "account",
                "get-access-token",
                "--resource",
                RESOURCE,
                "--tenant",
            ])
            .arg(&self.tenant_id)
            .args(["--output", "json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|_| TOKEN_REQUEST_FAILED)?;
        if !output.status.success() {
            return Err(TOKEN_REQUEST_FAILED);
        }
        if output.stdout.len() > MAX_CLI_OUTPUT_BYTES {
            return Err(INVALID_RESPONSE);
        }

        let response: Value =
            serde_json::from_slice(&output.stdout).map_err(|_| INVALID_RESPONSE)?;
        let token = response
            .get("accessToken")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or(INVALID_RESPONSE)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "System clock is before the Unix epoch")?
            .as_secs();
        validate_access_token(token, self, now)?;
        Ok(AccessToken(token.to_owned()))
    }

    /// Non-secret identity material for pool keys and fingerprints.
    pub fn identity_parts(&self) -> [&str; 5] {
        [
            "azure_cli",
            &self.tenant_id,
            &self.entra_object_id,
            &self.username,
            &self.azure_cli_path,
        ]
    }
}

fn build_cli_command(cli: &CliCommand) -> Command {
    let mut command = Command::new(&cli.program);
    command.args(&cli.prefix_args);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn resolve_cli_command(explicit_path: Option<&Path>) -> Result<CliCommand, &'static str> {
    #[cfg(windows)]
    {
        let path_dirs = std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default();
        resolve_windows_cli(explicit_path, &path_dirs)
    }
    #[cfg(not(windows))]
    {
        Ok(CliCommand::direct(
            explicit_path
                .unwrap_or_else(|| Path::new(DEFAULT_AZURE_CLI_PATH))
                .to_path_buf(),
        ))
    }
}

/// Windows cannot start `az.cmd` without a shell, so the official launcher is
/// replaced by the Python runtime next to its `wbin` directory.
#[cfg(any(windows, test))]
fn resolve_windows_cli(
    explicit_path: Option<&Path>,
    path_dirs: &[PathBuf],
) -> Result<CliCommand, &'static str> {
    if let Some(path) = explicit_path {
        if !path.is_absolute() {
            return Err("Azure CLI path must be absolute");
        }
        return resolve_explicit_windows_cli(path);
    }

    for directory in path_dirs.iter().filter(|directory| directory.is_absolute()) {
        let executable = directory.join("az.exe");
        if executable.is_file() {
            return Ok(CliCommand::direct(executable));
        }

        let command = directory.join("az.cmd");
        if command.is_file() {
            return resolve_azure_cli_cmd(&command);
        }
    }

    Err("Azure CLI was not found in absolute PATH directories")
}

#[cfg(any(windows, test))]
fn resolve_explicit_windows_cli(path: &Path) -> Result<CliCommand, &'static str> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("exe") => {
            if path.is_file() {
                Ok(CliCommand::direct(path.to_path_buf()))
            } else {
                Err("Azure CLI executable was not found")
            }
        }
        Some(extension) if extension.eq_ignore_ascii_case("cmd") => resolve_azure_cli_cmd(path),
        None => {
            let executable = path.with_extension("exe");
            if executable.is_file() {
                return Ok(CliCommand::direct(executable));
            }
            let command = path.with_extension("cmd");
            if command.is_file() {
                return resolve_azure_cli_cmd(&command);
            }
            Err("Azure CLI executable was not found")
        }
        Some(_) => Err("Azure CLI scripts and batch files are not supported"),
    }
}

#[cfg(any(windows, test))]
fn resolve_azure_cli_cmd(path: &Path) -> Result<CliCommand, &'static str> {
    let is_official_cli = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("az"))
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"));
    if !is_official_cli || !path.is_file() {
        return Err("Only the official Azure CLI az.cmd launcher is supported");
    }

    let python = path
        .parent()
        .and_then(Path::parent)
        .map(|directory| directory.join("python.exe"))
        .filter(|python| python.is_file())
        .ok_or("Azure CLI Python runtime was not found")?;
    Ok(CliCommand {
        program: python,
        prefix_args: vec![OsString::from("-IBm"), OsString::from("azure.cli")],
    })
}

fn is_absolute(value: &str) -> bool {
    Path::new(value).is_absolute()
}

fn is_public_azure_sql_host(host: Option<&str>) -> bool {
    host.filter(|value| value.trim() == *value)
        .and_then(|host| {
            host.to_ascii_lowercase()
                .strip_suffix(".database.windows.net")
                .map(|prefix| !prefix.is_empty())
        })
        .unwrap_or(false)
}

fn valid_tenant(value: &str) -> bool {
    !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

fn required_extra(params: &ConnectionParams, key: &str) -> Result<String, String> {
    params
        .extra
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("SQL Server Entra authentication requires {key}"))
}

fn validate_access_token(token: &str, auth: &AzureCliAuth, now: u64) -> Result<(), &'static str> {
    let mut segments = token.split('.');
    let (Some(header), Some(payload), Some(signature)) =
        (segments.next(), segments.next(), segments.next())
    else {
        return Err(INVALID_TOKEN);
    };
    if header.is_empty() || payload.is_empty() || signature.is_empty() || segments.next().is_some()
    {
        return Err(INVALID_TOKEN);
    }

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .map_err(|_| INVALID_TOKEN)?;
    let claims: Value = serde_json::from_slice(&payload_bytes).map_err(|_| INVALID_TOKEN)?;

    if !claim_string_matches(&claims, "tid", &auth.tenant_id)
        || !claim_string_matches(&claims, "oid", &auth.entra_object_id)
    {
        return Err(DIFFERENT_IDENTITY);
    }
    for key in ["upn", "unique_name", "preferred_username"] {
        if claims.get(key).is_some_and(|value| {
            !value
                .as_str()
                .is_some_and(|claim| claim.eq_ignore_ascii_case(&auth.username))
        }) {
            return Err(DIFFERENT_IDENTITY);
        }
    }

    let audience_matches = claims
        .get("aud")
        .and_then(Value::as_str)
        .is_some_and(|aud| aud == RESOURCE || aud == RESOURCE.trim_end_matches('/'));
    if !audience_matches {
        return Err("Azure CLI returned a token for a different resource");
    }

    let valid_until = now.saturating_add(EXPIRY_SAFETY_SECONDS);
    if claims
        .get("exp")
        .and_then(Value::as_u64)
        .is_none_or(|expires| expires <= valid_until)
    {
        return Err("Azure CLI returned an expired or near-expiry token");
    }

    Ok(())
}

fn claim_string_matches(claims: &Value, key: &str, expected: &str) -> bool {
    claims
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
pub(crate) mod fake_cli;

#[cfg(test)]
mod tests;
