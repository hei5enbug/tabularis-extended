//! Request-scoped cancellation for the pre-service `execute_query` and
//! `execute_query_batch` RPCs.
//!
//! The host cancels such a call with a `cancel` notification carrying the
//! numeric JSON-RPC id of the call. The stdin reader registers the call before
//! it is queued, so a notification that arrives while the call waits for a
//! worker still reaches it. Service-protocol requests use
//! `driver::service::registry` instead.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use mssql_tds::core::CancelHandle;
use once_cell::sync::Lazy;
use tokio::sync::Notify;

static REGISTRY: Lazy<CancellationRegistry> = Lazy::new(CancellationRegistry::default);

/// The process-wide registry shared by the stdin reader and the workers.
pub fn global() -> &'static CancellationRegistry {
    &REGISTRY
}

#[derive(Default)]
pub struct CancellationRegistry {
    entries: Arc<Mutex<HashMap<u64, RegistryEntry>>>,
    next_generation: AtomicU64,
}

struct RegistryEntry {
    generation: u64,
    control: Arc<QueryCancellation>,
}

/// Per-request handle and state. The control owns the one-shot TDS parent;
/// the TDS child handles of its statements share its cancellation token.
pub struct QueryCancellation {
    parent: Mutex<Option<CancelHandle>>,
    cancelled: AtomicBool,
    notify: Notify,
    query_dispatched: AtomicBool,
}

/// Keeps a request registered until the request finishes.
pub struct ExecutionLease {
    request_id: u64,
    generation: u64,
    control: Arc<QueryCancellation>,
    entries: Weak<Mutex<HashMap<u64, RegistryEntry>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    DuplicateRequestId,
}

/// Why a cancellable query call did not produce a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryExecutionError {
    /// The call was cancelled before any of its SQL reached the server, so
    /// nothing ran.
    CancelledBeforeDispatch,
    /// SQL was already dispatched when the call was cancelled. The TDS client
    /// sent an ATTENTION and reported the cancellation, but the driver cannot
    /// witness the server's acknowledgement.
    Cancelled,
    Driver(String),
}

impl CancellationRegistry {
    pub fn register(&self, request_id: u64) -> Result<ExecutionLease, RegisterError> {
        let generation = self
            .next_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let control = Arc::new(QueryCancellation::new());
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if entries.contains_key(&request_id) {
            return Err(RegisterError::DuplicateRequestId);
        }
        entries.insert(
            request_id,
            RegistryEntry {
                generation,
                control: Arc::clone(&control),
            },
        );
        Ok(ExecutionLease {
            request_id,
            generation,
            control,
            entries: Arc::downgrade(&self.entries),
        })
    }

    /// Cancel a currently registered request. Unknown, late, and repeated
    /// notifications are harmless no-ops.
    pub fn cancel(&self, request_id: u64) -> bool {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(entry) = entries.get(&request_id) else {
            return false;
        };
        entry.control.cancel();
        true
    }

