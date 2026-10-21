import { vi } from "vitest";
import type { PluginMapApplyRequest, PluginMapApplyAck, ServiceRequest, ServiceResponse, ServiceCapabilities, UsePluginServiceReturn } from "@tabularis/plugin-api";
import { WORLD, DEFAULT_STYLE, object, type FeaturePage, type LayerSource, type MapState } from "../src/models";

export const rawWrapper = { kind: "spatial", native_type: "geometry", srid: 4326, dimensions: "XYZM", encoding: "ewkb-base64", value: "AQEA-original+/==", is_empty: false };
export const mapFixture: MapState = { map_id: "fixture-map", name: "시험 지도", version: 1, viewport: { ...WORLD }, basemap: null, layers: [{ layer_id: "fixture-layer", connection_id: "inactive-connection", source: { kind: "query_result", result_id: "original-snapshot", result_set_index: 2, column_index: 4, skip_invalid: false, longitude_mode: "preserve" }, visible: true, style: DEFAULT_STYLE, generation: 0, result_references: [{ result_id: "original-snapshot", result_set_index: 2 }] }], selected_feature_refs: [] };
export const pageFixture: FeaturePage = {
  features: [{ type: "Feature", id: "original-snapshot:2:19", geometry: { type: "Point", coordinates: [179, 89] }, properties: { label: "<img src=x onerror=alert(1)>", g: rawWrapper } }],
  row_references: [{ feature_id: "original-snapshot:2:19", identity: null, snapshot_id: "original-snapshot", row_ordinal: 19 }],
  page: { next_token: null, has_more: false, resume_mode: "none" }, limits: { truncated: false, reasons: [] }, warnings: ["DIMENSIONS_REDUCED_TO_XY"], feature_errors: [],
};
export function applyRequest(action: "open" | "update" | "close", map: MapState = mapFixture, generation = 1): PluginMapApplyRequest {
  return { map_id: map.map_id, state_version: map.version, generation, state: { action, map: structuredClone(map) } as unknown as PluginMapApplyRequest["state"] };
}
export function response(request: ServiceRequest, data: unknown, resultId: string | null = null): ServiceResponse {
  return { protocol_version: 1, request_id: request.request_id, connection_id: request.connection_id ?? null, status: "succeeded", job_id: null, result_id: resultId, data: data as ServiceResponse["data"], page: { next_token: null, has_more: false, resume_mode: "none" }, limits: { truncated: false, reasons: [] }, metrics: { elapsed_ms: 0, retry_count: 0, request_charge: null }, warnings: [], error: null };
}
export const capabilities: ServiceCapabilities = { service_protocol: 1, operations: ["map.open", "map.create", "map.layer.add", "spatial.query_result", "spatial.table_query"], spatial_v1: true, documents_v1: false, cancel_v1: true, gui_instance_id: "fixture-gui" };
export const harness = {
  map: structuredClone(mapFixture),
  page: structuredClone(pageFixture),
  handler: null as ((request: PluginMapApplyRequest) => Promise<PluginMapApplyAck>) | null,
  requests: [] as ServiceRequest[],
  failNext: null as NonNullable<ServiceResponse["error"]>["code"] | null,
  openGuiApplied: true,
  unsubscribe: vi.fn(),
  closeModal: vi.fn(),
  openModal: vi.fn(),
  toast: { showInfo: vi.fn(async () => {}), showError: vi.fn(async () => {}), showWarning: vi.fn(async () => {}) },
  assets: { resolve: vi.fn(async (path: string) => ({ url: `blob:fixture-${path}`, dispose: vi.fn() })) },
  service: {} as UsePluginServiceReturn,
};
harness.service = {
  executeWrite: vi.fn(async () => { throw new Error("Query writes are unavailable in this map fixture."); }),
  saveArtifact: vi.fn(async () => true),
  importCredential: vi.fn(async () => { throw new Error("지도 fixture에서 credential.import는 지원하지 않습니다."); }),
  capabilities: vi.fn(async () => capabilities),
  subscribeMap: vi.fn(async handler => { harness.handler = handler; return harness.unsubscribe; }),
  call: vi.fn(async (request: ServiceRequest): Promise<ServiceResponse> => {
    harness.requests.push(structuredClone(request));
    if (harness.failNext) {
      const code = harness.failNext;
      harness.failNext = null;
      return { ...response(request, null), status: "failed", error: { code, message: "fixture failure", retryable: false, outcome: "not_applied", details: null } };
    }
    const input = object(request.input);
    let data: unknown = {};
    switch (request.operation) {
      case "result.get": data = { kind: "tabular", columns: [{ name: "g", column_index: 4, native_type: "geometry", srid: 4326, dimensions: "XYZM", type_oid: 17001 }, { name: "g", column_index: 5, native_type: "geography", srid: null, dimensions: null, type_oid: 17002 }], rows: [[null, null]] }; break;
      case "spatial.columns": data = { columns: [{ name: "g", column_index: 2, native_type: "geometry", type_oid: 17001, base_oid: 17001, typmod: -1, nullable: true, srid: null, srid_status: "mixed", dimensions: "XY", geometry_type: null, primary_key: ["tenant", "id"], spatial_index: true }] }; break;
      case "spatial.query_result": case "spatial.table_query": return { ...response(request, structuredClone(harness.page), "display-cache"), page: harness.page.page, limits: harness.page.limits, warnings: harness.page.warnings };
      case "spatial.feature": data = { original_row: { label: "<img src=x onerror=alert(1)>", g: rawWrapper }, identity: null }; break;
      case "spatial.export": data = { artifact_id: "fixture-artifact", bytes: 417, count: 1, checksum: "sha256:fixture", truncated: false }; break;
      case "map.create": harness.map = { ...structuredClone(mapFixture), map_id: "created-map", version: 0, name: String(input.name), layers: [] }; break;
      case "map.layer.add": {
        const source = structuredClone(input.source) as LayerSource;
        harness.map.layers.push({ layer_id: `added-${harness.map.layers.length}`, connection_id: request.connection_id!, source, visible: true, style: structuredClone(input.style) as typeof DEFAULT_STYLE, generation: 0, result_references: source.kind === "query_result" ? [{ result_id: source.result_id, result_set_index: source.result_set_index }] : [] });
        harness.map.version++; break;
      }
      case "map.layer.update": {
        const layer = harness.map.layers.find(layer => layer.layer_id === input.layer_id)!;
        if (typeof input.visible === "boolean") layer.visible = input.visible;
        if (input.style) layer.style = input.style as typeof DEFAULT_STYLE;
        layer.generation++; harness.map.version++; break;
      }
      case "map.layer.remove": harness.map.layers = harness.map.layers.filter(layer => layer.layer_id !== input.layer_id); harness.map.version++; break;
      case "map.viewport.set": {
        harness.map.viewport = input.viewport as MapState["viewport"];
        for (const layer of harness.map.layers) if (layer.source.kind === "table") { layer.source.viewport = harness.map.viewport; layer.generation++; }
        harness.map.version++; break;
      }
      case "map.selection.set": harness.map.selected_feature_refs = input.feature_refs as MapState["selected_feature_refs"]; harness.map.version++; break;
      case "map.update": if (typeof input.name === "string") harness.map.name = input.name; if ("basemap" in input) harness.map.basemap = input.basemap as MapState["basemap"]; harness.map.version++; break;
      case "map.list": data = { maps: [harness.map] }; break;
      case "map.open": case "map.close": {
        const ack = harness.handler ? await harness.handler(applyRequest(request.operation === "map.open" ? "open" : "close", harness.map, harness.map.version + 10)) : null;
        return response(request, { map: harness.map, state_version: harness.map.version, rendered_version: ack?.rendered_version ?? (harness.openGuiApplied ? harness.map.version : null), gui_applied: ack?.gui_applied ?? harness.openGuiApplied });
      }
      default: break;
    }
    if (request.operation.startsWith("map.") && request.operation !== "map.list") data = { map: structuredClone(harness.map), state_version: harness.map.version, rendered_version: null, gui_applied: false };
    return response(request, data);
  }),
};
const defaults = { call: vi.mocked(harness.service.call).getMockImplementation()!, subscribe: vi.mocked(harness.service.subscribeMap).getMockImplementation()!, assets: harness.assets.resolve.getMockImplementation()! };
export function resetHarness(): void {
  vi.clearAllMocks();
  harness.map = structuredClone(mapFixture);
  harness.page = structuredClone(pageFixture);
  harness.requests = [];
  harness.handler = null;
  harness.failNext = null;
  harness.openGuiApplied = true;
  vi.mocked(harness.service.capabilities).mockReset().mockResolvedValue(capabilities);
  vi.mocked(harness.service.call).mockReset().mockImplementation(defaults.call);
  vi.mocked(harness.service.saveArtifact).mockReset().mockResolvedValue(true);
  vi.mocked(harness.service.subscribeMap).mockReset().mockImplementation(defaults.subscribe);
  harness.assets.resolve.mockReset().mockImplementation(defaults.assets);
  harness.openModal.mockReset();
  harness.closeModal.mockReset();
}
