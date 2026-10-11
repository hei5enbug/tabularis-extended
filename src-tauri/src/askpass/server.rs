//! Askpass server.
//!
//! Listens on a private local socket for prompt requests coming from askpass
//! client processes (see `client.rs`) and bridges them to an [`AskpassUi`]
//! implementation. The server lives only as long as the ssh process that
//! needs it: the SSH tunnel code starts one, injects its endpoint into the
//! ssh command's environment, and drops it once the tunnel is up (or failed).

use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
const MAX_SOCKET_PATH_BYTES: usize = 100;

use super::client::SOCKET_ENV;
#[cfg(windows)]
use super::client::TOKEN_ENV;
use super::protocol::{decode_request, encode_response, PromptKind};

const ACCEPT_POLL_MS: u64 = 100;

/// User-interface side of an askpass exchange. Implementations block inside
/// [`AskpassUi::request`] until the user answers (or a timeout fires).
pub trait AskpassUi: Send + Sync {
    /// Ask the user to answer a secret or confirmation prompt. `None` means
    /// the prompt was cancelled.
    fn request(&self, kind: PromptKind, prompt: &str) -> Option<String>;
    /// Show a notification that requires no textual answer (e.g. "touch your
    /// security key"). Returns an identifier used to dismiss it later.
    fn show_notification(&self, prompt: &str) -> u64;
    /// Remove a notification previously shown via `show_notification`.
    fn dismiss_notification(&self, id: u64);
}

pub struct AskpassServer {
    endpoint: String,
    #[cfg(windows)]
    token: String,
    pending: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    #[cfg(unix)]
    socket_path: std::path::PathBuf,
    #[cfg(unix)]
    _socket_dir: tempfile::TempDir,
}

impl AskpassServer {
    /// Bind the local socket and spawn the accept loop.
    pub fn start(ui: Arc<dyn AskpassUi>) -> Result<Self, String> {
        let pending = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            use std::os::unix::net::UnixListener;

            let fallback_root = std::fs::canonicalize("/tmp").map_err(|e| {
                format!("Failed to resolve the short askpass socket directory: {e}")
            })?;
            let socket_dir = create_socket_dir(&std::env::temp_dir(), &fallback_root)
                .map_err(|e| format!("Failed to create askpass socket directory: {e}"))?;
            let socket_path = socket_dir.path().join("s");
            let listener = UnixListener::bind(&socket_path)
                .map_err(|e| format!("Failed to bind askpass socket: {}", e))?;
            // The socket carries secrets: restrict it to the current user.
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("Failed to restrict askpass socket permissions: {}", e))?;
            listener
                .set_nonblocking(true)
                .map_err(|e| format!("Failed to configure askpass socket: {}", e))?;

            let endpoint = socket_path.to_string_lossy().to_string();
            spawn_accept_loop(listener, ui, pending.clone(), stop.clone());
            Ok(Self {
                endpoint,
                pending,
                stop,
                socket_path,
                _socket_dir: socket_dir,
            })
        }

        #[cfg(windows)]
        {
            use std::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0")
                .map_err(|e| format!("Failed to bind askpass socket: {}", e))?;
            let endpoint = listener
                .local_addr()
                .map_err(|e| format!("Failed to read askpass socket address: {}", e))?
                .to_string();
            listener
                .set_nonblocking(true)
                .map_err(|e| format!("Failed to configure askpass socket: {}", e))?;

            let token = uuid::Uuid::new_v4().simple().to_string();
            spawn_accept_loop(listener, ui, pending.clone(), stop.clone(), token.clone());
            Ok(Self {
                endpoint,
                token,
                pending,
                stop,
            })
        }
    }

    /// Endpoint clients must connect to (socket path or `host:port`).
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Point ssh's askpass machinery at this server: ssh re-executes the
    /// Tabularis binary, which detects [`SOCKET_ENV`] and runs in client mode.
    pub fn configure_command(&self, command: &mut Command) -> Result<(), String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("Failed to locate Tabularis executable: {}", e))?;
        command
            .env("SSH_ASKPASS", exe)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(SOCKET_ENV, &self.endpoint);
        #[cfg(windows)]
        command.env(TOKEN_ENV, &self.token);
        Ok(())
    }

    /// Whether a prompt is currently waiting on the user. Callers use this to
    /// pause connection timeouts while the user is typing a PIN.
    pub fn has_pending(&self) -> bool {
        self.pending.load(Ordering::Relaxed) > 0
    }
}

#[cfg(unix)]
fn create_socket_dir(
    primary_root: &Path,
    fallback_root: &Path,
) -> std::io::Result<tempfile::TempDir> {
    let primary = create_private_socket_dir(primary_root)?;
    if socket_path_len(primary.path()) < MAX_SOCKET_PATH_BYTES {
        return Ok(primary);
    }
    drop(primary);

    let fallback = create_private_socket_dir(fallback_root)?;
    if socket_path_len(fallback.path()) >= MAX_SOCKET_PATH_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Askpass socket path is too long",
        ));
    }
    Ok(fallback)
}

