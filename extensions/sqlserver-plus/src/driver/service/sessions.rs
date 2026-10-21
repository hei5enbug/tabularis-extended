use super::{
    pool::{self, NativeConnection},
    registry::Scope,
    types::{DriverContext, SESSION_IDLE_MS},
    ServiceError,
};
use crate::models::ConnectionParams;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

struct Entry {
    connection_id: String,
    slot: Arc<Mutex<Slot>>,
}
static SESSIONS: Lazy<Mutex<HashMap<String, Entry>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static CAPACITY: Lazy<Arc<Semaphore>> = Lazy::new(|| Arc::new(Semaphore::new(64)));
pub struct Slot {
    pub client: Option<NativeConnection>,
    pub connection_id: String,
    pub binding: String,
    pub read_only: bool,
    pub last_used: Instant,
    pub dirty: bool,
    _permit: OwnedSemaphorePermit,
}
impl Slot {
    fn expired(&self) -> bool {
        self.last_used.elapsed() >= Duration::from_millis(SESSION_IDLE_MS)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenInput {
    pub database: Option<String>,
    pub mode: Mode,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    ReadOnly,
    Write,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishInput {
    pub action: Action,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Commit,
    Rollback,
    Close,
}
impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            pool::discard(client);
        }
    }
}
pub async fn open(
    client: NativeConnection,
    params: &ConnectionParams,
    context: &DriverContext,
    permit: OwnedSemaphorePermit,
) -> Value {
    let handle = uuid::Uuid::new_v4().to_string();
    let slot = Slot {
        client: Some(client),
        connection_id: context.connection_id.clone(),
        binding: pool::fingerprint(params, context),
        read_only: context.read_only,
        last_used: Instant::now(),
        dirty: false,
        _permit: permit,
    };
    SESSIONS.lock().await.insert(
        handle.clone(),
        Entry {
            connection_id: context.connection_id.clone(),
            slot: Arc::new(Mutex::new(slot)),
        },
    );
    json!({"session_handle":handle,"in_transaction":true,"rollback_confirmed":false})
}
pub fn reserve() -> Result<OwnedSemaphorePermit, ServiceError> {
    CAPACITY
        .clone()
        .try_acquire_owned()
        .map_err(|_| ServiceError::new("RESOURCE_LIMIT", "The physical session limit was reached"))
}
fn closed_error(message: &'static str) -> ServiceError {
    let mut error = ServiceError::argument(message);
    error.session_closed = true;
    error
}
pub async fn lock(
    handle: &str,
    params: &ConnectionParams,
    context: &DriverContext,
    scope: &Scope,
) -> Result<OwnedMutexGuard<Slot>, ServiceError> {
    if uuid::Uuid::parse_str(handle).is_err() {
        return Err(ServiceError::argument("The physical session was not found"));
    }
    let slot = SESSIONS
        .lock()
        .await
        .get(handle)
        .map(|entry| entry.slot.clone())
        .ok_or_else(|| closed_error("The physical session was not found"))?;
    let mut slot = tokio::time::timeout_at(scope.state.deadline, slot.lock_owned())
        .await
        .map_err(|_| {
            ServiceError::new(
                "DEADLINE_EXCEEDED",
                "The session lock deadline was exceeded",
            )
        })?;
    if slot.client.is_none() || slot.expired() {
        if let Some(client) = slot.client.take() {
            pool::discard(client);
        }
        SESSIONS.lock().await.remove(handle);
        return Err(closed_error("The physical session expired or closed"));
    }
    if slot.connection_id != context.connection_id
        || slot.binding != pool::fingerprint(params, context)
        || slot.read_only != context.read_only
    {
        return Err(ServiceError::new(
            "WRITE_NOT_ALLOWED",
            "The physical session connection or principal does not match",
        ));
    }
    context.validate_auth(params, scope.remaining()?)?;
    Ok(slot)
}
pub async fn forget(handle: &str) {
    SESSIONS.lock().await.remove(handle);
}
pub async fn invalidate(connection: &str) {
    let slots = SESSIONS
        .lock()
        .await
        .iter()
        .filter(|(_, entry)| entry.connection_id == connection)
        .map(|(id, entry)| (id.clone(), entry.slot.clone()))
        .collect::<Vec<_>>();
    for (id, slot) in slots {
        let mut slot = slot.lock().await;
        if let Some(client) = slot.client.take() {
            pool::discard(client);
        }
        forget(&id).await;
    }
}
pub async fn cleanup() {
    let slots = SESSIONS
        .lock()
        .await
        .iter()
        .map(|(id, entry)| (id.clone(), entry.slot.clone()))
        .collect::<Vec<_>>();
    for (id, slot) in slots {
        if let Ok(slot) = slot.try_lock() {
            if slot.expired() {
                drop(slot);
                forget(&id).await;
            }
        }
    }
}
pub async fn shutdown() {
    SESSIONS.lock().await.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn 읽기와_쓰기_session은_모두_유휴_만료되어_entry와_permit을_반환한다() {
        // given
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let capacity = Arc::new(Semaphore::new(2));
        let handles = [
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        ];
        let context: DriverContext = serde_json::from_value(json!({"protocol_version":1,"connection_id":uuid::Uuid::new_v4().to_string(),"request_id":"idle-policy"})).unwrap();
        runtime.block_on(async {
            for (handle, read_only) in handles.iter().zip([true, false]) {
                let slot = Slot {
                    client: None,
                    connection_id: context.connection_id.clone(),
                    binding: "policy-fixture".into(),
                    read_only,
                    last_used: Instant::now() - Duration::from_millis(SESSION_IDLE_MS + 1),
                    dirty: false,
                    _permit: capacity.clone().acquire_owned().await.unwrap(),
                };
                SESSIONS.lock().await.insert(
                    handle.clone(),
                    Entry {
                        connection_id: context.connection_id.clone(),
                        slot: Arc::new(Mutex::new(slot)),
                    },
                );
            }
        });
        let run = || {
            runtime.block_on(async {
                cleanup().await;
                let scope = Scope::reserve(&context).unwrap();
                let mut errors = Vec::new();
                for handle in &handles {
                    errors.push(
                        lock(handle, &ConnectionParams::default(), &context, &scope)
                            .await
                            .err()
                            .unwrap(),
                    );
                }
                let entries = SESSIONS.lock().await;
                let removed = handles.iter().all(|handle| !entries.contains_key(handle));
                (errors, removed, capacity.available_permits())
            })
        };
        // when
        let actual = run();
        // then
        assert!(actual.0.iter().all(|error| error.code == "INVALID_ARGUMENT"
            && error.session_closed
            && !error.unknown));
        assert!(actual.1);
        assert_eq!(actual.2, 2);
    }
}
