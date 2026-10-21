use super::{
    data_domain::{PreparedExecution, SpatialDataDomain},
    paging::{self, Budget, MAX_TRANSFORM_CALLS, PUBLIC_PAGE_BYTES},
};
use crate::{
    drivers::driver_trait::ServiceRpcMethod,
    services::{
        contracts::ErrorCode,
        error::{failure, ServiceResult},
        results::{CursorBinding, ResultKind},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use tabularis_spatial_core::{
    query_feature_id, Feature, FeatureError, Geometry, LayerSource, LimitStatus, PushResult,
    ResumeMode, RowReference, SpatialValue,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryCursor {
    pub source: LayerSource,
    pub source_hash: String,
    pub page_size: usize,
    pub next_row_offset: usize,
    pub budget: Budget,
}
pub struct BuiltPage {
    pub page: tabularis_spatial_core::FeaturePage,
    pub raw_rows: Vec<Value>,
    pub state: Option<Value>,
    pub original_result_id: Option<String>,
}

fn spatial_column(column: &Value, rows: &[Value], index: usize) -> bool {
    matches!(
        column["native_type"].as_str(),
        Some("geometry" | "geography")
    ) || rows.iter().any(|row| {
        row.as_array()
            .and_then(|row| row.get(index))
            .is_some_and(|value| serde_json::from_value::<SpatialValue>(value.clone()).is_ok())
    })
}
fn value(raw: &Value, source: &LayerSource, column: &Value) -> ServiceResult<Option<SpatialValue>> {
    if raw.is_null() {
        return Ok(None);
    }
    let value: SpatialValue =
        serde_json::from_value(raw.clone()).map_err(|_| failure(ErrorCode::UnsupportedType))?;
    source
        .validate_values(&[Some(value.clone())])
        .map_err(paging::core_error)?;
    if value.srid.is_some_and(|srid| srid > 999_999) {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    if let Some(known) = column["srid"].as_u64().filter(|srid| *srid > 0) {
        if value
            .srid
            .is_some_and(|srid| srid > 0 && srid as u64 != known)
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
    }
    Ok(Some(value))
}
fn skippable(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::UnsupportedType | ErrorCode::InvalidGeometry | ErrorCode::FeatureTooLarge
    )
}
fn skipped(index: usize, code: ErrorCode) -> FeatureError {
    FeatureError {
        index: index as u64,
        code: match code {
            ErrorCode::UnsupportedType => "UNSUPPORTED_TYPE",
            ErrorCode::InvalidGeometry => "INVALID_GEOMETRY",
            _ => "FEATURE_TOO_LARGE",
        }
        .into(),
        message: "The spatial value cannot be displayed.".into(),
    }
}
fn metadata_errors(
    page: &mut tabularis_spatial_core::PageBuilder,
    errors: Vec<FeatureError>,
    warnings: Vec<String>,
) -> ServiceResult<()> {
    let mut metadata = page.page().clone();
    if !errors.is_empty() {
        metadata.limits.truncate("SKIPPED_FEATURES");
        metadata.feature_errors.extend(errors);
    }
    for warning in warnings {
        if !metadata.warnings.contains(&warning) {
            metadata.warnings.push(warning);
        }
    }
    paging::checked_metadata(&mut metadata)?;
    page.set_metadata(
        metadata.page,
        metadata.limits,
        metadata.warnings,
        metadata.feature_errors,
    )
    .map_err(paging::core_error)
}
fn properties(columns: &[Value], row: &[Value]) -> ServiceResult<Map<String, Value>> {
    let values = columns
        .iter()
        .zip(row)
        .enumerate()
        .map(|(index, (column, value))| {
            let name = column
                .as_str()
                .or_else(|| column["name"].as_str())
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
            Ok(json!({"name":name,"index":index,"value":value}))
        })
        .collect::<ServiceResult<Vec<_>>>()?;
    Ok([("columns".into(), Value::Array(values))]
        .into_iter()
        .collect::<Map<String, Value>>())
}

pub async fn build(
    domain: &SpatialDataDomain,
    execution: &PreparedExecution,
    source: &LayerSource,
    binding: &CursorBinding,
    size: usize,
    prior: Option<QueryCursor>,
) -> ServiceResult<BuiltPage> {
    let (id, set_index, column_index, source_srid, longitude_mode, skip_invalid) = match source {
        LayerSource::QueryResult {
            result_id,
            result_set_index,
            column_index,
            source_srid,
            longitude_mode,
            skip_invalid,
        } => (
            result_id,
            *result_set_index as usize,
            *column_index as usize,
            *source_srid,
            *longitude_mode,
            *skip_invalid,
        ),
        _ => return Err(failure(ErrorCode::InvalidArgument)),
    };
    let snapshot = execution.runtime.results.snapshot(id, binding)?;
    let set = snapshot
        .sets
        .get(set_index)
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    if set.kind != ResultKind::Tabular
        || column_index >= set.columns.len()
        || set.rows.iter().any(|row| {
            row.as_array()
                .map_or(true, |row| row.len() != set.columns.len())
        })
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    if !spatial_column(&set.columns[column_index], &set.rows, column_index) {
        return Err(failure(ErrorCode::UnsupportedType));
    }
    let known_srid = set.columns[column_index]["srid"]
        .as_u64()
        .filter(|srid| *srid > 0);
    if known_srid
        .is_some_and(|known| known > 999_999 || source_srid.is_some_and(|source| source != known))
    {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let mut cursor = prior.unwrap_or(QueryCursor {
        source: source.clone(),
        source_hash: paging::source_hash(source)?,
        page_size: size,
        next_row_offset: 0,
        budget: Budget::default(),
    });
    if cursor.source != *source
        || cursor.source_hash != paging::source_hash(source)?
        || cursor.page_size != size
        || cursor.next_row_offset > set.rows.len()
    {
        return Err(failure(ErrorCode::InvalidPageToken));
    }
    let end = cursor
        .next_row_offset
        .saturating_add(size)
        .min(set.rows.len());
    let mut page = paging::builder(
        size,
        cursor.budget,
        LimitStatus {
            truncated: snapshot.limits.truncated,
            reasons: snapshot.limits.reasons.clone(),
        },
        snapshot.warnings,
        vec![],
    )?;
    let mut raw_rows = Vec::new();
    let mut calls = 0usize;
    let mut stop = false;
    let mut layer_full = false;
    while cursor.next_row_offset < end && !stop {
        domain.guard(execution)?;
        let start = cursor.next_row_offset;
        let mut values = Vec::new();
        let mut indices = Vec::new();
        for ordinal in start..end.min(start + 128) {
            let raw = &set.rows[ordinal][column_index];
            let parsed = match value(raw, source, &set.columns[column_index]) {
                Ok(value) => value,
                Err(error) if skip_invalid && skippable(error.code) => {
                    if !values.is_empty() {
                        break;
                    }
                    metadata_errors(&mut page, vec![skipped(ordinal, error.code)], vec![])?;
                    cursor.next_row_offset = ordinal + 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let mut candidate = values.clone();
            candidate.push(parsed.clone());
            let bytes = serde_json::to_vec(&json!({"values":candidate,"source_srid":source_srid,"longitude_mode":longitude_mode,"skip_invalid":skip_invalid})).map_err(|_| failure(ErrorCode::ResourceLimit))?.len();
            if bytes > PUBLIC_PAGE_BYTES {
                if !values.is_empty() {
                    break;
                }
                if skip_invalid {
                    metadata_errors(
                        &mut page,
                        vec![skipped(ordinal, ErrorCode::FeatureTooLarge)],
                        vec![],
                    )?;
                    cursor.next_row_offset = ordinal + 1;
                    continue;
                }
                return Err(failure(ErrorCode::FeatureTooLarge));
            }
            values.push(parsed);
            indices.push(ordinal);
        }
        if values.is_empty() {
            continue;
        }
        let mut pending = VecDeque::from([(values, indices)]);
        while let Some((values, indices)) = pending.pop_front() {
            if stop {
                break;
            }
            calls += 1;
            if calls > MAX_TRANSFORM_CALLS {
                return Err(failure(ErrorCode::ResourceLimit));
            }
            let result = domain.rpc(execution, ServiceRpcMethod::SpatialTransformValues, json!({"values":values,"source_srid":source_srid,"longitude_mode":longitude_mode,"skip_invalid":skip_invalid})).await;
            let result = match result {
                Err(error) if error.code == ErrorCode::ResourceLimit && values.len() > 1 => {
                    let middle = values.len() / 2;
                    pending.push_front((values[middle..].to_vec(), indices[middle..].to_vec()));
                    pending.push_front((values[..middle].to_vec(), indices[..middle].to_vec()));
                    continue;
                }
                Err(error) if skip_invalid && values.len() == 1 && skippable(error.code) => {
                    metadata_errors(&mut page, vec![skipped(indices[0], error.code)], vec![])?;
                    cursor.next_row_offset = indices[0] + 1;
                    continue;
                }
                other => other?,
            };
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Transformed {
                geometries: Vec<Value>,
                feature_errors: Vec<FeatureError>,
                warnings: Vec<String>,
                limits: LimitStatus,
            }
            let transformed: Transformed = paging::decode(&result)?;
            if transformed.geometries.len() != values.len() {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            let mut seen = HashMap::new();
            for error in transformed.feature_errors {
                let index = usize::try_from(error.index)
                    .map_err(|_| failure(ErrorCode::InvalidArgument))?;
                if index >= values.len()
                    || !skip_invalid
                    || seen.insert(index, error.code).is_some()
                {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
            }
            let mut metadata = tabularis_spatial_core::FeaturePage {
                warnings: transformed.warnings,
                limits: transformed.limits,
                ..Default::default()
            };
            paging::checked_metadata(&mut metadata)?;
            metadata_errors(&mut page, vec![], metadata.warnings)?;
            for (index, geometry) in transformed.geometries.iter().enumerate() {
                let ordinal = indices[index];
                if let Some(code) = seen.get(&index) {
                    let code = match code.as_str() {
                        "UNSUPPORTED_TYPE" => ErrorCode::UnsupportedType,
                        "INVALID_GEOMETRY" => ErrorCode::InvalidGeometry,
                        "FEATURE_TOO_LARGE" => ErrorCode::FeatureTooLarge,
                        _ => return Err(failure(ErrorCode::InvalidArgument)),
                    };
                    metadata_errors(&mut page, vec![skipped(ordinal, code)], vec![])?;
                    cursor.next_row_offset = ordinal + 1;
                    continue;
                }
                let geometry = Geometry::from_json(geometry).map_err(paging::core_error)?;
                let fid = query_feature_id(id, set_index as u64, ordinal as u64)
                    .map_err(paging::core_error)?;
                let row = set.rows[ordinal]
                    .as_array()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
                let feature = Feature::new(&fid, geometry, properties(&set.columns, row)?);
                let reference = RowReference {
                    feature_id: fid,
                    identity: None,
                    snapshot_id: Some(id.clone()),
                    row_ordinal: ordinal as u64,
                };
                match page.try_push(feature, reference) {
                    Ok(PushResult::Added) => {
                        raw_rows.push(set.rows[ordinal].clone());
                        cursor.next_row_offset = ordinal + 1;
                    }
                    Ok(PushResult::PageFull) => {
                        stop = true;
                        break;
                    }
                    Ok(PushResult::LayerFull) => {
                        layer_full = true;
                        stop = true;
                        break;
                    }
                    Err(error)
                        if skip_invalid
                            && error.code == tabularis_spatial_core::ErrorCode::FeatureTooLarge =>
                    {
                        metadata_errors(
                            &mut page,
                            vec![skipped(ordinal, ErrorCode::FeatureTooLarge)],
                            vec![],
                        )?;
                        cursor.next_row_offset = ordinal + 1;
                    }
                    Err(error) => return Err(paging::core_error(error)),
                }
            }
        }
    }
    let more = cursor.next_row_offset < set.rows.len();
    let (page, budget) = paging::finish_builder(
        page,
        more,
        ResumeMode::Materialized,
        cursor.budget,
        layer_full,
    )?;
    cursor.budget = budget;
    raw_rows.truncate(page.features.len());
    let state = if page.page.has_more {
        Some(serde_json::to_value(cursor).map_err(|_| failure(ErrorCode::ResourceLimit))?)
    } else {
        None
    };
    Ok(BuiltPage {
        page,
        raw_rows,
        state,
        original_result_id: Some(id.clone()),
    })
}
