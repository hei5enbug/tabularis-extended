import { Map as LibreMap, setWorkerUrl, type GeoJSONSource, type MapMouseEvent, type StyleSpecification } from "maplibre-gl";
import { cameraBounds, countGeometry, SpatialUiError, type Feature, type Geometry, type LayerState, type MapState, type Viewport } from "./models";
import { LayerDataStore } from "./data";
import type { MapAssets } from "./assets";

export function displayFeatures(features: Feature[], layerId: string, offset = 0) {
  const output: { type: "Feature"; id: string; geometry: Exclude<Geometry, { type: "GeometryCollection" }>; properties: { feature_id: string; layer_id: string } }[] = [];
  const visit = (geometry: Geometry, featureId: string) => {
    if (geometry.type === "GeometryCollection") { for (const child of geometry.geometries) visit(child, featureId); }
    else if (countGeometry(geometry) > 0) output.push({ type: "Feature", id: `${featureId}:${offset + output.length}`, geometry, properties: { feature_id: featureId, layer_id: layerId } });
  };
  for (const feature of features) if (feature.geometry) visit(feature.geometry, feature.id);
  return output;
}

export function viewportFromCamera(west: number, east: number, south: number, north: number): Viewport {
  const longitude = (value: number) => ((value + 180) % 360 + 360) % 360 - 180;
  const world = east - west >= 360;
  return { west: world ? -180 : longitude(west), east: world ? 180 : longitude(east), south: Math.max(-90, south), north: Math.min(90, north), world };
}

export class MapEngine {
  private map: LibreMap;
  private disposed = false;
  private sourceIds = new Set<string>();
  private appliedSources = new Map<string, { features: Feature[] | null; sourceKey: string; displayCount: number }>();
  private cameraKey = "";
  private styleKey = "";
  private programmatic = false;
  private renderController: AbortController | null = null;
  private activeLayerIds: string[] = [];
  private moving = (event: { originalEvent?: unknown }) => {
    if (this.programmatic || !event.originalEvent || this.disposed) return;
    const bounds = this.map.getBounds();
    this.onViewport(viewportFromCamera(bounds.getWest(), bounds.getEast(), bounds.getSouth(), bounds.getNorth()));
  };
  private click = (event: MapMouseEvent) => {
    const ids = this.activeLayerIds.filter(id => this.map.getLayer(id));
    const feature = ids.length ? this.map.queryRenderedFeatures(event.point, { layers: ids })[0] : null;
    if (feature && typeof feature.properties?.layer_id === "string" && typeof feature.properties?.feature_id === "string") this.onSelect(feature.properties.layer_id, feature.properties.feature_id);
  };

  constructor(container: HTMLElement, private assets: MapAssets, private data: LayerDataStore, private onViewport: (viewport: Viewport) => void, private onSelect: (layerId: string, featureId: string) => void, private background: string) {
    setWorkerUrl(assets.workerUrl);
    this.map = new LibreMap({ container, style: this.blankStyle(), attributionControl: false, renderWorldCopies: false });
    this.map.getCanvas().setAttribute("aria-label", "공간 지도. 방향키로 이동하고 + 또는 - 키로 확대하거나 축소합니다.");
    this.map.on("moveend", this.moving);
    this.map.on("click", this.click);
  }

  cancel(): void { this.renderController?.abort(); this.data.cancel(); }

