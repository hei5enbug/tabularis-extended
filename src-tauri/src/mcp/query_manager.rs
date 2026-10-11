use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{watch, Mutex, Semaphore};

use crate::ai_activity::{self, AiActivityEvent};
use crate::ai_approval::{self, PendingApproval, PollOutcome};
use crate::config::{
    self, AppConfig, DEFAULT_MCP_APPROVAL_MODE, DEFAULT_MCP_APPROVAL_TIMEOUT_SECONDS,
    DEFAULT_MCP_PREFLIGHT_EXPLAIN,
};
use crate::drivers::driver_trait::{DatabaseDriver, ManagedQueryFailure, ManagedQueryHandle};
use crate::models::{ConnectionParams, QueryResult, SavedConnection};

use super::sql_gate::validate_managed_select;

const MAX_QUEUE: usize = 16;
const MAX_SNAPSHOT_SLOTS: usize = 64;
const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_SNAPSHOT_BYTES: usize = 128 * 1024 * 1024;
const MAX_RETAINED_ROWS: usize = 10_000;
const SNAPSHOT_TTL: Duration = Duration::from_secs(15 * 60);
const CANCEL_GRACE: Duration = Duration::from_secs(5);
const MAX_MEMORY_JOBS: usize = 5_000;
const MAX_CURSORS: usize = 64;
const CURSOR_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_HISTORY_SQL_BYTES: usize = 4 * 1024;
const MAX_HISTORY_ITEMS: usize = 5_000;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryState {
    Queued,
    Running,
    CancelRequested,
    Succeeded,
    Failed,
    Cancelled,
}

