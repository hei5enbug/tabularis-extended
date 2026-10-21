use super::{
    data_domain::{PreparedExecution, SpatialDataDomain},
    paging::{self, Budget},
    query_result::BuiltPage,
};
use crate::{
    drivers::driver_trait::ServiceRpcMethod,
    services::{
        contracts::ErrorCode,
        error::{failure, ServiceResult},
        security::digest,
    },
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tabularis_spatial_core::{FeaturePage, LayerSource, PushResult, ResumeMode, RowIdentity};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableCursor {
    pub source: LayerSource,
    pub source_hash: String,
    pub page_size: usize,
    pub budget: Budget,
    pub tail: FeaturePage,
    pub upstream_token: Option<String>,
    pub upstream_mode: ResumeMode,
}

pub fn normalize(mut page: FeaturePage) -> ServiceResult<FeaturePage> {
    paging::checked_metadata(&mut page)?;
    for (feature, reference) in page.features.iter_mut().zip(&mut page.row_references) {
        if let Some(encoded) = feature.id.strip_prefix("pk:") {
            let bytes = URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| failure(ErrorCode::InvalidArgument))?;
            if URL_SAFE_NO_PAD.encode(&bytes) != encoded {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            let identity: RowIdentity =
                serde_json::from_slice(&bytes).map_err(|_| failure(ErrorCode::InvalidArgument))?;
            identity.validate().map_err(paging::core_error)?;
            if reference.identity.as_ref() != Some(&identity) || reference.feature_id != feature.id
            {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            let mut value =
                serde_json::to_value(identity).map_err(|_| failure(ErrorCode::InvalidArgument))?;
            value.sort_all_objects();
            let fid = format!(
                "pk:{}",
                digest(
                    &serde_json::to_vec(&value).map_err(|_| failure(ErrorCode::InvalidArgument))?
                )
            );
            feature.id = fid.clone();
            reference.feature_id = fid;
        } else if feature.id.len() > 128
            || feature.id.is_empty()
            || feature.id.chars().any(char::is_control)
            || reference.identity.is_some()
            || reference.snapshot_id.is_none()
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
    }
    page.validate().map_err(paging::core_error)?;
    Ok(page)
}
pub async fn build(
    domain: &SpatialDataDomain,
    execution: &PreparedExecution,
    source: &LayerSource,
    size: usize,
    prior: Option<TableCursor>,
) -> ServiceResult<BuiltPage> {
    let mut cursor = prior.unwrap_or(TableCursor {
        source: source.clone(),
        source_hash: paging::source_hash(source)?,
        page_size: size,
        budget: Budget::default(),
        tail: FeaturePage::default(),
        upstream_token: None,
        upstream_mode: ResumeMode::Native,
    });
    if cursor.source != *source
        || cursor.source_hash != paging::source_hash(source)?
        || cursor.page_size != size
    {
        return Err(failure(ErrorCode::InvalidPageToken));
    }
    if cursor.tail.features.is_empty() {
        let mut input =
            serde_json::to_value(source).map_err(|_| failure(ErrorCode::InvalidArgument))?;
        input
            .as_object_mut()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
            .remove("kind");
        input["page_size"] = json!(size);
        input["next_token"] = json!(cursor.upstream_token);
        input["limits"] =
            json!({"max_features":10000,"max_coordinates":250000,"max_bytes":33554432});
        let raw = domain
            .rpc(execution, ServiceRpcMethod::SpatialQueryTable, input)
            .await?;
        if serde_json::to_vec(&raw)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len()
            > 8 * 1024 * 1024
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let page = FeaturePage::from_json(&raw).map_err(paging::core_error)?;
        cursor.upstream_token = page.page.next_token.clone();
        cursor.upstream_mode = page.page.resume_mode;
        cursor.tail = normalize(page)?;
        cursor.tail.page = Default::default();
    }
    let mut page = paging::builder(
        size,
        cursor.budget,
        cursor.tail.limits.clone(),
        cursor.tail.warnings.clone(),
        cursor.tail.feature_errors.clone(),
    )?;
    let mut consumed = 0usize;
    let mut raw_rows = Vec::new();
    let mut layer_full = false;
    let skip_invalid = matches!(
        source,
        LayerSource::Table {
            skip_invalid: true,
            ..
        }
    );
    for (feature, reference) in cursor.tail.features.iter().zip(&cursor.tail.row_references) {
        domain.guard(execution)?;
        match page.try_push(feature.clone(), reference.clone()) {
            Ok(PushResult::Added) => {
                consumed += 1;
                raw_rows.push(Value::Object(feature.properties.clone()));
            }
            Ok(PushResult::PageFull) => break,
            Ok(PushResult::LayerFull) => {
                layer_full = true;
                break;
            }
            Err(error)
                if skip_invalid
                    && error.code == tabularis_spatial_core::ErrorCode::FeatureTooLarge =>
            {
                let mut metadata = page.page().clone();
                metadata.limits.truncate("SKIPPED_FEATURES");
                metadata
                    .feature_errors
                    .push(tabularis_spatial_core::FeatureError {
                        index: reference.row_ordinal,
                        code: "FEATURE_TOO_LARGE".into(),
                        message: "The spatial value cannot be displayed.".into(),
                    });
                paging::checked_metadata(&mut metadata)?;
                page.set_metadata(
                    metadata.page,
                    metadata.limits,
                    metadata.warnings,
                    metadata.feature_errors,
                )
                .map_err(paging::core_error)?;
                consumed += 1;
            }
            Err(error) => return Err(paging::core_error(error)),
        }
    }
    cursor.tail.features.drain(..consumed);
    cursor.tail.row_references.drain(..consumed);
    let more = !cursor.tail.features.is_empty() || cursor.upstream_token.is_some();
    let mode = if !cursor.tail.features.is_empty() {
        ResumeMode::Materialized
    } else {
        cursor.upstream_mode
    };
    let (page, budget) = paging::finish_builder(page, more, mode, cursor.budget, layer_full)?;
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
        original_result_id: None,
    })
}
