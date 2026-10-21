use crate::services::{
    contracts::ErrorCode,
    error::{failure, ServiceResult},
};
use std::{ffi::OsStr, path::PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub struct IsolationPaths {
    pub data_dir: Option<PathBuf>,
    pub plugin_dir: Option<PathBuf>,
}

pub fn checked_isolation_paths(
    data_dir: Option<&OsStr>,
    plugin_dir: Option<&OsStr>,
) -> ServiceResult<IsolationPaths> {
    let checked = |value: Option<&OsStr>| -> ServiceResult<Option<PathBuf>> {
        value
            .map(|value| {
                let path = PathBuf::from(value);
                if value.is_empty() || !path.is_absolute() {
                    Err(failure(ErrorCode::InvalidArgument))
                } else {
                    Ok(path)
                }
            })
            .transpose()
    };
    Ok(IsolationPaths {
        data_dir: checked(data_dir)?,
        plugin_dir: checked(plugin_dir)?,
    })
}

pub fn read_isolation_paths() -> ServiceResult<IsolationPaths> {
    let data = std::env::var_os("TABULARIS_DATA_DIR");
    let plugins = std::env::var_os("TABULARIS_PLUGIN_DIR");
    checked_isolation_paths(data.as_deref(), plugins.as_deref())
}