impl QueryState {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::CancelRequested => "cancel_requested",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultSetMetadata {
    pub result_set_index: usize,
    pub columns: Vec<String>,
    pub stored_row_count: usize,
    pub truncated: bool,
    pub truncation_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryStatus {
    pub query_id: String,
    pub state: QueryState,
    pub connection_id: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub elapsed_ms: u64,
    pub result_available: bool,
    pub cancellable: bool,
    pub result_sets: Option<Vec<ResultSetMetadata>>,
    pub error: Option<QueryError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultPage {
    pub query_id: String,
    pub result_set_index: usize,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub offset: usize,
    pub next_offset: usize,
    pub has_more: bool,
    pub stored_row_count: usize,
    pub truncated: bool,
    pub truncation_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CancelResult {
    pub query_id: String,
    pub state: QueryState,
    pub accepted: bool,
    pub already_terminal: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryHistoryItem {
    pub query_id: String,
    pub connection_id: Option<String>,
    pub connection_name: Option<String>,
    pub query: Option<String>,
    pub state: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub elapsed_ms: u64,
    pub rows: Option<usize>,
    pub result_available: bool,
    pub cancellable: bool,
    pub error: Option<QueryError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryHistoryPage {
    pub items: Vec<QueryHistoryItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QueryFilter {
    pub connection_id: Option<String>,
    pub status: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct ManagedError {
    pub code: &'static str,
    pub message: &'static str,
    pub invalid_argument: bool,
}

impl ManagedError {
    fn invalid(message: &'static str) -> Self {
        Self {
            code: "INVALID_ARGUMENT",
            message,
            invalid_argument: true,
        }
    }

    fn domain(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            invalid_argument: false,
        }
    }
}

pub struct ManagedJobSpec {
    pub connection: SavedConnection,
    pub params: ConnectionParams,
    pub driver: Arc<dyn DatabaseDriver>,
    pub active_driver_ids: Option<Vec<String>>,
    pub config: AppConfig,
    pub query: String,
    pub max_rows: usize,
    pub timeout: Duration,
    pub fingerprint: String,
    pub session_id: String,
    pub client_hint: Option<String>,
    pub audit_enabled: bool,
    pub max_audit_entries: usize,
}

#[derive(Clone, Serialize)]
struct StoredResultSet {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    truncated: bool,
    truncation_reason: Option<String>,
}

struct JobRecord {
    status: QueryStatus,
    query: String,
    connection_name: String,
    audit: QueryAuditContext,
    fingerprint: String,
    created_at: tokio::time::Instant,
    started_at: Option<tokio::time::Instant>,
    completed_at: Option<tokio::time::Instant>,
    deadline: tokio::time::Instant,
    cancel_tx: watch::Sender<bool>,
    cancel_handle: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    result_sets: Option<Vec<StoredResultSet>>,
    snapshot_bytes: usize,
    retained_rows: usize,
    reserved: bool,
    cancel_requested_at: Option<tokio::time::Instant>,
}

#[derive(Clone)]
struct QueryAuditContext {
    session_id: String,
    client_hint: Option<String>,
    connection_id: String,
    connection_name: String,
    query: String,
    enabled: bool,
    max_entries: usize,
}

impl QueryAuditContext {
    fn from_spec(spec: &ManagedJobSpec) -> Self {
        Self {
            session_id: spec.session_id.clone(),
            client_hint: spec.client_hint.clone(),
            connection_id: spec.connection.id.clone(),
            connection_name: spec.connection.name.clone(),
            query: bounded_text(&spec.query, MAX_HISTORY_SQL_BYTES),
            enabled: spec.audit_enabled,
            max_entries: spec.max_audit_entries,
        }
    }
}

struct CursorRecord {
    anchor_created_at: String,
    anchor_query_id: String,
    filter_key: String,
    expires_at: tokio::time::Instant,
}

#[derive(Default)]
struct ManagerState {
    jobs: HashMap<String, JobRecord>,
    reserved_bytes: usize,
    cursors: HashMap<String, CursorRecord>,
}

pub struct QueryManager {
    state: Mutex<ManagerState>,
    process_slots: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl QueryManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ManagerState::default()),
            process_slots: Mutex::new(HashMap::new()),
        })
    }

    pub async fn start(
        self: &Arc<Self>,
        spec: ManagedJobSpec,
    ) -> Result<QueryStatus, ManagedError> {
        let now = tokio::time::Instant::now();
        let created_at = ai_activity::now_iso8601();
        let query_id = ai_activity::new_uuid();
        let audit = QueryAuditContext::from_spec(&spec);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let deadline = now + spec.timeout;
        let mut state = self.state.lock().await;
        reap_expired(&mut state, now);
        if state.reserved_bytes.saturating_add(MAX_SNAPSHOT_BYTES) > MAX_TOTAL_SNAPSHOT_BYTES
            || state
                .jobs
                .values()
                .filter(|job| job.reserved || job.result_sets.is_some())
                .count()
                >= MAX_SNAPSHOT_SLOTS
        {
            return Err(ManagedError::domain(
                "RESULT_CAPACITY_EXCEEDED",
                "No result snapshot capacity is available",
            ));
        }
        let queued_count = state
            .jobs
            .values()
            .filter(|job| job.status.state == QueryState::Queued)
            .count();
        if queued_count >= MAX_QUEUE {
            return Err(ManagedError::domain(
                "QUEUE_FULL",
                "The managed query queue is full",
            ));
        }
        prune_metadata(&mut state);
        if state.jobs.len() >= MAX_MEMORY_JOBS {
            return Err(ManagedError::domain(
                "QUEUE_FULL",
                "The managed query metadata capacity is full",
            ));
        }
        state.reserved_bytes += MAX_SNAPSHOT_BYTES;
        let status = QueryStatus {
            query_id: query_id.clone(),
            state: QueryState::Queued,
            connection_id: spec.connection.id.clone(),
            created_at,
            started_at: None,
            completed_at: None,
            elapsed_ms: 0,
            result_available: false,
            cancellable: true,
            result_sets: None,
            error: None,
        };
        state.jobs.insert(
            query_id.clone(),
            JobRecord {
                status: status.clone(),
                query: bounded_text(&spec.query, MAX_HISTORY_SQL_BYTES),
                connection_name: spec.connection.name.clone(),
                audit: audit.clone(),
                fingerprint: spec.fingerprint.clone(),
                created_at: now,
                started_at: None,
                completed_at: None,
                deadline,
                cancel_tx,
                cancel_handle: None,
                result_sets: None,
                snapshot_bytes: 0,
                retained_rows: 0,
                reserved: true,
                cancel_requested_at: None,
            },
        );
        drop(state);

        if audit.enabled {
            record_query_event(&audit, &query_id, "start", "queued", 0, None, None);
        }

        let manager = self.clone();
        tokio::spawn(async move {
            manager.run_job(query_id, spec, cancel_rx).await;
        });
        Ok(status)
    }

    async fn run_job(
        self: Arc<Self>,
        query_id: String,
        mut spec: ManagedJobSpec,
        mut cancel_rx: watch::Receiver<bool>,
    ) {
        let deadline = {
            let state = self.state.lock().await;
            let Some(job) = state.jobs.get(&query_id) else {
                return;
            };
            job.deadline
        };

        if let Err(failure) = self
            .refresh_connection(&mut spec, &mut cancel_rx, deadline)
            .await
        {
            self.finish_failure_path(&query_id, failure, &spec).await;
            return;
        }

        let mut approved_query = match self
            .wait_for_approval(&spec, &mut cancel_rx, deadline)
            .await
        {
            Ok(query) => query,
            Err(FailurePath::CancelledBeforeDispatch) => {
                self.finish_without_execution(&query_id, QueryState::Cancelled, None, &spec)
                    .await;
                return;
            }
            Err(FailurePath::TimedOut) => {
                self.finish_without_execution(
                    &query_id,
                    QueryState::Failed,
                    Some(QueryError {
                        code: "QUERY_TIMEOUT".to_string(),
                        message: "The query deadline expired before execution".to_string(),
                    }),
                    &spec,
                )
                .await;
                return;
            }
            Err(FailurePath::Failed(error)) => {
                self.finish_without_execution(&query_id, QueryState::Failed, Some(error), &spec)
                    .await;
                return;
            }
        };

        let approval_mode_before_refresh = spec
            .config
            .mcp_approval_mode
            .as_deref()
            .unwrap_or(DEFAULT_MCP_APPROVAL_MODE)
            .to_string();
        spec.query = approved_query;
        if let Err(failure) = self
            .refresh_connection(&mut spec, &mut cancel_rx, deadline)
            .await
        {
            self.finish_failure_path(&query_id, failure, &spec).await;
            return;
        }
        let current_approval_mode = spec
            .config
            .mcp_approval_mode
            .as_deref()
            .unwrap_or(DEFAULT_MCP_APPROVAL_MODE);
        if approval_required_after_refresh(&approval_mode_before_refresh, current_approval_mode) {
            approved_query = match self
                .wait_for_approval(&spec, &mut cancel_rx, deadline)
                .await
            {
                Ok(query) => query,
                Err(failure) => {
                    self.finish_failure_path(&query_id, failure, &spec).await;
                    return;
                }
            };
            spec.query = approved_query;
            if let Err(failure) = self
                .refresh_connection(&mut spec, &mut cancel_rx, deadline)
                .await
            {
                self.finish_failure_path(&query_id, failure, &spec).await;
                return;
            }
        }
        self.update_query_text(&query_id, &spec.query).await;

        let dialect = spec.driver.manifest().capabilities.sql_dialect;
        let Some(dialect) = dialect else {
            self.finish_without_execution(
                &query_id,
                QueryState::Failed,
                Some(QueryError {
                    code: "UNSUPPORTED_DIALECT".to_string(),
                    message: "The active driver does not declare a SQL dialect".to_string(),
                }),
                &spec,
            )
            .await;
            return;
        };
        if validate_managed_select(&spec.query, dialect).is_err() {
            self.finish_without_execution(
                &query_id,
                QueryState::Failed,
                Some(QueryError {
                    code: "INVALID_QUERY".to_string(),
                    message: "The approved query is not a single read-only SELECT".to_string(),
                }),
                &spec,
            )
            .await;
            return;
        }
        if config::is_connection_readonly(&spec.config, &spec.connection.id)
            && ai_activity::classify_query_kind(&spec.query) != "select"
        {
            self.finish_without_execution(
                &query_id,
                QueryState::Failed,
                Some(QueryError {
                    code: "READONLY_BLOCKED".to_string(),
                    message: "The query was blocked by the current read-only policy".to_string(),
                }),
                &spec,
            )
            .await;
            return;
        }

        let process_slot = {
            let mut slots = self.process_slots.lock().await;
            slots
                .entry(spec.driver.manifest().id.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(2)))
                .clone()
        };
        let process_permit = tokio::select! {
            permit = process_slot.acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => {
                    self.finish_without_execution(&query_id, QueryState::Failed, Some(QueryError {
                        code: "DRIVER_UNAVAILABLE".to_string(),
                        message: "The database driver is unavailable".to_string(),
                    }), &spec).await;
                    return;
                }
            },
            _ = wait_for_cancel(&mut cancel_rx) => {
                self.finish_without_execution(&query_id, QueryState::Cancelled, None, &spec).await;
                return;
            }
            _ = tokio::time::sleep_until(deadline) => {
                self.finish_without_execution(&query_id, QueryState::Failed, Some(QueryError {
                    code: "QUERY_TIMEOUT".to_string(),
                    message: "The query deadline expired before execution".to_string(),
                }), &spec).await;
                return;
            }
        };

        let approval_mode_before_queue = spec
            .config
            .mcp_approval_mode
            .as_deref()
            .unwrap_or(DEFAULT_MCP_APPROVAL_MODE)
            .to_string();
        if let Err(failure) = self
            .refresh_connection(&mut spec, &mut cancel_rx, deadline)
            .await
        {
            self.finish_failure_path(&query_id, failure, &spec).await;
            return;
        }
        let approval_mode_after_queue = spec
            .config
            .mcp_approval_mode
            .as_deref()
            .unwrap_or(DEFAULT_MCP_APPROVAL_MODE);
        if approval_required_after_refresh(&approval_mode_before_queue, approval_mode_after_queue) {
            match self
                .wait_for_approval(&spec, &mut cancel_rx, deadline)
                .await
            {
                Ok(query) => spec.query = query,
                Err(failure) => {
                    self.finish_failure_path(&query_id, failure, &spec).await;
                    return;
                }
            }
            if let Err(failure) = self
                .refresh_connection(&mut spec, &mut cancel_rx, deadline)
                .await
            {
                self.finish_failure_path(&query_id, failure, &spec).await;
                return;
            }
        }
        self.update_query_text(&query_id, &spec.query).await;
        let Some(queued_dialect) = spec.driver.manifest().capabilities.sql_dialect else {
            self.finish_without_execution(
                &query_id,
                QueryState::Failed,
                Some(QueryError {
                    code: "UNSUPPORTED_DIALECT".to_string(),
                    message: "The active driver does not declare a SQL dialect".to_string(),
                }),
                &spec,
            )
            .await;
            return;
        };
        if validate_managed_select(&spec.query, queued_dialect).is_err() {
            self.finish_without_execution(
                &query_id,
                QueryState::Failed,
                Some(QueryError {
                    code: "INVALID_QUERY".to_string(),
                    message: "The approved query is not a single read-only SELECT".to_string(),
                }),
                &spec,
            )
            .await;
            return;
        }

        self.mark_running(&query_id).await;
        let handle = spec
            .driver
            .start_managed_query(
                &spec.params,
                &spec.query,
                spec.max_rows.min(MAX_RETAINED_ROWS) as u32,
                None,
                cancel_rx.clone(),
                deadline,
            )
            .await;
        let handle = match handle {
            Ok(handle) => handle,
            Err(ManagedQueryFailure::Cancelled {
                server_termination_confirmed: false,
            }) => {
                self.finish_without_execution(&query_id, QueryState::Cancelled, None, &spec)
                    .await;
                return;
            }
            Err(ManagedQueryFailure::NotDispatched) => {
                self.finish_without_execution(&query_id, QueryState::Cancelled, None, &spec)
                    .await;
                return;
            }
            Err(ManagedQueryFailure::Transport(message)) if message == "QUERY_TIMEOUT" => {
                self.finish_without_execution(
                    &query_id,
                    QueryState::Failed,
                    Some(QueryError {
                        code: "QUERY_TIMEOUT".to_string(),
                        message: "The query deadline expired before execution".to_string(),
                    }),
                    &spec,
                )
                .await;
                return;
            }
            Err(error) => {
                let query_error = sanitize_driver_error(&error);
                self.finish_without_execution(
                    &query_id,
                    QueryState::Failed,
                    Some(query_error),
                    &spec,
                )
                .await;
                return;
            }
        };
        let ManagedQueryHandle {
            request_id: _request_id,
            process_generation: _process_generation,
            result,
            request_cancel,
        } = handle;
        self.attach_cancel_handle(&query_id, request_cancel.clone())
            .await;
        if *cancel_rx.borrow() {
            let _ = request_cancel();
        }
        let _process_permit = process_permit;
        tokio::pin!(result);

        let outcome = tokio::select! {
            value = &mut result => value,
            _ = tokio::time::sleep_until(deadline) => {
                let sent = request_cancel();
                self.mark_deadline_cancellation(&query_id, sent).await;
                let grace_deadline = tokio::time::Instant::now() + CANCEL_GRACE;
                match tokio::time::timeout_at(grace_deadline, &mut result).await {
                    Ok(value) => value,
                    Err(_) => {
                        self.mark_cancel_not_confirmed(&query_id).await;
                        result.await
                    }
                }
            }
            _ = wait_for_cancel(&mut cancel_rx) => {
                let grace_deadline = tokio::time::Instant::now() + CANCEL_GRACE;
                match tokio::time::timeout_at(grace_deadline, &mut result).await {
                    Ok(value) => value,
                    Err(_) => {
                        self.mark_cancel_not_confirmed(&query_id).await;
                        result.await
                    }
                }
            }
        };

        match outcome {
            Ok(result) => match snapshot_result(result, spec.max_rows) {
                Ok((sets, bytes, rows)) => {
                    self.finish_success(&query_id, sets, bytes, rows, &spec)
                        .await;
                }
                Err(code) => {
                    self.finish_without_execution(
                        &query_id,
                        QueryState::Failed,
                        Some(QueryError {
                            code: code.to_string(),
                            message: match code {
                                "RESULT_TOO_LARGE" => "The result exceeds the per-query byte limit",
                                _ => "The result could not be retained safely",
                            }
                            .to_string(),
                        }),
                        &spec,
                    )
                    .await;
                }
            },
            Err(ManagedQueryFailure::Cancelled {
                server_termination_confirmed: true,
            }) => {
                self.finish_without_execution(&query_id, QueryState::Cancelled, None, &spec)
                    .await;
            }
            Err(ManagedQueryFailure::NotDispatched) => {
                self.finish_without_execution(&query_id, QueryState::Cancelled, None, &spec)
                    .await;
            }
            Err(error) => {
                let query_error = sanitize_driver_error(&error);
                self.finish_without_execution(
                    &query_id,
                    QueryState::Failed,
                    Some(query_error),
                    &spec,
                )
                .await;
            }
        }
    }

