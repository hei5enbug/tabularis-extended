use super::{
    contracts::ErrorCode,
    error::{failure, ServiceResult},
};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn value_hash(value: &Value) -> String {
    digest(&serde_json::to_vec(value).unwrap_or_default())
}

pub fn opaque_id(value: &str) -> ServiceResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(())
}

fn is_secret_name(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['-', '_', ' '], "");
    matches!(
        key.as_str(),
        "password"
            | "dbpassword"
            | "clientsecret"
            | "secret"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "accountkey"
            | "key"
            | "connectionstring"
            | "connectionuri"
            | "privatekey"
            | "certificate"
            | "passphrase"
    ) || [
        "password",
        "clientsecret",
        "accesstoken",
        "refreshtoken",
        "accountkey",
        "privatekey",
        "passphrase",
    ]
    .iter()
    .any(|suffix| key.ends_with(suffix))
}

/// Connection settings that are saved without the user typing them in the
/// app: no secret fields, and no `extra` key that looks like one.
pub fn reject_secret_fields(settings: &Value) -> ServiceResult<()> {
    let Some(object) = settings.as_object() else {
        return Ok(());
    };
    let rejected = || failure(ErrorCode::SecretNotAccepted);
    for (key, value) in object {
        if is_secret_name(key) {
            return Err(rejected());
        }
        if key == "extra" {
            for extra_key in value.as_object().into_iter().flat_map(|extra| extra.keys()) {
                let lower = extra_key.to_ascii_lowercase();
                if ["password", "secret", "token", "key"]
                    .iter()
                    .any(|word| lower.contains(word))
                {
                    return Err(rejected());
                }
            }
        }
    }
    Ok(())
}

const TARGET_KEYS: &[&str] = &[
    "driver",
    "host",
    "port",
    "username",
    "ssl_mode",
    "ssl_ca",
    "ssl_cert",
    "ssl_key",
    "use_iam_auth",
    "enable_cleartext_plugin",
    "ssh_enabled",
    "ssh_connection_id",
    "ssh_host",
    "ssh_port",
    "ssh_user",
    "ssh_key_file",
    "k8s_enabled",
    "k8s_connection_id",
    "k8s_context",
    "k8s_namespace",
    "k8s_resource_type",
    "k8s_resource_name",
    "k8s_port",
    "k8s_kubectl_path",
    "k8s_kubeconfig_path",
    "ssm_enabled",
    "ssm_target",
    "ssm_profile",
    "ssm_region",
    "proxy",
];
const FIXED_EXTRA_KEYS: &[&str] = &[
    "endpoint",
    "auth_mode",
    "auth_source",
    "tenant_id",
    "client_id",
    "trust_server_certificate",
    "integrated",
    "entra_object_id",
    "azure_cli_path",
];

fn blank(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => true,
        Value::String(text) => text.is_empty(),
        Value::Object(object) => object.is_empty(),
        _ => false,
    }
}

/// The parts of connection settings that decide where credentials go.
fn credential_target(settings: &Value) -> serde_json::Map<String, Value> {
    let mut target = serde_json::Map::new();
    let Some(object) = settings.as_object() else {
        return target;
    };
    let mut extra = serde_json::Map::new();
    for (key, value) in object {
        if key == "extra" {
            for (name, value) in value.as_object().into_iter().flatten() {
                if !blank(value) {
                    extra.insert(name.clone(), value.clone());
                }
            }
        } else if FIXED_EXTRA_KEYS.contains(&key.as_str()) {
            if !blank(value) {
                extra.insert(key.clone(), value.clone());
            }
        } else if TARGET_KEYS.contains(&key.as_str()) && !blank(value) {
            target.insert(key.clone(), value.clone());
        }
    }
    if !extra.is_empty() {
        target.insert("extra".into(), Value::Object(extra));
    }
    target
}

/// True when applying `replacement` settings to `current` settings moves the
/// driver, host, port, user, TLS, tunnel, proxy or extra fields.
pub fn credential_target_changed(current: &Value, replacement: &Value) -> bool {
    credential_target(current) != credential_target(replacement)
}

pub fn validate_settings(value: &Value) -> ServiceResult<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_secret_name(key) {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
                validate_settings(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_settings(value)?;
            }
        }
        Value::String(value) if value.len() > 16_384 => {
            return Err(failure(ErrorCode::InvalidArgument))
        }
        _ => {}
    }
    Ok(())
}

pub fn sanitized_settings(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter_map(|(key, value)| {
                    let mut item = serde_json::Map::new();
                    item.insert(key.clone(), Value::Null);
                    validate_settings(&Value::Object(item))
                        .ok()
                        .map(|_| (key.clone(), sanitized_settings(value)))
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(sanitized_settings).collect()),
        Value::String(value)
            if reqwest::Url::parse(value)
                .is_ok_and(|url| !url.username().is_empty() || url.password().is_some()) =>
        {
            Value::Null
        }
        value => value.clone(),
    }
}