#[cfg(unix)]
fn create_private_socket_dir(root: &Path) -> std::io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::Builder::new().prefix("ta-").tempdir_in(root)?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

#[cfg(unix)]
fn socket_path_len(directory: &Path) -> usize {
    use std::os::unix::ffi::OsStrExt;
    directory.join("s").as_os_str().as_bytes().len()
}

#[cfg(all(test, unix))]
mod socket_path_tests {
    use super::{create_socket_dir, socket_path_len, MAX_SOCKET_PATH_BYTES};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn 긴_임시_경로는_짧은_비공개_경로로_대체한다() {
        // Given: 기본 임시 경로가 Unix 소켓 경로 제한을 넘는다.
        let primary_root = tempfile::tempdir().unwrap();
        let deep_root = primary_root.path().join("x".repeat(120));
        fs::create_dir(&deep_root).unwrap();
        let fallback_root = tempfile::tempdir().unwrap();

        // When: 소켓 디렉터리를 선택한다.
        let socket_dir = create_socket_dir(&deep_root, fallback_root.path()).unwrap();

        // Then: 짧은 대체 경로 아래에만 private 디렉터리가 남는다.
        assert!(socket_dir.path().starts_with(fallback_root.path()));
        assert!(socket_path_len(socket_dir.path()) < MAX_SOCKET_PATH_BYTES);
        assert_eq!(
            fs::metadata(socket_dir.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(fs::read_dir(&deep_root).unwrap().count(), 0);
    }
}

impl Drop for AskpassServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
fn spawn_accept_loop(
    listener: std::os::unix::net::UnixListener,
    ui: Arc<dyn AskpassUi>,
    pending: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let ui = ui.clone();
                    let pending = pending.clone();
                    thread::spawn(move || handle_connection(stream, ui, pending));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(ACCEPT_POLL_MS));
                }
                Err(e) => {
                    eprintln!("[Askpass] Accept failed: {}", e);
                    thread::sleep(Duration::from_millis(ACCEPT_POLL_MS));
                }
            }
        }
    });
}

#[cfg(windows)]
fn spawn_accept_loop(
    listener: std::net::TcpListener,
    ui: Arc<dyn AskpassUi>,
    pending: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    token: String,
) {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let ui = ui.clone();
                    let pending = pending.clone();
                    let token = token.clone();
                    thread::spawn(move || {
                        let mut reader =
                            BufReader::new(stream.try_clone().expect("clone askpass stream"));
                        let mut auth = String::new();
                        if reader.read_line(&mut auth).is_err()
                            || auth.trim_end_matches(['\n', '\r']) != format!("AUTH {}", token)
                        {
                            eprintln!("[Askpass] Rejected connection with bad token");
                            return;
                        }
                        handle_authenticated(reader, stream, ui, pending);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(ACCEPT_POLL_MS));
                }
                Err(e) => {
                    eprintln!("[Askpass] Accept failed: {}", e);
                    thread::sleep(Duration::from_millis(ACCEPT_POLL_MS));
                }
            }
        }
    });
}

#[cfg(unix)]
fn handle_connection(
    stream: std::os::unix::net::UnixStream,
    ui: Arc<dyn AskpassUi>,
    pending: Arc<AtomicUsize>,
) {
    let reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[Askpass] Failed to clone stream: {}", e);
            return;
        }
    });
    handle_authenticated(reader, stream, ui, pending);
}

/// Serve a single askpass exchange on an already-authenticated connection.
fn handle_authenticated<R, W>(
    mut reader: BufReader<R>,
    mut writer: W,
    ui: Arc<dyn AskpassUi>,
    pending: Arc<AtomicUsize>,
) where
    R: std::io::Read,
    W: Write,
{
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let Some((kind, prompt)) = decode_request(line.trim_end_matches(['\n', '\r'])) else {
        eprintln!("[Askpass] Ignoring malformed request");
        return;
    };

    pending.fetch_add(1, Ordering::Relaxed);
    // Make sure the counter is decremented on every exit path.
    let _guard = PendingGuard(pending);

    match kind {
        PromptKind::Secret | PromptKind::Confirm => {
            let response = ui.request(kind, &prompt);
            let reply = format!("{}\n", encode_response(response.as_deref()));
            if let Err(e) = writer
                .write_all(reply.as_bytes())
                .and_then(|_| writer.flush())
            {
                eprintln!("[Askpass] Failed to send response: {}", e);
            }
        }
        PromptKind::Notify => {
            // No answer expected: keep the notification up until the client
            // process dies (ssh kills it once the key was touched), which we
            // observe as EOF on the connection.
            let id = ui.show_notification(&prompt);
            let mut rest = String::new();
            let _ = reader.read_line(&mut rest);
            ui.dismiss_notification(id);
        }
    }
}

struct PendingGuard(Arc<AtomicUsize>);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
