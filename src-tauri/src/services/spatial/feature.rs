use super::{
    data_domain::{self, SpatialDataDomain},
    paging,
};
use crate::{
    drivers::driver_trait::ServiceRpcMethod,
    services::{
        contracts::{ErrorCode, ServiceRequest, ServiceResponse},
        dispatch::CallerContext,
        error::{failure, ServiceResult},
        jobs::JobLease,
        results::ResultKind,
    },
};
use serde::Deserialize;
use serde_json::{json, Value};
use tabularis_spatial_core::{Feature, LayerSource, RowIdentity, RowReference};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    kind: String,
    source: LayerSource,
    source_hash: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedRow {
    feature: Feature,
    reference: RowReference,
    raw_row: Value,
}

pub async fn execute(
    domain: &SpatialDataDomain,
    request: &ServiceRequest,
    caller: &CallerContext,
    lease: &JobLease,
) -> ServiceResult<ServiceResponse> {
    if let Some(id) = request.input["result_id"].as_str() {
        let mut lookup = request.clone();
        lookup.map_id = None;
        lookup.expected_version = None;
        let execution = data_domain::prepare_feature(domain, &lookup, caller, lease, None).await?;
        let binding = execution.runtime.results.result_binding(id)?;
        data_domain::authorize_binding(&binding, &execution.connection, &execution.permission)?;
        let snapshot = execution.runtime.results.snapshot(id, &binding)?;
        let index = request.input["result_set_index"].as_u64().unwrap_or(0) as usize;
        let set = snapshot
            .sets
            .get(index)
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        if set.kind != ResultKind::JsonValues || set.columns.len() != 1 {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let marker: Marker = paging::decode(&set.columns[0])?;
        if marker.kind != "spatial_page_v1"
            || marker.source_hash != paging::source_hash(&marker.source)?
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let execution =
            data_domain::prepare_feature(domain, request, caller, lease, Some(&marker.source))
                .await?;
        if let LayerSource::QueryResult { result_id, .. } = &marker.source {
            let original = execution.runtime.results.result_binding(result_id)?;
            if original != binding {
                return Err(failure(ErrorCode::InvalidPageToken));
            }
            data_domain::authorize_binding(
                &original,
                &execution.connection,
                &execution.permission,
            )?;
            execution.runtime.results.snapshot(result_id, &original)?;
        }
        let fid = request.input["feature_id"]
            .as_str()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        let mut found = None;
        for row in &set.rows {
            let cached: CachedRow = paging::decode(row)?;
            if cached.feature.id == fid {
                if found.is_some() || cached.reference.feature_id != fid {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
                tabularis_spatial_core::FeaturePage {
                    features: vec![cached.feature.clone()],
                    row_references: vec![cached.reference.clone()],
                    ..Default::default()
                }
                .validate()
                .map_err(paging::core_error)?;
                found = Some(cached);
            }
        }
        let cached = found.ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        domain.revalidate(&execution).await?;
        if let LayerSource::QueryResult { result_id, .. } = &marker.source {
            let original = execution.runtime.results.result_binding(result_id)?;
            if original != binding {
                return Err(failure(ErrorCode::InvalidPageToken));
            }
            execution.runtime.results.snapshot(result_id, &original)?;
        }
        Ok(data_domain::success(
            request,
            json!({"feature":cached.feature,"row_reference":cached.reference,"row":cached.raw_row,"source":"snapshot"}),
        ))
    } else {
        let identity: RowIdentity = paging::decode(&request.input["identity"])?;
        identity.validate().map_err(paging::core_error)?;
        let execution = data_domain::prepare_feature(domain, request, caller, lease, None).await?;
        let data = domain
            .rpc(
                &execution,
                ServiceRpcMethod::SpatialReadFeature,
                json!({"table":request.input["table"],"identity":identity}),
            )
            .await?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct CurrentRow {
            row: Value,
            identity: RowIdentity,
            source: String,
        }
        let current: CurrentRow = paging::decode(&data)?;
        if (!current.row.is_null() && !current.row.is_object())
            || current.identity != identity
            || current.source != "current_row"
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        domain.revalidate(&execution).await?;
        Ok(data_domain::success(request, data))
    }
}