    async fn wait_for_approval(
        &self,
        spec: &ManagedJobSpec,
        cancel_rx: &mut watch::Receiver<bool>,
        deadline: tokio::time::Instant,
    ) -> Result<String, FailurePath> {
        let mode = spec
            .config
            .mcp_approval_mode
            .as_deref()
            .unwrap_or(DEFAULT_MCP_APPROVAL_MODE);
        if mode != "all" {
            return Ok(spec.query.clone());
        }
        if !crate::heartbeat::is_alive() {
            return Err(FailurePath::Failed(QueryError {
                code: "HOST_UNAVAILABLE".to_string(),
                message: "The app is not available to approve this query".to_string(),
            }));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(FailurePath::TimedOut);
        }
        let timeout_secs = spec
            .config
            .mcp_approval_timeout_seconds
            .unwrap_or(DEFAULT_MCP_APPROVAL_TIMEOUT_SECONDS) as u64;
        let approval_id = ai_approval::new_approval_id();
        let (explain_plan, explain_error) = if spec
            .config
            .mcp_preflight_explain
            .unwrap_or(DEFAULT_MCP_PREFLIGHT_EXPLAIN)
        {
            let explain = super::preflight::preflight_explain(
                &spec.connection.params.driver,
                &spec.params,
                &spec.query,
                None,
            );
            let result = tokio::select! {
                result = explain => result,
                _ = wait_for_cancel(cancel_rx) => return Err(FailurePath::CancelledBeforeDispatch),
                _ = tokio::time::sleep_until(deadline) => return Err(FailurePath::TimedOut),
            };
            (result.plan, result.error)
        } else {
            (None, None)
        };
        let pending = PendingApproval {
            id: approval_id.clone(),
            created_at: ai_activity::now_iso8601(),
            session_id: spec.session_id.clone(),
            connection_id: spec.connection.id.clone(),
            connection_name: spec.connection.name.clone(),
            query: spec.query.clone(),
            query_kind: "select".to_string(),
            client_hint: spec.client_hint.clone(),
            explain_plan,
            explain_error,
        };
        ai_approval::write_pending(&pending).map_err(|_| {
            FailurePath::Failed(QueryError {
                code: "APPROVAL_UNAVAILABLE".to_string(),
                message: "The query approval request could not be saved".to_string(),
            })
        })?;
        let poll = ai_approval::poll_decision_with_liveness(
            &approval_id,
            timeout_secs,
            super::APPROVAL_POLL_INTERVAL_MS,
            crate::heartbeat::is_alive,
        );
        tokio::pin!(poll);
        let result = tokio::select! {
            result = &mut poll => {
                cleanup_pending_approval(&approval_id);
                result.map_err(|_| FailurePath::Failed(QueryError {
                    code: "APPROVAL_UNAVAILABLE".to_string(),
                    message: "The query approval service failed".to_string(),
                }))?
            },
            _ = wait_for_cancel(cancel_rx) => {
                cleanup_pending_approval(&approval_id);
                return Err(FailurePath::CancelledBeforeDispatch);
            },
            _ = tokio::time::sleep_until(deadline) => {
                cleanup_pending_approval(&approval_id);
                return Err(FailurePath::TimedOut);
            },
        };
        match result {
            PollOutcome::Decided(decision) if decision.decision == "approve" => {
                let query = decision
                    .edited_query
                    .as_deref()
                    .map(str::trim)
                    .filter(|edited| !edited.is_empty())
                    .unwrap_or(&spec.query)
                    .to_string();
                let active = super::find_connection(&spec.connection.id)
                    .map(|connection| {
                        super::connection_fingerprint(
                            &connection,
                            &spec.driver.manifest().id,
                            &spec.driver.manifest().version,
                        ) == spec.fingerprint
                    })
                    .unwrap_or(false);
                if !active {
                    return Err(FailurePath::Failed(QueryError {
                        code: "CONNECTION_ACCESS_CHANGED".to_string(),
                        message: "The connection changed while approval was pending".to_string(),
                    }));
                }
                Ok(query)
            }
            PollOutcome::Decided(_) => Err(FailurePath::Failed(QueryError {
                code: "APPROVAL_DENIED".to_string(),
                message: "The query was not approved".to_string(),
            })),
            PollOutcome::TimedOut => Err(FailurePath::Failed(QueryError {
                code: "APPROVAL_TIMEOUT".to_string(),
                message: "The query approval request expired".to_string(),
            })),
            PollOutcome::HostUnavailable => Err(FailurePath::Failed(QueryError {
                code: "HOST_UNAVAILABLE".to_string(),
                message: "The app is not available to approve this query".to_string(),
            })),
        }
    }

