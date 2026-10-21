use super::{
    data_domain::{self, PreparedExecution, SpatialDataDomain},
    export_sink::{ExportDestination, SpatialExportSinkFactory},
    export_stream::{ExportLimits, FeatureStream},
    paging::{self, Budget},
    query_result::{self, QueryCursor},
    table_query,
};
use crate::{
    drivers::driver_trait::ServiceRpcMethod,
    services::{
        contracts::{ErrorCode, Limits, Operation, ServiceRequest, ServiceResponse},
        dispatch::CallerContext,
        error::{failure, ServiceResult},
        extensions::OperationExtension,
        jobs::JobLease,
        results::CursorBinding,
        runtime::ServiceRuntime,
        security::digest,
    },
};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::HashSet,
    sync::{Arc, Weak},
};
use tabularis_spatial_core::{FeaturePage, LayerSource};

const OPERATIONS: &[Operation] = &[Operation::SpatialExport];
const MAX_EXPORT_PAGES: usize = 10_000;

pub struct SpatialExportDomain {
    runtime: Weak<ServiceRuntime>,
    data: Arc<SpatialDataDomain>,
    sink: Arc<dyn SpatialExportSinkFactory>,
}

impl SpatialExportDomain {
    pub fn attach(
        runtime: &Arc<ServiceRuntime>,
        data: Arc<SpatialDataDomain>,
        sink: Arc<dyn SpatialExportSinkFactory>,
    ) -> ServiceResult<Arc<Self>> {
        let domain = Arc::new(Self {
            runtime: Arc::downgrade(runtime),
            data,
            sink,
        });
        runtime.register_extension(domain.clone())?;
        Ok(domain)
    }

