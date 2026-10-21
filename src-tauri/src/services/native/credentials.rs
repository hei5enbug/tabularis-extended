use super::NativeCore;
use crate::services::{
    backend::CancellationSignal,
    contracts::{ErrorCode, Operation, ServiceRequest},
    dispatch::{CallerContext, CallerTransport},
    error::{failure, ServiceResult},
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    Stdin,
    Descriptor(u8),
}

pub async fn register_credential(
    core: &NativeCore,
    request: &ServiceRequest,
    caller: &CallerContext,
    source: CredentialSource,
    signal: CancellationSignal,
) -> ServiceResult<()> {
    if caller.transport != CallerTransport::Cli || request.operation != Operation::CredentialImport
    {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    core.decode(&serde_json::to_value(request).map_err(|_| failure(ErrorCode::InvalidArgument))?)?;
    let channel = &request.input["input_channel"];
    let descriptor = match source {
        CredentialSource::Stdin if channel["kind"] == "protected_stdin" => 0,
        CredentialSource::Descriptor(fd)
            if fd >= 3
                && channel["kind"] == "file_descriptor"
                && channel["descriptor"].as_u64() == Some(fd as u64) =>
        {
            fd as i32
        }
        _ => return Err(failure(ErrorCode::InvalidArgument)),
    };
    let timeout = Duration::from_millis(request.deadline_ms.min(30_000));
    let read_signal = signal.clone();
    let mut worker =
        tokio::task::spawn_blocking(move || read_secret(descriptor, timeout, &read_signal));
    let bytes = tokio::select! {biased;
        _=signal.cancelled()=>{signal.cancel();worker.abort();return Err(failure(ErrorCode::Cancelled));},
        _=tokio::time::sleep(timeout)=>{signal.cancel();worker.abort();return Err(failure(ErrorCode::DeadlineExceeded));},
        result=&mut worker=>result.map_err(|_|failure(ErrorCode::CapabilityUnavailable))??,
    };
    core.platform
        .runtime
        .secret_inputs
        .register(caller, &request.request_id, channel, bytes)
}

#[cfg(unix)]
fn read_secret(
    descriptor: i32,
    timeout: Duration,
    signal: &CancellationSignal,
) -> ServiceResult<Vec<u8>> {
    use std::{fs::File, io::Read, os::fd::FromRawFd};
    let duplicated = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let mut file = unsafe { File::from_raw_fd(duplicated) };
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if signal.is_cancelled() {
            return Err(failure(ErrorCode::Cancelled));
        }
        if Instant::now() >= deadline {
            return Err(failure(ErrorCode::DeadlineExceeded));
        }
        let mut poll = libc::pollfd {
            fd: duplicated,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, 50) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if ready == 0 {
            continue;
        }
        let count = file
            .read(&mut chunk)
            .map_err(|_| failure(ErrorCode::InvalidArgument))?;
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > 64 * 1024 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if bytes.is_empty() || std::str::from_utf8(&bytes).is_err() {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(bytes)
}
#[cfg(windows)]
fn read_secret(
    descriptor: i32,
    _timeout: Duration,
    signal: &CancellationSignal,
) -> ServiceResult<Vec<u8>> {
    use std::io::Read;
    if descriptor != 0 {
        return Err(failure(ErrorCode::UnsupportedOperation));
    }
    if signal.is_cancelled() {
        return Err(failure(ErrorCode::Cancelled));
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure(ErrorCode::InvalidArgument))?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 || std::str::from_utf8(&bytes).is_err() {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(bytes)
}
