//! Connections pinned to a host session (an editor tab).
//!
//! `execute_query_batch` already runs every statement of one batch on a
//! single pooled connection, so `BEGIN … COMMIT` inside a script works.
//! What did not work is the workflow the transaction exists for: run
//! `BEGIN`, inspect, run the changes, verify them, and only then `COMMIT`,
//! each as its own run. Between runs the connection went back to the pool,
//! so the next run could land on a different one and the transaction was
//! stranded.
//!
//! When the host sends a `session_id`, a batch that leaves a transaction
//! open keeps its connection here until the session commits, rolls back,
//! closes, or goes idle. The pool recycles with `RecyclingMethod::Fast`,
//! which resets nothing, so a pinned connection is always rolled back
//! before it goes back to the pool — otherwise the next borrower would
//! inherit the transaction and its locks.
//!
//! Each session has its own lock, held for a whole run, so two overlapping
//! calls for one session run one after the other on the same connection
//! instead of each taking a fresh one.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use deadpool_postgres::Client;
use tokio::sync::{Mutex, OwnedMutexGuard};

/// A pooled client held between batches because the session that owns it
/// left an explicit transaction open.
struct PinnedSession {
    client: Client,
    last_used: Instant,
}

/// A session's pinned connection, if any. Holding the guard serializes runs.
#[derive(Default)]
pub struct Slot(Option<PinnedSession>);

/// A slot dropped while still holding a connection closes it rather than
/// returning it to the pool, where the next borrower would inherit its transaction.
impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(pinned) = self.0.take() {
            drop(Client::take(pinned.client));
        }
    }
}

impl Slot {
    /// Take the pinned connection; the caller must [`Slot::pin`] it again or end its transaction.
    pub fn take(&mut self) -> Option<Client> {
        self.0.take().map(|s| s.client)
    }

    /// Pin `client` until the session ends its transaction.
    pub fn pin(&mut self, client: Client) {
        self.0 = Some(PinnedSession {
            client,
            last_used: Instant::now(),
        });
    }
}

/// A pinned connection holds its transaction's locks until the session ends
/// it. An abandoned session would hold them indefinitely, so one untouched
/// for this long is rolled back and released by the periodic
/// [`sweep_idle`].
const MAX_IDLE: Duration = Duration::from_secs(30 * 60);

type SessionMap = HashMap<String, Arc<Mutex<Slot>>>;

fn sessions() -> &'static Mutex<SessionMap> {
    static SESSIONS: OnceLock<Mutex<SessionMap>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// End the transaction before the client goes back to the pool.
///
/// A failing `ROLLBACK` is only logged: the connection is already unusable
/// and the pool will discard it.
pub async fn rollback_and_release(client: Client) {
    if let Err(e) = client.batch_execute("ROLLBACK").await {
        log::warn!("ROLLBACK while releasing a pinned session failed: {e}");
    }
}

/// Lock `session_id`'s slot, waiting for any run already holding it.
pub async fn lock(session_id: &str) -> OwnedMutexGuard<Slot> {
    let slot = sessions()
        .lock()
        .await
        .entry(session_id.to_string())
        .or_default()
        .clone();
    slot.lock_owned().await
}

/// Roll back and release every session idle past [`MAX_IDLE`], and forget
/// slots nothing holds. A slot in use by a run is skipped.
pub async fn sweep_idle() {
    sweep_service_idle().await;
    let mut expired = Vec::new();
    {
        let now = Instant::now();
        let mut map = sessions().lock().await;
        map.retain(|_, slot| {
            let Ok(mut guard) = slot.try_lock() else {
                return true;
            };
            if guard
                .0
                .as_ref()
                .is_some_and(|s| now.duration_since(s.last_used) > MAX_IDLE)
            {
                expired.extend(guard.take());
            }
            // Only the map holds it and nothing is pinned, so nobody can be waiting on it.
            guard.0.is_some() || Arc::strong_count(slot) > 1
        });
    }

    if expired.is_empty() {
        return;
    }
    log::info!(
        "Releasing {} pinned session(s) idle for over {} minutes",
        expired.len(),
        MAX_IDLE.as_secs() / 60
    );
    for client in expired {
        rollback_and_release(client).await;
    }
}

/// Roll back and release the connection pinned to `session_id`, if any,
/// after any run still in flight for it finishes.
pub async fn release(session_id: &str) {
    let client = lock(session_id).await.take();
    if let Some(client) = client {
        log::info!("Releasing pinned session {session_id}");
        rollback_and_release(client).await;
    }
}

