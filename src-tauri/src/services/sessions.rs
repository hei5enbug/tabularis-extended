use super::{
    backend::{DriverBackend, DriverContext, SavedConnection, SessionFinish},
    contracts::ErrorCode,
    dispatch::CallerContext,
    error::{failure, ServiceResult},
    storage::now_ms,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

pub const SESSION_IDLE_MS: u64 = 5 * 60 * 1000;
const MAX_SESSIONS: usize = 64;

struct Session {
    connection: SavedConnection,
    caller_id: String,
    owner_instance_id: String,
    handle: String,
    read_only: bool,
    last_used_ms: u64,
    closed: bool,
    invalidated: bool,
}
pub struct SessionManager {
    sessions: Mutex<HashMap<String, (String, Arc<AsyncMutex<Session>>)>>,
    driver: Arc<dyn DriverBackend>,
    idle_ms: u64,
}

impl SessionManager {
    pub fn new(driver: Arc<dyn DriverBackend>) -> Self {
        Self::with_idle(driver, SESSION_IDLE_MS)
    }
    pub fn with_idle(driver: Arc<dyn DriverBackend>, idle_ms: u64) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            driver,
            idle_ms: idle_ms.min(SESSION_IDLE_MS),
        }
    }
    pub async fn is_write(
        &self,
        id: &str,
        connection_id: &str,
        caller: &CallerContext,
    ) -> ServiceResult<bool> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .get(id)
            .map(|(_, session)| session.clone())
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let session = session.lock().await;
        if session.closed
            || session.connection.id != connection_id
            || session.caller_id != caller.caller_id
            || session.owner_instance_id != caller.owner_instance_id
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        Ok(!session.read_only)
    }
    pub async fn open(
        &self,
        connection: &SavedConnection,
        database: &str,
        read_only: bool,
        caller: &CallerContext,
        context: DriverContext,
    ) -> ServiceResult<String> {
        if connection.driver == "cosmos-nosql" {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        let id = format!("s-{}", uuid::Uuid::new_v4());
        {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            if sessions.len() >= MAX_SESSIONS {
                return Err(failure(ErrorCode::ResourceLimit));
            }
            sessions.insert(
                id.clone(),
                (
                    connection.id.clone(),
                    Arc::new(AsyncMutex::new(Session {
                        connection: connection.clone(),
                        caller_id: caller.caller_id.clone(),
                        owner_instance_id: caller.owner_instance_id.clone(),
                        handle: String::new(),
                        read_only,
                        last_used_ms: now_ms(),
                        closed: true,
                        invalidated: false,
                    })),
                ),
            );
        }
        let handle = match tokio::time::timeout(
            Duration::from_millis(context.deadline_ms),
            self.driver.reserve_session(connection, database, context),
        )
        .await
        {
            Ok(Ok(handle)) => handle,
            result => {
                self.sessions
                    .lock()
                    .map_err(|_| failure(ErrorCode::ResourceLimit))?
                    .remove(&id);
                return Err(match result {
                    Ok(Err(error)) => error,
                    _ => failure(ErrorCode::DeadlineExceeded),
                });
            }
        };
        let session = self
            .sessions
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .get(&id)
            .map(|(_, session)| session.clone());
        let Some(session) = session else {
            self.driver.discard_session(&connection.id, &handle).await;
            return Err(failure(ErrorCode::AuthRequired));
        };
        let mut session = session.lock().await;
        if session.invalidated {
            self.driver.discard_session(&connection.id, &handle).await;
            return Err(failure(ErrorCode::AuthRequired));
        }
        session.handle = handle;
        session.closed = false;
        Ok(id)
    }
    pub async fn acquire(
        &self,
        id: &str,
        connection: &SavedConnection,
        caller: &CallerContext,
        write: bool,
        context: &DriverContext,
    ) -> ServiceResult<SessionLease> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .get(id)
            .map(|(_, session)| session.clone())
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let mut guard = tokio::time::timeout(
            Duration::from_millis(context.deadline_ms),
            session.lock_owned(),
        )
        .await
        .map_err(|_| failure(ErrorCode::DeadlineExceeded))?;
        if guard.closed
            || guard.connection.id != connection.id
            || guard.connection.auth_revision != connection.auth_revision
            || guard.caller_id != caller.caller_id
            || guard.owner_instance_id != caller.owner_instance_id
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        if now_ms().saturating_sub(guard.last_used_ms) >= self.idle_ms {
            guard.closed = true;
            self.rollback_or_discard(&guard, context.clone()).await;
            self.sessions
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                .remove(id);
            return Err(failure(ErrorCode::DeadlineExceeded));
        }
        if write && guard.read_only {
            return Err(failure(ErrorCode::WriteNotAllowed));
        }
        guard.last_used_ms = now_ms();
        Ok(SessionLease { guard })
    }
    async fn rollback_or_discard(&self, session: &Session, mut context: DriverContext) {
        context.session_handle = Some(session.handle.clone());
        context.read_only = session.read_only;
        if !matches!(
            tokio::time::timeout(
                Duration::from_millis(context.deadline_ms),
                self.driver.finish_session(
                    &session.connection,
                    &session.handle,
                    SessionFinish::Rollback,
                    context
                )
            )
            .await,
            Ok(Ok(()))
        ) {
            self.driver
                .discard_session(&session.connection.id, &session.handle)
                .await;
        }
    }
    pub async fn finish(
        &self,
        id: &str,
        connection: &SavedConnection,
        caller: &CallerContext,
        finish: SessionFinish,
        context: DriverContext,
    ) -> ServiceResult<()> {
        let mut lease = self
            .acquire(id, connection, caller, false, &context)
            .await?;
        lease.guard.closed = true;
        let mut context = context;
        context.session_handle = Some(lease.guard.handle.clone());
        // The driver checks the session's own mode, and rolling back or closing
        // a write session needs no write intent of its own.
        context.read_only = lease.guard.read_only;
        let result = tokio::time::timeout(
            Duration::from_millis(context.deadline_ms),
            self.driver
                .finish_session(connection, &lease.guard.handle, finish, context),
        )
        .await;
        self.sessions
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .remove(id);
        match result {
            Ok(Ok(())) => Ok(()),
            other => {
                self.driver
                    .discard_session(&connection.id, &lease.guard.handle)
                    .await;
                Err(match other {
                    Ok(Err(error)) => error,
                    _ => failure(ErrorCode::OutcomeUnknown),
                })
            }
        }
    }
    pub async fn expire_idle(&self, context: DriverContext) -> ServiceResult<usize> {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .iter()
            .map(|(id, (_, session))| (id.clone(), session.clone()))
            .collect();
        let mut removed = 0;
        for (id, session) in sessions {
            if let Ok(mut guard) = session.try_lock_owned() {
                if !guard.closed && now_ms().saturating_sub(guard.last_used_ms) >= self.idle_ms {
                    guard.closed = true;
                    self.rollback_or_discard(&guard, context.clone()).await;
                    self.sessions
                        .lock()
                        .map_err(|_| failure(ErrorCode::ResourceLimit))?
                        .remove(&id);
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
    pub async fn invalidate_connection(
        &self,
        connection_id: &str,
        context: DriverContext,
    ) -> ServiceResult<()> {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .iter()
            .filter(|(_, (id, _))| id == connection_id)
            .map(|(id, (_, session))| (id.clone(), session.clone()))
            .collect();
        for (id, session) in sessions {
            let mut guard = session.lock().await;
            guard.invalidated = true;
            if !guard.closed {
                guard.closed = true;
                self.rollback_or_discard(&guard, context.clone()).await;
            }
            self.sessions
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                .remove(&id);
        }
        Ok(())
    }
    pub async fn discard(&self, id: &str, connection_id: &str) -> ServiceResult<()> {
        let session = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            if sessions
                .get(id)
                .is_some_and(|(stored, _)| stored != connection_id)
            {
                return Err(failure(ErrorCode::PermissionDenied));
            }
            sessions.remove(id)
        };
        if let Some((stored, session)) = session {
            if stored != connection_id {
                return Err(failure(ErrorCode::PermissionDenied));
            }
            let mut guard = session.lock().await;
            guard.closed = true;
            guard.invalidated = true;
            if !guard.handle.is_empty() {
                self.driver
                    .discard_session(connection_id, &guard.handle)
                    .await;
            }
        }
        Ok(())
    }
}

pub struct SessionLease {
    guard: OwnedMutexGuard<Session>,
}
impl SessionLease {
    pub fn handle(&self) -> &str {
        &self.guard.handle
    }
    pub fn read_only(&self) -> bool {
        self.guard.read_only
    }
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        self.guard.last_used_ms = now_ms();
    }
}
