mod files;
#[cfg(test)]
mod tests;

use super::{
    backend::CancellationSignal,
    contracts::{ErrorCode, Outcome},
    error::{failure, ServiceResult},
    results::{check_binding, CursorBinding},
    storage::{now_ms, PrivateStore},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
pub const ARTIFACT_TTL_MS: u64 = 15 * 60 * 1000;
pub const MAX_ARTIFACTS: usize = 64;
const ACTIVE_MS: u64 = 120_000;
const CLEANUP_GRACE_MS: u64 = 5000;
const MAX_DIRECTORY_ENTRIES: usize = 256;
const MAX_CHUNK_BYTES: usize = 65_536;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactBinding {
    pub caller_id: String,
    pub driver_id: String,
    pub cursor: CursorBinding,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactMetadata {
    pub artifact_id: String,
    pub filename: String,
    pub mime_type: String,
    pub bytes: u64,
    pub feature_count: u64,
    pub coordinate_count: u64,
    pub checksum: String,
    pub truncated: bool,
    pub reasons: Vec<String>,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactChunk {
    pub artifact_id: String,
    pub offset: u64,
    pub bytes_base64: String,
    pub eof: bool,
    pub checksum: String,
    pub chunk_checksum: String,
}

#[derive(Clone, Copy)]
pub struct ArtifactLimits {
    pub max_payload_bytes: u64,
    pub max_profile_bytes: u64,
    pub max_entries: usize,
    pub ttl_ms: u64,
}
impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            max_payload_bytes: MAX_ARTIFACT_BYTES,
            max_profile_bytes: MAX_ARTIFACT_BYTES,
            max_entries: MAX_ARTIFACTS,
            ttl_ms: ARTIFACT_TTL_MS,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    binding: ArtifactBinding,
    state: State,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum State {
    Pending {
        filename: String,
        max_bytes: u64,
        deadline_at_ms: u64,
    },
    Ready {
        metadata: ArtifactMetadata,
        identity: files::FileIdentity,
    },
}

pub struct ArtifactStore {
    store: PrivateStore,
    limits: ArtifactLimits,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl ArtifactStore {
    pub fn new(root: PathBuf) -> ServiceResult<Arc<Self>> {
        Self::with_limits(root, ArtifactLimits::default())
    }
    pub fn with_limits(root: PathBuf, limits: ArtifactLimits) -> ServiceResult<Arc<Self>> {
        Self::construct(root, limits, Arc::new(now_ms))
    }
    fn construct(
        root: PathBuf,
        limits: ArtifactLimits,
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> ServiceResult<Arc<Self>> {
        if limits.max_payload_bytes == 0
            || limits.max_payload_bytes > MAX_ARTIFACT_BYTES
            || limits.max_profile_bytes == 0
            || limits.max_profile_bytes > MAX_ARTIFACT_BYTES
            || limits.max_entries == 0
            || limits.max_entries > MAX_ARTIFACTS
            || limits.ttl_ms == 0
            || limits.ttl_ms > ARTIFACT_TTL_MS
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let root = files::prepare_root(&root)?;
        let store = Arc::new(Self {
            store: PrivateStore::new(root, 64 * 1024)?,
            limits,
            clock,
        });
        let _lock = store.lock()?;
        store.cleanup_locked()?;
        drop(_lock);
        Ok(store)
    }
    fn now(&self) -> u64 {
        (self.clock)()
    }
    fn lock(&self) -> ServiceResult<File> {
        files::directory(self.store.root())?;
        let file = self.store.lock()?;
        let identity = files::identity(&file)?;
        files::verify(&self.store.root().join("store.lock"), &file, &identity)?;
        Ok(file)
    }
    fn path(&self, id: &str, extension: &str) -> PathBuf {
        self.store.root().join(format!("{id}.{extension}"))
    }
    fn record(&self, id: &str) -> ServiceResult<Record> {
        files::read_json(&self.path(id, "json"))
    }
    fn ids(&self) -> ServiceResult<Vec<String>> {
        let mut ids = Vec::new();
        for (index, entry) in fs::read_dir(self.store.root())
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .enumerate()
        {
            if index >= MAX_DIRECTORY_ENTRIES {
                return Err(failure(ErrorCode::ResourceLimit));
            }
            let entry = entry.map_err(|_| failure(ErrorCode::ResourceLimit))?;
            if let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
            {
                if valid_id(id) {
                    ids.push(id.to_owned());
                }
            }
        }
        Ok(ids)
    }
    fn cleanup_locked(&self) -> ServiceResult<()> {
        let now = self.now();
        for id in self.ids()? {
            let record = self.record(&id)?;
            let expired = match record.state {
                State::Pending { deadline_at_ms, .. } => {
                    now >= deadline_at_ms.saturating_add(CLEANUP_GRACE_MS)
                }
                State::Ready { metadata, .. } => now >= metadata.expires_at_ms,
            };
            if expired {
                self.remove_locked(&id)?;
            }
        }
        Ok(())
    }
    fn remove_locked(&self, id: &str) -> ServiceResult<()> {
        for extension in ["part", "geojson"] {
            match fs::remove_file(self.path(id, extension)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(failure(ErrorCode::ResourceLimit)),
            }
        }
        self.store.remove(id)
    }
    fn checked_record(
        &self,
        id: &str,
        binding: &ArtifactBinding,
    ) -> ServiceResult<(ArtifactMetadata, files::FileIdentity)> {
        if !valid_id(id) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let record = self.record(id)?;
        validate_binding(&record.binding, binding)?;
        match record.state {
            State::Ready { metadata, identity }
                if metadata.artifact_id == id && self.now() < metadata.expires_at_ms =>
            {
                Ok((metadata, identity))
            }
            _ => Err(failure(ErrorCode::ResultExpired)),
        }
    }
    pub fn binding_for(&self, id: &str, caller_id: &str) -> ServiceResult<ArtifactBinding> {
        if !valid_id(id) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let _lock = self.lock()?;
        let record = self.record(id)?;
        if record.binding.caller_id != caller_id {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        match record.state {
            State::Ready { metadata, .. }
                if metadata.artifact_id == id && self.now() < metadata.expires_at_ms =>
            {
                Ok(record.binding)
            }
            _ => Err(failure(ErrorCode::ResultExpired)),
        }
    }
    pub fn reserve(
        self: &Arc<Self>,
        binding: ArtifactBinding,
        filename: String,
        max_bytes: u64,
        deadline_at_ms: u64,
    ) -> ServiceResult<ArtifactWriter> {
        validate_filename(&filename)?;
        validate_binding_input(&binding)?;
        validate_deadline(self.now(), deadline_at_ms)?;
        if max_bytes == 0 || max_bytes > self.limits.max_payload_bytes {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let _lock = self.lock()?;
        self.cleanup_locked()?;
        let ids = self.ids()?;
        let mut total = 0u64;
        for id in &ids {
            let charged = match self.record(id)?.state {
                State::Pending { max_bytes, .. } => max_bytes,
                State::Ready { metadata, .. } => metadata.bytes,
            };
            total = total
                .checked_add(charged)
                .ok_or_else(|| failure(ErrorCode::ResourceLimit))?;
        }
        if ids.len() >= self.limits.max_entries
            || total
                .checked_add(max_bytes)
                .filter(|total| *total <= self.limits.max_profile_bytes)
                .is_none()
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let id = format!("artifact-{}", uuid::Uuid::new_v4());
        let record = Record {
            binding: binding.clone(),
            state: State::Pending {
                filename,
                max_bytes,
                deadline_at_ms,
            },
        };
        self.store.write(&id, &record)?;
        let file = match files::create(&self.path(&id, "part")) {
            Ok(file) => file,
            Err(error) => {
                let _ = self.remove_locked(&id);
                return Err(error);
            }
        };
        Ok(ArtifactWriter {
            store: self.clone(),
            id,
            binding,
            max_bytes,
            deadline_at_ms,
            file: Some(file),
            bytes: 0,
            hasher: Sha256::new(),
            finished: false,
        })
    }
    pub fn metadata(&self, id: &str, binding: &ArtifactBinding) -> ServiceResult<ArtifactMetadata> {
        let _lock = self.lock()?;
        let (metadata, identity) = self.checked_record(id, binding)?;
        let path = self.path(id, "geojson");
        let file = files::open(&path)?;
        files::verify(&path, &file, &identity)?;
        if identity.bytes != metadata.bytes {
            return Err(failure(ErrorCode::ResultExpired));
        }
        Ok(metadata)
    }
    pub fn read_chunk(
        &self,
        id: &str,
        binding: &ArtifactBinding,
        offset: u64,
        limit: usize,
    ) -> ServiceResult<ArtifactChunk> {
        if limit == 0 || limit > MAX_CHUNK_BYTES {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let (metadata, identity) = {
            let _lock = self.lock()?;
            self.checked_record(id, binding)?
        };
        if offset > metadata.bytes {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let path = self.path(id, "geojson");
        let mut file = files::open(&path)?;
        files::verify(&path, &file, &identity)?;
        if identity.bytes != metadata.bytes {
            return Err(failure(ErrorCode::ResultExpired));
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| failure(ErrorCode::ResultExpired))?;
        let length = (metadata.bytes - offset).min(limit as u64) as usize;
        let mut bytes = vec![0; length];
        file.read_exact(&mut bytes)
            .map_err(|_| failure(ErrorCode::ResultExpired))?;
        files::verify(&path, &file, &identity)?;
        let reply = ArtifactChunk {
            artifact_id: id.to_owned(),
            offset,
            bytes_base64: STANDARD.encode(&bytes),
            eof: offset + length as u64 == metadata.bytes,
            checksum: metadata.checksum,
            chunk_checksum: format!("{:x}", Sha256::digest(&bytes)),
        };
        if serde_json::to_vec(&reply)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len()
            > 100 * 1024
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        Ok(reply)
    }
    pub fn copy_to_new(
        &self,
        id: &str,
        binding: &ArtifactBinding,
        destination: &Path,
        signal: &CancellationSignal,
        deadline_at_ms: u64,
        mut guard: impl FnMut() -> ServiceResult<()>,
    ) -> ServiceResult<ArtifactMetadata> {
        validate_deadline(self.now(), deadline_at_ms)?;
        check_copy(self.now(), deadline_at_ms, signal)?;
        guard()?;
        let metadata = self.metadata(id, binding)?;
        if !destination.is_absolute() || destination.file_name().is_none() {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
            .canonicalize()
            .map_err(|_| failure(ErrorCode::PermissionDenied))?;
        if !parent.is_dir() {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let destination = parent.join(
            destination
                .file_name()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
        );
        match fs::symlink_metadata(&destination) {
            Ok(_) => return Err(failure(ErrorCode::InvalidArgument)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(failure(ErrorCode::PermissionDenied)),
        }
        let source_path = self.path(id, "geojson");
        let mut source = files::open(&source_path)?;
        let (_, identity) = {
            let _lock = self.lock()?;
            self.checked_record(id, binding)?
        };
        files::verify(&source_path, &source, &identity)?;
        let mut temporary = files::CopyTemporary::new(&parent)?;
        let mut hash = Sha256::new();
        let mut copied = 0u64;
        let mut buffer = [0u8; MAX_CHUNK_BYTES];
        while copied < metadata.bytes {
            check_copy(self.now(), deadline_at_ms, signal)?;
            guard()?;
            files::verify(&source_path, &source, &identity)?;
            let length = (metadata.bytes - copied).min(MAX_CHUNK_BYTES as u64) as usize;
            source
                .read_exact(&mut buffer[..length])
                .map_err(|_| failure(ErrorCode::ResultExpired))?;
            temporary
                .file
                .write_all(&buffer[..length])
                .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            hash.update(&buffer[..length]);
            copied += length as u64;
            files::verify(&source_path, &source, &identity)?;
            guard()?;
        }
        if format!("{:x}", hash.finalize()) != metadata.checksum {
            return Err(failure(ErrorCode::ResultExpired));
        }
        temporary
            .file
            .flush()
            .and_then(|_| temporary.file.sync_all())
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        self.metadata(id, binding)?;
        guard()?;
        check_copy(self.now(), deadline_at_ms, signal)?;
        let temporary_identity = files::identity(&temporary.file)?;
        files::verify(&temporary.path, &temporary.file, &temporary_identity)?;
        fs::hard_link(&temporary.path, &destination)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        let applied = (|| {
            temporary.remove()?;
            files::sync_parent(&parent)?;
            guard()?;
            check_copy(self.now(), deadline_at_ms, signal)?;
            Ok(metadata)
        })();
        applied.map_err(|mut error: super::contracts::ServiceError| {
            error.outcome = Outcome::Applied;
            error
        })
    }
}

pub struct ArtifactWriter {
    store: Arc<ArtifactStore>,
    id: String,
    binding: ArtifactBinding,
    max_bytes: u64,
    deadline_at_ms: u64,
    file: Option<File>,
    bytes: u64,
    hasher: Sha256,
    finished: bool,
}
impl ArtifactWriter {
    pub fn artifact_id(&self) -> &str {
        &self.id
    }
    pub fn append(&mut self, bytes: &[u8]) -> ServiceResult<()> {
        if self.store.now() >= self.deadline_at_ms {
            return Err(failure(ErrorCode::DeadlineExceeded));
        }
        let total = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= self.max_bytes)
            .ok_or_else(|| failure(ErrorCode::ResourceLimit))?;
        self.file
            .as_mut()
            .ok_or_else(|| failure(ErrorCode::ResultExpired))?
            .write_all(bytes)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        self.hasher.update(bytes);
        self.bytes = total;
        Ok(())
    }
    pub fn finish(
        mut self,
        feature_count: u64,
        coordinate_count: u64,
        truncated: bool,
        reasons: Vec<String>,
    ) -> ServiceResult<ArtifactMetadata> {
        if feature_count > 9_007_199_254_740_991
            || coordinate_count > 9_007_199_254_740_991
            || reasons.len() > 64
            || reasons.iter().any(|reason| {
                reason.is_empty() || reason.len() > 256 || reason.chars().any(char::is_control)
            })
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let mut file = self
            .file
            .take()
            .ok_or_else(|| failure(ErrorCode::ResultExpired))?;
        file.flush()
            .and_then(|_| file.sync_all())
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        let _lock = self.store.lock()?;
        let record = self.store.record(&self.id)?;
        validate_binding(&record.binding, &self.binding)?;
        let filename = match record.state {
            State::Pending {
                filename,
                max_bytes,
                deadline_at_ms,
            } if max_bytes == self.max_bytes && deadline_at_ms == self.deadline_at_ms => filename,
            _ => return Err(failure(ErrorCode::ResultExpired)),
        };
        if self.store.now() >= self.deadline_at_ms {
            return Err(failure(ErrorCode::DeadlineExceeded));
        }
        let part = self.store.path(&self.id, "part");
        let before = files::identity(&file)?;
        files::verify(&part, &file, &before)?;
        if before.bytes != self.bytes
            || fs::symlink_metadata(self.store.path(&self.id, "geojson")).is_ok()
        {
            return Err(failure(ErrorCode::ResultExpired));
        }
        fs::rename(&part, self.store.path(&self.id, "geojson"))
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        files::sync_parent(self.store.store.root())?;
        let identity = files::identity(&file)?;
        let created_at_ms = self.store.now();
        let metadata = ArtifactMetadata {
            artifact_id: self.id.clone(),
            filename,
            mime_type: "application/geo+json".into(),
            bytes: self.bytes,
            feature_count,
            coordinate_count,
            checksum: format!("{:x}", self.hasher.clone().finalize()),
            truncated,
            reasons,
            created_at_ms,
            expires_at_ms: created_at_ms.saturating_add(self.store.limits.ttl_ms),
        };
        if self.store.now() >= self.deadline_at_ms {
            return Err(failure(ErrorCode::DeadlineExceeded));
        }
        self.store.store.write(
            &self.id,
            &Record {
                binding: self.binding.clone(),
                state: State::Ready {
                    metadata: metadata.clone(),
                    identity,
                },
            },
        )?;
        self.finished = true;
        Ok(metadata)
    }
}
impl Drop for ArtifactWriter {
    fn drop(&mut self) {
        self.file.take();
        if !self.finished {
            if let Ok(_lock) = self.store.lock() {
                if self
                    .store
                    .record(&self.id)
                    .is_ok_and(|record| record.binding == self.binding)
                {
                    let _ = self.store.remove_locked(&self.id);
                }
            }
        }
    }
}

fn valid_id(id: &str) -> bool {
    id.strip_prefix("artifact-")
        .and_then(|id| {
            uuid::Uuid::parse_str(id).ok().filter(|uuid| {
                uuid.get_version_num() == 4
                    && uuid.as_bytes()[8] & 0xc0 == 0x80
                    && uuid.to_string() == id
            })
        })
        .is_some()
}
fn validate_filename(filename: &str) -> ServiceResult<()> {
    if filename.is_empty()
        || filename.len() > 255
        || filename == "."
        || filename == ".."
        || filename
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':'))
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(())
}
fn validate_binding_input(binding: &ArtifactBinding) -> ServiceResult<()> {
    let cursor = &binding.cursor;
    let required = [
        &binding.caller_id,
        &binding.driver_id,
        &cursor.connection_id,
        &cursor.permission_scope,
        &cursor.auth_revision,
        &cursor.query_hash,
        &cursor.result_revision,
    ];
    let optional = [
        &cursor.sdk_version,
        &cursor.container_rid,
        &cursor.endpoint_identity,
        &cursor.partition_hash,
    ];
    if required
        .into_iter()
        .any(|value| value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control))
        || optional.into_iter().flatten().any(|value| {
            value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control)
        })
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(())
}
fn validate_binding(actual: &ArtifactBinding, expected: &ArtifactBinding) -> ServiceResult<()> {
    if actual.caller_id != expected.caller_id || actual.driver_id != expected.driver_id {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    check_binding(&actual.cursor, &expected.cursor)
}
fn validate_deadline(now: u64, deadline: u64) -> ServiceResult<()> {
    if deadline <= now {
        return Err(failure(ErrorCode::DeadlineExceeded));
    }
    if deadline - now > ACTIVE_MS {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(())
}
fn check_copy(now: u64, deadline: u64, signal: &CancellationSignal) -> ServiceResult<()> {
    if signal.is_cancelled() {
        return Err(failure(ErrorCode::Cancelled));
    }
    if now >= deadline {
        return Err(failure(ErrorCode::DeadlineExceeded));
    }
    Ok(())
}