/// Roll back and release every pinned connection, so shutdown leaves no
/// transaction open on the server. A session a run still holds is skipped
/// rather than waited for; its connection closes with the process.
pub async fn release_all() {
    release_service_all().await;
    let mut clients: Vec<Client> = Vec::new();
    // A busy slot stays in the map, so the run holding it can still pin into a tracked slot.
    sessions()
        .lock()
        .await
        .retain(|_, slot| match slot.try_lock() {
            Ok(mut guard) => {
                clients.extend(guard.take());
                false
            }
            Err(_) => true,
        });

    if clients.is_empty() {
        return;
    }
    log::info!("Releasing {} pinned session(s)", clients.len());
    for client in clients {
        rollback_and_release(client).await;
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;

pub struct ServiceSlot {
    pub client: Option<Client>,
    pub connection_id: String,
    pub read_only: bool,
    pub last_used: Instant,
    invalidated: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for ServiceSlot {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            drop(Client::take(client));
        }
    }
}

struct ServiceEntry {
    connection_id: String,
    slot: Arc<Mutex<ServiceSlot>>,
    invalidated: Arc<std::sync::atomic::AtomicBool>,
}
type ServiceMap = HashMap<String, ServiceEntry>;
fn service_sessions() -> &'static Mutex<ServiceMap> {
    static SERVICE_SESSIONS: OnceLock<Mutex<ServiceMap>> = OnceLock::new();
    SERVICE_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn open_service(client: Client, connection_id: String, read_only: bool) -> String {
    let handle = uuid::Uuid::new_v4().to_string();
    let invalidated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    service_sessions().lock().await.insert(
        handle.clone(),
        ServiceEntry {
            connection_id: connection_id.clone(),
            invalidated: invalidated.clone(),
            slot: Arc::new(Mutex::new(ServiceSlot {
                client: Some(client),
                connection_id,
                read_only,
                last_used: Instant::now(),
                invalidated,
            })),
        },
    );
    handle
}

pub async fn lock_service(
    handle: &str,
    connection_id: &str,
    read_only: bool,
) -> Result<OwnedMutexGuard<ServiceSlot>, crate::handlers::spatial::SpatialError> {
    use crate::handlers::spatial::SpatialError;
    let slot = service_sessions()
        .lock()
        .await
        .get(handle)
        .map(|entry| entry.slot.clone())
        .ok_or_else(|| SpatialError::new("CURSOR_EXPIRED", "Service session does not exist"))?;
    let mut guard = slot.lock_owned().await;
    if guard.invalidated.load(std::sync::atomic::Ordering::Acquire) {
        if let Some(client) = guard.client.take() {
            drop(Client::take(client));
        }
        return Err(SpatialError::new(
            "CURSOR_EXPIRED",
            "Service session authentication was invalidated",
        ));
    }
    if guard.connection_id != connection_id {
        return Err(SpatialError::new(
            "PERMISSION_DENIED",
            "Session belongs to another connection",
        ));
    }
    if guard.read_only != read_only {
        return Err(SpatialError::new(
            "WRITE_NOT_ALLOWED",
            "Session access mode cannot be changed",
        ));
    }
    if guard.last_used.elapsed() >= Duration::from_secs(300) {
        if let Some(client) = guard.client.take() {
            rollback_service_client(client).await;
        }
        return Err(SpatialError::new(
            "CURSOR_EXPIRED",
            "Service session expired",
        ));
    }
    if guard.client.is_none() {
        return Err(SpatialError::new(
            "CURSOR_EXPIRED",
            "Service session is closed",
        ));
    }
    guard.last_used = Instant::now();
    Ok(guard)
}

pub async fn forget_service(handle: &str) {
    service_sessions().lock().await.remove(handle);
}

pub async fn invalidate_service_connection(connection_id: &str) -> usize {
    let entries = {
        let mut map = service_sessions().lock().await;
        let handles = map
            .iter()
            .filter_map(|(handle, entry)| {
                (entry.connection_id == connection_id).then_some(handle.clone())
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|handle| {
                map.remove(&handle).inspect(|entry| {
                    entry
                        .invalidated
                        .store(true, std::sync::atomic::Ordering::Release)
                })
            })
            .collect::<Vec<_>>()
    };
    let count = entries.len();
    for entry in entries {
        entry
            .invalidated
            .store(true, std::sync::atomic::Ordering::Release);
        if let Ok(mut slot) = entry.slot.try_lock() {
            if let Some(client) = slot.client.take() {
                drop(Client::take(client));
            }
        } else {
            let pending = entry.slot.clone();
            tokio::spawn(async move {
                let mut slot = pending.lock_owned().await;
                if let Some(client) = slot.client.take() {
                    drop(Client::take(client));
                }
            });
        }
    }
    count
}

pub async fn rollback_service_client(client: Client) -> bool {
    if confirm_rollback(&client).await {
        true
    } else {
        drop(Client::take(client));
        false
    }
}

pub async fn confirm_rollback(client: &tokio_postgres::Client) -> bool {
    for _ in 0..2 {
        match tokio::time::timeout(Duration::from_secs(5), client.batch_execute("ROLLBACK")).await {
            Ok(Ok(())) => return true,
            Ok(Err(error)) if error.code().is_some_and(|code| code.code() == "57014") => {}
            _ => return false,
        }
    }
    false
}

async fn sweep_service_idle() {
    let mut clients = Vec::new();
    service_sessions().lock().await.retain(|_, entry| {
        let Ok(mut guard) = entry.slot.try_lock() else {
            return true;
        };
        if guard.last_used.elapsed() >= Duration::from_secs(300) {
            clients.extend(guard.client.take());
            false
        } else {
            guard.client.is_some()
        }
    });
    for client in clients {
        rollback_service_client(client).await;
    }
}

async fn release_service_all() {
    let mut clients = Vec::new();
    service_sessions().lock().await.retain(|_, entry| {
        let Ok(mut guard) = entry.slot.try_lock() else {
            return true;
        };
        clients.extend(guard.client.take());
        false
    });
    for client in clients {
        rollback_service_client(client).await;
    }
}