  async apply(state: MapState, signal: AbortSignal, approvedBasemapUrl: string | null = null): Promise<void> {
    if (this.disposed) throw new DOMException("Unmounted", "AbortError");
    this.renderController?.abort();
    const controller = new AbortController();
    this.renderController = controller;
    const abort = () => controller.abort();
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) abort();
    this.data.mark(state.layers);
    try {
      const basemap = state.basemap?.style_url === approvedBasemapUrl ? state.basemap : null;
      const styleKey = basemap?.style_url ?? "blank";
      if (this.styleKey !== styleKey) {
        this.styleKey = styleKey;
        this.sourceIds.clear();
        this.appliedSources.clear();
        this.map.setStyle(basemap?.style_url ?? this.blankStyle());
      }
      await this.waitFor("style", controller.signal);
      await Promise.all(state.layers.filter(layer => layer.visible).map(layer => this.data.load(layer, controller.signal)));
      if (controller.signal.aborted || this.disposed) throw new DOMException("Cancelled", "AbortError");
      this.syncSources(state);
      const cameraKey = JSON.stringify(state.viewport);
      if (this.cameraKey !== cameraKey) {
        this.programmatic = true;
        this.cameraKey = cameraKey;
        this.map.fitBounds(cameraBounds(state.viewport), { duration: 0, padding: 12 });
        this.programmatic = false;
      }
      await this.waitFor("idle", controller.signal);
    } finally { signal.removeEventListener("abort", abort); }
  }

  async refresh(state: MapState, signal: AbortSignal): Promise<void> {
    this.syncSources(state);
    await this.waitFor("idle", signal);
  }

  private syncSources(state: MapState): void {
    this.activeLayerIds = [];
    const live = new Set(state.layers.map(layer => `spatial:${layer.layer_id}`));
    for (const id of this.sourceIds) if (!live.has(id)) {
      for (const kind of ["point", "line", "polygon"]) if (this.map.getLayer(`${id}:${kind}`)) this.map.removeLayer(`${id}:${kind}`);
      if (this.map.getSource(id)) this.map.removeSource(id);
      this.sourceIds.delete(id);
      this.appliedSources.delete(id);
    }
    for (const layer of state.layers) {
      const id = `spatial:${layer.layer_id}`;
      const data = this.data.get(layer.layer_id);
      const sourceKey = JSON.stringify([layer.connection_id, layer.source]);
      const features = data?.sourceKey === sourceKey ? data.page.features : null;
      const source = this.map.getSource(id) as GeoJSONSource | undefined;
      const applied = this.appliedSources.get(id);
      if (!source || applied?.features !== features || applied.sourceKey !== sourceKey) {
        const append = source && features && applied?.features && applied.sourceKey === sourceKey
          && features.length > applied.features.length && applied.features.every((feature, index) => feature === features[index]);
        const display = displayFeatures(append ? features.slice(applied.features!.length) : features ?? [], layer.layer_id, append ? applied.displayCount : 0);
        if (append) source.updateData({ add: display });
        else {
          const collection = { type: "FeatureCollection" as const, features: display };
          if (source) source.setData(collection);
          else { this.map.addSource(id, { type: "geojson", data: collection }); this.sourceIds.add(id); }
        }
        this.appliedSources.set(id, { features, sourceKey, displayCount: (append ? applied.displayCount : 0) + display.length });
      }
      this.syncLayer(layer, id, state);
    }
  }

  private syncLayer(layer: LayerState, id: string, state: MapState): void {
    const selected = state.selected_feature_refs.filter(reference => reference.layer_id === layer.layer_id).map(reference => reference.feature_id);
    const selection = ["in", ["get", "feature_id"], ["literal", selected]];
    const definitions = [
      { kind: "polygon", type: "fill", filter: "Polygon", paint: { "fill-color": ["case", selection, "#f59e0b", layer.style.polygon?.color ?? "#3b82f6"], "fill-opacity": layer.style.polygon?.opacity ?? 0.35 } },
      { kind: "line", type: "line", filter: "LineString", paint: { "line-color": ["case", selection, "#f59e0b", layer.style.line?.color ?? "#3b82f6"], "line-opacity": layer.style.line?.opacity ?? 0.9, "line-width": layer.style.line?.width ?? 2 } },
      { kind: "point", type: "circle", filter: "Point", paint: { "circle-color": ["case", selection, "#f59e0b", layer.style.point?.color ?? "#3b82f6"], "circle-opacity": layer.style.point?.opacity ?? 0.9, "circle-radius": layer.style.point?.radius ?? 5 } },
    ];
    for (const definition of definitions) {
      const layerId = `${id}:${definition.kind}`;
      if (!this.map.getLayer(layerId)) this.map.addLayer({ id: layerId, source: id, type: definition.type, filter: ["==", ["geometry-type"], definition.filter], paint: definition.paint } as Parameters<LibreMap["addLayer"]>[0]);
      for (const [name, value] of Object.entries(definition.paint)) this.map.setPaintProperty(layerId, name as Parameters<LibreMap["setPaintProperty"]>[1], value);
      this.map.setLayoutProperty(layerId, "visibility", layer.visible ? "visible" : "none");
      this.activeLayerIds.push(layerId);
    }
  }

  private waitFor(stage: "style" | "idle", signal: AbortSignal): Promise<void> {
    if (signal.aborted) return Promise.reject(new DOMException("Cancelled", "AbortError"));
    if (stage === "style" && this.map.isStyleLoaded()) return Promise.resolve();
    return new Promise((resolve, reject) => {
      let rendered = false;
      const ready = () => this.map.isStyleLoaded() && this.map.areTilesLoaded() && [...this.sourceIds].every(id => this.map.isSourceLoaded(id));
      const render = () => { rendered = true; };
      const success = () => {
        if (stage === "style" ? this.map.isStyleLoaded() : rendered && ready()) settle();
      };
      const error = (event: { error: { message: string } }) => settle(new Error(event.error.message));
      const abort = () => settle(new DOMException("Cancelled", "AbortError"));
      const timeout = setTimeout(() => settle(new SpatialUiError("APPLY_TIMEOUT", "지도 렌더 완료를 기다리는 시간이 초과되었습니다.")), 15000);
      const settle = (failure?: Error) => {
        clearTimeout(timeout);
        this.map.off("style.load", success);
        this.map.off("styledata", success);
        this.map.off("idle", success);
        this.map.off("render", render);
        this.map.off("error", error);
        signal.removeEventListener("abort", abort);
        if (failure) reject(failure); else resolve();
      };
      this.map.on("style.load", success);
      this.map.on("styledata", success);
      this.map.on("idle", success);
      this.map.on("render", render);
      this.map.on("error", error);
      signal.addEventListener("abort", abort, { once: true });
      this.map.triggerRepaint();
    });
  }

  private blankStyle(): StyleSpecification { return { version: 8, sources: {}, layers: [{ id: "background", type: "background", paint: { "background-color": this.background } }] }; }
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.cancel();
    this.map.off("moveend", this.moving);
    this.map.off("click", this.click);
    this.map.remove();
    this.appliedSources.clear();
    this.sourceIds.clear();
    this.data.dispose();
    this.assets.dispose();
  }
}
