use super::{Error, Result};
use serde::Deserialize;
use std::{fs, io::Read, path::Path};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub auth_mode: String,
    pub username: Option<String>,
    pub tenant_id: Option<String>,
    pub entra_object_id: Option<String>,
    pub azure_cli_path: Option<String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub nonproduction_resource: bool,
    pub host: String,
    pub database: String,
    pub principal: Principal,
    pub report: Option<String>,
}
#[derive(Clone, Copy)]
pub struct Flags {
    pub live: bool,
}
impl Flags {
    pub fn validate(self) -> Result<()> {
        if self.live {
            Ok(())
        } else {
            Err(Error("LIVE_OPT_IN_REQUIRED"))
        }
    }
}
fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}
pub fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}
fn cli_path(value: &str) -> bool {
    text(value, 1024) && value.trim() == value && Path::new(value).is_absolute()
}
impl Principal {
    pub fn is_entra(&self) -> bool {
        self.auth_mode == "entra_user"
    }
    fn validate(&self) -> Result<()> {
        let username = self.username.as_ref().is_some_and(|v| text(v, 128));
        match self.auth_mode.as_str() {
            "sql_password"
                if username
                    && self.tenant_id.is_none()
                    && self.entra_object_id.is_none()
                    && self.azure_cli_path.is_none() =>
            {
                Ok(())
            }
            "entra_user"
                if username
                    && self.tenant_id.as_ref().is_some_and(|v| uuid(v))
                    && self.entra_object_id.as_ref().is_some_and(|v| uuid(v))
                    && self.azure_cli_path.as_ref().is_none_or(|v| cli_path(v)) =>
            {
                Ok(())
            }
            _ => Err(Error("INVALID_AUTH_CONFIG")),
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        let label = self
            .host
            .strip_suffix(".database.windows.net")
            .ok_or(Error("INVALID_HOST"))?;
        if label.is_empty()
            || label.len() > 63
            || !label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || label.starts_with('-')
            || label.ends_with('-')
        {
            return Err(Error("INVALID_HOST"));
        }
        if self.version != 2
            || !self.nonproduction_resource
            || !text(&self.database, 128)
            || self.database.contains(['[', ']'])
            || ["master", "tempdb", "model", "msdb"]
                .iter()
                .any(|name| self.database.eq_ignore_ascii_case(name))
        {
            return Err(Error("INVALID_CONFIG"));
        }
        self.principal.validate()?;
        if self
            .report
            .as_ref()
            .is_some_and(|p| !Path::new(p).is_absolute() || p.contains('\0'))
        {
            return Err(Error("INVALID_REPORT_PATH"));
        }
        Ok(())
    }
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 64 * 1024 || std::str::from_utf8(bytes).is_err() {
            return Err(Error("INVALID_CONFIG"));
        }
        let config: Self = serde_json::from_slice(bytes).map_err(|_| Error("INVALID_CONFIG"))?;
        config.validate()?;
        Ok(config)
    }
}

#[cfg(unix)]
pub fn private_file(path: &Path, cap: usize) -> Result<Vec<u8>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    #[cfg(target_os = "macos")]
    const NOFOLLOW: i32 = 0x100;
    #[cfg(not(target_os = "macos"))]
    const NOFOLLOW: i32 = 0x20000;
    if !path.is_absolute() {
        return Err(Error("INVALID_CONFIG_PATH"));
    }
    let before = fs::symlink_metadata(path).map_err(|_| Error("CONFIG_IO_FAILED"))?;
    if !before.is_file()
        || before.file_type().is_symlink()
        || before.mode() & 0o077 != 0
        || before.uid() != unsafe { getuid() }
        || before.size() > cap as u64
    {
        return Err(Error("CONFIG_NOT_PRIVATE"));
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW)
        .open(path)
        .map_err(|_| Error("CONFIG_IO_FAILED"))?;
    let initial = file.metadata().map_err(|_| Error("CONFIG_IO_FAILED"))?;
    if initial.ino() != before.ino() || initial.dev() != before.dev() {
        return Err(Error("CONFIG_CHANGED"));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error("CONFIG_IO_FAILED"))?;
    let after = fs::symlink_metadata(path).map_err(|_| Error("CONFIG_CHANGED"))?;
    let final_meta = file.metadata().map_err(|_| Error("CONFIG_CHANGED"))?;
    if bytes.len() > cap
        || bytes.len() as u64 != initial.size()
        || after.ino() != initial.ino()
        || after.dev() != initial.dev()
        || after.file_type().is_symlink()
        || after.mtime() != initial.mtime()
        || after.mtime_nsec() != initial.mtime_nsec()
        || final_meta.size() != initial.size()
        || final_meta.ctime() != initial.ctime()
        || final_meta.ctime_nsec() != initial.ctime_nsec()
    {
        return Err(Error("CONFIG_CHANGED"));
    }
    Ok(bytes)
}
#[cfg(not(unix))]
pub fn private_file(_: &Path, _: usize) -> Result<Vec<u8>> {
    Err(Error("UNSUPPORTED_PLATFORM"))
}

pub fn load(path: &Path) -> Result<Config> {
    Config::parse(&private_file(path, 64 * 1024)?)
}