    async fn mark_running(&self, query_id: &str) {
        let mut state = self.state.lock().await;
        if let Some(job) = state.jobs.get_mut(query_id) {
            if job.status.state == QueryState::Queued {
                job.status.state = QueryState::Running;
                job.started_at = Some(tokio::time::Instant::now());
                job.status.started_at = Some(ai_activity::now_iso8601());
            }
        }
    }

    async fn attach_cancel_handle(
        &self,
        query_id: &str,
        cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        let mut state = self.state.lock().await;
        if let Some(job) = state.jobs.get_mut(query_id) {
            job.cancel_handle = Some(cancel);
        }
    }

    async fn update_query_text(&self, query_id: &str, query: &str) {
        let mut state = self.state.lock().await;
        if let Some(job) = state.jobs.get_mut(query_id) {
            job.query = bounded_text(query, MAX_HISTORY_SQL_BYTES);
            job.audit.query = job.query.clone();
        }
    }

    async fn refresh_connection(
        &self,
        spec: &mut ManagedJobSpec,
        cancel_rx: &mut watch::Receiver<bool>,
        deadline: tokio::time::Instant,
    ) -> Result<(), FailurePath> {
        spec.config = config::load_config_from_disk();
        spec.active_driver_ids = spec.config.active_external_drivers.clone();
        let verified_driver = tokio::select! {
            result = super::managed_tools::verify_connection_access(
                &spec.connection,
                &spec.config,
            ) => result.map_err(|_| {
                FailurePath::Failed(QueryError {
                    code: "CONNECTION_ACCESS_CHANGED".to_string(),
                    message: "The connection or active driver is no longer available".to_string(),
                })
            })?,
            _ = wait_for_cancel(cancel_rx) => return Err(FailurePath::CancelledBeforeDispatch),
            _ = tokio::time::sleep_until(deadline) => return Err(FailurePath::TimedOut),
        };
        let resolved = tokio::select! {
            result = super::resolve_db_driver(
                &spec.connection.id,
                spec.active_driver_ids.as_deref(),
            ) => result,
            _ = wait_for_cancel(cancel_rx) => return Err(FailurePath::CancelledBeforeDispatch),
            _ = tokio::time::sleep_until(deadline) => return Err(FailurePath::TimedOut),
        };
        let (connection, params, driver) = resolved.map_err(|_| {
            FailurePath::Failed(QueryError {
                code: "CONNECTION_ACCESS_CHANGED".to_string(),
                message: "The connection or active driver is no longer available".to_string(),
            })
        })?;
        if driver.manifest().id != verified_driver.manifest().id
            || driver.manifest().version != verified_driver.manifest().version
        {
            return Err(FailurePath::Failed(QueryError {
                code: "CONNECTION_ACCESS_CHANGED".to_string(),
                message: "The active driver changed before execution".to_string(),
            }));
        }
        let fingerprint = super::connection_fingerprint(
            &connection,
            &driver.manifest().id,
            &driver.manifest().version,
        );
        if fingerprint != spec.fingerprint {
            return Err(FailurePath::Failed(QueryError {
                code: "CONNECTION_ACCESS_CHANGED".to_string(),
                message: "The connection configuration or driver access changed".to_string(),
            }));
        }
        spec.connection = connection;
        spec.params = params;
        spec.driver = driver;
        Ok(())
    }

    async fn finish_failure_path(
        &self,
        query_id: &str,
        failure: FailurePath,
        spec: &ManagedJobSpec,
    ) {
        match failure {
            FailurePath::CancelledBeforeDispatch => {
                self.finish_without_execution(query_id, QueryState::Cancelled, None, spec)
                    .await;
            }
            FailurePath::TimedOut => {
                self.finish_without_execution(
                    query_id,
                    QueryState::Failed,
                    Some(QueryError {
                        code: "QUERY_TIMEOUT".to_string(),
                        message: "The query deadline expired before execution".to_string(),
                    }),
                    spec,
                )
                .await;
            }
            FailurePath::Failed(error) => {
                self.finish_without_execution(query_id, QueryState::Failed, Some(error), spec)
                    .await;
            }
        }
    }

    async fn mark_deadline_cancellation(&self, query_id: &str, sent: bool) {
        let mut state = self.state.lock().await;
        if let Some(job) = state.jobs.get_mut(query_id) {
            if job.status.state.is_terminal() {
                return;
            }
            job.status.state = QueryState::CancelRequested;
            job.cancel_requested_at = Some(tokio::time::Instant::now());
            job.status.error = Some(QueryError {
                code: if sent {
                    "QUERY_TIMEOUT".to_string()
                } else {
                    "CANCELLATION_UNSUPPORTED".to_string()
                },
                message: if sent {
                    "The query deadline expired; cancellation is pending".to_string()
                } else {
                    "The driver could not deliver native cancellation".to_string()
                },
            });
        }
    }

