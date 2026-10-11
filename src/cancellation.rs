//! Request-scoped cancellation state shared by the stdin reader and query worker.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use mssql_tds::core::CancelHandle;

#[derive(Default)]
pub struct CancellationRegistry {
    entries: Arc<Mutex<HashMap<u64, RegistryEntry>>>,
    next_generation: AtomicU64,
}

struct RegistryEntry {
    generation: u64,
    control: Arc<QueryCancellation>,
}

/// Per-request handle and state. The registry owns the one-shot TDS parent;
/// the observer and TDS child share its cancellation token and ACK witness.
pub struct QueryCancellation {
    parent: Mutex<Option<CancelHandle>>,
    observer: CancelHandle,
    query_dispatched: AtomicBool,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryExecutionError {
    CancelledConfirmed,
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

    /// Cancel every currently registered execution. Used when the response
    /// channel is saturated and the reader must stop accepting more input.
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
        let parent = CancelHandle::new();
        let observer = parent.child_handle();
        Self {
            parent: Mutex::new(Some(parent)),
            observer,
            query_dispatched: AtomicBool::new(false),
        }
    }

    pub fn cancel(&self) {
        if let Some(parent) = self
            .parent
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            parent.cancel();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.observer.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.observer.cancelled().await;
    }

    pub fn tds_child_handle(&self) -> CancelHandle {
        self.observer.child_handle()
    }

    /// Start one SQL statement within this request. The same handle lock used
    /// by `cancel` makes the cancellation check and dispatch marker atomic
    /// with respect to a notification arriving just before dispatch.
    pub fn begin_query_dispatch(&self) -> bool {
        let _parent = self
            .parent
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.observer.is_cancelled() {
            return false;
        }
        self.query_dispatched.store(true, Ordering::Release);
        true
    }

    /// Mark a fully consumed statement complete. Batch RPCs can then report a
    /// cancellation between statements as confirmed before the next dispatch.
    pub fn finish_query_dispatch(&self) {
        self.query_dispatched.store(false, Ordering::Release);
    }

    pub fn query_was_dispatched(&self) -> bool {
        self.query_dispatched.load(Ordering::Acquire)
    }

    pub fn was_attention_acknowledged(&self) -> bool {
        self.observer.was_attention_acknowledged()
    }

    pub fn cancelled_before_dispatch(&self) -> bool {
        self.is_cancelled() && !self.query_was_dispatched()
    }

    pub fn confirms_tds_cancellation(&self, error: &mssql_tds::error::Error) -> bool {
        self.is_cancelled()
            && self.was_attention_acknowledged()
            && matches!(error, mssql_tds::error::Error::OperationCancelledError(_))
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
    fn 오래된_세대의_guard는_새_세대의_등록을_지우지_않는다() {
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

    #[tokio::test]
    async fn 취소된_query_control은_tds_하위_handle에_같은_상태를_전달한다() {
        // given
        let registry = CancellationRegistry::default();
        let lease = registry.register(41).unwrap();
        let child = lease.control().tds_child_handle();

        // when
        assert!(registry.cancel(41));
        child.cancelled().await;

        // then
        assert!(child.is_cancelled());
        assert!(lease.control().cancelled_before_dispatch());
        assert!(!child.was_attention_acknowledged());
    }

    #[test]
    fn 로컬_취소와_취소_오류만으로는_서버_확인을_주장하지_않는다() {
        // given
        let registry = CancellationRegistry::default();
        let lease = registry.register(42).unwrap();
        let error =
            mssql_tds::error::Error::OperationCancelledError("request cancelled".to_string());

        // when
        assert!(registry.cancel(42));

        // then
        assert!(!lease.control().was_attention_acknowledged());
        assert!(!lease.control().confirms_tds_cancellation(&error));
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
    }
}
