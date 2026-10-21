export const PERFORMANCE_FEATURES = 10_000;
export const PERFORMANCE_COORDINATES = 250_000;
export const PERFORMANCE_PAGE_SIZE = 1000;
export const PERFORMANCE_PAGE_BYTES = 8 * 1024 * 1024 - 64 * 1024;
export const PERFORMANCE_LAYER_BYTES = 32 * 1024 * 1024;
export const PERFORMANCE_PHASES = ['open', 'page', 'render', 'viewport', 'close'];

export function coordinateCount(geometry) {
  if (!geometry) return 0;
  if (geometry.type === 'GeometryCollection') return geometry.geometries.reduce((sum, child) => sum + coordinateCount(child), 0);
  const count = coordinates => typeof coordinates[0] === 'number' ? 1 : coordinates.reduce((sum, child) => sum + count(child), 0);
  return count(geometry.coordinates);
}

export function performanceEnvelope(request, page, index) {
  return { protocol_version: 1, request_id: request.request_id, connection_id: request.connection_id ?? null, status: 'succeeded', job_id: null, result_id: `v1-performance-cache-${index}`,
    data: page, page: page.page, limits: page.limits, metrics: { elapsed_ms: 0, retry_count: 0, request_charge: null }, warnings: [], error: null };
}

export function makePerformancePages() {
  const pages = [];
  for (let offset = 0; offset < PERFORMANCE_FEATURES; offset += PERFORMANCE_PAGE_SIZE) {
    const features = [], row_references = [];
    for (let index = offset; index < offset + PERFORMANCE_PAGE_SIZE; index++) {
      const west = -10 + index % 100 * 0.2, south = -7 + Math.floor(index / 100) * 0.14;
      const coordinates = Array.from({ length: 25 }, (_, vertex) => [Number((west + vertex * 0.003).toFixed(6)), Number((south + vertex % 2 * 0.002).toFixed(6))]);
      const id = `v1-query:0:${index}`;
      features.push({ type: 'Feature', id, geometry: { type: 'LineString', coordinates }, properties: { label: `own ${index}` } });
      row_references.push({ feature_id: id, identity: null, snapshot_id: 'v1-query', row_ordinal: index });
    }
    const more = offset + PERFORMANCE_PAGE_SIZE < PERFORMANCE_FEATURES;
    pages.push({ features, row_references, page: { next_token: more ? `v1-page-${pages.length + 1}` : null, has_more: more, resume_mode: more ? 'materialized' : 'none' }, limits: { truncated: false, reasons: [] }, warnings: [], feature_errors: [] });
  }
  return pages;
}

export function performanceExpectations(pages) {
  const bytes = value => new TextEncoder().encode(JSON.stringify(value)).byteLength;
  const request = { request_id: '00000000-0000-4000-8000-000000000000', connection_id: 'v1-inactive-connection' };
  const evidence = pages.map((page, index) => ({ index, feature_count: page.features.length, coordinate_count: page.features.reduce((sum, feature) => sum + coordinateCount(feature.geometry), 0), response_json_bytes: bytes(performanceEnvelope(request, page, index)) }));
  return { feature_count: evidence.reduce((sum, page) => sum + page.feature_count, 0), coordinate_count: evidence.reduce((sum, page) => sum + page.coordinate_count, 0),
    response_json_bytes: evidence.reduce((sum, page) => sum + page.response_json_bytes, 0), geojson_bytes: bytes({ type: 'FeatureCollection', features: pages.flatMap(page => page.features) }), pages: evidence };
}

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])]));
  return value;
}

