use super::{
    config::{Config, Principal},
    Error, Result,
};
use serde::{Deserialize, Serialize};

pub const SECRET_BYTES: usize = 128 * 1024;
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Auth {
    DbPassword { password: String, identity: String },
}
fn wipe(value: &mut String) {
    let mut bytes = std::mem::take(value).into_bytes();
    bytes.fill(0);
}
impl Drop for Auth {
    fn drop(&mut self) {
        let Self::DbPassword { password, .. } = self;
        wipe(password);
    }
}
impl Auth {
    pub fn secret(&self) -> &str {
        let Self::DbPassword { password, .. } = self;
        password
    }
    pub fn validate(&self, principal: &Principal) -> Result<()> {
        if self.secret().is_empty() || self.secret().len() > 64 * 1024 {
            return Err(Error("INVALID_SECRET"));
        }
        let Self::DbPassword { identity, .. } = self;
        if principal.auth_mode == "sql_password" && !identity.is_empty() && identity.len() <= 512 {
            Ok(())
        } else {
            Err(Error("AUTH_MODE_MISMATCH"))
        }
    }
    pub fn wire(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}
/// Secrets supplied through stdin. An `entra_user` principal carries none: the
/// driver acquires its own Azure CLI token, so stdin may be empty or `{}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub principal: Option<Auth>,
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
impl Credentials {
    pub fn read(reader: &mut dyn SecretReader, config: &Config) -> Result<Self> {
        let mut bytes = reader.read_bounded(SECRET_BYTES)?;
        let entra = config.principal.is_entra();
        let parsed = if bytes.len() > SECRET_BYTES || std::str::from_utf8(&bytes).is_err() {
            Err(Error("INVALID_SECRET_INPUT"))
        } else if entra && bytes.iter().all(u8::is_ascii_whitespace) {
            Ok(Self { principal: None })
        } else {
            serde_json::from_slice::<Self>(&bytes).map_err(|_| Error("INVALID_SECRET_INPUT"))
        };
        bytes.fill(0);
        let credentials = parsed?;
        match (&credentials.principal, entra) {
            (None, true) => {}
            (Some(auth), false) => auth.validate(&config.principal)?,
            _ => return Err(Error("AUTH_MODE_MISMATCH")),
        }
        Ok(credentials)
    }
    pub fn canaries(&self) -> Vec<Vec<u8>> {
        self.principal
            .iter()
            .flat_map(|auth| {
                [
                    auth.secret().as_bytes().to_vec(),
                    serde_json::to_vec(auth.secret()).unwrap_or_default(),
                ]
            })
            .collect()
    }
}