    /// Cancel every registered request. Used when the response channel is
    /// gone or the reader stops accepting input.
    pub fn cancel_all(&self) -> usize {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for entry in entries.values() {
            entry.control.cancel();
        }
        entries.len()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

impl QueryCancellation {
    fn new() -> Self {
        Self {
            parent: Mutex::new(Some(CancelHandle::new())),
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
            query_dispatched: AtomicBool::new(false),
        }
    }

    fn lock_parent(&self) -> std::sync::MutexGuard<'_, Option<CancelHandle>> {
        self.parent
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn cancel(&self) {
        let mut parent = self.lock_parent();
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
        if let Some(parent) = parent.take() {
            parent.cancel();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// A handle for one TDS statement. After cancellation it is already
    /// cancelled, so a statement that starts late stops at once.
    pub fn tds_child_handle(&self) -> CancelHandle {
        match self.lock_parent().as_ref() {
            Some(parent) => parent.child_handle(),
            None => {
                let parent = CancelHandle::new();
                let child = parent.child_handle();
                parent.cancel();
                child
            }
        }
    }

    /// Start one SQL statement within this request. The lock `cancel` takes
    /// makes the cancellation check and the dispatch marker atomic with
    /// respect to a notification arriving just before dispatch.
    pub fn begin_query_dispatch(&self) -> bool {
        let _parent = self.lock_parent();
        if self.is_cancelled() {
            return false;
        }
        self.query_dispatched.store(true, Ordering::Release);
        true
    }

    /// Mark a fully consumed statement complete. A batch cancelled between
    /// two statements is then a cancellation before the next dispatch.
    pub fn finish_query_dispatch(&self) {
        self.query_dispatched.store(false, Ordering::Release);
    }

    pub fn query_was_dispatched(&self) -> bool {
        self.query_dispatched.load(Ordering::Acquire)
    }

    pub fn cancelled_before_dispatch(&self) -> bool {
        self.is_cancelled() && !self.query_was_dispatched()
    }

    /// Whether a TDS error is the cancellation this request asked for.
    pub fn is_requested_cancellation(&self, error: &mssql_tds::error::Error) -> bool {
        self.is_cancelled() && matches!(error, mssql_tds::error::Error::OperationCancelledError(_))
    }
}

impl ExecutionLease {
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn control(&self) -> &QueryCancellation {
        &self.control
    }
}

impl Drop for ExecutionLease {
    fn drop(&mut self) {
        let Some(entries) = self.entries.upgrade() else {
            return;
        };
        let mut entries = entries.lock().unwrap_or_else(|error| error.into_inner());
        if entries
            .get(&self.request_id)
            .is_some_and(|entry| entry.generation == self.generation)
        {
            entries.remove(&self.request_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 활성_요청_id_중복은_새_실행을_등록하지_않는다() {
        // given
        let registry = CancellationRegistry::default();
        let first = registry.register(7).unwrap();
        let neighbor = registry.register(8).unwrap();

        // when
        let duplicate = registry.register(7);

        // then
        assert!(matches!(duplicate, Err(RegisterError::DuplicateRequestId)));
        assert_eq!(registry.len(), 2);
        assert!(!first.control().is_cancelled());
        assert!(!neighbor.control().is_cancelled());
    }

    #[test]
    fn 취소_알림은_같은_id의_요청에만_전달된다() {
        // given
        let registry = CancellationRegistry::default();
        let target = registry.register(17).unwrap();
        let neighbor = registry.register(18).unwrap();

        // when
        let first_cancel = registry.cancel(17);
        let repeated_cancel = registry.cancel(17);

        // then
        assert!(first_cancel);
        assert!(repeated_cancel);
        assert!(target.control().is_cancelled());
        assert!(!neighbor.control().is_cancelled());
    }

    #[test]
    fn 완료된_id의_늦은_취소는_아무_요청도_취소하지_않는다() {
        // given
        let registry = CancellationRegistry::default();
        let lease = registry.register(29).unwrap();
        drop(lease);

        // when
        let canceled = registry.cancel(29);

        // then
        assert!(!canceled);
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn 오래된_세대의_lease는_새_세대의_등록을_지우지_않는다() {
        // given
        let registry = CancellationRegistry::default();
        let stale = registry.register(31).unwrap();
        let request_id = stale.request_id;
        {
            let mut entries = registry
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            entries.remove(&request_id);
        }
        let current = registry.register(request_id).unwrap();
        let current_generation = current.generation;

        // when
        drop(stale);

        // then
        let entries = registry
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(
            entries.get(&request_id).map(|entry| entry.generation),
            Some(current_generation)
        );
        assert!(!current.control().is_cancelled());
    }

    #[test]
    fn 전체_취소는_등록된_모든_요청에_전달된다() {
        // given
        let registry = CancellationRegistry::default();
        let first = registry.register(51).unwrap();
        let second = registry.register(52).unwrap();

        // when
        let cancelled = registry.cancel_all();

        // then
        assert_eq!(cancelled, 2);
        assert!(first.control().is_cancelled());
        assert!(second.control().is_cancelled());
    }

    #[tokio::test]
    async fn 취소된_query_control은_tds_하위_handle에_같은_상태를_전달한다() {
        // given
        let registry = CancellationRegistry::default();
        let lease = registry.register(41).unwrap();
        let child = lease.control().tds_child_handle();

        // when
        assert!(registry.cancel(41));
        lease.control().cancelled().await;
        let late_child = lease.control().tds_child_handle();

        // then
        assert!(lease.control().cancelled_before_dispatch());
        drop((child, late_child));
    }

    #[test]
    fn 취소_오류는_취소를_요청한_요청에서만_요청된_취소로_인정된다() {
        // given
        let registry = CancellationRegistry::default();
        let lease = registry.register(42).unwrap();
        let cancelled =
            mssql_tds::error::Error::OperationCancelledError("request cancelled".to_string());
        let timeout = mssql_tds::error::Error::TimeoutError(
            mssql_tds::error::TimeoutErrorType::String("query".to_string()),
        );

        // when
        let before_cancel = lease.control().is_requested_cancellation(&cancelled);
        registry.cancel(42);

        // then
        assert!(!before_cancel);
        assert!(lease.control().is_requested_cancellation(&cancelled));
        assert!(!lease.control().is_requested_cancellation(&timeout));
    }

    #[test]
    fn 완료된_statement는_다음_batch_dispatch를_대기한다() {
        // given
        let registry = CancellationRegistry::default();
        let lease = registry.register(43).unwrap();
        assert!(lease.control().begin_query_dispatch());

        // when
        lease.control().finish_query_dispatch();
        registry.cancel(43);

        // then
        assert!(lease.control().cancelled_before_dispatch());
        assert!(!lease.control().query_was_dispatched());
        assert!(!lease.control().begin_query_dispatch());
    }

    #[test]
    fn dispatch된_statement_도중의_취소는_dispatch_전_취소가_아니다() {
        // given
        let registry = CancellationRegistry::default();
        let lease = registry.register(44).unwrap();
        assert!(lease.control().begin_query_dispatch());

        // when
        registry.cancel(44);

        // then
        assert!(lease.control().is_cancelled());
        assert!(lease.control().query_was_dispatched());
        assert!(!lease.control().cancelled_before_dispatch());
    }
}