export function validatePerformance(report, expected) {
  const invalid = () => { throw Object.assign(new Error('INVALID_PERFORMANCE_REPORT'), { code: 'INVALID_PERFORMANCE_REPORT' }); };
  if (!report || report.measurement !== 'requestAnimationFrame_gap_ms' || report.threshold_ms !== 100 || report.allowed_over_threshold !== 1 || report.preparation_before_measurement !== true) invalid();
  if (expected.feature_count !== PERFORMANCE_FEATURES || expected.coordinate_count !== PERFORMANCE_COORDINATES || expected.pages.length !== 10 || expected.response_json_bytes > PERFORMANCE_LAYER_BYTES || expected.geojson_bytes > PERFORMANCE_LAYER_BYTES || expected.pages.some(page => page.response_json_bytes > PERFORMANCE_PAGE_BYTES || page.feature_count > 1000 || page.coordinate_count > 100_000)) invalid();
  if (JSON.stringify(canonical(report.dataset)) !== JSON.stringify(canonical(expected)) || JSON.stringify(canonical(report.returned_pages)) !== JSON.stringify(canonical(expected.pages)) || report.worker_feature_count !== PERFORMANCE_FEATURES || report.worker_coordinate_count !== PERFORMANCE_COORDINATES || report.rendered_feature_count !== PERFORMANCE_FEATURES) invalid();
  if (report.rendered_coordinate_count !== PERFORMANCE_COORDINATES || report.listed_feature_count !== 100 || report.last_feature_id !== 'v1-query:0:9999') invalid();
  for (const [name, version, generation] of [['open_ack', 0, 1], ['viewport_ack', 1, 2], ['close_ack', 1, 3]]) {
    const ack = report[name];
    if (!ack || ack.gui_applied !== true || ack.state_version !== version || ack.rendered_version !== version || ack.generation !== generation) invalid();
  }
  const metrics = report.metrics;
  if (!metrics || !['workers_started', 'worker_messages', 'worker_geojson', 'gl_contexts', 'gl_draws', 'viewport_gl_draws'].every(name => Number.isSafeInteger(metrics[name]) && metrics[name] > 0)
    || metrics.workers_terminated !== metrics.workers_started || metrics.assets_created !== 2 || metrics.assets_disposed !== 2 || metrics.subscriptions !== 1 || metrics.unsubscribed !== 1 || metrics.external_attempts !== 0 || metrics.worker_raw_canary !== false) invalid();
  const cleanup = report.cleanup;
  if (!cleanup || !['worker_baseline', 'global_listener_baseline', 'assets_baseline', 'css_baseline', 'canvas_baseline', 'modal_baseline', 'root_shutdown', 'unsubscribed', 'ack_cleanup_complete'].every(name => cleanup[name] === true) || !Number.isSafeInteger(cleanup.global_listeners_before) || cleanup.global_listeners_before < 0 || cleanup.global_listeners_after !== cleanup.global_listeners_before) invalid();
  const timing = report.timing;
  if (!timing || !Array.isArray(timing.phases) || timing.phases.length !== PERFORMANCE_PHASES.length || !Number.isSafeInteger(timing.sample_count) || timing.sample_count < 2 || !Number.isSafeInteger(timing.over_100ms_count) || timing.over_100ms_count < 0 || timing.over_100ms_count >= 2
    || !Number.isFinite(timing.max_gap_ms) || timing.max_gap_ms <= 0 || !Number.isFinite(timing.elapsed_ms) || timing.elapsed_ms <= 0) invalid();
  if (!PERFORMANCE_PHASES.every(name => timing.phases.some(value => value.name === name && Number.isSafeInteger(value.sample_count) && value.sample_count > 0 && Number.isSafeInteger(value.over_100ms_count) && value.over_100ms_count >= 0 && Number.isFinite(value.max_gap_ms) && value.max_gap_ms > 0 && Number.isFinite(value.elapsed_ms) && value.elapsed_ms > 0))
    || timing.phases.reduce((sum, value) => sum + value.sample_count, 0) !== timing.sample_count || timing.phases.reduce((sum, value) => sum + value.over_100ms_count, 0) !== timing.over_100ms_count || Math.max(...timing.phases.map(value => value.max_gap_ms)) !== timing.max_gap_ms) invalid();
  if (!Array.isArray(timing.over_100ms) || timing.over_100ms.length !== timing.over_100ms_count || timing.over_100ms.some(value => !PERFORMANCE_PHASES.includes(value.phase) || !Number.isFinite(value.gap_ms) || value.gap_ms <= 100)
    || (timing.max_gap_ms > 100) !== (timing.over_100ms_count > 0) || report.no_external_requests !== true || report.query_snapshot_only !== true) invalid();
  return report;
}