    async fn mark_cancel_not_confirmed(&self, query_id: &str) {
        let mut state = self.state.lock().await;
        if let Some(job) = state.jobs.get_mut(query_id) {
            if !job.status.state.is_terminal() {
                job.status.state = QueryState::CancelRequested;
                job.status.error = Some(QueryError {
                    code: "CANCEL_NOT_CONFIRMED".to_string(),
                    message: "The driver has not confirmed that the query stopped".to_string(),
                });
            }
        }
    }

    async fn finish_without_execution(
        &self,
        query_id: &str,
        state_value: QueryState,
        error: Option<QueryError>,
        _spec: &ManagedJobSpec,
    ) {
        let event = {
            let mut state = self.state.lock().await;
            let (event, release_reservation) = {
                let Some(job) = state.jobs.get_mut(query_id) else {
                    return;
                };
                if job.status.state.is_terminal() {
                    return;
                }
                job.status.state = state_value.clone();
                job.status.error = error.clone();
                job.status.cancellable = false;
                job.status.completed_at = Some(ai_activity::now_iso8601());
                job.completed_at = Some(tokio::time::Instant::now());
                let release_reservation = job.reserved;
                job.reserved = false;
                update_elapsed(job);
                ((job.status.clone(), job.audit.clone()), release_reservation)
            };
            if release_reservation {
                state.reserved_bytes = state.reserved_bytes.saturating_sub(MAX_SNAPSHOT_BYTES);
            }
            Some(event)
        };
        if let Some((status, audit)) = event {
            if audit.enabled {
                record_query_event(
                    &audit,
                    query_id,
                    "terminal",
                    status.state.as_str(),
                    status.elapsed_ms,
                    status.error.as_ref(),
                    None,
                );
            }
        }
    }

    async fn finish_success(
        &self,
        query_id: &str,
        result_sets: Vec<StoredResultSet>,
        bytes: usize,
        rows: usize,
        spec: &ManagedJobSpec,
    ) {
        let (status_for_event, audit) = {
            let mut state = self.state.lock().await;
            let Some(existing) = state.jobs.get(query_id) else {
                return;
            };
            if existing.status.state.is_terminal() {
                return;
            }
            let next_reserved_bytes = state
                .reserved_bytes
                .saturating_sub(MAX_SNAPSHOT_BYTES)
                .saturating_add(bytes);
            if bytes > MAX_SNAPSHOT_BYTES || next_reserved_bytes > MAX_TOTAL_SNAPSHOT_BYTES {
                drop(state);
                self.finish_without_execution(
                    query_id,
                    QueryState::Failed,
                    Some(QueryError {
                        code: "RESULT_CAPACITY_EXCEEDED".to_string(),
                        message: "The result snapshot budget is exhausted".to_string(),
                    }),
                    spec,
                )
                .await;
                return;
            }
            state.reserved_bytes = next_reserved_bytes;
            let job = state.jobs.get_mut(query_id).expect("job checked above");
            job.reserved = false;
            job.snapshot_bytes = bytes;
            job.result_sets = Some(result_sets);
            job.status.state = QueryState::Succeeded;
            job.status.error = None;
            job.status.cancellable = false;
            job.status.result_available = true;
            job.status.completed_at = Some(ai_activity::now_iso8601());
            job.completed_at = Some(tokio::time::Instant::now());
            job.retained_rows = rows;
            job.status.result_sets = job.result_sets.as_ref().map(|sets| result_metadata(sets));
            update_elapsed(job);
            (job.status.clone(), job.audit.clone())
        };
        if audit.enabled {
            record_query_event(
                &audit,
                query_id,
                "terminal",
                "succeeded",
                status_for_event.elapsed_ms,
                None,
                Some(rows),
            );
        }
    }

    pub async fn status(&self, query_id: &str) -> Result<QueryStatus, ManagedError> {
        let mut state = self.state.lock().await;
        reap_expired(&mut state, tokio::time::Instant::now());
        let job = state.jobs.get_mut(query_id).ok_or_else(|| {
            ManagedError::domain("QUERY_NOT_FOUND", "The query is not owned by this server")
        })?;
        update_elapsed(job);
        Ok(job.status.clone())
    }

    pub async fn verify_fingerprint(
        &self,
        query_id: &str,
        fingerprint: &str,
    ) -> Result<(), ManagedError> {
        let state = self.state.lock().await;
        let job = state.jobs.get(query_id).ok_or_else(|| {
            ManagedError::domain("QUERY_NOT_FOUND", "The query is not owned by this server")
        })?;
        if job.fingerprint != fingerprint {
            return Err(ManagedError::domain(
                "CONNECTION_ACCESS_CHANGED",
                "The connection configuration or access changed",
            ));
        }
        Ok(())
    }

