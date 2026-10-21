use crate::services::{
    contracts::{ErrorCode, Limits, Metrics, Operation, Page},
    error::{failure, ServiceResult},
    results::{
        CursorBinding, ResultKind, ResultReservation, ResultSet, ResultSnapshot, ResultStore,
        SpatialCursorState, StoredPage, MAX_PAGE_BYTES, RESULT_TTL_MS,
    },
    security::value_hash,
    storage::now_ms,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tabularis_spatial_core::{
    CoreError, FeatureError, FeaturePage, LayerBudget, LayerLimits, LayerSource, LimitStatus,
    PageBuilder, PageInfo, PageLimits, ResumeMode,
};

pub const PUBLIC_PAGE_BYTES: usize = MAX_PAGE_BYTES - 64 * 1024;
pub const MAX_TRANSFORM_CALLS: usize = 2048;
const TOKEN_PLACEHOLDER: &str = "s-00000000-0000-4000-8000-000000000000";

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub features: u64,
    pub coordinates: u64,
    pub bytes: u64,
}
impl From<LayerBudget> for Budget {
    fn from(value: LayerBudget) -> Self {
        Self {
            features: value.features,
            coordinates: value.coordinates,
            bytes: value.bytes,
        }
    }
}
impl From<Budget> for LayerBudget {
    fn from(value: Budget) -> Self {
        Self {
            features: value.features,
            coordinates: value.coordinates,
            bytes: value.bytes,
        }
    }
}

