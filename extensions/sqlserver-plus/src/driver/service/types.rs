use super::ServiceError;
use crate::driver::azure_cli_auth::AzureCliAuth;
use crate::models::ConnectionParams;
use serde::Deserialize;
use serde_json::{json, Value};

pub const FRAME_LIMIT: usize = 16 * 1024 * 1024 - 64 * 1024;
pub const SESSION_IDLE_MS: u64 = 300_000;
pub fn valid_rpc_id(id: &Value) -> bool {
    match id {
        Value::Null => true,
        Value::String(value) => value.chars().count() <= 128,
        Value::Number(value) => value.as_f64().is_some_and(|number| {
            number.is_finite() && number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_991.0
        }),
        _ => false,
    }
}
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransientAuth {
    DbPassword { password: String, identity: String },
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverContext {
    pub protocol_version: u32,
    pub connection_id: String,
    pub request_id: String,
    #[serde(default = "deadline")]
    pub deadline_ms: u64,
    #[serde(default = "read_only")]
    pub read_only: bool,
    pub auth: Option<TransientAuth>,
    pub session_handle: Option<String>,
}
fn deadline() -> u64 {
    30_000
}
fn read_only() -> bool {
    true
}
impl DriverContext {
    fn parse_context(params: &Value) -> Result<Self, ServiceError> {
        only_keys(params, &["params", "driver_context", "input"])?;
        if params["driver_context"]["auth"]["kind"]
            .as_str()
            .is_some_and(|kind| kind.starts_with("entra"))
        {
            return Err(ServiceError::argument(
                "Host-supplied Entra tokens are not accepted; the driver acquires its own",
            ));
        }
        let context: Self = serde_json::from_value(params["driver_context"].clone())
            .map_err(|_| ServiceError::argument("A valid driver context is required"))?;
        if context.protocol_version != 1 {
            return Err(ServiceError::new(
                "PROTOCOL_MISMATCH",
                "Unsupported service protocol",
            ));
        }
        if [&context.connection_id, &context.request_id]
            .iter()
            .any(|v| v.is_empty() || v.chars().count() > 128)
            || context.deadline_ms == 0
            || context.deadline_ms > 120_000
        {
            return Err(ServiceError::argument("Invalid driver context"));
        }
        Ok(context)
    }
    pub fn parse_control(params: &Value) -> Result<Self, ServiceError> {
        let context = Self::parse_context(params)?;
        let connection = params["params"]
            .as_object()
            .ok_or_else(|| ServiceError::argument("Connection params must be an object"))?;
        if connection
            .get("connection_id")
            .is_some_and(|id| !id.is_null() && id.as_str() != Some(&context.connection_id))
        {
            return Err(ServiceError::argument(
                "The control connection does not match",
            ));
        }
        Ok(context)
    }
    pub fn parse(params: &Value) -> Result<(Self, ConnectionParams), ServiceError> {
        let context = Self::parse_context(params)?;
        let connection: ConnectionParams = serde_json::from_value(params["params"].clone())
            .map_err(|_| ServiceError::argument("Connection params are required"))?;
        let connection = crate::connection::resolve_connection_params(&connection)
            .map_err(|_| ServiceError::argument("Invalid connection settings"))?;
        if connection.database.primary().trim().is_empty() {
            return Err(ServiceError::argument("An explicit database is required"));
        }
        Ok((context, connection))
    }
    pub fn validate_auth(&self, params: &ConnectionParams) -> Result<(), ServiceError> {
        match (
            params.auth_mode.as_deref().unwrap_or("sql_password"),
            &self.auth,
        ) {
            ("integrated", None) => Ok(()),
            ("sql_password", Some(TransientAuth::DbPassword { password, identity }))
                if !password.is_empty()
                    && !identity.is_empty()
                    && params.username.as_ref().is_some_and(|v| !v.is_empty())
                    && params.password.as_ref().is_none_or(|v| v == password) =>
            {
                Ok(())
            }
            ("entra_user", None) => entra_auth(params).map(|_| ()),
            ("entra_user", Some(_)) => Err(ServiceError::argument(
                "Entra connections must not carry transient authentication",
            )),
            _ => Err(ServiceError::new(
                "AUTH_REQUIRED",
                "A matching transient authentication context is required",
            )),
        }
    }
    pub fn identity(&self, params: &ConnectionParams) -> Value {
        match &self.auth {
            Some(TransientAuth::DbPassword { identity, .. }) => {
                json!({"kind":"db_password","identity":identity})
            }
            None => match entra_auth(params) {
                Ok(Some(auth)) => json!({"kind":"entra_user","identity":auth.identity_parts()}),
                _ => json!({"kind":"integrated"}),
            },
        }
    }
}
/// Driver-owned Entra identity for this connection, if it uses `entra_user`.
pub fn entra_auth(params: &ConnectionParams) -> Result<Option<AzureCliAuth>, ServiceError> {
    AzureCliAuth::from_connection_params(params, &crate::settings::current())
        .map_err(|_| ServiceError::argument("Invalid Entra connection settings"))
}
pub fn only_keys(value: &Value, keys: &[&str]) -> Result<(), ServiceError> {
    let object = value
        .as_object()
        .ok_or_else(|| ServiceError::argument("An object input is required"))?;
    if object.keys().any(|key| !keys.contains(&key.as_str())) {
        return Err(ServiceError::argument("Unknown operation fields"));
    }
    Ok(())
}
pub fn input<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, ServiceError> {
    serde_json::from_value(params["input"].clone())
        .map_err(|_| ServiceError::argument("Invalid operation input"))
}
