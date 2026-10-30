use super::{
    super::super::{
        contracts::ErrorCode,
        error::{failure, ServiceResult},
        storage::current_windows_sid,
    },
    VerifiedPeer,
};
use std::{
    io,
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, LocalFree, ERROR_INVALID_PARAMETER, STILL_ACTIVE},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    },
    System::{
        Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
        Threading::{
            GetExitCodeProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

pub(crate) enum Stream {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}
pub(crate) struct Listener {
    path: PathBuf,
    server: Option<NamedPipeServer>,
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(server) => Pin::new(server).poll_read(cx, buf),
            Self::Client(client) => Pin::new(client).poll_read(cx, buf),
        }
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Server(server) => Pin::new(server).poll_write(cx, buf),
            Self::Client(client) => Pin::new(client).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(server) => Pin::new(server).poll_flush(cx),
            Self::Client(client) => Pin::new(client).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(server) => Pin::new(server).poll_shutdown(cx),
            Self::Client(client) => Pin::new(client).poll_shutdown(cx),
        }
    }
}

fn create(path: &Path, first: bool) -> ServiceResult<NamedPipeServer> {
    let sid = current_windows_sid()?;
    let descriptor: Vec<u16> = format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        let mut security = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor.as_ptr(),
            SDDL_REVISION_1,
            &mut security,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security,
            bInheritHandle: 0,
        };
        let result = ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                path,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
            .map_err(|_| failure(ErrorCode::PermissionDenied));
        LocalFree(security);
        result
    }
}
pub(crate) fn bind(path: &Path) -> ServiceResult<Listener> {
    Ok(Listener {
        path: path.to_owned(),
        server: Some(create(path, true)?),
    })
}
pub(crate) async fn accept(listener: &mut Listener) -> ServiceResult<(Stream, VerifiedPeer)> {
    let server = listener
        .server
        .as_ref()
        .ok_or_else(|| failure(ErrorCode::DriverExited))?;
    server
        .connect()
        .await
        .map_err(|_| failure(ErrorCode::DriverExited))?;
    let server = listener
        .server
        .take()
        .ok_or_else(|| failure(ErrorCode::DriverExited))?;
    listener.server = Some(create(&listener.path, false)?);
    let peer = unsafe {
        let mut pid = 0;
        if GetNamedPipeClientProcessId(server.as_raw_handle().cast(), &mut pid) == 0 {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        verify_pid(pid)?
    };
    Ok((Stream::Server(server), peer))
}
pub(crate) async fn connect(path: &Path, expected_pid: u32) -> ServiceResult<Stream> {
    let client = loop {
        match ClientOptions::new().open(path) {
            Ok(client) => break client,
            Err(error) if error.raw_os_error() == Some(231) => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await
            }
            Err(_) => return Err(failure(ErrorCode::GuiUnavailable)),
        }
    };
    unsafe {
        let mut pid = 0;
        if GetNamedPipeServerProcessId(client.as_raw_handle().cast(), &mut pid) == 0
            || pid != expected_pid
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        verify_pid(pid)?;
    }
    Ok(Stream::Client(client))
}
/// False when the process has exited. A named pipe goes away with its
/// process, so the process is the whole check here; a process that cannot be
/// opened for another reason counts as alive.
pub(crate) fn instance_alive(process_id: u32, _endpoint: &Path) -> bool {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
        if process.is_null() {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut code = 0u32;
        let read = GetExitCodeProcess(process, &mut code);
        CloseHandle(process);
        read == 0 || code == STILL_ACTIVE as u32
    }
}
unsafe fn verify_pid(pid: u32) -> ServiceResult<VerifiedPeer> {
    if pid == 0 {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if process.is_null() {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    let mut token = std::ptr::null_mut();
    let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
    CloseHandle(process);
    if opened == 0 {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    let result = (|| {
        let mut needed = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        if needed < std::mem::size_of::<TOKEN_USER>() as u32 || needed > 65536 {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let mut buffer = vec![0u64; (needed as usize + 7) / 8];
        if GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        ) == 0
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let mut length = 0;
        while length < 256 && *sid.add(length) != 0 {
            length += 1;
        }
        let text = if length == 256 {
            Err(failure(ErrorCode::PermissionDenied))
        } else {
            String::from_utf16(std::slice::from_raw_parts(sid, length))
                .map_err(|_| failure(ErrorCode::PermissionDenied))
        };
        LocalFree(sid.cast());
        let identity = text?;
        if identity != current_windows_sid()? {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        Ok(VerifiedPeer {
            process_id: pid,
            user_identity: identity,
        })
    })();
    CloseHandle(token);
    result
}
