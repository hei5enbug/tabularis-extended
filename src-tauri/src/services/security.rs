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

pub fn validate_settings(value: &Value) -> ServiceResult<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase().replace(['-', '_', ' '], "");
                if matches!(
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
                {
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
