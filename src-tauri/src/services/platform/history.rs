//! Query history for service jobs.
//!
//! Every job writes a start event and a terminal event under the same job id.
//! The record reuses the MCP audit model (`AiActivityEvent`) and follows its
//! privacy switch (`ai_audit_enabled`) and retention (`ai_audit_max_entries`).
//! It stores the SQL text, connection id, status, timestamps, elapsed time,
//! row count and a sanitized error. Credentials and rows never reach it.
//!
//! Files in the profile directory: `service_history.jsonl` (active),
//! `service_history.{1..5}.jsonl` (rotated archives, 1 is the newest) and
//! `service_history.lock`. Writers and the rotation run under one exclusive
//! `fs2` lock, so several processes can share the directory.

use crate::{
    ai_activity::{self, AiActivityEvent, EventFilter},
    services::{
        contracts::{ErrorCode, JobStatus, ServiceResponse},
        error::{failure, ServiceResult},
        history::{
            status_name, HistoryEntry, HistoryMeta, HistoryPage, HistoryQuery, HistoryStore,
            LiveJobs, MAX_LIST_LIMIT,
        },
        storage::now_ms,
    },
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

const ACTIVE: &str = "service_history.jsonl";
const LOCK: &str = "service_history.lock";
const ARCHIVES: usize = 5;
const DEFAULT_LIST_LIMIT: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Start,
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    #[serde(flatten)]
    pub event: AiActivityEvent,
    pub job_id: String,
    pub phase: Phase,
    pub owner_instance_id: String,
    pub timestamp_ms: u64,
}

#[derive(Clone, Copy)]
struct Settings {
    enabled: bool,
    max_entries: usize,
}

pub struct QueryHistory {
    profile: PathBuf,
    instance_id: String,
}

impl QueryHistory {
    pub fn new(profile: PathBuf, instance_id: String) -> Self {
        Self {
            profile,
            instance_id,
        }
    }

    fn settings(&self) -> Option<Settings> {
        let config = match crate::persistence::service_read_file(
            &self.profile.join("config.json"),
            16 * 1024 * 1024,
            false,
        ) {
            Ok(Some(bytes)) => serde_json::from_slice::<crate::config::AppConfig>(&bytes).ok()?,
            Ok(None) => crate::config::AppConfig::default(),
            Err(_) => return None,
        };
        Some(Settings {
            enabled: config
                .ai_audit_enabled
                .unwrap_or(crate::config::DEFAULT_AI_AUDIT_ENABLED),
            max_entries: config
                .ai_audit_max_entries
                .unwrap_or(crate::config::DEFAULT_AI_AUDIT_MAX_ENTRIES)
                as usize,
        })
    }

    fn record(
        &self,
        meta: &HistoryMeta,
        phase: Phase,
        status: &str,
        duration_ms: u64,
        rows: Option<u64>,
        error: Option<String>,
    ) -> HistoryRecord {
        let query = meta.query.clone();
        HistoryRecord {
            event: AiActivityEvent {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: self.instance_id.clone(),
                timestamp: ai_activity::now_iso8601(),
                tool: meta.operation.clone(),
                connection_id: meta.connection_id.clone(),
                connection_name: None,
                database: None,
                query_kind: query
                    .as_deref()
                    .map(|query| ai_activity::classify_query_kind(query).to_owned()),
                query,
                duration_ms,
                status: status.into(),
                rows: rows.map(|rows| rows as usize),
                error,
                client_hint: Some("service".into()),
                approval_id: None,
            },
            job_id: meta.job_id.clone(),
            phase,
            owner_instance_id: self.instance_id.clone(),
            timestamp_ms: now_ms(),
        }
    }

    fn start_event(&self, meta: &HistoryMeta, status: JobStatus) {
        let record = self.record(meta, Phase::Start, status_name(status), 0, None, None);
        self.write(record);
    }

    fn terminal_event(&self, meta: &HistoryMeta, response: &ServiceResponse) {
        let rows = row_count(response);
        let error = response.error.as_ref().map(|error| {
            let code = serde_json::to_value(error.code)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            format!("{code}: {}", failure(error.code).message)
        });
        let record = self.record(
            meta,
            Phase::Terminal,
            status_name(response.status),
            now_ms().saturating_sub(meta.started_ms),
            rows,
            error,
        );
        self.write(record);
    }

    fn interrupted_event(&self, meta: &HistoryMeta, status: JobStatus, code: ErrorCode) {
        let code_name = serde_json::to_value(code)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        let record = self.record(
            meta,
            Phase::Terminal,
            status_name(status),
            now_ms().saturating_sub(meta.started_ms),
            None,
            Some(format!("{code_name}: {}", failure(code).message)),
        );
        self.write(record);
    }

