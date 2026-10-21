use crate::services::{
    contracts::{ErrorCode, Operation},
    error::{failure, ServiceResult},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowAuthority {
    Main,
    Detached,
}

pub fn window_authority(
    label: &str,
    url: &str,
    dev_origin: Option<&str>,
    registered: bool,
) -> ServiceResult<WindowAuthority> {
    if !registered {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    let url = url::Url::parse(url).map_err(|_| failure(ErrorCode::PermissionDenied))?;
    let native =
        (url.scheme() == "tauri" && url.host_str() == Some("localhost") && url.port().is_none())
            || (matches!(url.scheme(), "http" | "https")
                && url.host_str() == Some("tauri.localhost")
                && url.port().is_none());
    let dev = dev_origin
        .and_then(|value| url::Url::parse(value).ok())
        .is_some_and(|configured| configured.origin() == url.origin());
    if !native && !dev || !url.username().is_empty() || url.password().is_some() {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    if label == "main" {
        return Ok(WindowAuthority::Main);
    }
    let tab = label
        .strip_prefix("results-window-")
        .filter(|tab| !tab.is_empty())
        .ok_or_else(|| failure(ErrorCode::PermissionDenied))?;
    let mut tabs = url.query_pairs().filter(|(key, _)| key == "tab");
    if url.path() != "/results-window"
        || tabs.next().map_or(true, |(_, value)| value != tab)
        || tabs.next().is_some()
    {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    Ok(WindowAuthority::Detached)
}

impl WindowAuthority {
    pub fn main_only(self) -> ServiceResult<()> {
        if self == Self::Main {
            Ok(())
        } else {
            Err(failure(ErrorCode::PermissionDenied))
        }
    }
    pub fn operation(self, operation: Operation) -> ServiceResult<()> {
        if self == Self::Main
            || matches!(
                operation,
                Operation::ResultGet
                    | Operation::QueryNext
                    | Operation::SpatialColumns
                    | Operation::SpatialQueryResult
                    | Operation::SpatialTableQuery
                    | Operation::SpatialFeature
                    | Operation::JobGet
                    | Operation::JobResult
                    | Operation::JobCancel
                    | Operation::MapGet
                    | Operation::MapList
            )
        {
            Ok(())
        } else {
            Err(failure(ErrorCode::PermissionDenied))
        }
    }
}
