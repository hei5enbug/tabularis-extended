use super::{
    contracts::{ErrorCode, Limits, Metrics, Page, ResumeMode},
    error::{failure, ServiceResult},
    security::{digest, opaque_id},
    storage::{now_ms, PrivateStore},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};

pub const RESULT_TTL_MS: u64 = 15 * 60 * 1000;
pub const MAX_RESULT_ROWS: usize = 10_000;
pub const MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 16_384;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    #[default]
    Tabular,
    Documents,
    JsonValues,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultSet {
    #[serde(default)]
    pub kind: ResultKind,
    pub columns: Vec<Value>,
    pub rows: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultSnapshot {
    pub sets: Vec<ResultSet>,
    pub revision: String,
    pub limits: Limits,
    pub allow_truncation: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResultOrigin {
    pub database: String,
    pub container: String,
}
impl ResultOrigin {
    pub fn from_input(input: &Value) -> ServiceResult<Self> {
        let field = |name: &str| {
            input[name]
                .as_str()
                .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
                .map(str::to_owned)
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))
        };
        Ok(Self {
            database: field("database")?,
            container: field("container")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorBinding {
    pub connection_id: String,
    pub permission_scope: String,
    pub auth_revision: String,
    pub query_hash: String,
    pub result_revision: String,
    pub sdk_version: Option<String>,
    pub container_rid: Option<String>,
    pub endpoint_identity: Option<String>,
    pub partition_hash: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CursorState {
    pub binding: CursorBinding,
    pub native_state: Value,
    pub tail: Vec<Value>,
    pub columns: Vec<Value>,
    pub page_size: usize,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredPage {
    pub data: Value,
    pub page: Page,
    pub result_id: Option<String>,
    pub limits: Limits,
    pub metrics: Metrics,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct SnapshotRecord {
    binding: CursorBinding,
    created_at_ms: u64,
    expires_at_ms: u64,
    checksum: String,
    snapshot: ResultSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<ResultOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    readonly_guarantee: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct TokenRecord {
    binding: CursorBinding,
    expires_at_ms: u64,
    result_id: Option<String>,
    set_index: usize,
    offset: usize,
    page_size: usize,
    cursor: Option<CursorState>,
    in_flight: bool,
    replay: Option<StoredPage>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SpatialCursorState {
    pub binding: CursorBinding,
    pub state: Value,
    pub page_size: usize,
    pub expires_at_ms: u64,
}

pub enum SpatialNextPage {
    Replay(StoredPage),
    State(SpatialCursorState),
}

#[derive(Serialize, Deserialize)]
struct SpatialTokenRecord {
    binding: CursorBinding,
    expires_at_ms: u64,
    source_result_id: Option<String>,
    state: Option<SpatialCursorState>,
    in_flight: bool,
    replay: Option<StoredPage>,
    checksum: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_hash: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "record")]
enum CacheRecord {
    Snapshot(SnapshotRecord),
    Token(TokenRecord),
    SpatialToken(Box<SpatialTokenRecord>),
    Reservation {
        connection_id: String,
        expires_at_ms: u64,
        bytes: u64,
    },
    AuthRevision {
        connection_id: String,
        revision: String,
    },
}

pub struct ResultStore {
    store: Arc<PrivateStore>,
    ttl_ms: u64,
    max_bytes: u64,
    /// Held shared for the life of the store so that a starting process can
    /// tell whether any other process still uses this directory.
    _alive: std::fs::File,
}

fn snapshot_checksum(
    snapshot: &ResultSnapshot,
    origin: Option<&ResultOrigin>,
) -> ServiceResult<String> {
    let bytes = match origin {
        Some(origin) => {
            serde_json::to_vec(&serde_json::json!({"snapshot":snapshot,"origin":origin}))
        }
        None => serde_json::to_vec(snapshot),
    }
    .map_err(|_| failure(ErrorCode::ResourceLimit))?;
    Ok(digest(&bytes))
}

impl ResultStore {
    pub fn new(root: PathBuf) -> ServiceResult<Self> {
        Self::with_limits(root, RESULT_TTL_MS, MAX_CACHE_BYTES)
    }
    pub fn with_limits(root: PathBuf, ttl_ms: u64, max_bytes: u64) -> ServiceResult<Self> {
        let store = Arc::new(PrivateStore::new(
            root,
            (MAX_RESULT_BYTES + MAX_PAGE_BYTES + 1024 * 1024) as u64,
        )?);
        let alive = store.named_file("alive.lock")?;
        let result = Self {
            store,
            ttl_ms: ttl_ms.min(RESULT_TTL_MS),
            max_bytes: max_bytes.min(MAX_CACHE_BYTES),
            _alive: alive,
        };
        result.startup_sweep()?;
        Ok(result)
    }
    /// Removes what an earlier process left behind. Another running process
    /// keeps its own results, so everything is swept only when this process
    /// is the only user of the directory; expired entries always go.
    fn startup_sweep(&self) -> ServiceResult<()> {
        let _lock = self.store.lock()?;
        let sole_user = FileExt::try_lock_exclusive(&self._alive).is_ok();
        FileExt::lock_shared(&self._alive).map_err(|_| failure(ErrorCode::ResourceLimit))?;
        self.store.remove_temporaries()?;
        if !sole_user {
            return self.cleanup_locked();
        }
        for id in self.store.ids()? {
            match self.store.read::<CacheRecord>(&id) {
                Ok(Some(CacheRecord::AuthRevision { .. })) => {}
                _ => self.store.remove(&id)?,
            }
        }
        Ok(())
    }
    pub fn root(&self) -> &std::path::Path {
        self.store.root()
    }
    pub fn auth_revision_marker(&self, connection_id: &str) -> ServiceResult<Option<String>> {
        let _lock = self.store.lock()?;
        match self
            .store
            .read::<CacheRecord>(&format!("a-{}", digest(connection_id.as_bytes())))?
        {
            Some(CacheRecord::AuthRevision {
                connection_id: stored,
                revision,
            }) if stored == connection_id => Ok(Some(revision)),
            None => Ok(None),
            _ => Err(failure(ErrorCode::AuthRequired)),
        }
    }
    fn cleanup_locked(&self) -> ServiceResult<()> {
        let now = now_ms();
        for id in self.store.ids()? {
            if let Some(record) = self.store.read::<CacheRecord>(&id)? {
                let expiry = match record {
                    CacheRecord::Snapshot(record) => record.expires_at_ms,
                    CacheRecord::Token(record) => record.expires_at_ms,
                    CacheRecord::SpatialToken(record) => record.expires_at_ms,
                    CacheRecord::Reservation { expires_at_ms, .. } => expires_at_ms,
                    CacheRecord::AuthRevision { .. } => u64::MAX,
                };
                if expiry <= now {
                    self.store.remove(&id)?;
                }
            }
        }
        Ok(())
    }
    fn write_locked(&self, id: &str, record: &CacheRecord) -> ServiceResult<()> {
        let new_bytes = serde_json::to_vec(record)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len() as u64;
        let old_bytes = self
            .store
            .read::<CacheRecord>(id)?
            .map(|old| {
                serde_json::to_vec(&old)
                    .map(|bytes| bytes.len() as u64)
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        if self
            .used_bytes()?
            .saturating_sub(old_bytes)
            .saturating_add(new_bytes)
            > self.max_bytes
            || (old_bytes == 0 && self.store.ids()?.len() >= MAX_CACHE_ENTRIES)
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        self.store.write(id, record)
    }
    pub fn insert(
        &self,
        binding: CursorBinding,
        snapshot: ResultSnapshot,
    ) -> ServiceResult<String> {
        self.insert_reserved(binding, snapshot, None)
    }
    pub fn insert_reserved(
        &self,
        binding: CursorBinding,
        snapshot: ResultSnapshot,
        reservation: Option<&ResultReservation>,
    ) -> ServiceResult<String> {
        self.insert_reserved_with_origin(binding, snapshot, reservation, None)
    }
    pub fn insert_reserved_with_origin(
        &self,
        binding: CursorBinding,
        snapshot: ResultSnapshot,
        reservation: Option<&ResultReservation>,
        origin: Option<ResultOrigin>,
    ) -> ServiceResult<String> {
        self.insert_labeled(binding, snapshot, reservation, origin, None)
    }
    /// Stores a result together with the read-only guarantee that applied
    /// when it ran, so `result.get` reports it without re-running anything.
    pub fn insert_labeled(
        &self,
        binding: CursorBinding,
        snapshot: ResultSnapshot,
        reservation: Option<&ResultReservation>,
        origin: Option<ResultOrigin>,
        readonly_guarantee: Option<&str>,
    ) -> ServiceResult<String> {
        let snapshot = bound_snapshot(snapshot)?;
        if snapshot.revision != binding.result_revision {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let origin = origin.filter(|_| {
            snapshot
                .sets
                .iter()
                .any(|set| matches!(set.kind, ResultKind::Documents | ResultKind::JsonValues))
        });
        if let Some(origin) = &origin {
            ResultOrigin::from_input(
                &serde_json::to_value(origin).map_err(|_| failure(ErrorCode::InvalidArgument))?,
            )?;
        }
        let checksum = snapshot_checksum(&snapshot, origin.as_ref())?;
        let id = format!("r-{}", uuid::Uuid::new_v4());
        let created = now_ms();
        let record = CacheRecord::Snapshot(SnapshotRecord {
            binding,
            created_at_ms: created,
            expires_at_ms: created.saturating_add(self.ttl_ms),
            checksum,
            snapshot,
            origin,
            readonly_guarantee: readonly_guarantee.map(str::to_owned),
        });
        let _lock = self.store.lock()?;
        self.cleanup_locked()?;
        self.check_auth_revision_locked(&record)?;
        if let Some(reservation) = reservation {
            self.consume_reservation_locked(reservation)?;
        }
        self.write_locked(&id, &record)?;
        Ok(id)
    }
    pub fn snapshot(
        &self,
        result_id: &str,
        binding: &CursorBinding,
    ) -> ServiceResult<ResultSnapshot> {
        Ok(self.snapshot_record(result_id, binding)?.snapshot)
    }
    fn snapshot_record(
        &self,
        result_id: &str,
        binding: &CursorBinding,
    ) -> ServiceResult<SnapshotRecord> {
        opaque_id(result_id)?;
        let _lock = self.store.lock()?;
        self.snapshot_record_locked(result_id, binding)
    }
    fn snapshot_record_locked(
        &self,
        result_id: &str,
        binding: &CursorBinding,
    ) -> ServiceResult<SnapshotRecord> {
        match self.store.read::<CacheRecord>(result_id)? {
            Some(CacheRecord::Snapshot(record)) => {
                if record.expires_at_ms <= now_ms() {
                    self.store.remove(result_id)?;
                    return Err(failure(ErrorCode::ResultExpired));
                }
                check_binding(&record.binding, binding)?;
                self.check_origin_revision_locked(&record.binding)?;
                if snapshot_checksum(&record.snapshot, record.origin.as_ref())? != record.checksum {
                    return Err(failure(ErrorCode::ResultExpired));
                }
                Ok(record)
            }
            _ => Err(failure(ErrorCode::ResultExpired)),
        }
    }
    pub fn result_binding(&self, result_id: &str) -> ServiceResult<CursorBinding> {
        opaque_id(result_id)?;
        match self.store.read::<CacheRecord>(result_id)? {
            Some(CacheRecord::Snapshot(record)) if record.expires_at_ms > now_ms() => {
                Ok(record.binding)
            }
            _ => Err(failure(ErrorCode::ResultExpired)),
        }
    }
    pub fn page(
        &self,
        result_id: &str,
        binding: &CursorBinding,
        set_index: usize,
        offset: usize,
        limit: usize,
    ) -> ServiceResult<StoredPage> {
        let record = self.snapshot_record(result_id, binding)?;
        let snapshot = record.snapshot;
        let set = snapshot
            .sets
            .get(set_index)
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let mut data = match set.kind {
            ResultKind::Tabular => {
                serde_json::json!({"kind":"tabular","columns":set.columns,"rows":[]})
            }
            ResultKind::Documents => serde_json::json!({"kind":"documents","documents":[]}),
            ResultKind::JsonValues => serde_json::json!({"kind":"json_values","values":[]}),
        };
        data["result_set_index"] = set_index.into();
        data["result_set_count"] = snapshot.sets.len().into();
        data["offset"] = offset.into();
        data["stored_row_count"] = set.rows.len().into();
        data["result_revision"] = snapshot.revision.into();
        if let Some(label) = record.readonly_guarantee {
            data["readonly_guarantee"] = label.into();
        }
        if matches!(set.kind, ResultKind::Documents | ResultKind::JsonValues) {
            if let Some(origin) = record.origin {
                data["origin"] =
                    serde_json::to_value(origin).map_err(|_| failure(ErrorCode::ResourceLimit))?;
            }
        }
        let overhead = serde_json::to_vec(&data)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len();
        let budget = (MAX_PAGE_BYTES - 64 * 1024)
            .checked_sub(overhead)
            .ok_or_else(|| failure(ErrorCode::ResourceLimit))?;
        let (rows, next_offset) = bounded_rows(&set.rows, offset, limit.min(1000), budget)?;
        let field = match set.kind {
            ResultKind::Tabular => "rows",
            ResultKind::Documents => "documents",
            ResultKind::JsonValues => "values",
        };
        data[field] = rows.into();
        let has_more = next_offset < set.rows.len();
        let token = if has_more {
            Some(self.materialized_token(
                result_id,
                binding,
                set_index,
                next_offset,
                limit.min(1000),
            )?)
        } else {
            None
        };
        Ok(StoredPage {
            data,
            page: Page {
                next_token: token,
                has_more,
                resume_mode: ResumeMode::Materialized,
            },
            result_id: Some(result_id.into()),
            limits: snapshot.limits,
            metrics: Metrics::default(),
            warnings: snapshot.warnings,
        })
    }
    pub fn get_origin(
        &self,
        result_id: &str,
        binding: &CursorBinding,
    ) -> ServiceResult<Option<ResultOrigin>> {
        Ok(self.snapshot_record(result_id, binding)?.origin)
    }
    pub fn token_origin(
        &self,
        token: &str,
        binding: &CursorBinding,
    ) -> ServiceResult<Option<ResultOrigin>> {
        opaque_id(token).map_err(|_| failure(ErrorCode::InvalidPageToken))?;
        let _lock = self.store.lock()?;
        let record = match self.store.read::<CacheRecord>(token)? {
            Some(CacheRecord::Token(record)) if record.expires_at_ms > now_ms() => record,
            Some(CacheRecord::Token(_)) => return Err(failure(ErrorCode::CursorExpired)),
            _ => return Err(failure(ErrorCode::InvalidPageToken)),
        };
        check_binding(&record.binding, binding)?;
        self.check_origin_revision_locked(&record.binding)?;
        match record.result_id {
            Some(result_id) => Ok(self.snapshot_record_locked(&result_id, binding)?.origin),
            None => Err(failure(ErrorCode::InvalidPageToken)),
        }
    }
    fn check_origin_revision_locked(&self, binding: &CursorBinding) -> ServiceResult<()> {
        let id = format!("a-{}", digest(binding.connection_id.as_bytes()));
        if let Some(CacheRecord::AuthRevision { revision, .. }) = self.store.read(&id)? {
            if revision != binding.auth_revision {
                return Err(failure(ErrorCode::AuthRequired));
            }
        }
        Ok(())
    }
    fn materialized_token(
        &self,
        result_id: &str,
        binding: &CursorBinding,
        set_index: usize,
        offset: usize,
        page_size: usize,
    ) -> ServiceResult<String> {
        let _lock = self.store.lock()?;
        let result = match self.store.read::<CacheRecord>(result_id)? {
            Some(CacheRecord::Snapshot(record)) => record,
            _ => return Err(failure(ErrorCode::ResultExpired)),
        };
        if result.expires_at_ms <= now_ms() {
            return Err(failure(ErrorCode::ResultExpired));
        }
        check_binding(&result.binding, binding)?;
        for id in self.store.ids()? {
            if let Some(CacheRecord::Token(token)) = self.store.read::<CacheRecord>(&id)? {
                if token.result_id.as_deref() == Some(result_id)
                    && token.set_index == set_index
                    && token.offset == offset
                    && token.page_size == page_size
                    && token.binding == *binding
                {
                    return Ok(id);
                }
            }
        }
        let id = format!("t-{}", uuid::Uuid::new_v4());
        self.write_locked(
            &id,
            &CacheRecord::Token(TokenRecord {
                binding: binding.clone(),
                expires_at_ms: result.expires_at_ms,
                result_id: Some(result_id.into()),
                set_index,
                offset,
                page_size,
                cursor: None,
                in_flight: false,
                replay: None,
            }),
        )?;
        Ok(id)
    }
    pub fn cursor_token(&self, state: CursorState) -> ServiceResult<String> {
        self.cursor_token_reserved(state, None)
    }
    pub fn cursor_token_reserved(
        &self,
        mut state: CursorState,
        reservation: Option<&ResultReservation>,
    ) -> ServiceResult<String> {
        if state.page_size == 0
            || state.page_size > 1000
            || state.binding.sdk_version.is_none()
            || state.binding.container_rid.is_none()
            || state.binding.endpoint_identity.is_none()
            || state.binding.partition_hash.is_none()
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if state.tail.len() > MAX_RESULT_ROWS
            || serde_json::to_vec(&state)
                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                .len()
                > MAX_RESULT_BYTES
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        state.expires_at_ms = state
            .expires_at_ms
            .min(now_ms().saturating_add(self.ttl_ms));
        let id = format!("t-{}", uuid::Uuid::new_v4());
        let record = CacheRecord::Token(TokenRecord {
            binding: state.binding.clone(),
            expires_at_ms: state.expires_at_ms,
            result_id: None,
            set_index: 0,
            offset: 0,
            page_size: state.page_size,
            cursor: Some(state),
            in_flight: false,
            replay: None,
        });
        let _lock = self.store.lock()?;
        self.cleanup_locked()?;
        self.check_auth_revision_locked(&record)?;
        if let Some(reservation) = reservation {
            self.consume_reservation_locked(reservation)?;
        }
        self.write_locked(&id, &record)?;
        Ok(id)
    }
    pub fn token_binding(&self, token: &str) -> ServiceResult<CursorBinding> {
        opaque_id(token).map_err(|_| failure(ErrorCode::InvalidPageToken))?;
        let _lock = self.store.lock()?;
        match self.store.read::<CacheRecord>(token)? {
            Some(CacheRecord::Token(record)) if record.expires_at_ms > now_ms() => {
                Ok(record.binding)
            }
            Some(CacheRecord::Token(_)) => Err(failure(ErrorCode::CursorExpired)),
            Some(CacheRecord::SpatialToken(record)) => {
                self.check_spatial_locked(&record, &record.binding)?;
                Ok(record.binding)
            }
            _ => Err(failure(ErrorCode::InvalidPageToken)),
        }
    }
    pub fn attach_cursor_result(
        &self,
        token: &str,
        result_id: &str,
        binding: &CursorBinding,
    ) -> ServiceResult<()> {
        opaque_id(token)?;
        opaque_id(result_id)?;
        let _lock = self.store.lock()?;
        let snapshot = match self.store.read::<CacheRecord>(result_id)? {
            Some(CacheRecord::Snapshot(record)) if record.expires_at_ms > now_ms() => record,
            _ => return Err(failure(ErrorCode::ResultExpired)),
        };
        check_binding(&snapshot.binding, binding)?;
        let mut token_record = match self.store.read::<CacheRecord>(token)? {
            Some(CacheRecord::Token(record)) if record.expires_at_ms > now_ms() => record,
            _ => return Err(failure(ErrorCode::InvalidPageToken)),
        };
        check_binding(&token_record.binding, binding)?;
        token_record.result_id = Some(result_id.into());
        self.write_locked(token, &CacheRecord::Token(token_record))
    }
    pub fn begin_next(&self, token: &str, binding: &CursorBinding) -> ServiceResult<NextPage> {
        opaque_id(token).map_err(|_| failure(ErrorCode::InvalidPageToken))?;
        let _lock = self.store.lock()?;
        let mut record = match self.store.read::<CacheRecord>(token)? {
            Some(CacheRecord::Token(record)) => record,
            _ => return Err(failure(ErrorCode::InvalidPageToken)),
        };
        if record.expires_at_ms <= now_ms() {
            return Err(failure(ErrorCode::CursorExpired));
        }
        check_binding(&record.binding, binding)?;
        if let Some(page) = record.replay {
            return Ok(NextPage::Replay(page));
        }
        if record.in_flight {
            return Err(failure(ErrorCode::RateLimited));
        }
        if let Some(cursor) = record.cursor.clone() {
            record.in_flight = true;
            self.write_locked(token, &CacheRecord::Token(record))?;
            Ok(NextPage::Native(cursor))
        } else {
            Ok(NextPage::Materialized {
                result_id: record
                    .result_id
                    .ok_or_else(|| failure(ErrorCode::InvalidPageToken))?,
                set_index: record.set_index,
                offset: record.offset,
                limit: record.page_size,
            })
        }
    }
    pub fn finish_next(
        &self,
        token: &str,
        binding: &CursorBinding,
        page: StoredPage,
    ) -> ServiceResult<StoredPage> {
        let _lock = self.store.lock()?;
        let mut record = match self.store.read::<CacheRecord>(token)? {
            Some(CacheRecord::Token(record)) => record,
            _ => return Err(failure(ErrorCode::InvalidPageToken)),
        };
        check_binding(&record.binding, binding)?;
        if record.expires_at_ms <= now_ms() {
            return Err(failure(ErrorCode::CursorExpired));
        }
        if let Some(replay) = record.replay {
            return Ok(replay);
        }
        record.in_flight = false;
        record.cursor = None;
        record.replay = Some(page.clone());
        self.write_locked(token, &CacheRecord::Token(record))?;
        Ok(page)
    }
    pub fn fail_next(&self, token: &str) -> ServiceResult<()> {
        let _lock = self.store.lock()?;
        self.store.remove(token)
    }
    pub fn release(&self, result_id: &str, binding: &CursorBinding) -> ServiceResult<()> {
        self.snapshot(result_id, binding)?;
        let _lock = self.store.lock()?;
        for id in self.store.ids()? {
            let remove = match self.store.read::<CacheRecord>(&id)? {
                Some(CacheRecord::Token(record)) => record.result_id.as_deref() == Some(result_id),
                Some(CacheRecord::SpatialToken(record)) => {
                    record.source_result_id.as_deref() == Some(result_id)
                }
                _ => false,
            };
            if remove {
                self.store.remove(&id)?;
            }
        }
        self.store.remove(result_id)
    }
    pub fn invalidate_connection(&self, connection_id: &str) -> ServiceResult<()> {
        let _lock = self.store.lock()?;
        for id in self.store.ids()? {
            let remove = match self.store.read::<CacheRecord>(&id)? {
                Some(CacheRecord::Snapshot(record)) => {
                    record.binding.connection_id == connection_id
                }
                Some(CacheRecord::Token(record)) => record.binding.connection_id == connection_id,
                Some(CacheRecord::SpatialToken(record)) => {
                    record.binding.connection_id == connection_id
                }
                Some(CacheRecord::Reservation {
                    connection_id: stored,
                    ..
                }) => stored == connection_id,
                _ => false,
            };
            if remove {
                self.store.remove(&id)?;
            }
        }
        Ok(())
    }
    pub fn cleanup(&self) -> ServiceResult<()> {
        let _lock = self.store.lock()?;
        self.cleanup_locked()
    }
    pub fn invalidate_connection_revision(
        &self,
        connection_id: &str,
        revision: &str,
    ) -> ServiceResult<()> {
        let _lock = self.store.lock()?;
        for id in self.store.ids()? {
            let remove = match self.store.read::<CacheRecord>(&id)? {
                Some(CacheRecord::Snapshot(record)) => {
                    record.binding.connection_id == connection_id
                }
                Some(CacheRecord::Token(record)) => record.binding.connection_id == connection_id,
                Some(CacheRecord::SpatialToken(record)) => {
                    record.binding.connection_id == connection_id
                }
                Some(CacheRecord::Reservation {
                    connection_id: stored,
                    ..
                }) => stored == connection_id,
                _ => false,
            };
            if remove {
                self.store.remove(&id)?;
            }
        }
        let id = format!("a-{}", digest(connection_id.as_bytes()));
        self.write_locked(
            &id,
            &CacheRecord::AuthRevision {
                connection_id: connection_id.into(),
                revision: revision.into(),
            },
        )
    }
    fn check_auth_revision_locked(&self, record: &CacheRecord) -> ServiceResult<()> {
        let binding = match record {
            CacheRecord::Snapshot(record) => &record.binding,
            CacheRecord::Token(record) => &record.binding,
            CacheRecord::SpatialToken(record) => &record.binding,
            _ => return Ok(()),
        };
        let id = format!("a-{}", digest(binding.connection_id.as_bytes()));
        if let Some(CacheRecord::AuthRevision { revision, .. }) = self.store.read(&id)? {
            if revision != binding.auth_revision {
                return Err(failure(ErrorCode::AuthRequired));
            }
        }
        Ok(())
    }
    fn used_bytes(&self) -> ServiceResult<u64> {
        let mut bytes = self.store.bytes()?;
        for id in self.store.ids()? {
            if let Some(CacheRecord::Reservation {
                bytes: reserved, ..
            }) = self.store.read::<CacheRecord>(&id)?
            {
                bytes = bytes.saturating_add(reserved);
            }
        }
        Ok(bytes)
    }
    pub fn reserve_query(
        &self,
        connection_id: &str,
        deadline_ms: u64,
    ) -> ServiceResult<ResultReservation> {
        let _lock = self.store.lock()?;
        self.cleanup_locked()?;
        let bytes = (MAX_RESULT_BYTES + MAX_PAGE_BYTES + 1024 * 1024) as u64;
        if self
            .used_bytes()?
            .saturating_add(bytes)
            .saturating_add(1024)
            > self.max_bytes
            || self.store.ids()?.len() >= MAX_CACHE_ENTRIES
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let id = format!("q-{}", uuid::Uuid::new_v4());
        self.store.write(
            &id,
            &CacheRecord::Reservation {
                connection_id: connection_id.into(),
                expires_at_ms: now_ms()
                    .saturating_add(deadline_ms.min(120_000))
                    .saturating_add(1000),
                bytes,
            },
        )?;
        Ok(ResultReservation {
            store: self.store.clone(),
            id,
        })
    }
    fn consume_reservation_locked(&self, reservation: &ResultReservation) -> ServiceResult<()> {
        if !Arc::ptr_eq(&reservation.store, &self.store) {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        match self.store.read::<CacheRecord>(&reservation.id)? {
            Some(CacheRecord::Reservation { expires_at_ms, .. }) if expires_at_ms > now_ms() => {
                self.store.remove(&reservation.id)
            }
            None => Ok(()),
            _ => Err(failure(ErrorCode::DeadlineExceeded)),
        }
    }

    pub fn spatial_token(
        &self,
        state: SpatialCursorState,
        source_result_id: Option<&str>,
    ) -> ServiceResult<String> {
        self.spatial_token_reserved(state, source_result_id, None)
    }
    pub fn spatial_token_reserved(
        &self,
        state: SpatialCursorState,
        source_result_id: Option<&str>,
        reservation: Option<&ResultReservation>,
    ) -> ServiceResult<String> {
        self.create_spatial_token(state, source_result_id, None, reservation)
    }
    pub fn spatial_token_bound_reserved(
        &self,
        state: SpatialCursorState,
        source_result_id: Option<&str>,
        request_hash: &str,
        reservation: Option<&ResultReservation>,
    ) -> ServiceResult<String> {
        if request_hash.len() != 64 || !request_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        self.create_spatial_token(
            state,
            source_result_id,
            Some(request_hash.to_owned()),
            reservation,
        )
    }
    pub fn check_spatial_request(
        &self,
        token: &str,
        binding: &CursorBinding,
        request_hash: &str,
    ) -> ServiceResult<()> {
        let _lock = self.store.lock()?;
        let record = self.spatial_record_locked(token, binding)?;
        if record.request_hash.as_deref() != Some(request_hash) {
            return Err(failure(ErrorCode::InvalidPageToken));
        }
        Ok(())
    }
    fn create_spatial_token(
        &self,
        mut state: SpatialCursorState,
        source_result_id: Option<&str>,
        request_hash: Option<String>,
        reservation: Option<&ResultReservation>,
    ) -> ServiceResult<String> {
        if state.page_size == 0 || state.page_size > 1000 {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        if serde_json::to_vec(&state)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len()
            > MAX_RESULT_BYTES
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        if let Some(id) = source_result_id {
            opaque_id(id)?;
        }
        state.expires_at_ms = state
            .expires_at_ms
            .min(now_ms().saturating_add(self.ttl_ms));
        let mut record = SpatialTokenRecord {
            binding: state.binding.clone(),
            expires_at_ms: state.expires_at_ms,
            source_result_id: source_result_id.map(str::to_owned),
            state: Some(state),
            in_flight: false,
            replay: None,
            checksum: String::new(),
            request_hash,
        };
        record.checksum = spatial_checksum(&record)?;
        let _lock = self.store.lock()?;
        self.cleanup_locked()?;
        self.check_spatial_locked(&record, &record.binding)?;
        if let Some(reservation) = reservation {
            self.consume_reservation_locked(reservation)?;
        }
        let id = format!("s-{}", uuid::Uuid::new_v4());
        self.write_locked(&id, &CacheRecord::SpatialToken(Box::new(record)))?;
        Ok(id)
    }
    fn spatial_record_locked(
        &self,
        token: &str,
        binding: &CursorBinding,
    ) -> ServiceResult<SpatialTokenRecord> {
        opaque_id(token).map_err(|_| failure(ErrorCode::InvalidPageToken))?;
        let record = match self.store.read::<CacheRecord>(token)? {
            Some(CacheRecord::SpatialToken(record)) => *record,
            _ => return Err(failure(ErrorCode::InvalidPageToken)),
        };
        self.check_spatial_locked(&record, binding)?;
        Ok(record)
    }
    fn check_spatial_locked(
        &self,
        record: &SpatialTokenRecord,
        binding: &CursorBinding,
    ) -> ServiceResult<()> {
        if record.expires_at_ms <= now_ms() {
            return Err(failure(ErrorCode::CursorExpired));
        }
        check_binding(&record.binding, binding)?;
        let marker = format!("a-{}", digest(binding.connection_id.as_bytes()));
        match self.store.read::<CacheRecord>(&marker)? {
            Some(CacheRecord::AuthRevision {
                connection_id,
                revision,
            }) if connection_id == binding.connection_id && revision == binding.auth_revision => {}
            None => {}
            _ => return Err(failure(ErrorCode::AuthRequired)),
        }
        if record.checksum != spatial_checksum(record)? {
            return Err(failure(ErrorCode::InvalidPageToken));
        }
        if let Some(state) = &record.state {
            check_binding(&state.binding, binding)?;
            if state.expires_at_ms != record.expires_at_ms
                || state.page_size == 0
                || state.page_size > 1000
            {
                return Err(failure(ErrorCode::InvalidPageToken));
            }
        }
        if let Some(source) = &record.source_result_id {
            self.snapshot_record_locked(source, binding)?;
        }
        Ok(())
    }
    pub fn begin_spatial_next(
        &self,
        token: &str,
        binding: &CursorBinding,
    ) -> ServiceResult<SpatialNextPage> {
        let _lock = self.store.lock()?;
        let mut record = self.spatial_record_locked(token, binding)?;
        if let Some(page) = record.replay {
            return Ok(SpatialNextPage::Replay(page));
        }
        if record.in_flight {
            return Err(failure(ErrorCode::RateLimited));
        }
        let state = record
            .state
            .clone()
            .ok_or_else(|| failure(ErrorCode::InvalidPageToken))?;
        record.in_flight = true;
        record.checksum = spatial_checksum(&record)?;
        self.write_locked(token, &CacheRecord::SpatialToken(Box::new(record)))?;
        Ok(SpatialNextPage::State(state))
    }
    pub fn finish_spatial_next(
        &self,
        token: &str,
        binding: &CursorBinding,
        page: StoredPage,
    ) -> ServiceResult<StoredPage> {
        if serde_json::to_vec(&page)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len()
            > MAX_PAGE_BYTES - 64 * 1024
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let _lock = self.store.lock()?;
        let mut record = self.spatial_record_locked(token, binding)?;
        if let Some(replay) = record.replay {
            return Ok(replay);
        }
        if !record.in_flight {
            return Err(failure(ErrorCode::InvalidPageToken));
        }
        record.in_flight = false;
        record.state = None;
        record.replay = Some(page.clone());
        record.checksum = spatial_checksum(&record)?;
        self.write_locked(token, &CacheRecord::SpatialToken(Box::new(record)))?;
        Ok(page)
    }
    pub fn fail_spatial_next(&self, token: &str, binding: &CursorBinding) -> ServiceResult<()> {
        opaque_id(token).map_err(|_| failure(ErrorCode::InvalidPageToken))?;
        let _lock = self.store.lock()?;
        match self.store.read::<CacheRecord>(token)? {
            Some(CacheRecord::SpatialToken(record)) => {
                check_binding(&record.binding, binding)?;
                self.store.remove(token)
            }
            None => Ok(()),
            _ => Err(failure(ErrorCode::InvalidPageToken)),
        }
    }
}

fn spatial_checksum(record: &SpatialTokenRecord) -> ServiceResult<String> {
    let tuple = (
        &record.binding,
        record.expires_at_ms,
        &record.source_result_id,
        &record.state,
        record.in_flight,
        &record.replay,
    );
    let mut value = match &record.request_hash {
        Some(hash) => serde_json::to_value((tuple, hash)),
        None => serde_json::to_value(tuple),
    }
    .map_err(|_| failure(ErrorCode::ResourceLimit))?;
    value.sort_all_objects();
    Ok(digest(
        &serde_json::to_vec(&value).map_err(|_| failure(ErrorCode::ResourceLimit))?,
    ))
}

pub struct ResultReservation {
    store: Arc<PrivateStore>,
    id: String,
}
impl Drop for ResultReservation {
    fn drop(&mut self) {
        if let Ok(_lock) = self.store.lock() {
            let _ = self.store.remove(&self.id);
        }
    }
}

pub enum NextPage {
    Replay(StoredPage),
    Materialized {
        result_id: String,
        set_index: usize,
        offset: usize,
        limit: usize,
    },
    Native(CursorState),
}

pub fn check_binding(actual: &CursorBinding, expected: &CursorBinding) -> ServiceResult<()> {
    if actual != expected {
        return Err(failure(ErrorCode::InvalidPageToken));
    }
    Ok(())
}

fn bound_snapshot(mut snapshot: ResultSnapshot) -> ServiceResult<ResultSnapshot> {
    let mut count = 0usize;
    let mut metadata = snapshot.clone();
    for set in &mut metadata.sets {
        set.rows.clear();
    }
    let mut bytes = serde_json::to_vec(&metadata)
        .map_err(|_| failure(ErrorCode::ResourceLimit))?
        .len()
        .saturating_add(1024);
    let mut truncated = false;
    if snapshot.sets.len() > 128 {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    for set in &mut snapshot.sets {
        if bytes > MAX_RESULT_BYTES {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let mut retained = Vec::new();
        for row in std::mem::take(&mut set.rows) {
            let size = serde_json::to_vec(&row)
                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                .len()
                + 1;
            if count >= MAX_RESULT_ROWS || bytes.saturating_add(size) > MAX_RESULT_BYTES {
                truncated = true;
                continue;
            }
            if truncated {
                continue;
            }
            bytes += size;
            count += 1;
            retained.push(row);
        }
        set.rows = retained;
    }
    if truncated {
        if !snapshot.allow_truncation {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        snapshot.limits.truncated = true;
        snapshot.limits.reasons.push(
            if count >= MAX_RESULT_ROWS {
                "row_limit"
            } else {
                "byte_limit"
            }
            .into(),
        );
    }
    Ok(snapshot)
}

fn bounded_rows(
    rows: &[Value],
    offset: usize,
    limit: usize,
    budget: usize,
) -> ServiceResult<(Vec<Value>, usize)> {
    if limit == 0 || offset > rows.len() {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let mut page = Vec::new();
    let mut bytes = 0usize;
    for row in rows.iter().skip(offset).take(limit) {
        let size = serde_json::to_vec(row)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len()
            + 1;
        if bytes.saturating_add(size) > budget {
            if page.is_empty() {
                return Err(failure(ErrorCode::DocumentTooLarge));
            }
            break;
        }
        bytes += size;
        page.push(row.clone());
    }
    let next = offset + page.len();
    Ok((page, next))
}