    pub async fn fetch(
        &self,
        query_id: &str,
        result_set_index: usize,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, ManagedError> {
        if limit == 0 || limit > 1_000 {
            return Err(ManagedError::invalid("limit must be between 1 and 1000"));
        }
        let mut state = self.state.lock().await;
        reap_expired(&mut state, tokio::time::Instant::now());
        let job = state.jobs.get(query_id).ok_or_else(|| {
            ManagedError::domain("QUERY_NOT_FOUND", "The query is not owned by this server")
        })?;
        if !job.status.state.is_terminal() {
            return Err(ManagedError::domain(
                "RESULT_PENDING",
                "The query result is not complete",
            ));
        }
        let sets = job.result_sets.as_ref().ok_or_else(|| {
            ManagedError::domain("RESULT_EXPIRED", "The query result is unavailable")
        })?;
        let set = sets.get(result_set_index).ok_or_else(|| {
            ManagedError::invalid("result_set_index is outside the stored result")
        })?;
        if offset > usize::MAX - limit {
            return Err(ManagedError::invalid(
                "offset is outside the supported range",
            ));
        }
        let start = offset.min(set.rows.len());
        let end = start.saturating_add(limit).min(set.rows.len());
        let rows = set.rows[start..end].to_vec();
        Ok(ResultPage {
            query_id: query_id.to_string(),
            result_set_index,
            columns: set.columns.clone(),
            rows,
            offset,
            next_offset: end,
            has_more: end < set.rows.len(),
            stored_row_count: set.rows.len(),
            truncated: set.truncated,
            truncation_reason: set.truncation_reason.clone(),
        })
    }

    pub async fn cancel(&self, query_id: &str) -> Result<CancelResult, ManagedError> {
        let (cancel_tx, cancel_handle, current_state, queued_terminal) = {
            let mut state = self.state.lock().await;
            reap_expired(&mut state, tokio::time::Instant::now());
            let (branch, release_reservation) = {
                let job = state.jobs.get_mut(query_id).ok_or_else(|| {
                    ManagedError::domain("QUERY_NOT_FOUND", "The query is not owned by this server")
                })?;
                if job.status.state.is_terminal() {
                    return Ok(CancelResult {
                        query_id: query_id.to_string(),
                        state: job.status.state.clone(),
                        accepted: false,
                        already_terminal: true,
                    });
                }
                if job.status.state == QueryState::Queued {
                    let cancel_tx = job.cancel_tx.clone();
                    let audit = job.audit.clone();
                    job.status.state = QueryState::Cancelled;
                    job.status.cancellable = false;
                    job.status.completed_at = Some(ai_activity::now_iso8601());
                    job.completed_at = Some(tokio::time::Instant::now());
                    let release_reservation = job.reserved;
                    job.reserved = false;
                    update_elapsed(job);
                    let status = job.status.clone();
                    let _ = cancel_tx.send(true);
                    (
                        (cancel_tx, None, status.state.clone(), Some((status, audit))),
                        release_reservation,
                    )
                } else {
                    if job.status.state == QueryState::CancelRequested {
                        return Ok(CancelResult {
                            query_id: query_id.to_string(),
                            state: QueryState::CancelRequested,
                            accepted: true,
                            already_terminal: false,
                        });
                    }
                    job.status.state = QueryState::CancelRequested;
                    job.cancel_requested_at = Some(tokio::time::Instant::now());
                    (
                        (
                            job.cancel_tx.clone(),
                            job.cancel_handle.clone(),
                            job.status.state.clone(),
                            None,
                        ),
                        false,
                    )
                }
            };
            if release_reservation {
                state.reserved_bytes = state.reserved_bytes.saturating_sub(MAX_SNAPSHOT_BYTES);
            }
            branch
        };
        if let Some((status, audit)) = queued_terminal {
            if audit.enabled {
                record_query_event(
                    &audit,
                    query_id,
                    "terminal",
                    "cancelled",
                    status.elapsed_ms,
                    None,
                    None,
                );
            }
            return Ok(CancelResult {
                query_id: query_id.to_string(),
                state: QueryState::Cancelled,
                accepted: true,
                already_terminal: false,
            });
        }
        let delivered = cancel_handle
            .as_ref()
            .map(|cancel| cancel())
            .unwrap_or(true);
        if !delivered {
            let mut state = self.state.lock().await;
            if let Some(job) = state.jobs.get_mut(query_id) {
                if job.status.state == QueryState::CancelRequested {
                    job.status.state = QueryState::Running;
                    job.cancel_requested_at = None;
                    job.status.error = Some(QueryError {
                        code: "CANCELLATION_UNSUPPORTED".to_string(),
                        message: "The active driver cannot deliver native cancellation".to_string(),
                    });
                }
                if job.status.state.is_terminal() {
                    return Ok(CancelResult {
                        query_id: query_id.to_string(),
                        state: job.status.state.clone(),
                        accepted: false,
                        already_terminal: true,
                    });
                }
            }
            return Err(ManagedError::domain(
                "CANCELLATION_UNSUPPORTED",
                "The active driver cannot deliver native cancellation",
            ));
        }
        let _ = cancel_tx.send(true);
        let state_value = self
            .status(query_id)
            .await
            .map(|status| status.state)
            .unwrap_or(current_state);
        Ok(CancelResult {
            query_id: query_id.to_string(),
            state: state_value,
            accepted: true,
            already_terminal: false,
        })
    }

    pub async fn release(&self, query_id: &str) -> Result<Value, ManagedError> {
        let mut state = self.state.lock().await;
        reap_expired(&mut state, tokio::time::Instant::now());
        let (released, snapshot_bytes) = {
            let job = state.jobs.get_mut(query_id).ok_or_else(|| {
                ManagedError::domain("QUERY_NOT_FOUND", "The query is not owned by this server")
            })?;
            if !job.status.state.is_terminal() {
                return Err(ManagedError::domain(
                    "RESULT_PENDING",
                    "An active query result cannot be released",
                ));
            }
            let released = job.result_sets.take().is_some();
            job.status.result_available = false;
            job.status.result_sets = None;
            let snapshot_bytes = std::mem::take(&mut job.snapshot_bytes);
            (released, snapshot_bytes)
        };
        state.reserved_bytes = state.reserved_bytes.saturating_sub(snapshot_bytes);
        Ok(json!({
            "query_id": query_id,
            "released": released,
            "result_available": false
        }))
    }

    pub async fn list_history(
        &self,
        filter: QueryFilter,
        persisted_events: Vec<AiActivityEvent>,
        cursor: Option<&str>,
    ) -> Result<QueryHistoryPage, ManagedError> {
        let page_limit = filter.limit.clamp(1, 200);
        let filter_value = json!({
            "connection_id": &filter.connection_id,
            "status": &filter.status,
            "since": &filter.since,
            "until": &filter.until,
            "limit": page_limit
        });
        let filter_key = serde_json::to_string(&filter_value).unwrap_or_default();
        let mut state = self.state.lock().await;
        reap_expired(&mut state, tokio::time::Instant::now());
        let anchor = if let Some(cursor) = cursor {
            let token = state
                .cursors
                .remove(cursor)
                .filter(|record| {
                    record.expires_at > tokio::time::Instant::now()
                        && record.filter_key == filter_key
                })
                .ok_or_else(|| ManagedError::invalid("cursor is invalid or expired"))?;
            Some((token.anchor_created_at, token.anchor_query_id))
        } else {
            None
        };
        let mut items = persisted_history_items(persisted_events);
        for (query_id, job) in &state.jobs {
            let mut status = job.status.clone();
            update_elapsed_record(
                &mut status,
                job.started_at,
                job.created_at,
                job.completed_at,
            );
            let current = QueryHistoryItem {
                query_id: query_id.clone(),
                connection_id: Some(status.connection_id.clone()),
                connection_name: Some(job.connection_name.clone()),
                query: Some(job.query.clone()),
                state: status.state.as_str().to_string(),
                created_at: status.created_at.clone(),
                started_at: status.started_at.clone(),
                completed_at: status.completed_at.clone(),
                elapsed_ms: status.elapsed_ms,
                rows: job.completed_at.map(|_| job.retained_rows),
                result_available: job.result_sets.is_some(),
                cancellable: status.cancellable,
                error: status.error.clone(),
            };
            items.retain(|item| item.query_id != *query_id);
            items.push(current);
        }
        items.retain(|item| history_matches(item, &filter));
        items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.query_id.cmp(&a.query_id))
        });
        items.truncate(MAX_HISTORY_ITEMS);
        if let Some((anchor_created_at, anchor_query_id)) = anchor {
            items.retain(|item| {
                item.created_at < anchor_created_at
                    || (item.created_at == anchor_created_at && item.query_id < anchor_query_id)
            });
        }
        let has_more = items.len() > page_limit;
        let page = items.iter().take(page_limit).cloned().collect::<Vec<_>>();
        let next_cursor = if has_more {
            let last = page.last().expect("a nonempty page has a last item");
            let cursor = ai_activity::new_uuid();
            if state.cursors.len() >= MAX_CURSORS {
                if let Some(oldest) = state
                    .cursors
                    .iter()
                    .min_by_key(|(_, record)| record.expires_at)
                    .map(|(key, _)| key.clone())
                {
                    state.cursors.remove(&oldest);
                }
            }
            state.cursors.insert(
                cursor.clone(),
                CursorRecord {
                    anchor_created_at: last.created_at.clone(),
                    anchor_query_id: last.query_id.clone(),
                    filter_key,
                    expires_at: tokio::time::Instant::now() + CURSOR_TTL,
                },
            );
            Some(cursor)
        } else {
            None
        };
        Ok(QueryHistoryPage {
            items: page,
            next_cursor,
        })
    }

    pub async fn shutdown(&self) {
        let jobs = {
            let state = self.state.lock().await;
            state
                .jobs
                .values()
                .filter(|job| !job.status.state.is_terminal())
                .map(|job| (job.cancel_tx.clone(), job.cancel_handle.clone()))
                .collect::<Vec<_>>()
        };
        for (cancel_tx, cancel_handle) in jobs {
            let _ = cancel_tx.send(true);
            if let Some(cancel) = cancel_handle {
                let _ = cancel();
            }
        }
        let grace_deadline = tokio::time::Instant::now() + CANCEL_GRACE;
        loop {
            let active = {
                let state = self.state.lock().await;
                state
                    .jobs
                    .values()
                    .any(|job| !job.status.state.is_terminal())
            };
            if !active || tokio::time::Instant::now() >= grace_deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let unfinished = {
            let mut state = self.state.lock().await;
            let mut unfinished = Vec::new();
            for (query_id, job) in &mut state.jobs {
                if job.status.state.is_terminal() {
                    continue;
                }
                job.status.state = QueryState::CancelRequested;
                job.status.cancellable = false;
                job.status.error = Some(QueryError {
                    code: "CANCEL_NOT_CONFIRMED".to_string(),
                    message: "The server is shutting down before the query stopped".to_string(),
                });
                job.status.completed_at = Some(ai_activity::now_iso8601());
                job.completed_at = Some(tokio::time::Instant::now());
                job.cancel_requested_at = Some(tokio::time::Instant::now());
                update_elapsed(job);
                unfinished.push((query_id.clone(), job.status.elapsed_ms, job.audit.clone()));
            }
            unfinished
        };
        for (query_id, elapsed_ms, audit) in unfinished {
            if audit.enabled {
                let error = QueryError {
                    code: "CANCEL_NOT_CONFIRMED".to_string(),
                    message: "The server is shutting down before the query stopped".to_string(),
                };
                record_query_event(
                    &audit,
                    &query_id,
                    "terminal",
                    "unknown",
                    elapsed_ms,
                    Some(&error),
                    None,
                );
            }
        }
    }
}

