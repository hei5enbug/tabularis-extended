import type { UsePluginServiceReturn } from "@tabularis/plugin-api";
import { call } from "./service";
import { countGeometry, LAYER_LIMIT, SpatialUiError, parsePage, pageBytes, type FeaturePage, type LayerState } from "./models";

export interface LayerData { page: FeaturePage; resultId: string | null; featureResultIds: Map<string, string>; bytes: number; coordinates: number; sourceKey: string; generation: number }
export function sourceKey(layer: LayerState): string { return JSON.stringify([layer.connection_id, layer.source]); }

export class LayerDataStore {
  private values = new Map<string, LayerData>();
  private controllers = new Map<string, AbortController>();
  private current = new Map<string, { generation: number; sourceKey: string }>();
  constructor(private service: UsePluginServiceReturn) {}

  get(layerId: string): LayerData | undefined { return this.values.get(layerId); }
  mark(layers: LayerState[]): void {
    const live = new Set(layers.map(layer => layer.layer_id));
    for (const [id, controller] of this.controllers) if (!live.has(id)) { controller.abort(); this.controllers.delete(id); }
    for (const id of this.values.keys()) if (!live.has(id)) this.values.delete(id);
    for (const id of this.current.keys()) if (!live.has(id)) this.current.delete(id);
    for (const layer of layers) {
      const previous = this.current.get(layer.layer_id);
      const key = sourceKey(layer);
      if (previous && (previous.generation !== layer.generation || previous.sourceKey !== key)) this.controllers.get(layer.layer_id)?.abort();
      this.current.set(layer.layer_id, { generation: layer.generation, sourceKey: key });
    }
  }

  async load(layer: LayerState, signal: AbortSignal, more = false): Promise<LayerData> {
    const key = sourceKey(layer);
    const cached = this.values.get(layer.layer_id);
    if (!more && cached?.sourceKey === key) {
      const result = { ...cached, generation: layer.generation };
      this.values.set(layer.layer_id, result);
      return result;
    }
    if (more && (!cached || cached.sourceKey !== key || !cached.page.page.has_more)) throw new SpatialUiError("INVALID_PAGE_TOKEN", "이 레이어는 더 불러올 수 없습니다.");
    this.controllers.get(layer.layer_id)?.abort();
    const controller = new AbortController();
    this.controllers.set(layer.layer_id, controller);
    const abort = () => controller.abort();
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) abort();
    try {
      const source = layer.source;
      const { kind: _kind, ...input } = source;
      const token = more ? cached?.page.page.next_token : null;
      const response = source.kind === "query_result"
        ? await call(this.service, "spatial.query_result", { ...input as Omit<Extract<typeof source, { kind: "query_result" }>, "kind">, page_size: 1000, next_token: token }, { connection_id: layer.connection_id }, controller.signal)
        : await call(this.service, "spatial.table_query", { ...input as Omit<Extract<typeof source, { kind: "table" }>, "kind">, page_size: 1000, next_token: token }, { connection_id: layer.connection_id }, controller.signal);
      const current = this.current.get(layer.layer_id);
      if (controller.signal.aborted || current?.generation !== layer.generation || current.sourceKey !== key) throw new DOMException("Stale layer", "AbortError");
      const page = parsePage(response.data);
      const bytes = pageBytes(page);
      const coordinates = page.features.reduce((sum, feature) => sum + countGeometry(feature.geometry), 0);
      const totalBytes = (more ? cached?.bytes ?? 0 : 0) + bytes;
      const totalCoordinates = (more ? cached?.coordinates ?? 0 : 0) + coordinates;
      const count = (more ? cached?.page.features.length ?? 0 : 0) + page.features.length;
      if (totalBytes > LAYER_LIMIT.bytes || totalCoordinates > LAYER_LIMIT.coordinates || count > LAYER_LIMIT.features) {
        if (cached && more) {
          const cappedPage = { ...cached.page, features: [...cached.page.features], row_references: [...cached.page.row_references], warnings: [...new Set([...cached.page.warnings, ...page.warnings])], feature_errors: [...cached.page.feature_errors, ...page.feature_errors], page: { next_token: null, has_more: false, resume_mode: "none" as const }, limits: { truncated: true, reasons: [...new Set([...cached.page.limits.reasons, ...page.limits.reasons, "LAYER_LIMIT"])] } };
          while (pageBytes(cappedPage) > LAYER_LIMIT.bytes && cappedPage.features.length) { cappedPage.features.pop(); cappedPage.row_references.pop(); }
          if (pageBytes(cappedPage) > LAYER_LIMIT.bytes) throw new SpatialUiError("RESOURCE_LIMIT", "상한 정보가 레이어 byte budget을 초과했습니다.");
          const capped = { ...cached, page: cappedPage, coordinates: cappedPage.features.reduce((sum, feature) => sum + countGeometry(feature.geometry), 0) };
          this.values.set(layer.layer_id, capped);
          return capped;
        }
        throw new SpatialUiError("RESOURCE_LIMIT", "레이어 누적 상한을 초과했습니다.");
      }
      let combined = page;
      if (more && cached) {
        const ids = new Set(cached.page.features.map(feature => feature.id));
        if (page.features.some(feature => ids.has(feature.id))) throw new SpatialUiError("INVALID_ARGUMENT", "다음 page의 feature ID가 중복됩니다.");
        combined = { ...page, features: [...cached.page.features, ...page.features], row_references: [...cached.page.row_references, ...page.row_references], warnings: [...new Set([...cached.page.warnings, ...page.warnings])], feature_errors: [...cached.page.feature_errors, ...page.feature_errors], limits: { truncated: page.limits.truncated || cached.page.limits.truncated, reasons: [...new Set([...cached.page.limits.reasons, ...page.limits.reasons])] } };
      }
      const featureResultIds = new Map(more ? cached?.featureResultIds : []);
      if (response.result_id) for (const feature of page.features) featureResultIds.set(feature.id, response.result_id);
      const result = { page: combined, resultId: response.result_id, featureResultIds, bytes: totalBytes, coordinates: totalCoordinates, sourceKey: key, generation: layer.generation };
      this.values.set(layer.layer_id, result);
      return result;
    } finally {
      signal.removeEventListener("abort", abort);
      if (this.controllers.get(layer.layer_id) === controller) this.controllers.delete(layer.layer_id);
    }
  }

  cancel(): void { for (const controller of this.controllers.values()) controller.abort(); this.controllers.clear(); }
  dispose(): void { this.cancel(); this.values.clear(); this.current.clear(); }
}
