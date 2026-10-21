mod layout;
pub mod windows_quote;

#[cfg(windows)]
mod windows;

pub use layout::Layout;

pub const HEAP_ARGUMENT: &str = "--max-old-space-size=512";
pub const REMOVED_ENVIRONMENT: [&str; 8] = [
    "NODE_OPTIONS",
    "NODE_PATH",
    "NODE_TLS_REJECT_UNAUTHORIZED",
    "NODE_EXTRA_CA_CERTS",
    "NODE_USE_SYSTEM_CA",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "OPENSSL_CONF",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchError {
    InvalidArguments,
    InvalidBundle,
    InvalidCommandLine,
    LaunchFailed,
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidArguments => "INVALID_ARGUMENT: launcher does not accept arguments.",
            Self::InvalidBundle => "INVALID_BUNDLE: bundled runtime or entry is unavailable.",
            Self::InvalidCommandLine => "INVALID_ARGUMENT: launcher command line is invalid.",
            Self::LaunchFailed => "LAUNCH_FAILED: bundled runtime could not be started or monitored.",
        })
    }
}

impl std::error::Error for LaunchError {}

#[cfg(unix)]
pub fn launch(layout: &Layout) -> Result<u32, LaunchError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn execv(path: *const std::ffi::c_char, argv: *const *const std::ffi::c_char) -> std::ffi::c_int;
    }
    let node = CString::new(layout.node.as_os_str().as_bytes()).map_err(|_| LaunchError::InvalidCommandLine)?;
    let heap = CString::new(HEAP_ARGUMENT).map_err(|_| LaunchError::InvalidCommandLine)?;
    let entry = CString::new(layout.entry.as_os_str().as_bytes()).map_err(|_| LaunchError::InvalidCommandLine)?;
    let arguments = [node.as_ptr(), heap.as_ptr(), entry.as_ptr(), std::ptr::null()];
    std::env::set_current_dir(&layout.root).map_err(|_| LaunchError::LaunchFailed)?;
    for name in REMOVED_ENVIRONMENT {
        std::env::remove_var(name);
    }
    unsafe { execv(node.as_ptr(), arguments.as_ptr()); }
    Err(LaunchError::LaunchFailed)
}

#[cfg(windows)]
pub fn launch(layout: &Layout) -> Result<u32, LaunchError> {
    windows::launch(layout)
}