enum FailurePath {
    CancelledBeforeDispatch,
    TimedOut,
    Failed(QueryError),
}

async fn wait_for_cancel(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow() {
            return;
        }
        if cancel.changed().await.is_err() {
            return;
        }
    }
}

fn update_elapsed(job: &mut JobRecord) {
    update_elapsed_record(
        &mut job.status,
        job.started_at,
        job.created_at,
        job.completed_at,
    );
}

fn update_elapsed_record(
    status: &mut QueryStatus,
    started_at: Option<tokio::time::Instant>,
    created_at: tokio::time::Instant,
    completed_at: Option<tokio::time::Instant>,
) {
    let from = started_at.unwrap_or(created_at);
    let to = completed_at.unwrap_or_else(tokio::time::Instant::now);
    status.elapsed_ms = to.saturating_duration_since(from).as_millis() as u64;
}

fn approval_required_after_refresh(previous_mode: &str, current_mode: &str) -> bool {
    current_mode == "all" && previous_mode != "all"
}

fn reap_expired(state: &mut ManagerState, now: tokio::time::Instant) {
    for job in state.jobs.values_mut() {
        if job
            .completed_at
            .map(|completed| now.saturating_duration_since(completed) >= SNAPSHOT_TTL)
            .unwrap_or(false)
            && job.result_sets.take().is_some()
        {
            state.reserved_bytes = state.reserved_bytes.saturating_sub(job.snapshot_bytes);
            job.snapshot_bytes = 0;
            job.status.result_available = false;
            job.status.result_sets = None;
        }
    }
    state.cursors.retain(|_, cursor| cursor.expires_at > now);
}

fn prune_metadata(state: &mut ManagerState) {
    if state.jobs.len() < MAX_MEMORY_JOBS {
        return;
    }
    let mut completed = state
        .jobs
        .iter()
        .filter(|(_, job)| job.status.state.is_terminal())
        .filter_map(|(id, job)| job.completed_at.map(|at| (id.clone(), at)))
        .collect::<Vec<_>>();
    completed.sort_by_key(|(_, at)| *at);
    for (id, _) in completed
        .into_iter()
        .take(state.jobs.len() - MAX_MEMORY_JOBS + 1)
    {
        if let Some(job) = state.jobs.remove(&id) {
            state.reserved_bytes = state
                .reserved_bytes
                .saturating_sub(job.snapshot_bytes)
                .saturating_sub(if job.reserved { MAX_SNAPSHOT_BYTES } else { 0 });
        }
        if state.jobs.len() < MAX_MEMORY_JOBS {
            break;
        }
    }
}

fn snapshot_result(
    result: QueryResult,
    requested_rows: usize,
) -> Result<(Vec<StoredResultSet>, usize, usize), &'static str> {
    let row_limit = requested_rows.min(MAX_RETAINED_ROWS);
    let mut queue = std::collections::VecDeque::from([result]);
    let mut sets = Vec::new();
    let mut retained_rows = 0usize;
    let mut estimated_bytes = 2usize;
    while let Some(mut result) = queue.pop_front() {
        if let Some(additional) = result.additional_results.take() {
            queue.extend(additional);
        }
        let result_row_count = result.rows.len();
        let column_bytes = serialized_len(&result.columns)?;
        estimated_bytes = estimated_bytes
            .saturating_add(column_bytes)
            .saturating_add(96);
        if estimated_bytes > MAX_SNAPSHOT_BYTES {
            return Err("RESULT_TOO_LARGE");
        }
        let mut rows = Vec::new();
        let mut truncated = result.truncated;
        let mut truncation_reason = result.truncated.then(|| "driver_limit".to_string());
        for row in result.rows {
            if retained_rows >= row_limit {
                truncated = true;
                truncation_reason = Some("max_rows".to_string());
                break;
            }
            let row_bytes = serialized_len(&row)?;
            estimated_bytes = estimated_bytes.saturating_add(row_bytes).saturating_add(2);
            if estimated_bytes > MAX_SNAPSHOT_BYTES {
                return Err("RESULT_TOO_LARGE");
            }
            rows.push(row);
            retained_rows += 1;
        }
        if result_row_count > rows.len() && retained_rows >= row_limit {
            truncated = true;
            truncation_reason = Some("max_rows".to_string());
        }
        sets.push(StoredResultSet {
            columns: result.columns,
            rows,
            truncated,
            truncation_reason,
        });
    }
    let bytes = serialized_len(&sets)?;
    if bytes > MAX_SNAPSHOT_BYTES {
        return Err("RESULT_TOO_LARGE");
    }
    Ok((sets, bytes, retained_rows))
}

fn serialized_len<T: Serialize>(value: &T) -> Result<usize, &'static str> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0 = self.0.saturating_add(buf.len());
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(|_| "RESULT_TOO_LARGE")?;
    Ok(counter.0)
}

fn result_metadata(sets: &[StoredResultSet]) -> Vec<ResultSetMetadata> {
    sets.iter()
        .enumerate()
        .map(|(result_set_index, set)| ResultSetMetadata {
            result_set_index,
            columns: set.columns.clone(),
            stored_row_count: set.rows.len(),
            truncated: set.truncated,
            truncation_reason: set.truncation_reason.clone(),
        })
        .collect()
}

