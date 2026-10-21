use super::super::{
    export_stream::{ExportLimits, FeatureStream, PREFIX, SUFFIX},
    ExportArtifact, ExportDestination, ExportStats, SpatialExportDomain, SpatialExportSink,
    SpatialExportSinkFactory,
};
use super::{spatial_support as spatial, support};
use crate::services::{
    contracts::{ErrorCode, Operation, Outcome, ServiceRequest, ServiceResponse},
    error::{failure, ServiceResult},
    extensions::OperationExtension,
    results::{ResultKind, ResultSet, ResultSnapshot},
    security::digest,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use tabularis_spatial_core::Feature;

#[derive(Default)]
pub(super) struct SinkState {
    pub destinations: Vec<ExportDestination>,
    pub published: Vec<Vec<u8>>,
    pub stats: Vec<ExportStats>,
    pub dropped: usize,
    pub append_calls: usize,
    pub finish_calls: usize,
}

pub(super) struct SinkFactory {
    pub state: Arc<Mutex<SinkState>>,
    pub fail_append_at: AtomicUsize,
    pub mode: AtomicUsize,
    pub reject_reservation: AtomicBool,
    pub hook_at: AtomicUsize,
    pub hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl SinkFactory {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Default::default(),
            fail_append_at: AtomicUsize::new(0),
            mode: AtomicUsize::new(0),
            reject_reservation: AtomicBool::new(false),
            hook_at: AtomicUsize::new(0),
            hook: Mutex::new(None),
        })
    }
}

