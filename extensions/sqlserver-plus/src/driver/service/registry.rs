use super::{types::DriverContext, ServiceError};
use mssql_tds::core::CancelHandle;
use once_cell::sync::Lazy;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{sync::Semaphore, time::Instant};

#[cfg(test)]
pub static SHUTDOWN_GATE: std::sync::RwLock<()> = std::sync::RwLock::new(());
type Key = (String, String);
static REQUESTS: Lazy<Mutex<HashMap<Key, Arc<State>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static CONNECTION_LIMITS: Lazy<Mutex<HashMap<String, Arc<Semaphore>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    Cancelled,
    Deadline,
}
const NO_CAUSE: u8 = 0;
pub struct State {
    cancel: Mutex<Option<CancelHandle>>,
    cancelled: AtomicBool,
    cause: AtomicU8,
    sealed: AtomicBool,
    write_dispatched: AtomicBool,
    session_touched: AtomicBool,
    opened_session: Mutex<Option<String>>,
    pub deadline: Instant,
    notify: tokio::sync::Notify,
}
pub struct Scope {
    pub state: Arc<State>,
    key: Key,
}
impl State {
    pub fn cause(&self) -> Option<Cause> {
        match self.cause.load(Ordering::Acquire) {
            1 => Some(Cause::Cancelled),
            2 => Some(Cause::Deadline),
            _ => None,
        }
    }
    fn seal(&self) {
        let _serialized = self.cancel.lock().unwrap();
        self.sealed.store(true, Ordering::Release);
    }
    pub fn mark_write(&self) {
        self.write_dispatched.store(true, Ordering::Release);
    }
    pub async fn wait_cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}
impl Scope {
    pub fn reserve(context: &DriverContext) -> Result<Self, ServiceError> {
        let key = (context.connection_id.clone(), context.request_id.clone());
        let mut requests = REQUESTS.lock().unwrap();
        if requests.contains_key(&key) {
            return Err(ServiceError::argument(
                "The request ID is already active on this connection",
            ));
        }
        let state = Arc::new(State {
            cancel: Mutex::new(Some(CancelHandle::new())),
            cancelled: AtomicBool::new(false),
            cause: AtomicU8::new(NO_CAUSE),
            sealed: AtomicBool::new(false),
            write_dispatched: AtomicBool::new(false),
            session_touched: AtomicBool::new(false),
            opened_session: Mutex::new(None),
            deadline: Instant::now() + Duration::from_millis(context.deadline_ms),
            notify: tokio::sync::Notify::new(),
        });
        requests.insert(key.clone(), state.clone());
        Ok(Self { state, key })
    }
    pub fn cancel_handle(&self) -> CancelHandle {
        self.state
            .cancel
            .lock()
            .unwrap()
            .as_ref()
            .map(CancelHandle::child_handle)
            .unwrap_or_else(|| {
                let parent = CancelHandle::new();
                let child = parent.child_handle();
                parent.cancel();
                child
            })
    }
    pub fn remaining(&self) -> Result<u64, ServiceError> {
        if self.cancelled() {
            return Err(match self.cause() {
                Some(Cause::Deadline) => {
                    ServiceError::new("DEADLINE_EXCEEDED", "The request deadline was exceeded")
                }
                _ => ServiceError::new("CANCELLED", "The request was cancelled"),
            });
        }
        let remaining = self
            .state
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining == 0 {
            return Err(ServiceError::new(
                "DEADLINE_EXCEEDED",
                "The request deadline was exceeded",
            ));
        }
        Ok(remaining)
    }
    pub fn cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }
    pub fn cause(&self) -> Option<Cause> {
        self.state.cause()
    }
    pub fn mark_write(&self) {
        self.state.mark_write();
    }
    pub fn write_dispatched(&self) -> bool {
        self.state.write_dispatched.load(Ordering::Acquire)
    }
    pub fn touch_session(&self) {
        self.state.session_touched.store(true, Ordering::Release);
    }
    pub fn session_touched(&self) -> bool {
        self.state.session_touched.load(Ordering::Acquire)
    }
    pub fn register_opened_session(&self, handle: &str) {
        *self.state.opened_session.lock().unwrap() = Some(handle.to_owned());
    }
    pub fn opened_session(&self) -> Option<String> {
        self.state.opened_session.lock().unwrap().clone()
    }
    #[cfg(test)]
    pub fn stop(&self) {
        stop(&self.state, Cause::Cancelled);
    }
    pub fn expire(&self) {
        stop(&self.state, Cause::Deadline);
    }
    pub fn seal(&self) {
        self.state.seal();
    }
    pub fn connection_id(&self) -> &str {
        &self.key.0
    }
    pub async fn wait_cancelled(&self) {
        self.state.wait_cancelled().await;
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.seal();
        let mut requests = REQUESTS.lock().unwrap();
        if requests
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.state))
        {
            requests.remove(&self.key);
        }
    }
}
/// Signals one execution exactly once. Returns false when the entry is
/// already sealed (completed) or was already signalled, so repeated or late
/// calls are no-ops. Sealing and signalling share one lock, which makes the
/// outcome of a completion-versus-cancel race a single decision.
fn stop(state: &State, cause: Cause) -> bool {
    let mut parent = state.cancel.lock().unwrap();
    if state.sealed.load(Ordering::Acquire) || state.cancelled.load(Ordering::Acquire) {
        return false;
    }
    state.cause.store(
        match cause {
            Cause::Cancelled => 1,
            Cause::Deadline => 2,
        },
        Ordering::Release,
    );
    state.cancelled.store(true, Ordering::Release);
    state.notify.notify_waiters();
    if let Some(handle) = parent.take() {
        handle.cancel();
    }
    true
}
pub fn cancel(connection: &str, request: &str) -> bool {
    let requests = REQUESTS.lock().unwrap();
    requests
        .get(&(connection.to_owned(), request.to_owned()))
        .is_some_and(|state| stop(state, Cause::Cancelled))
}
pub fn cancel_connection(connection: &str) {
    for ((id, _), state) in REQUESTS.lock().unwrap().iter() {
        if id == connection {
            stop(state, Cause::Cancelled);
        }
    }
}
pub fn cancel_all() {
    for state in REQUESTS.lock().unwrap().values() {
        stop(state, Cause::Cancelled);
    }
}
#[cfg(test)]
pub fn active_count(connection: &str) -> usize {
    REQUESTS
        .lock()
        .unwrap()
        .keys()
        .filter(|(id, _)| id == connection)
        .count()
}
pub fn connection_limit(connection: &str) -> Arc<Semaphore> {
    CONNECTION_LIMITS
        .lock()
        .unwrap()
        .entry(connection.to_owned())
        .or_insert_with(|| Arc::new(Semaphore::new(4)))
        .clone()
}
pub fn cleanup() {
    CONNECTION_LIMITS
        .lock()
        .unwrap()
        .retain(|_, semaphore| Arc::strong_count(semaphore) > 1);
}
