use super::super::{
    contracts::ErrorCode,
    error::{failure, ServiceResult},
};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct VerifiedPeer {
    pub process_id: u32,
    pub user_identity: String,
}

pub fn current_local_caller(
    owner_instance_id: String,
    transport: super::super::dispatch::CallerTransport,
) -> ServiceResult<super::super::dispatch::CallerContext> {
    #[cfg(unix)]
    let identity = unsafe { libc::geteuid() }.to_string();
    #[cfg(windows)]
    let identity = super::super::storage::current_windows_sid()?;
    Ok(super::super::dispatch::CallerContext {
        transport,
        caller_id: format!("local_user:{identity}"),
        owner_instance_id,
    })
}

#[cfg(unix)]
pub(crate) type Stream = tokio::net::UnixStream;
#[cfg(unix)]
pub(crate) type Listener = tokio::net::UnixListener;

#[cfg(unix)]
pub(crate) fn bind(path: &Path) -> ServiceResult<Listener> {
    use std::os::unix::fs::PermissionsExt;
    let listener = Listener::bind(path).map_err(|_| failure(ErrorCode::GuiUnavailable))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| failure(ErrorCode::PermissionDenied))?;
    Ok(listener)
}
#[cfg(unix)]
pub(crate) async fn accept(listener: &mut Listener) -> ServiceResult<(Stream, VerifiedPeer)> {
    let (stream, _) = listener
        .accept()
        .await
        .map_err(|_| failure(ErrorCode::DriverExited))?;
    let peer = verify(&stream)?;
    Ok((stream, peer))
}
#[cfg(unix)]
fn verify(stream: &Stream) -> ServiceResult<VerifiedPeer> {
    let peer = stream
        .peer_cred()
        .map_err(|_| failure(ErrorCode::PermissionDenied))?;
    if peer.uid() != unsafe { libc::geteuid() } {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    let pid = peer
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| failure(ErrorCode::PermissionDenied))?;
    Ok(VerifiedPeer {
        process_id: pid,
        user_identity: peer.uid().to_string(),
    })
}
#[cfg(unix)]
pub(crate) async fn connect(path: &Path, expected_pid: u32) -> ServiceResult<Stream> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| failure(ErrorCode::GuiUnavailable))?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    let stream = Stream::connect(path)
        .await
        .map_err(|_| failure(ErrorCode::GuiUnavailable))?;
    if verify(&stream)?.process_id != expected_pid {
        return Err(failure(ErrorCode::GuiUnavailable));
    }
    Ok(stream)
}

#[cfg(windows)]
#[path = "windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{accept, bind, connect, Listener, Stream};