    fn write(&self, record: HistoryRecord) {
        if let Err(error) = self.append(&record) {
            log::warn!("Service history write failed: {:?}", error.code);
        }
    }

    /// Appends one record under the lock and rotates when the active file is
    /// full. Writes nothing when auditing is disabled or the setting cannot
    /// be read.
    pub fn append(&self, record: &HistoryRecord) -> ServiceResult<()> {
        let Some(settings) = self.settings().filter(|settings| settings.enabled) else {
            return Ok(());
        };
        let line = serde_json::to_string(record).map_err(|_| failure(ErrorCode::ResourceLimit))?;
        let _lock = self.lock()?;
        let active = self.profile.join(ACTIVE);
        let mut file = open_private_append(&active)?;
        writeln!(file, "{line}")
            .and_then(|_| file.flush())
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        drop(file);
        if settings.max_entries > 0 && count_lines(&active) >= settings.max_entries {
            self.rotate()?;
        }
        Ok(())
    }

    fn lock(&self) -> ServiceResult<File> {
        fs::create_dir_all(&self.profile).map_err(|_| failure(ErrorCode::ResourceLimit))?;
        let file = open_private(&self.profile.join(LOCK), false)?;
        file.lock_exclusive()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        Ok(file)
    }

    fn archive(&self, index: usize) -> PathBuf {
        self.profile.join(format!("service_history.{index}.jsonl"))
    }

    /// Shifts every archive up by one and moves the active file to archive 1.
    /// Each step is a rename, and the caller holds the lock.
    fn rotate(&self) -> ServiceResult<()> {
        let last = self.archive(ARCHIVES);
        if last.exists() {
            fs::remove_file(&last).map_err(|_| failure(ErrorCode::ResourceLimit))?;
        }
        for index in (1..ARCHIVES).rev() {
            let from = self.archive(index);
            if from.exists() {
                fs::rename(&from, self.archive(index + 1))
                    .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            }
        }
        let active = self.profile.join(ACTIVE);
        if active.exists() {
            fs::rename(&active, self.archive(1)).map_err(|_| failure(ErrorCode::ResourceLimit))?;
        }
        Ok(())
    }

    fn read_records(&self) -> ServiceResult<Vec<HistoryRecord>> {
        let _lock = self.lock()?;
        let mut paths: Vec<PathBuf> = (1..=ARCHIVES).rev().map(|i| self.archive(i)).collect();
        paths.push(self.profile.join(ACTIVE));
        let mut records = Vec::new();
        for path in paths {
            let Ok(file) = File::open(&path) else {
                continue;
            };
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Ok(record) = serde_json::from_str::<HistoryRecord>(line.trim()) {
                    records.push(record);
                }
            }
        }
        Ok(records)
    }

    fn list_entries(&self, query: &HistoryQuery, live: &LiveJobs) -> ServiceResult<HistoryPage> {
        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        if limit == 0 || limit > MAX_LIST_LIMIT {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let position = query.cursor.as_deref().map(decode_cursor).transpose()?;
        let mut jobs: HashMap<String, HistoryEntry> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for record in self.read_records()? {
            let entry = jobs.entry(record.job_id.clone()).or_insert_with(|| {
                order.push(record.job_id.clone());
                HistoryEntry {
                    job_id: record.job_id.clone(),
                    owner_instance_id: Some(record.owner_instance_id.clone()),
                    operation: record.event.tool.clone(),
                    connection_id: record.event.connection_id.clone(),
                    query: record.event.query.clone(),
                    query_kind: record.event.query_kind.clone(),
                    status: JobStatus::OutcomeUnknown,
                    started_at_ms: record.timestamp_ms,
                    finished_at_ms: None,
                    elapsed_ms: None,
                    row_count: None,
                    error: None,
                    legacy: false,
                    cancellable: false,
                }
            });
            if record.phase == Phase::Start {
                entry.started_at_ms = record.timestamp_ms;
                entry.query = record.event.query.clone();
                entry.query_kind = record.event.query_kind.clone();
                entry.connection_id = record.event.connection_id.clone();
            } else {
                entry.status = parse_status(&record.event.status);
                entry.finished_at_ms = Some(record.timestamp_ms);
                entry.elapsed_ms = Some(record.event.duration_ms);
                entry.row_count = record.event.rows.map(|rows| rows as u64);
                entry.error = record.event.error.clone();
            }
        }
        let mut entries: Vec<HistoryEntry> =
            order
                .into_iter()
                .filter_map(|id| jobs.remove(&id))
                .map(|mut entry| {
                    if entry.finished_at_ms.is_none() {
                        if let Some(status) = live.statuses.get(&entry.job_id).filter(|_| {
                            entry.owner_instance_id.as_deref() == Some(&live.instance_id)
                        }) {
                            entry.status = *status;
                            entry.cancellable = !matches!(
                                status,
                                JobStatus::Succeeded
                                    | JobStatus::Failed
                                    | JobStatus::Cancelled
                                    | JobStatus::Interrupted
                                    | JobStatus::OutcomeUnknown
                            );
                        }
                    }
                    entry
                })
                .collect();
        entries.extend(self.legacy_entries());
        entries.retain(|entry| matches_query(entry, query));
        entries.sort_by(|a, b| (b.started_at_ms, &b.job_id).cmp(&(a.started_at_ms, &a.job_id)));
        if let Some((started, job)) = position {
            entries.retain(|entry| {
                (entry.started_at_ms, entry.job_id.as_str()) < (started, job.as_str())
            });
        }
        let more = entries.len() > limit;
        entries.truncate(limit);
        let next_cursor = more
            .then(|| entries.last())
            .flatten()
            .map(|entry| encode_cursor(entry.started_at_ms, &entry.job_id));
        Ok(HistoryPage {
            entries,
            next_cursor,
        })
    }

    fn legacy_entries(&self) -> Vec<HistoryEntry> {
        ai_activity::read_events_in(&self.profile, &EventFilter::default())
            .unwrap_or_default()
            .into_iter()
            .map(|event| {
                let started = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
                    .map(|time| time.timestamp_millis().max(0) as u64)
                    .unwrap_or(0);
                let status = if event.status == "success" {
                    JobStatus::Succeeded
                } else {
                    JobStatus::Failed
                };
                HistoryEntry {
                    job_id: format!("legacy-{}", event.id),
                    owner_instance_id: None,
                    operation: event.tool,
                    connection_id: event.connection_id,
                    query: event.query,
                    query_kind: event.query_kind,
                    status,
                    started_at_ms: started,
                    finished_at_ms: Some(started.saturating_add(event.duration_ms)),
                    elapsed_ms: Some(event.duration_ms),
                    row_count: event.rows.map(|rows| rows as u64),
                    error: event.error,
                    legacy: true,
                    cancellable: false,
                }
            })
            .collect()
    }
}

