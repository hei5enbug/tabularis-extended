//! Query history contract for service jobs.
//!
//! Every job records a start event and a terminal event under the same job
//! id. The store itself is supplied by the host (`platform::history`), which
//! owns the file format, the privacy switch and the retention settings; the
//! runtime only reports job lifecycle events and reads history back through
//! this trait.

use super::{
    contracts::{ErrorCode, JobStatus, ServiceResponse},
    error::ServiceResult,
    storage::now_ms,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MAX_LIST_LIMIT: usize = 200;
const MAX_QUERY_BYTES: usize = 64 * 1024;

/// What a job reports about itself when it starts.
#[derive(Clone, Debug)]
pub struct HistoryMeta {
    pub job_id: String,
    pub operation: String,
    pub connection_id: Option<String>,
    pub query: Option<String>,
    pub started_ms: u64,
}

impl HistoryMeta {
    pub fn new(
        job_id: &str,
        operation: &str,
        connection_id: Option<&str>,
        query: Option<&str>,
    ) -> Self {
        Self {
            job_id: job_id.into(),
            operation: operation.into(),
            connection_id: connection_id.map(str::to_owned),
            query: query.map(truncate_query),
            started_ms: now_ms(),
        }
    }
}

fn truncate_query(query: &str) -> String {
    let mut end = query.len().min(MAX_QUERY_BYTES);
    while !query.is_char_boundary(end) {
        end -= 1;
    }
    query[..end].to_owned()
}

#[derive(Clone, Debug, Default)]
pub struct HistoryQuery {
    pub connection_id: Option<String>,
    pub status: Option<JobStatus>,
    pub since_ms: Option<u64>,
    pub until_ms: Option<u64>,
    /// 1 to 200; 50 when absent.
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub job_id: String,
    pub owner_instance_id: Option<String>,
    pub operation: String,
    pub connection_id: Option<String>,
    pub query: Option<String>,
    pub query_kind: Option<String>,
    pub status: JobStatus,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub row_count: Option<u64>,
    pub error: Option<String>,
    /// Record written by the MCP audit log before service history existed.
    pub legacy: bool,
    /// True only for a job this process owns and can still cancel.
    pub cancellable: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub entries: Vec<HistoryEntry>,
    pub next_cursor: Option<String>,
}

/// Jobs this process still holds, with their live status.
#[derive(Clone, Debug, Default)]
pub struct LiveJobs {
    pub instance_id: String,
    pub statuses: HashMap<String, JobStatus>,
}

pub trait HistoryStore: Send + Sync {
    fn record_start(&self, meta: &HistoryMeta, status: JobStatus);
    fn record_terminal(&self, meta: &HistoryMeta, response: &ServiceResponse);
    /// A job that ended without a response (dropped, process stopping).
    fn record_interrupted(&self, meta: &HistoryMeta, status: JobStatus, code: ErrorCode);
    /// Jobs newest first. A start without a terminal event reads as an
    /// unknown outcome unless `live` shows this process still holds it.
    fn list(&self, query: &HistoryQuery, live: &LiveJobs) -> ServiceResult<HistoryPage>;
}

pub fn status_name(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "queued",
        JobStatus::Running => "running",
        JobStatus::Succeeded => "succeeded",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
        JobStatus::Interrupted => "interrupted",
        JobStatus::OutcomeUnknown => "outcome_unknown",
    }
}
