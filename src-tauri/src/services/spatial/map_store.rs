use crate::{
    persistence,
    services::{
        contracts::{ErrorCode, Operation, Outcome, ServiceRequest},
        dispatch::CallerContext,
        error::{failure, ServiceResult},
        jobs::JobLease,
        results::CursorBinding,
        security::{opaque_id, value_hash},
        storage::now_ms,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tabularis_spatial_core::{
    BasemapChange, FeatureRef, LayerSource, LayerState, MapState, Style, Viewport, MAX_SAFE_INTEGER,
};

const LEDGER_TTL_MS: u64 = 15 * 60 * 1000;
const MAX_LEDGER: usize = 64;
#[derive(Clone, Copy)]
pub struct MapStoreLimits {
    pub maps: usize,
    pub layers: usize,
    pub selected: usize,
    pub state_bytes: usize,
    pub record_bytes: usize,
}
impl Default for MapStoreLimits {
    fn default() -> Self {
        Self {
            maps: 128,
            layers: 32,
            selected: 1000,
            state_bytes: 32 * 1024,
            record_bytes: 256 * 1024,
        }
    }
}

#[derive(Clone)]
pub struct StoredMap {
    pub map: MapState,
    pub apply_generation: u64,
    pub replayed: bool,
    pub(crate) source_bindings: BTreeMap<String, CursorBinding>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerEntry {
    caller: String,
    request_id: String,
    hash: String,
    operation: Operation,
    post_version: u64,
    apply_generation: u64,
    created_at_ms: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MapRecord {
    storage_version: u8,
    owner: String,
    state: MapState,
    apply_generation: u64,
    checkpoint_ms: Option<u64>,
    create_entry: LedgerEntry,
    ledger: Vec<LedgerEntry>,
    #[serde(default)]
    source_bindings: BTreeMap<String, CursorBinding>,
}
impl MapRecord {
    fn public(&self, replayed: bool) -> StoredMap {
        StoredMap {
            map: self.state.clone(),
            apply_generation: self.apply_generation,
            replayed,
            source_bindings: self.source_bindings.clone(),
        }
    }
}

pub enum MapChange {
    Update {
        name: Option<String>,
        basemap: BasemapChange,
    },
    Add {
        layer: Box<LayerState>,
        binding: Option<CursorBinding>,
    },
    Remove(String),
    Layer {
        id: String,
        visible: Option<bool>,
        style: Option<Style>,
    },
    Viewport(Viewport),
    Selection(Vec<FeatureRef>),
    Open,
    Close,
    Save,
}
impl MapChange {
    fn operation(&self) -> Operation {
        match self {
            Self::Update { .. } => Operation::MapUpdate,
            Self::Add { .. } => Operation::MapLayerAdd,
            Self::Remove(_) => Operation::MapLayerRemove,
            Self::Layer { .. } => Operation::MapLayerUpdate,
            Self::Viewport(_) => Operation::MapViewportSet,
            Self::Selection(_) => Operation::MapSelectionSet,
            Self::Open => Operation::MapOpen,
            Self::Close => Operation::MapClose,
            Self::Save => Operation::MapSave,
        }
    }
    fn requires_version(&self) -> bool {
        !matches!(self, Self::Open | Self::Close)
    }
}
pub struct MapStore {
    root: PathBuf,
    limits: MapStoreLimits,
}
impl MapStore {
    pub fn new(profile: &Path) -> ServiceResult<Self> {
        Self::with_limits(profile, MapStoreLimits::default())
    }
    pub fn with_limits(profile: &Path, limits: MapStoreLimits) -> ServiceResult<Self> {
        let maximum = MapStoreLimits::default();
        if !profile.is_absolute()
            || [
                limits.maps,
                limits.layers,
                limits.selected,
                limits.state_bytes,
                limits.record_bytes,
            ]
            .contains(&0)
            || limits.maps > maximum.maps
            || limits.layers > maximum.layers
            || limits.selected > maximum.selected
            || limits.state_bytes > maximum.state_bytes
            || limits.record_bytes > maximum.record_bytes
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let store = Self {
            root: profile.join("maps-v1"),
            limits,
        };
        let _lock = store.global_lock()?;
        Ok(store)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn global_lock(&self) -> ServiceResult<std::fs::File> {
        persistence::service_lock_file(&self.root.join("maps.lock"))
    }
    fn map_lock(&self, id: &str) -> ServiceResult<std::fs::File> {
        opaque_id(id)?;
        persistence::service_lock_file(&self.root.join(format!("{id}.lock")))
    }
    fn path(&self, id: &str) -> ServiceResult<PathBuf> {
        opaque_id(id)?;
        Ok(self.root.join(format!("{id}.json")))
    }
    fn ids_locked(&self) -> ServiceResult<Vec<String>> {
        let mut ids = Vec::new();
        for entry in
            std::fs::read_dir(&self.root).map_err(|_| failure(ErrorCode::PermissionDenied))?
        {
            let entry = entry.map_err(|_| failure(ErrorCode::PermissionDenied))?;
            if let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
            {
                opaque_id(id)?;
                ids.push(id.to_owned());
                if ids.len() > self.limits.maps {
                    return Err(failure(ErrorCode::ResourceLimit));
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
    fn read_record(&self, id: &str) -> ServiceResult<MapRecord> {
        let bytes =
            persistence::service_read_file(&self.path(id)?, self.limits.record_bytes as u64, true)?
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let raw: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| failure(ErrorCode::InvalidArgument))?;
        check_query_bindings(&raw)?;
        let record: MapRecord =
            serde_json::from_value(raw).map_err(|_| failure(ErrorCode::InvalidArgument))?;
        self.validate_record(&record)?;
        if record.state.map_id != id {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        Ok(record)
    }
    fn validate_record(&self, record: &MapRecord) -> ServiceResult<()> {
        if record.storage_version != 1
            || record.apply_generation > MAX_SAFE_INTEGER
            || record.ledger.len() > MAX_LEDGER
            || record.owner.is_empty()
            || record.owner.len() > 512
            || record.owner.chars().any(char::is_control)
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        validate_map(&record.state, self.limits)?;
        for entry in std::iter::once(&record.create_entry).chain(record.ledger.iter()) {
            opaque_id(&entry.request_id)?;
            if entry.caller != record.owner
                || entry.hash.len() != 64
                || !entry.hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !super::MAP_OPERATIONS.contains(&entry.operation)
                || entry.post_version > MAX_SAFE_INTEGER
                || entry.apply_generation > MAX_SAFE_INTEGER
            {
                return Err(failure(ErrorCode::InvalidArgument));
            }
        }
        if record.create_entry.operation != Operation::MapCreate {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        for (id, binding) in &record.source_bindings {
            if !record.state.layers.iter().any(|layer| {
                layer.layer_id == *id
                    && layer.connection_id == binding.connection_id
                    && matches!(layer.source, LayerSource::QueryResult { .. })
            }) {
                return Err(failure(ErrorCode::AuthRequired));
            }
        }
        for layer in &record.state.layers {
            if matches!(layer.source, LayerSource::QueryResult { .. })
                && !record.source_bindings.contains_key(&layer.layer_id)
            {
                return Err(failure(ErrorCode::AuthRequired));
            }
        }
        Ok(())
    }
    fn write_record(&self, record: &MapRecord, lease: &JobLease) -> ServiceResult<()> {
        self.validate_record(record)?;
        let bytes = serde_json::to_vec(record).map_err(|_| failure(ErrorCode::ResourceLimit))?;
        if bytes.len() > self.limits.record_bytes {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        lease.mark_write_dispatched()?;
        let publication =
            persistence::service_write_atomic(&self.path(&record.state.map_id)?, &bytes);
        if publication.is_ok()
            || publication
                .as_ref()
                .is_err_and(|error| error.outcome == Outcome::Applied)
        {
            confirm_published(lease)?;
        }
        publication
    }
    pub fn get(&self, id: &str, caller: &CallerContext) -> ServiceResult<StoredMap> {
        let _lock = self.map_lock(id)?;
        let record = self.read_record(id)?;
        owner(&record, caller)?;
        Ok(record.public(false))
    }
    pub fn list(&self, caller: &CallerContext) -> ServiceResult<Vec<StoredMap>> {
        let ids = {
            let _lock = self.global_lock()?;
            self.ids_locked()?
        };
        let mut maps = Vec::new();
        for id in ids {
            let _lock = self.map_lock(&id)?;
            let record = self.read_record(&id)?;
            if record.owner == caller.caller_id {
                maps.push(record.public(false));
            }
        }
        Ok(maps)
    }
    pub fn inspect(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
    ) -> ServiceResult<StoredMap> {
        let id = request
            .map_id
            .as_deref()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let _lock = self.map_lock(id)?;
        let record = self.read_record(id)?;
        owner(&record, caller)?;
        let replay = replay_entry(&record, request, caller)?;
        Ok(record.public(replay))
    }
    pub fn create(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        map: MapState,
        lease: &JobLease,
    ) -> ServiceResult<StoredMap> {
        if request.operation != Operation::MapCreate
            || map.version != 0
            || !map.layers.is_empty()
            || !map.selected_feature_refs.is_empty()
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        validate_map(&map, self.limits)?;
        opaque_id(&request.request_id)?;
        let _global = self.global_lock()?;
        let ids = self.ids_locked()?;
        for id in &ids {
            let record = self.read_record(id)?;
            if record.owner == caller.caller_id
                && record.create_entry.request_id == request.request_id
            {
                let _map = self.map_lock(id)?;
                let record = self.read_record(id)?;
                owner(&record, caller)?;
                check_replay(&record, &record.create_entry, request)?;
                confirm_published(lease)?;
                return Ok(record.public(true));
            }
        }
        if ids.len() >= self.limits.maps || self.path(&map.map_id)?.exists() {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let _map = self.map_lock(&map.map_id)?;
        let record = MapRecord {
            storage_version: 1,
            owner: caller.caller_id.clone(),
            create_entry: entry(request, caller, map.version, 0)?,
            state: map,
            apply_generation: 0,
            checkpoint_ms: None,
            ledger: Vec::new(),
            source_bindings: BTreeMap::new(),
        };
        self.write_record(&record, lease)?;
        Ok(record.public(false))
    }
    pub fn mutate(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        observed: &StoredMap,
        change: MapChange,
        lease: &JobLease,
    ) -> ServiceResult<StoredMap> {
        if request.operation != change.operation() {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let id = request
            .map_id
            .as_deref()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let _lock = self.map_lock(id)?;
        let mut record = self.read_record(id)?;
        owner(&record, caller)?;
        if replay_entry(&record, request, caller)? {
            confirm_published(lease)?;
            return Ok(record.public(true));
        }
        if record.state.version != observed.map.version
            || record.apply_generation != observed.apply_generation
        {
            return Err(conflict(record.state.version, false));
        }
        if (change.requires_version() && request.expected_version != Some(record.state.version))
            || request
                .expected_version
                .is_some_and(|version| version != record.state.version)
        {
            return Err(conflict(record.state.version, false));
        }
        let version = record.state.version;
        let checkpoint = matches!(change, MapChange::Save);
        match change {
            MapChange::Update { name, basemap } => {
                record
                    .state
                    .update(version, name, basemap)
                    .map_err(core_error)?;
            }
            MapChange::Add { layer, binding } => {
                let id = layer.layer_id.clone();
                if let Some(binding) = binding {
                    record.source_bindings.insert(id, binding);
                }
                record
                    .state
                    .add_layer(version, *layer)
                    .map_err(core_error)?;
            }
            MapChange::Remove(id) => {
                record
                    .state
                    .remove_layer(version, &id)
                    .map_err(core_error)?;
                record.source_bindings.remove(&id);
            }
            MapChange::Layer { id, visible, style } => {
                record
                    .state
                    .update_layer(version, &id, visible, style)
                    .map_err(core_error)?;
            }
            MapChange::Viewport(viewport) => {
                record
                    .state
                    .set_viewport(version, viewport)
                    .map_err(core_error)?;
            }
            MapChange::Selection(refs) => {
                record
                    .state
                    .set_selection(version, refs)
                    .map_err(core_error)?;
            }
            MapChange::Open | MapChange::Close => {}
            MapChange::Save => {
                record.checkpoint_ms = Some(now_ms());
            }
        }
        if !checkpoint {
            record.apply_generation = record
                .apply_generation
                .checked_add(1)
                .filter(|value| *value <= MAX_SAFE_INTEGER)
                .ok_or_else(|| failure(ErrorCode::ResourceLimit))?;
        }
        record
            .ledger
            .retain(|entry| entry.created_at_ms.saturating_add(LEDGER_TTL_MS) > now_ms());
        if record.ledger.len() >= MAX_LEDGER {
            record.ledger.remove(0);
        }
        record.ledger.push(entry(
            request,
            caller,
            record.state.version,
            record.apply_generation,
        )?);
        self.write_record(&record, lease)?;
        Ok(record.public(false))
    }
    pub fn confirm_replay(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
    ) -> ServiceResult<StoredMap> {
        let id = request
            .map_id
            .as_deref()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let _lock = self.map_lock(id)?;
        let record = self.read_record(id)?;
        owner(&record, caller)?;
        if !replay_entry(&record, request, caller)? {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        confirm_published(lease)?;
        Ok(record.public(true))
    }
}
fn confirm_published(lease: &JobLease) -> ServiceResult<()> {
    lease.mark_write_confirmed().map_err(|mut error| {
        error.outcome = Outcome::Applied;
        error
    })
}
fn check_query_bindings(record: &serde_json::Value) -> ServiceResult<()> {
    if let Some(layers) = record["state"]["layers"].as_array() {
        for layer in layers {
            if layer["source"]["kind"] == "query_result" {
                let id = layer["layer_id"]
                    .as_str()
                    .ok_or_else(|| failure(ErrorCode::AuthRequired))?;
                let binding: CursorBinding =
                    serde_json::from_value(record["source_bindings"][id].clone())
                        .map_err(|_| failure(ErrorCode::AuthRequired))?;
                if Some(binding.connection_id.as_str()) != layer["connection_id"].as_str()
                    || binding.auth_revision.is_empty()
                    || binding.permission_scope.is_empty()
                    || binding.query_hash.is_empty()
                    || binding.result_revision.is_empty()
                {
                    return Err(failure(ErrorCode::AuthRequired));
                }
            }
        }
    }
    Ok(())
}
fn owner(record: &MapRecord, caller: &CallerContext) -> ServiceResult<()> {
    if record.owner != caller.caller_id {
        Err(failure(ErrorCode::PermissionDenied))
    } else {
        Ok(())
    }
}
fn entry(
    request: &ServiceRequest,
    caller: &CallerContext,
    version: u64,
    generation: u64,
) -> ServiceResult<LedgerEntry> {
    opaque_id(&request.request_id)?;
    Ok(LedgerEntry {
        caller: caller.caller_id.clone(),
        request_id: request.request_id.clone(),
        hash: request_hash(request)?,
        operation: request.operation,
        post_version: version,
        apply_generation: generation,
        created_at_ms: now_ms(),
    })
}
fn request_hash(request: &ServiceRequest) -> ServiceResult<String> {
    let mut value =
        serde_json::to_value(request).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    value.sort_all_objects();
    Ok(value_hash(&value))
}
fn replay_entry(
    record: &MapRecord,
    request: &ServiceRequest,
    caller: &CallerContext,
) -> ServiceResult<bool> {
    if let Some(entry) = record.ledger.iter().find(|entry| {
        entry.caller == caller.caller_id
            && entry.request_id == request.request_id
            && entry.created_at_ms.saturating_add(LEDGER_TTL_MS) > now_ms()
    }) {
        check_replay(record, entry, request)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
fn check_replay(
    record: &MapRecord,
    entry: &LedgerEntry,
    request: &ServiceRequest,
) -> ServiceResult<()> {
    if entry.hash != request_hash(request)? || entry.operation != request.operation {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    if record.state.version != entry.post_version
        || record.apply_generation != entry.apply_generation
    {
        return Err(conflict(record.state.version, true));
    }
    Ok(())
}
pub(crate) fn conflict(version: u64, applied: bool) -> crate::services::contracts::ServiceError {
    let mut error = failure(ErrorCode::VersionConflict);
    error.details = json!({"current_version":version});
    if applied {
        error.outcome = Outcome::Applied;
    }
    error
}
pub(crate) fn core_error(
    error: tabularis_spatial_core::CoreError,
) -> crate::services::contracts::ServiceError {
    let code = match error.code {
        tabularis_spatial_core::ErrorCode::VersionConflict => ErrorCode::VersionConflict,
        tabularis_spatial_core::ErrorCode::ResourceLimit => ErrorCode::ResourceLimit,
        tabularis_spatial_core::ErrorCode::NotFound => ErrorCode::InvalidArgument,
        _ => ErrorCode::InvalidArgument,
    };
    let mut result = failure(code);
    if let Some(version) = error.current_version {
        result.details = json!({"current_version":version});
    }
    result
}
fn validate_map(map: &MapState, limits: MapStoreLimits) -> ServiceResult<()> {
    map.validate().map_err(core_error)?;
    opaque_id(&map.map_id)?;
    if map.name.chars().count() > 256 || map.name.chars().any(char::is_control) {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    if map.layers.len() > limits.layers
        || map.selected_feature_refs.len() > limits.selected
        || serde_json::to_vec(map)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len()
            > limits.state_bytes
    {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    for layer in &map.layers {
        opaque_id(&layer.layer_id)?;
        if layer.connection_id.chars().any(char::is_control) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
    }
    for reference in &map.selected_feature_refs {
        if reference.feature_id.chars().count() > 128
            || reference.feature_id.chars().any(char::is_control)
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
    }
    Ok(())
}