pub fn core_error(error: CoreError) -> crate::services::contracts::ServiceError {
    use tabularis_spatial_core::ErrorCode as Core;
    failure(match error.code {
        Core::UnknownSrid => ErrorCode::SridRequired,
        Core::InvalidGeometry => ErrorCode::InvalidGeometry,
        Core::UnsupportedType => ErrorCode::UnsupportedType,
        Core::UnsupportedOperation => ErrorCode::UnsupportedOperation,
        Core::FeatureTooLarge => ErrorCode::FeatureTooLarge,
        Core::ResourceLimit => ErrorCode::ResourceLimit,
        Core::VersionConflict | Core::StaleGeneration => ErrorCode::VersionConflict,
        _ => ErrorCode::InvalidArgument,
    })
}
pub fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> ServiceResult<T> {
    serde_json::from_value(value.clone()).map_err(|_| failure(ErrorCode::InvalidArgument))
}
pub fn source_hash(source: &LayerSource) -> ServiceResult<String> {
    let mut value =
        serde_json::to_value(source).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    value.sort_all_objects();
    Ok(value_hash(&value))
}
pub fn request_hash(
    source: &LayerSource,
    page_size: usize,
    operation: Operation,
) -> ServiceResult<String> {
    let mut value = json!({"source":source,"page_size":page_size,"operation":operation});
    value.sort_all_objects();
    Ok(value_hash(&value))
}
pub fn source_from_input(input: &Value, kind: &str) -> ServiceResult<LayerSource> {
    let mut value = input.clone();
    let object = value
        .as_object_mut()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    object.remove("next_token");
    object.remove("page_size");
    object.insert("kind".into(), json!(kind));
    let source: LayerSource = decode(&value)?;
    source.validate().map_err(core_error)?;
    let srid = match &source {
        LayerSource::QueryResult { source_srid, .. } | LayerSource::Table { source_srid, .. } => {
            *source_srid
        }
    };
    if srid.is_some_and(|srid| srid > 999_999) {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(source)
}
pub fn page_size(input: &Value) -> ServiceResult<usize> {
    let size = input
        .get("page_size")
        .and_then(Value::as_u64)
        .unwrap_or(1000);
    if !(1..=1000).contains(&size) {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    Ok(size as usize)
}
pub fn safe_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
pub fn checked_metadata(page: &mut FeaturePage) -> ServiceResult<()> {
    if page.warnings.len() > 64
        || page.limits.reasons.len() > 64
        || page.feature_errors.len() > 1000
        || page
            .warnings
            .iter()
            .chain(&page.limits.reasons)
            .any(|code| !safe_code(code))
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    for error in &mut page.feature_errors {
        if error.index > 9_007_199_254_740_991
            || !matches!(
                error.code.as_str(),
                "UNSUPPORTED_TYPE" | "INVALID_GEOMETRY" | "FEATURE_TOO_LARGE"
            )
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        error.message = "The spatial value cannot be displayed.".into();
    }
    let metadata =
        json!({"warnings":page.warnings,"limits":page.limits,"feature_errors":page.feature_errors});
    if serde_json::to_vec(&metadata)
        .map_err(|_| failure(ErrorCode::ResourceLimit))?
        .len()
        > 192 * 1024
    {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    Ok(())
}
pub fn builder(
    size: usize,
    budget: Budget,
    limits: LimitStatus,
    warnings: Vec<String>,
    errors: Vec<FeatureError>,
) -> ServiceResult<PageBuilder> {
    let metadata = FeaturePage {
        page: PageInfo {
            next_token: Some(TOKEN_PLACEHOLDER.into()),
            has_more: true,
            resume_mode: ResumeMode::Materialized,
        },
        limits,
        warnings,
        feature_errors: errors,
        ..Default::default()
    };
    let stored = stored_page(
        metadata.clone(),
        "r-00000000-0000-4000-8000-000000000000".into(),
    )?;
    let overhead = serde_json::to_vec(&stored)
        .map_err(|_| failure(ErrorCode::ResourceLimit))?
        .len()
        .saturating_sub(metadata.serialized_bytes().map_err(core_error)? as usize);
    let max_bytes = PUBLIC_PAGE_BYTES
        .checked_sub(overhead)
        .ok_or_else(|| failure(ErrorCode::ResourceLimit))?;
    let mut builder = PageBuilder::new(
        PageLimits {
            max_features: size as u64,
            max_coordinates: 100_000,
            max_bytes: max_bytes as u64,
        },
        LayerLimits::default(),
        budget.into(),
    )
    .map_err(core_error)?;
    builder
        .set_metadata(
            metadata.page,
            metadata.limits,
            metadata.warnings,
            metadata.feature_errors,
        )
        .map_err(core_error)?;
    Ok(builder)
}
pub fn finish_builder(
    mut builder: PageBuilder,
    more: bool,
    mode: ResumeMode,
    prior: Budget,
    layer_full: bool,
) -> ServiceResult<(FeaturePage, Budget)> {
    let mut metadata = builder.page().clone();
    let limits = LayerLimits::default();
    let layer_full = layer_full
        || (more
            && (prior
                .features
                .saturating_add(metadata.features.len() as u64)
                >= limits.max_features
                || prior
                    .coordinates
                    .saturating_add(metadata.coordinate_count().map_err(core_error)?)
                    >= limits.max_coordinates
                || prior
                    .bytes
                    .saturating_add(metadata.serialized_bytes().map_err(core_error)?)
                    >= limits.max_bytes));
    if layer_full {
        metadata.limits.truncate("LAYER_LIMIT");
    }
    builder
        .set_metadata(
            if more && !layer_full {
                PageInfo {
                    next_token: Some(TOKEN_PLACEHOLDER.into()),
                    has_more: true,
                    resume_mode: mode,
                }
            } else {
                PageInfo::default()
            },
            metadata.limits,
            metadata.warnings,
            metadata.feature_errors,
        )
        .map_err(core_error)?;
    let (page, budget) = builder.finish().map_err(core_error)?;
    Ok((page, budget.into()))
}
pub fn marker(source: &LayerSource) -> ServiceResult<Value> {
    Ok(json!({"kind":"spatial_page_v1","source":source,"source_hash":source_hash(source)?}))
}
pub fn snapshot_page(
    page: &FeaturePage,
    source: &LayerSource,
    binding: &CursorBinding,
    raw_rows: &[Value],
) -> ServiceResult<ResultSnapshot> {
    if page.features.len() != raw_rows.len() {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let rows = page.features.iter().zip(&page.row_references).zip(raw_rows).map(|((feature, reference), raw)| json!({"feature":feature,"reference":reference,"raw_row":raw})).collect();
    Ok(ResultSnapshot {
        sets: vec![ResultSet {
            kind: ResultKind::JsonValues,
            columns: vec![marker(source)?],
            rows,
        }],
        revision: binding.result_revision.clone(),
        limits: Limits {
            truncated: page.limits.truncated,
            reasons: page.limits.reasons.clone(),
        },
        allow_truncation: false,
        warnings: page.warnings.clone(),
    })
}
pub fn cursor_state(binding: &CursorBinding, state: Value, size: usize) -> SpatialCursorState {
    SpatialCursorState {
        binding: binding.clone(),
        state,
        page_size: size,
        expires_at_ms: now_ms().saturating_add(RESULT_TTL_MS),
    }
}
pub fn stored_page(page: FeaturePage, result_id: String) -> ServiceResult<StoredPage> {
    let stored = StoredPage {
        data: serde_json::to_value(&page).map_err(|_| failure(ErrorCode::ResourceLimit))?,
        page: Page {
            next_token: page.page.next_token,
            has_more: page.page.has_more,
            resume_mode: match page.page.resume_mode {
                ResumeMode::None => crate::services::contracts::ResumeMode::None,
                ResumeMode::Native => crate::services::contracts::ResumeMode::Native,
                ResumeMode::Materialized => crate::services::contracts::ResumeMode::Materialized,
            },
        },
        result_id: Some(result_id),
        limits: Limits {
            truncated: page.limits.truncated,
            reasons: page.limits.reasons,
        },
        metrics: Metrics::default(),
        warnings: page.warnings,
    };
    if serde_json::to_vec(&stored)
        .map_err(|_| failure(ErrorCode::ResourceLimit))?
        .len()
        > PUBLIC_PAGE_BYTES
    {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    Ok(stored)
}
pub struct Publication<'a> {
    store: &'a ResultStore,
    binding: &'a CursorBinding,
    result: Option<String>,
    token: Option<String>,
    committed: bool,
}
impl<'a> Publication<'a> {
    pub fn new(store: &'a ResultStore, binding: &'a CursorBinding) -> Self {
        Self {
            store,
            binding,
            result: None,
            token: None,
            committed: false,
        }
    }
    pub fn cache(
        &mut self,
        snapshot: ResultSnapshot,
        reservation: &ResultReservation,
    ) -> ServiceResult<String> {
        let id = self
            .store
            .insert_reserved(self.binding.clone(), snapshot, Some(reservation))?;
        self.result = Some(id.clone());
        Ok(id)
    }
    pub fn token(
        &mut self,
        state: SpatialCursorState,
        original: Option<&str>,
        hash: &str,
    ) -> ServiceResult<String> {
        let id = self
            .store
            .spatial_token_bound_reserved(state, original, hash, None)?;
        self.token = Some(id.clone());
        Ok(id)
    }
    pub fn commit(mut self) {
        self.committed = true;
    }
}
impl Drop for Publication<'_> {
    fn drop(&mut self) {
        if !self.committed {
            if let Some(token) = &self.token {
                let _ = self.store.fail_spatial_next(token, self.binding);
            }
            if let Some(id) = &self.result {
                let _ = self.store.release(id, self.binding);
            }
        }
    }
}
