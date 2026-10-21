use super::{types::DriverContext, ServiceError};
use mssql_tds::core::CancelHandle;
use once_cell::sync::Lazy;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{sync::Semaphore, time::Instant};

type Key = (String, String);
static REQUESTS: Lazy<Mutex<HashMap<Key, Arc<State>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static CONNECTION_LIMITS: Lazy<Mutex<HashMap<String, Arc<Semaphore>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
pub struct State {
    cancel: Mutex<Option<CancelHandle>>,
    cancelled: AtomicBool,
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
    pub fn mark_write(&self) {
        self.write_dispatched.store(true, Ordering::Release);
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
            return Err(ServiceError::new("CANCELLED", "The request was cancelled"));
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
    pub fn stop(&self) {
        stop(&self.state);
    }
    pub fn seal(&self) {
        self.state.sealed.store(true, Ordering::Release);
    }
    pub fn connection_id(&self) -> &str {
        &self.key.0
    }
    pub async fn wait_cancelled(&self) {
        loop {
            let notified = self.state.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.cancelled() {
                return;
            }
            notified.await;
        }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.seal();
        REQUESTS.lock().unwrap().remove(&self.key);
    }
}
fn stop(state: &State) {
    if state.sealed.load(Ordering::Acquire) {
        return;
    }
    state.cancelled.store(true, Ordering::Release);
    state.notify.notify_waiters();
    if let Some(handle) = state.cancel.lock().unwrap().take() {
        handle.cancel();
    }
}
pub fn cancel(connection: &str, request: &str) -> bool {
    let requests = REQUESTS.lock().unwrap();
    if let Some(state) = requests.get(&(connection.to_owned(), request.to_owned())) {
        if !state.sealed.load(Ordering::Acquire) {
            stop(state);
            return true;
        }
    }
    false
}
pub fn cancel_connection(connection: &str) {
    for ((id, _), state) in REQUESTS.lock().unwrap().iter() {
        if id == connection {
            stop(state);
        }
    }
}
pub fn cancel_all() {
    for state in REQUESTS.lock().unwrap().values() {
        stop(state);
    }
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
