use super::{
    config::{uuid, Config, Principal},
    Error, Result, REQUEST_MS,
};
use serde::{Deserialize, Serialize};

pub const SQL_SCOPE: &str = "https://database.windows.net/.default";
pub const SECRET_BYTES: usize = 128 * 1024;
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Auth {
    DbPassword {
        password: String,
        identity: String,
    },
    EntraToken {
        access_token: String,
        expires_at_ms: u64,
        identity: String,
        tenant_id: String,
        client_id: String,
        principal_id: String,
        scope: String,
    },
}
fn wipe(value: &mut String) {
    let mut bytes = std::mem::take(value).into_bytes();
    bytes.fill(0);
}
impl Drop for Auth {
    fn drop(&mut self) {
        match self {
            Self::DbPassword { password, .. } => wipe(password),
            Self::EntraToken { access_token, .. } => wipe(access_token),
        }
    }
}
impl Auth {
    pub fn secret(&self) -> &str {
        match self {
            Self::DbPassword { password, .. } => password,
            Self::EntraToken { access_token, .. } => access_token,
        }
    }
    pub fn validate(&self, principal: &Principal, now: u64) -> Result<()> {
        if self.secret().is_empty() || self.secret().len() > 64 * 1024 {
            return Err(Error("INVALID_SECRET"));
        }
        match self {
            Self::DbPassword { identity, .. }
                if principal.auth_mode == "sql_password"
                    && !identity.is_empty()
                    && identity.len() <= 512 =>
            {
                Ok(())
            }
            Self::EntraToken {
                expires_at_ms,
                identity,
                tenant_id,
                client_id,
                principal_id,
                scope,
                ..
            } if matches!(
                principal.auth_mode.as_str(),
                "entra_user" | "entra_service_principal"
            ) =>
            {
                if identity.is_empty()
                    || identity.len() > 512
                    || !uuid(tenant_id)
                    || !uuid(client_id)
                    || !uuid(principal_id)
                    || principal.tenant_id.as_ref() != Some(tenant_id)
                    || principal.client_id.as_ref() != Some(client_id)
                    || scope != SQL_SCOPE
                {
                    return Err(Error("AUTH_CONTRACT_MISMATCH"));
                }
                if *expires_at_ms
                    < now
                        .checked_add(REQUEST_MS + 300_000)
                        .ok_or(Error("AUTH_EXPIRED"))?
                {
                    return Err(Error("AUTH_EXPIRED"));
                }
                Ok(())
            }
            _ => Err(Error("AUTH_MODE_MISMATCH")),
        }
    }
    pub fn wire(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub writer: Auth,
    pub reader: Auth,
    pub refresh: Option<Auth>,
}
pub trait SecretReader {
    fn read_bounded(&mut self, cap: usize) -> Result<Vec<u8>>;
}
pub struct StdinSecrets;
impl SecretReader for StdinSecrets {
    fn read_bounded(&mut self, cap: usize) -> Result<Vec<u8>> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error("SECRET_READ_FAILED"))?;
        Ok(bytes)
    }
}
pub fn refresh_matches(old: &Auth, new: &Auth) -> bool {
    match (old, new) {
        (
            Auth::EntraToken {
                access_token: a,
                expires_at_ms: ea,
                identity: ia,
                tenant_id: ta,
                client_id: ca,
                principal_id: pa,
                scope: sa,
            },
            Auth::EntraToken {
                access_token: b,
                expires_at_ms: eb,
                identity: ib,
                tenant_id: tb,
                client_id: cb,
                principal_id: pb,
                scope: sb,
            },
        ) => a != b && ea != eb && ia == ib && ta == tb && ca == cb && pa == pb && sa == sb,
        _ => false,
    }
}
impl Credentials {
    pub fn read(reader: &mut dyn SecretReader, config: &Config, now: u64) -> Result<Self> {
        let mut bytes = reader.read_bounded(SECRET_BYTES)?;
        let parsed = if bytes.len() > SECRET_BYTES || std::str::from_utf8(&bytes).is_err() {
            Err(Error("INVALID_SECRET_INPUT"))
        } else {
            serde_json::from_slice::<Self>(&bytes).map_err(|_| Error("INVALID_SECRET_INPUT"))
        };
        bytes.fill(0);
        let credentials = parsed?;
        credentials.writer.validate(&config.writer, now)?;
        credentials.reader.validate(&config.reader, now)?;
        if config.writer.auth_mode == "sql_password" {
            if credentials.refresh.is_some() {
                return Err(Error("INVALID_REFRESH"));
            }
        } else {
            let refresh = credentials
                .refresh
                .as_ref()
                .ok_or(Error("REFRESH_REQUIRED"))?;
            refresh.validate(&config.writer, now)?;
            if !refresh_matches(&credentials.writer, refresh) {
                return Err(Error("REFRESH_IDENTITY_MISMATCH"));
            }
        }
        Ok(credentials)
    }
    pub fn canaries(&self) -> Vec<Vec<u8>> {
        [&self.writer, &self.reader]
            .into_iter()
            .chain(self.refresh.iter())
            .flat_map(|auth| {
                [
                    auth.secret().as_bytes().to_vec(),
                    serde_json::to_vec(auth.secret()).unwrap_or_default(),
                ]
            })
            .collect()
    }
}