fn matches_query(entry: &HistoryEntry, query: &HistoryQuery) -> bool {
    query
        .connection_id
        .as_deref()
        .is_none_or(|id| entry.connection_id.as_deref() == Some(id))
        && query.status.is_none_or(|status| entry.status == status)
        && query
            .since_ms
            .is_none_or(|since| entry.started_at_ms >= since)
        && query
            .until_ms
            .is_none_or(|until| entry.started_at_ms <= until)
}

fn parse_status(value: &str) -> JobStatus {
    serde_json::from_value(serde_json::json!(value)).unwrap_or(JobStatus::OutcomeUnknown)
}

fn row_count(response: &ServiceResponse) -> Option<u64> {
    let data = &response.data;
    data["stored_row_count"]
        .as_u64()
        .or_else(|| data["affected_rows"].as_u64())
        .or_else(|| {
            ["rows", "documents", "values"]
                .iter()
                .find_map(|key| data[*key].as_array().map(|rows| rows.len() as u64))
        })
}

fn encode_cursor(started_ms: u64, job_id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{started_ms}:{job_id}"))
}

fn decode_cursor(cursor: &str) -> ServiceResult<(u64, String)> {
    let invalid = || failure(ErrorCode::InvalidPageToken);
    if cursor.len() > 512 {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid())?;
    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
    let (started, job) = text.split_once(':').ok_or_else(invalid)?;
    Ok((started.parse().map_err(|_| invalid())?, job.to_owned()))
}

fn open_private(path: &Path, append: bool) -> ServiceResult<File> {
    let mut options = OpenOptions::new();
    options.create(true);
    if append {
        options.append(true);
    } else {
        options.read(true).write(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|_| failure(ErrorCode::ResourceLimit))
}

fn open_private_append(path: &Path) -> ServiceResult<File> {
    open_private(path, true)
}

fn count_lines(path: &Path) -> usize {
    File::open(path)
        .map(|file| BufReader::new(file).lines().count())
        .unwrap_or(0)
}

impl HistoryStore for QueryHistory {
    fn record_start(&self, meta: &HistoryMeta, status: JobStatus) {
        self.start_event(meta, status);
    }
    fn record_terminal(&self, meta: &HistoryMeta, response: &ServiceResponse) {
        self.terminal_event(meta, response);
    }
    fn record_interrupted(&self, meta: &HistoryMeta, status: JobStatus, code: ErrorCode) {
        self.interrupted_event(meta, status, code);
    }
    /// Service history merged with the older MCP audit log, which comes back
    /// as completed legacy history.
    fn list(&self, query: &HistoryQuery, live: &LiveJobs) -> ServiceResult<HistoryPage> {
        self.list_entries(query, live)
    }
}