fn sanitize_driver_error(error: &ManagedQueryFailure) -> QueryError {
    match error {
        ManagedQueryFailure::Remote { code, .. } => QueryError {
            code: if *code == -32800 {
                "QUERY_CANCELLED_UNCONFIRMED".to_string()
            } else {
                "QUERY_FAILED".to_string()
            },
            message: "The database rejected the query".to_string(),
        },
        ManagedQueryFailure::Transport(message) if message == "CANCELLATION_UNSUPPORTED" => {
            QueryError {
                code: "CANCELLATION_UNSUPPORTED".to_string(),
                message: "The active driver does not support native query cancellation".to_string(),
            }
        }
        ManagedQueryFailure::Transport(_) => QueryError {
            code: "DRIVER_UNAVAILABLE".to_string(),
            message: "The database driver did not complete the query".to_string(),
        },
        ManagedQueryFailure::NotDispatched => QueryError {
            code: "QUERY_CANCELLED".to_string(),
            message: "The query was cancelled before it was sent to the database".to_string(),
        },
        ManagedQueryFailure::Cancelled {
            server_termination_confirmed: true,
        } => QueryError {
            code: "QUERY_CANCELLED".to_string(),
            message: "The query was cancelled".to_string(),
        },
        ManagedQueryFailure::Cancelled {
            server_termination_confirmed: false,
        } => QueryError {
            code: "CANCEL_NOT_CONFIRMED".to_string(),
            message: "The driver did not confirm that the query stopped".to_string(),
        },
    }
}

fn history_matches(item: &QueryHistoryItem, filter: &QueryFilter) -> bool {
    if filter
        .connection_id
        .as_deref()
        .map(|id| item.connection_id.as_deref() != Some(id))
        .unwrap_or(false)
    {
        return false;
    }
    if filter
        .status
        .as_deref()
        .map(|status| item.state != status)
        .unwrap_or(false)
    {
        return false;
    }
    if filter.since.is_some() || filter.until.is_some() {
        let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(&item.created_at) else {
            return false;
        };
        if let Some(since) = filter.since.as_deref() {
            let Ok(since) = chrono::DateTime::parse_from_rfc3339(since) else {
                return false;
            };
            if created_at < since {
                return false;
            }
        }
        if let Some(until) = filter.until.as_deref() {
            let Ok(until) = chrono::DateTime::parse_from_rfc3339(until) else {
                return false;
            };
            if created_at > until {
                return false;
            }
        }
    }
    true
}

fn persisted_history_items(events: Vec<AiActivityEvent>) -> Vec<QueryHistoryItem> {
    let mut unique = HashSet::new();
    let mut grouped: HashMap<String, Vec<AiActivityEvent>> = HashMap::new();
    let mut legacy = Vec::new();
    for event in events {
        if !unique.insert(event.id.clone()) {
            continue;
        }
        if let Some(query_id) = event.query_id.clone() {
            grouped.entry(query_id).or_default().push(event);
        } else if event.tool == "run_query" {
            legacy.push(QueryHistoryItem {
                query_id: event.id,
                connection_id: event.connection_id,
                connection_name: event.connection_name,
                query: event.query,
                state: if event.status == "success" {
                    "succeeded".to_string()
                } else {
                    "failed".to_string()
                },
                created_at: event.timestamp.clone(),
                started_at: Some(event.timestamp.clone()),
                completed_at: Some(event.timestamp),
                elapsed_ms: event.duration_ms,
                rows: event.rows,
                result_available: false,
                cancellable: false,
                error: event.error.map(|_| QueryError {
                    code: "QUERY_FAILED".to_string(),
                    message: "The legacy query failed".to_string(),
                }),
            });
        }
    }
    for (query_id, mut events) in grouped {
        events.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        let start = events
            .iter()
            .find(|event| event.event_phase.as_deref() == Some("start"));
        let terminal = events
            .iter()
            .rev()
            .find(|event| event.event_phase.as_deref() == Some("terminal"));
        let latest = terminal.or(start).or_else(|| events.last());
        if let Some(event) = latest {
            let state = if terminal.is_some() {
                event.status.clone()
            } else {
                "unknown".to_string()
            };
            legacy.push(QueryHistoryItem {
                query_id,
                connection_id: event.connection_id.clone(),
                connection_name: event.connection_name.clone(),
                query: event.query.clone(),
                state,
                created_at: start
                    .map(|event| event.timestamp.clone())
                    .unwrap_or_else(|| event.timestamp.clone()),
                started_at: start.map(|event| event.timestamp.clone()),
                completed_at: terminal.map(|event| event.timestamp.clone()),
                elapsed_ms: terminal.map(|event| event.duration_ms).unwrap_or(0),
                rows: terminal.and_then(|event| event.rows),
                result_available: false,
                cancellable: false,
                error: terminal.and_then(|event| {
                    event.error.as_ref().map(|_| QueryError {
                        code: "QUERY_FAILED".to_string(),
                        message: "The query failed".to_string(),
                    })
                }),
            });
        }
    }
    legacy
}

fn record_query_event(
    audit: &QueryAuditContext,
    query_id: &str,
    phase: &str,
    status: &str,
    duration_ms: u64,
    error: Option<&QueryError>,
    rows: Option<usize>,
) {
    let event = build_query_event(audit, query_id, phase, status, duration_ms, error, rows);
    let max_entries = audit.max_entries.clamp(1, 5_000);
    if let Err(error) = ai_activity::append_and_rotate(&event, max_entries) {
        log::warn!("Could not append managed-query history event: {error}");
    }
}

fn build_query_event(
    audit: &QueryAuditContext,
    query_id: &str,
    phase: &str,
    status: &str,
    duration_ms: u64,
    error: Option<&QueryError>,
    rows: Option<usize>,
) -> AiActivityEvent {
    AiActivityEvent {
        id: ai_activity::new_uuid(),
        session_id: audit.session_id.clone(),
        timestamp: ai_activity::now_iso8601(),
        tool: "start_query".to_string(),
        connection_id: Some(audit.connection_id.clone()),
        connection_name: Some(audit.connection_name.clone()),
        query: Some(audit.query.clone()),
        query_kind: Some("select".to_string()),
        duration_ms,
        status: status.to_string(),
        rows,
        error: error.map(|error| format!("{}: {}", error.code, error.message)),
        client_hint: audit.client_hint.clone(),
        approval_id: None,
        query_id: Some(query_id.to_string()),
        event_phase: Some(phase.to_string()),
    }
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn cleanup_pending_approval(approval_id: &str) {
    let directory = crate::paths::get_app_config_dir().join("pending_approvals");
    cleanup_pending_approval_in(&directory, approval_id);
}

fn cleanup_pending_approval_in(directory: &std::path::Path, approval_id: &str) {
    let _ = std::fs::remove_file(directory.join(format!("{approval_id}.pending.json")));
    let _ = std::fs::remove_file(directory.join(format!("{approval_id}.decision.json")));
}

fn manager() -> Arc<QueryManager> {
    static MANAGER: OnceLock<Arc<QueryManager>> = OnceLock::new();
    MANAGER.get_or_init(QueryManager::new).clone()
}

pub fn global_manager() -> Arc<QueryManager> {
    manager()
}

#[cfg(test)]
#[path = "query_manager_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "query_lifecycle_tests.rs"]
mod lifecycle_tests;