    fn source(request: &ServiceRequest) -> ServiceResult<(LayerSource, String, ExportLimits)> {
        if request.input["format"] != "geojson" {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let filename = request.input["filename"]
            .as_str()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
        if filename.is_empty()
            || filename.len() > 255
            || matches!(filename, "." | "..")
            || filename
                .chars()
                .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':'))
        {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let source: LayerSource = paging::decode(&request.input["source"])?;
        source.validate().map_err(paging::core_error)?;
        let srid = match &source {
            LayerSource::QueryResult { source_srid, .. }
            | LayerSource::Table { source_srid, .. } => source_srid,
        };
        if srid.is_some_and(|srid| srid > 999_999) {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        Ok((
            source,
            filename.into(),
            ExportLimits::from_input(&request.input)?,
        ))
    }

    fn snapshot(
        execution: &PreparedExecution,
        source: &LayerSource,
        binding: &CursorBinding,
    ) -> ServiceResult<()> {
        if let LayerSource::QueryResult { result_id, .. } = source {
            execution.runtime.results.snapshot(result_id, binding)?;
        }
        Ok(())
    }

    fn emit(
        &self,
        execution: &PreparedExecution,
        stream: &mut FeatureStream,
        page: &FeaturePage,
    ) -> ServiceResult<bool> {
        stream.merge_page(page)?;
        for feature in &page.features {
            self.data.guard(execution)?;
            if !stream.feature(feature)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn query(
        &self,
        execution: &PreparedExecution,
        source: &LayerSource,
        binding: &CursorBinding,
        limits: ExportLimits,
        stream: &mut FeatureStream,
    ) -> ServiceResult<()> {
        let mut prior: Option<QueryCursor> = None;
        let mut previous_offset = 0;
        for _ in 0..MAX_EXPORT_PAGES {
            self.data.revalidate(execution).await?;
            Self::snapshot(execution, source, binding)?;
            let built =
                query_result::build(&self.data, execution, source, binding, 1000, prior).await?;
            self.data.revalidate(execution).await?;
            Self::snapshot(execution, source, binding)?;
            if built.page.page.has_more != built.state.is_some() {
                return Err(failure(ErrorCode::InvalidArgument));
            }
            if !self.emit(execution, stream, &built.page)? || !built.page.page.has_more {
                return Ok(());
            }
            let mut cursor: QueryCursor = paging::decode(
                built
                    .state
                    .as_ref()
                    .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
            )?;
            if cursor.next_row_offset <= previous_offset {
                return Err(failure(ErrorCode::InvalidPageToken));
            }
            previous_offset = cursor.next_row_offset;
            if limits.explicit {
                cursor.budget = Budget::default();
            }
            prior = Some(cursor);
        }
        Err(failure(ErrorCode::ResourceLimit))
    }

    async fn table(
        &self,
        execution: &PreparedExecution,
        source: &LayerSource,
        limits: ExportLimits,
        stream: &mut FeatureStream,
    ) -> ServiceResult<()> {
        let mut next: Option<String> = None;
        let mut seen = HashSet::new();
        for _ in 0..MAX_EXPORT_PAGES {
            let mut input =
                serde_json::to_value(source).map_err(|_| failure(ErrorCode::InvalidArgument))?;
            input
                .as_object_mut()
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
                .remove("kind");
            input["page_size"] = json!(1000);
            input["next_token"] = json!(next);
            input["limits"] = json!({"max_features":limits.features,"max_coordinates":limits.coordinates,"max_bytes":limits.bytes});
            if limits.explicit {
                input["export_limit_bytes"] = json!(limits.bytes);
            }
            let raw = self
                .data
                .rpc(execution, ServiceRpcMethod::SpatialQueryTable, input)
                .await?;
            if serde_json::to_vec(&raw)
                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                .len()
                > 8 * 1024 * 1024
            {
                return Err(failure(ErrorCode::ResourceLimit));
            }
            let page =
                table_query::normalize(FeaturePage::from_json(&raw).map_err(paging::core_error)?)?;
            self.data.revalidate(execution).await?;
            if !self.emit(execution, stream, &page)? || !page.page.has_more {
                return Ok(());
            }
            let token = page
                .page
                .next_token
                .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
            if !seen.insert(digest(token.as_bytes())) {
                return Err(failure(ErrorCode::InvalidPageToken));
            }
            next = Some(token);
        }
        Err(failure(ErrorCode::ResourceLimit))
    }
}

#[async_trait]
impl OperationExtension for SpatialExportDomain {
    fn operations(&self) -> &[Operation] {
        OPERATIONS
    }

    async fn execute(
        &self,
        request: &ServiceRequest,
        caller: &CallerContext,
        lease: &JobLease,
    ) -> ServiceResult<ServiceResponse> {
        if request.operation != Operation::SpatialExport || self.runtime.upgrade().is_none() {
            return Err(failure(ErrorCode::UnsupportedOperation));
        }
        let (source, filename, limits) = Self::source(request)?;
        let execution =
            data_domain::prepare_feature(&self.data, request, caller, lease, Some(&source)).await?;
        let binding = match &source {
            LayerSource::QueryResult { result_id, .. } => {
                let binding = execution.runtime.results.result_binding(result_id)?;
                data_domain::authorize_binding(
                    &binding,
                    &execution.connection,
                    &execution.permission,
                )?;
                Self::snapshot(&execution, &source, &binding)?;
                binding
            }
            LayerSource::Table { .. } => CursorBinding {
                connection_id: execution.connection.id.clone(),
                permission_scope: execution.permission.identity.clone(),
                auth_revision: execution.connection.auth_revision.clone(),
                query_hash: paging::source_hash(&source)?,
                result_revision: uuid::Uuid::new_v4().to_string(),
                sdk_version: None,
                container_rid: None,
                endpoint_identity: None,
                partition_hash: None,
            },
        };
        self.data.guard(&execution)?;
        let sink = self.sink.reserve(ExportDestination {
            binding: binding.clone(),
            caller_id: caller.caller_id.clone(),
            driver_id: execution.connection.driver.clone(),
            filename: filename.clone(),
            max_bytes: limits.bytes,
            deadline_at_ms: lease.deadline_at_ms,
        })?;
        let mut stream = FeatureStream::new(sink, limits)?;
        match &source {
            LayerSource::QueryResult { .. } => {
                self.query(&execution, &source, &binding, limits, &mut stream)
                    .await?
            }
            LayerSource::Table { .. } => {
                self.table(&execution, &source, limits, &mut stream).await?
            }
        }
        self.data.guard(&execution)?;
        stream.seal()?;
        self.data.revalidate(&execution).await?;
        Self::snapshot(&execution, &source, &binding)?;
        self.data.guard(&execution)?;
        let (artifact, stats, warnings) = stream.finish()?;
        let mut response = data_domain::success(
            request,
            json!({"artifact_id":artifact.artifact_id,"filename":filename,"bytes":artifact.bytes,"feature_count":stats.feature_count,"coordinate_count":stats.coordinate_count,"checksum":artifact.checksum,"truncated":stats.truncated,"reasons":stats.reasons}),
        );
        response.limits = Limits {
            truncated: stats.truncated,
            reasons: stats.reasons,
        };
        response.warnings = warnings;
        Ok(response)
    }
}