struct Sink {
    factory: Arc<Mutex<SinkState>>,
    fail_append_at: usize,
    mode: usize,
    hook_at: usize,
    hook: Option<Box<dyn FnOnce() + Send>>,
    bytes: Vec<u8>,
    published: bool,
}
impl SpatialExportSinkFactory for SinkFactory {
    fn reserve(&self, destination: ExportDestination) -> ServiceResult<Box<dyn SpatialExportSink>> {
        if self.reject_reservation.load(Ordering::SeqCst) {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        self.state.lock().unwrap().destinations.push(destination);
        Ok(Box::new(Sink {
            factory: self.state.clone(),
            fail_append_at: self.fail_append_at.load(Ordering::SeqCst),
            mode: self.mode.load(Ordering::SeqCst),
            hook_at: self.hook_at.load(Ordering::SeqCst),
            hook: self.hook.lock().unwrap().take(),
            bytes: vec![],
            published: false,
        }))
    }
}
impl SpatialExportSink for Sink {
    fn append(&mut self, bytes: &[u8]) -> ServiceResult<()> {
        let count = {
            let mut state = self.factory.lock().unwrap();
            state.append_calls += 1;
            state.append_calls
        };
        if count == self.fail_append_at {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        self.bytes.extend_from_slice(bytes);
        if count == self.hook_at {
            if let Some(hook) = self.hook.take() {
                hook();
            }
        }
        Ok(())
    }
    fn finish(mut self: Box<Self>, stats: ExportStats) -> ServiceResult<ExportArtifact> {
        self.factory.lock().unwrap().finish_calls += 1;
        if self.mode == 4 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let bytes = self.bytes.len() as u64;
        let checksum = digest(&self.bytes);
        {
            let mut state = self.factory.lock().unwrap();
            state.published.push(std::mem::take(&mut self.bytes));
            state.stats.push(stats);
        }
        self.published = true;
        Ok(ExportArtifact {
            artifact_id: if self.mode == 2 {
                "artifact-invalid".into()
            } else {
                format!("artifact-{}", uuid::Uuid::new_v4())
            },
            bytes: bytes + u64::from(self.mode == 3),
            checksum: if self.mode == 1 {
                "0".repeat(64)
            } else {
                checksum
            },
        })
    }
}
impl Drop for Sink {
    fn drop(&mut self) {
        if !self.published {
            self.factory.lock().unwrap().dropped += 1;
        }
    }
}

struct Harness {
    spatial: spatial::Harness,
    factory: Arc<SinkFactory>,
    domain: Arc<SpatialExportDomain>,
}
impl Harness {
    fn new() -> Self {
        let spatial = spatial::Harness::new();
        let factory = SinkFactory::new();
        let domain = SpatialExportDomain::attach(
            &spatial.base.runtime,
            spatial.domain.clone(),
            factory.clone(),
        )
        .unwrap();
        Self {
            spatial,
            factory,
            domain,
        }
    }
    async fn call(&self, input: &ServiceRequest) -> ServiceResponse {
        self.spatial.call(input).await
    }
}

fn export(source: Value, limit: Option<u64>) -> ServiceRequest {
    let mut input = json!({"source":source,"format":"geojson","filename":"fixture.geojson"});
    if let Some(limit) = limit {
        input["export_limit_bytes"] = json!(limit);
    }
    let mut request = spatial::request(Operation::SpatialExport, input);
    request.deadline_ms = 120_000;
    request
}
fn point() -> Feature {
    Feature::from_json(&json!({"type":"Feature","id":"fixture","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}})).unwrap()
}
fn published(factory: &SinkFactory) -> Value {
    serde_json::from_slice(&factory.state.lock().unwrap().published[0]).unwrap()
}
fn snapshot(
    h: &Harness,
    rows: Vec<Value>,
    limits: crate::services::contracts::Limits,
    warnings: Vec<String>,
) -> String {
    let binding = support::binding();
    h.spatial
        .base
        .runtime
        .results
        .insert(
            binding.clone(),
            ResultSnapshot {
                sets: vec![ResultSet {
                    kind: ResultKind::Tabular,
                    columns: spatial::columns(),
                    rows,
                }],
                revision: binding.result_revision,
                limits,
                allow_truncation: false,
                warnings,
            },
        )
        .unwrap()
}

#[tokio::test]
async fn export만_추가로_등록하고_예약을_연결_주체와_파일에_묶는다() {
    // given
    let h = Harness::new();
    let id = snapshot(
        &h,
        vec![json!([spatial::value(), "old"])],
        Default::default(),
        vec![],
    );
    let input = export(support::query_source(&id), None);
    // when
    let actual = h.call(&input).await;
    // then
    assert!(actual.error.is_none());
    assert_eq!(
        OperationExtension::operations(h.domain.as_ref()),
        &[Operation::SpatialExport]
    );
    assert_eq!(actual.data["feature_count"], 1);
    assert_eq!(actual.data["coordinate_count"], 1);
    assert_eq!(actual.data["filename"], "fixture.geojson");
    assert_eq!(
        h.factory.state.lock().unwrap().destinations[0].binding,
        support::binding()
    );
    assert_eq!(
        h.factory.state.lock().unwrap().destinations[0].caller_id,
        support::caller().caller_id
    );
    assert_eq!(
        h.factory.state.lock().unwrap().destinations[0].driver_id,
        "postgresql"
    );
    assert_eq!(
        actual.data["bytes"],
        h.factory.state.lock().unwrap().published[0].len()
    );
    assert_eq!(
        actual.data["checksum"],
        digest(&h.factory.state.lock().unwrap().published[0])
    );
    assert_eq!(h.spatial.base.driver.0.load(Ordering::SeqCst), 0);
    assert_eq!(spatial::cache_count(&h.spatial.base.runtime, "s-"), 0);
}

struct SnapshotEvidence {
    response: ServiceResponse,
    before: Value,
    after: Value,
}
async fn export_snapshot(h: &Harness, input: &ServiceRequest, id: &str) -> SnapshotEvidence {
    let before = serde_json::to_value(
        h.spatial
            .base
            .runtime
            .results
            .snapshot(id, &support::binding())
            .unwrap(),
    )
    .unwrap();
    let response = h.call(input).await;
    let after = serde_json::to_value(
        h.spatial
            .base
            .runtime
            .results
            .snapshot(id, &support::binding())
            .unwrap(),
    )
    .unwrap();
    SnapshotEvidence {
        response,
        before,
        after,
    }
}
#[tokio::test]
async fn 중복_열과_다중_결과와_null_empty를_원본_sql없이_그대로_내보낸다() {
    // given
    let h = Harness::new();
    let mut empty = spatial::value();
    empty["is_empty"] = json!(true);
    let binding = support::binding();
    let id = h
        .spatial
        .base
        .runtime
        .results
        .insert(
            binding.clone(),
            ResultSnapshot {
                sets: vec![
                    ResultSet {
                        kind: ResultKind::Tabular,
                        columns: vec![json!({"name":"unused"})],
                        rows: vec![],
                    },
                    ResultSet {
                        kind: ResultKind::Tabular,
                        columns: spatial::columns(),
                        rows: vec![
                            json!([spatial::value(), "old"]),
                            json!([null, "null"]),
                            json!([empty, "empty"]),
                        ],
                    },
                ],
                revision: binding.result_revision,
                limits: Default::default(),
                allow_truncation: false,
                warnings: vec![],
            },
        )
        .unwrap();
    let mut source = support::query_source(&id);
    source["result_set_index"] = json!(1);
    let input = export(source, None);
    // when
    let actual = export_snapshot(&h, &input, &id).await;
    // then
    assert!(actual.response.error.is_none());
    assert_eq!(actual.before, actual.after);
    assert_eq!(
        published(&h.factory)["features"].as_array().unwrap().len(),
        3
    );
    assert_eq!(
        published(&h.factory)["features"][1]["geometry"],
        Value::Null
    );
    assert_eq!(
        published(&h.factory)["features"][2]["geometry"],
        Value::Null
    );
    assert_eq!(
        published(&h.factory)["features"][0]["properties"]["columns"][1],
        json!({"name":"duplicate","index":1,"value":"old"})
    );
    assert_eq!(
        published(&h.factory)["features"][0]["id"],
        format!("{id}:1:0")
    );
    assert_eq!(h.spatial.base.driver.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 알려지지_않은_srid는_skip으로_숨기지_않고_partial을_정리한다() {
    // given
    let h = Harness::new();
    let mut value = spatial::value();
    value["srid"] = Value::Null;
    let id = snapshot(&h, vec![json!([value, "old"])], Default::default(), vec![]);
    let mut source = support::query_source(&id);
    source["source_srid"] = Value::Null;
    source["skip_invalid"] = json!(true);
    let input = export(source, None);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::SridRequired);
    assert!(h.factory.state.lock().unwrap().published.is_empty());
    assert_eq!(h.factory.state.lock().unwrap().dropped, 1);
    assert!(h.spatial.spatial.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn 원본_잘림과_무효_생략과_경고를_동기화하며_중복을_제거한다() {
    // given
    let h = Harness::new();
    let id = snapshot(
        &h,
        vec![
            json!(["POINT(1 2)", "invalid"]),
            json!([spatial::value(), "valid"]),
        ],
        crate::services::contracts::Limits {
            truncated: true,
            reasons: vec!["SOURCE_LIMIT".into()],
        },
        vec!["SOURCE_WARNING".into(), "SOURCE_WARNING".into()],
    );
    let mut source = support::query_source(&id);
    source["skip_invalid"] = json!(true);
    let input = export(source, None);
    // when
    let actual = h.call(&input).await;
    // then
    assert!(actual.error.is_none());
    assert_eq!(actual.data["truncated"], true);
    assert!(actual.limits.truncated);
    assert!(actual.limits.reasons.contains(&"SOURCE_LIMIT".into()));
    assert!(actual.limits.reasons.contains(&"SKIPPED_FEATURES".into()));
    assert_eq!(actual.data["reasons"], json!(actual.limits.reasons));
    assert_eq!(actual.warnings, vec!["SOURCE_WARNING"]);
    assert_eq!(
        published(&h.factory)["features"][0]["id"],
        format!("{id}:0:1")
    );
}

async fn stream_features(
    factory: &Arc<SinkFactory>,
    limits: ExportLimits,
    features: Vec<Feature>,
) -> ServiceResult<(ExportArtifact, ExportStats)> {
    let sink = factory.reserve(ExportDestination {
        binding: support::binding(),
        caller_id: "fixture".into(),
        driver_id: "postgresql".into(),
        filename: "fixture.geojson".into(),
        max_bytes: limits.bytes,
        deadline_at_ms: u64::MAX,
    })?;
    let mut stream = FeatureStream::new(sink, limits)?;
    for feature in &features {
        if !stream.feature(feature)? {
            break;
        }
    }
    stream.seal()?;
    let (artifact, stats, _) = stream.finish()?;
    Ok((artifact, stats))
}
#[tokio::test]
async fn 바이트_경계에서_완전한_feature와_닫는_접미사까지_보존한다() {
    // given
    let factory = SinkFactory::new();
    let feature = point();
    let bytes = (PREFIX.len() + serde_json::to_vec(&feature).unwrap().len() + SUFFIX.len()) as u64;
    let limits = ExportLimits {
        bytes,
        features: 100,
        coordinates: 100,
        explicit: true,
    };
    // when
    let actual = stream_features(&factory, limits, vec![feature.clone(), feature])
        .await
        .unwrap();
    // then
    assert_eq!(actual.0.bytes, bytes);
    assert_eq!(actual.1.feature_count, 1);
    assert_eq!(actual.1.reasons, vec!["EXPORT_BYTE_LIMIT"]);
    assert_eq!(published(&factory)["type"], "FeatureCollection");
    assert_eq!(published(&factory)["features"].as_array().unwrap().len(), 1);
    assert_eq!(
        factory.state.lock().unwrap().published[0].last(),
        Some(&b'}')
    );
}
#[tokio::test]
async fn 빈_collection조차_들어가지_않으면_예약과_db를_호출하지_않는다() {
    // given
    let h = Harness::new();
    let input = export(
        support::table(),
        Some((PREFIX.len() + SUFFIX.len() - 1) as u64),
    );
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ResourceLimit);
    assert!(h.factory.state.lock().unwrap().destinations.is_empty());
    assert!(h.spatial.spatial.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn 좌표_한도를_넘는_feature는_중간에_자르지_않고_빈_geojson으로_닫는다() {
    // given
    let factory = SinkFactory::new();
    let feature=Feature::from_json(&json!({"type":"Feature","id":"line","geometry":{"type":"LineString","coordinates":[[0,0],[1,1]]},"properties":{}})).unwrap();
    let limits = ExportLimits {
        bytes: 4096,
        features: 100,
        coordinates: 1,
        explicit: true,
    };
    // when
    let actual = stream_features(&factory, limits, vec![feature])
        .await
        .unwrap();
    // then
    assert_eq!(actual.1.feature_count, 0);
    assert_eq!(actual.1.coordinate_count, 0);
    assert_eq!(actual.1.reasons, vec!["EXPORT_COORDINATE_LIMIT"]);
    assert!(published(&factory)["features"]
        .as_array()
        .unwrap()
        .is_empty());
}
#[tokio::test]
async fn 기본_feature_한도는_layer_limit도_함께_보고한다() {
    // given
    let factory = SinkFactory::new();
    let limits = ExportLimits {
        bytes: 4096,
        features: 1,
        coordinates: 100,
        explicit: false,
    };
    // when
    let actual = stream_features(&factory, limits, vec![point(), point()])
        .await
        .unwrap();
    // then
    assert_eq!(actual.1.feature_count, 1);
    assert_eq!(
        actual.1.reasons,
        vec!["EXPORT_FEATURE_LIMIT", "LAYER_LIMIT"]
    );
    assert_eq!(published(&factory)["features"].as_array().unwrap().len(), 1);
}

async fn table_pages(h: &Harness) -> ServiceResponse {
    h.call(&export(support::table(), Some(64 * 1024 * 1024)))
        .await
}
#[tokio::test]
async fn 네이티브_opaque커서와_명시_export상한만_전달하고_저장설정을_바꾸지_않는다() {
    // given
    let h = Harness::new();
    let first = spatial::feature_page(1, 0, Some("private-upstream-canary"));
    let mut last = spatial::feature_page(1, 0, None);
    last["features"][0]["id"] = json!("fixture-snapshot:1");
    last["row_references"][0]["feature_id"] = json!("fixture-snapshot:1");
    last["row_references"][0]["row_ordinal"] = json!(1);
    h.spatial
        .spatial
        .responses
        .lock()
        .unwrap()
        .extend([Ok(first), Ok(last)]);
    // when
    let actual = table_pages(&h).await;
    // then
    assert!(actual.error.is_none());
    assert_eq!(actual.data["feature_count"], 2);
    assert_eq!(
        h.spatial.spatial.calls.lock().unwrap()[1].2["next_token"],
        "private-upstream-canary"
    );
    assert_eq!(
        h.spatial.spatial.calls.lock().unwrap()[0].2["limits"],
        json!({"max_features":1000000,"max_coordinates":10000000,"max_bytes":67108864})
    );
    assert_eq!(
        h.spatial.spatial.calls.lock().unwrap()[0].2["export_limit_bytes"],
        67108864
    );
    assert_eq!(
        h.spatial.spatial.calls.lock().unwrap()[0].2["viewport"],
        json!({"west":-10.0,"east":10.0,"south":-10.0,"north":10.0,"world":false})
    );
    assert!(!serde_json::to_string(&actual)
        .unwrap()
        .contains("private-upstream-canary"));
    assert_eq!(
        h.spatial.base.connections.0.lock().unwrap()["conn-a"].settings,
        json!({})
    );
    assert_eq!(spatial::cache_count(&h.spatial.base.runtime, "s-"), 0);
}
#[tokio::test]
async fn 기본_export는_기존_네이티브_상한과_잘림을_정직하게_유지한다() {
    // given
    let h = Harness::new();
    let mut page = spatial::feature_page(1, 0, None);
    page["limits"] = json!({"truncated":true,"reasons":["LAYER_LIMIT"]});
    h.spatial
        .spatial
        .responses
        .lock()
        .unwrap()
        .push_back(Ok(page));
    let input = export(support::table(), None);
    // when
    let actual = h.call(&input).await;
    // then
    assert!(actual.error.is_none());
    assert!(actual.limits.truncated);
    assert_eq!(actual.limits.reasons, vec!["LAYER_LIMIT"]);
    assert_eq!(
        h.spatial.spatial.calls.lock().unwrap()[0].2["limits"],
        json!({"max_features":10000,"max_coordinates":250000,"max_bytes":33554432})
    );
    assert!(h.spatial.spatial.calls.lock().unwrap()[0]
        .2
        .get("export_limit_bytes")
        .is_none());
}
#[tokio::test]
async fn 반복된_네이티브_커서는_세번째_db호출없이_partial을_폐기한다() {
    // given
    let h = Harness::new();
    h.spatial.spatial.responses.lock().unwrap().extend([
        Ok(spatial::feature_page(1, 0, Some("same"))),
        Ok(spatial::feature_page(1, 0, Some("same"))),
    ]);
    let input = export(support::table(), Some(64 * 1024 * 1024));
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::InvalidPageToken);
    assert_eq!(h.spatial.spatial.calls.lock().unwrap().len(), 2);
    assert!(h.factory.state.lock().unwrap().published.is_empty());
    assert_eq!(h.factory.state.lock().unwrap().dropped, 1);
}
#[tokio::test]
async fn 예약_거부는_첫_db호출보다_먼저_실패한다() {
    // given
    let h = Harness::new();
    h.factory.reject_reservation.store(true, Ordering::SeqCst);
    let input = export(support::table(), None);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ResourceLimit);
    assert!(h.spatial.spatial.calls.lock().unwrap().is_empty());
    assert!(h.factory.state.lock().unwrap().published.is_empty());
}
#[tokio::test]
async fn append실패는_완료된_artifact없이_partial을_정리한다() {
    // given
    let h = Harness::new();
    h.factory.fail_append_at.store(2, Ordering::SeqCst);
    let id = snapshot(
        &h,
        vec![json!([spatial::value(), "old"])],
        Default::default(),
        vec![],
    );
    let input = export(support::query_source(&id), None);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ResourceLimit);
    assert_eq!(h.factory.state.lock().unwrap().finish_calls, 0);
    assert_eq!(h.factory.state.lock().unwrap().dropped, 1);
    assert!(h.factory.state.lock().unwrap().published.is_empty());
}
#[tokio::test]
async fn finish이후_잘못된_checksum은_게시된_outcome을_applied로_유지한다() {
    // given
    let h = Harness::new();
    h.factory.mode.store(1, Ordering::SeqCst);
    let id = snapshot(
        &h,
        vec![json!([spatial::value(), "old"])],
        Default::default(),
        vec![],
    );
    let input = export(support::query_source(&id), None);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(
        actual.error.as_ref().unwrap().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(actual.error.as_ref().unwrap().outcome, Outcome::Applied);
    assert_eq!(h.factory.state.lock().unwrap().published.len(), 1);
    assert!(actual.data.get("artifact_id").is_none());
}

async fn cancel_export(h: &Harness, input: ServiceRequest, near_finish: bool) -> ServiceResponse {
    let gate = Arc::new(tokio::sync::Barrier::new(2));
    *h.spatial.spatial.gate.lock().unwrap() = Some(gate.clone());
    let job_slot = Arc::new(Mutex::new(None::<String>));
    if near_finish {
        let slot = job_slot.clone();
        let runtime = h.spatial.base.runtime.clone();
        h.factory.hook_at.store(2, Ordering::SeqCst);
        *h.factory.hook.lock().unwrap() = Some(Box::new(move || {
            runtime
                .jobs
                .cancel(
                    slot.lock().unwrap().as_deref().unwrap(),
                    "map-owner",
                    &support::caller(),
                )
                .unwrap();
        }));
    }
    let queued = h
        .spatial
        .base
        .runtime
        .submit(input, support::caller())
        .await;
    let job = queued.job_id.unwrap();
    *job_slot.lock().unwrap() = Some(job.clone());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while h.spatial.spatial.entered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    if near_finish {
        gate.wait().await;
    } else {
        h.spatial
            .base
            .runtime
            .jobs
            .cancel(&job, "map-owner", &support::caller())
            .unwrap();
    }
    spatial::terminal(&h.spatial.base.runtime, &job).await
}
#[tokio::test]
async fn 변환_중_취소는_sink를_완료하지_않고_partial을_폐기한다() {
    // given
    let h = Harness::new();
    let id = snapshot(
        &h,
        vec![json!([spatial::value(), "old"])],
        Default::default(),
        vec![],
    );
    let input = export(support::query_source(&id), None);
    // when
    let actual = cancel_export(&h, input, false).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::Cancelled);
    assert_eq!(h.factory.state.lock().unwrap().finish_calls, 0);
    assert_eq!(h.factory.state.lock().unwrap().dropped, 1);
    assert!(h.factory.state.lock().unwrap().published.is_empty());
}
#[tokio::test]
async fn 마지막_feature뒤_취소도_finish전에_차단한다() {
    // given
    let h = Harness::new();
    let id = snapshot(
        &h,
        vec![json!([spatial::value(), "old"])],
        Default::default(),
        vec![],
    );
    let input = export(support::query_source(&id), None);
    // when
    let actual = cancel_export(&h, input, true).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::Cancelled);
    assert_eq!(h.factory.state.lock().unwrap().finish_calls, 0);
    assert!(h.factory.state.lock().unwrap().published.is_empty());
    assert_eq!(h.factory.state.lock().unwrap().dropped, 1);
}
#[tokio::test]
async fn finish직전_authrevision변경은_이전_주체_artifact를_게시하지_않는다() {
    // given
    let h = Harness::new();
    let id = snapshot(
        &h,
        vec![json!([spatial::value(), "old"])],
        Default::default(),
        vec![],
    );
    let connections = h.spatial.base.connections.clone();
    h.factory.hook_at.store(3, Ordering::SeqCst);
    *h.factory.hook.lock().unwrap() = Some(Box::new(move || {
        connections
            .0
            .lock()
            .unwrap()
            .get_mut("conn-a")
            .unwrap()
            .auth_revision = "changed".into();
    }));
    let input = export(support::query_source(&id), None);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::AuthRequired);
    assert_eq!(h.factory.state.lock().unwrap().finish_calls, 0);
    assert!(h.factory.state.lock().unwrap().published.is_empty());
}
#[tokio::test]
async fn finish직전_원본_ttl만료는_새로운_export수명으로_덮지_않는다() {
    // given
    let h = Harness::new();
    let id = snapshot(
        &h,
        vec![json!([spatial::value(), "old"])],
        Default::default(),
        vec![],
    );
    let runtime = h.spatial.base.runtime.clone();
    let original = id.clone();
    h.factory.hook_at.store(3, Ordering::SeqCst);
    *h.factory.hook.lock().unwrap() = Some(Box::new(move || spatial::expire(&runtime, &original)));
    let input = export(support::query_source(&id), None);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::ResultExpired);
    assert!(h.factory.state.lock().unwrap().published.is_empty());
    assert_eq!(h.factory.state.lock().unwrap().finish_calls, 0);
}
#[tokio::test]
async fn finish직전_지도세대변경은_오래된_레이어_export를_거부한다() {
    // given
    let h = Harness::new();
    let id = snapshot(
        &h,
        vec![json!([spatial::value(), "old"])],
        Default::default(),
        vec![],
    );
    let map = h.spatial.base.create().await;
    let added = h
        .spatial
        .base
        .call(&support::layer(
            &map,
            0,
            "conn-a",
            support::query_source(&id),
        ))
        .await;
    assert!(added.error.is_none());
    let store = h.spatial.base.domain.store().clone();
    let observed = store.get(&map, &support::caller()).unwrap();
    let change = support::request(
        Operation::MapUpdate,
        Some(&map),
        Some(1),
        json!({"name":"changed"}),
    );
    let lease = h
        .spatial
        .base
        .runtime
        .jobs
        .begin(&change, &support::caller(), true, false)
        .await
        .unwrap();
    h.factory.hook_at.store(3, Ordering::SeqCst);
    *h.factory.hook.lock().unwrap() = Some(Box::new(move || {
        store
            .mutate(
                &change,
                &support::caller(),
                &observed,
                super::super::MapChange::Update {
                    name: Some("changed".into()),
                    basemap: tabularis_spatial_core::BasemapChange::Keep,
                },
                &lease,
            )
            .unwrap();
    }));
    let mut input = export(support::query_source(&id), None);
    input.map_id = Some(map);
    input.expected_version = Some(1);
    // when
    let actual = h.call(&input).await;
    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::VersionConflict);
    assert!(h.factory.state.lock().unwrap().published.is_empty());
    assert_eq!(h.factory.state.lock().unwrap().finish_calls, 0);
}
#[tokio::test]
async fn 명시_export는_원본32mib이하_snapshot을_전체32mib이상_geojson으로_스트리밍한다() {
    // given
    let h = Harness::new();
    let rows = (0..7800)
        .map(|_| json!([spatial::value(), "x".repeat(4100)]))
        .collect();
    let id = snapshot(&h, rows, Default::default(), vec![]);
    let input = export(support::query_source(&id), Some(64 * 1024 * 1024));
    // when
    let actual = h.call(&input).await;
    // then
    assert!(actual.error.is_none());
    assert_eq!(actual.data["feature_count"], 7800);
    assert!(actual.data["bytes"].as_u64().unwrap() > 32 * 1024 * 1024);
    assert_eq!(actual.data["truncated"], false);
    assert_eq!(
        published(&h.factory)["features"].as_array().unwrap().len(),
        7800
    );
    assert_eq!(h.spatial.base.driver.0.load(Ordering::SeqCst), 0);
    assert_eq!(spatial::cache_count(&h.spatial.base.runtime, "s-"), 0);
}

#[tokio::test]
async fn 파일명과_상한이_유효하지_않으면_db없이_실패한다() {
    // given
    let h = Harness::new();
    let mut inputs = vec![];
    for filename in ["../escape", "a\\b", "a:b", ".", "..", "\u{7f}"] {
        let mut input = export(support::table(), None);
        input.input["filename"] = json!(filename);
        inputs.push(input);
    }
    let mut oversized = export(support::table(), None);
    oversized.input["filename"] = json!("a".repeat(256));
    inputs.push(oversized);
    // when
    let actual = invalid_exports(&h, inputs).await;
    // then
    assert!(actual
        .iter()
        .all(|response| response.error.as_ref().unwrap().code == ErrorCode::InvalidArgument));
    assert!(h.spatial.spatial.calls.lock().unwrap().is_empty());
    assert!(h.factory.state.lock().unwrap().destinations.is_empty());
}
async fn invalid_exports(h: &Harness, inputs: Vec<ServiceRequest>) -> Vec<ServiceResponse> {
    let mut responses = vec![];
    for input in inputs {
        responses.push(h.call(&input).await);
    }
    responses
}
