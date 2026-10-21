import { vi } from "vitest";

export class FakeMap {
  static instances: FakeMap[] = [];
  static autoEvents = true;
  static lifecycle: string[] = [];
  events = new Map<string, Set<(event: Record<string, unknown>) => void>>();
  sources = new Map<string, { data: unknown; setData: ReturnType<typeof vi.fn>; updateData: ReturnType<typeof vi.fn> }>();
  layers = new Map<string, Record<string, unknown>>();
  canvas = document.createElement("canvas");
  styleLoaded = true;
  tilesLoaded = true;
  sourcesLoaded = true;
  clickFeature: unknown = null;
  bounds = { west: -180, east: 180, south: -85, north: 85 };
  removed = false;
  setPaintProperty = vi.fn();
  setLayoutProperty = vi.fn();
  fitBounds = vi.fn(() => this.emit("moveend", {}));
  setStyle = vi.fn(() => { this.sources.clear(); this.layers.clear(); });
  constructor(public options: Record<string, unknown>) { FakeMap.instances.push(this); }
  on(name: string, handler: (event: Record<string, unknown>) => void) { if (!this.events.has(name)) this.events.set(name, new Set()); this.events.get(name)!.add(handler); return this; }
  off(name: string, handler: (event: Record<string, unknown>) => void) { this.events.get(name)?.delete(handler); return this; }
  emit(name: string, event: Record<string, unknown> = {}) { for (const handler of [...this.events.get(name) ?? []]) handler(event); }
  triggerRepaint() { if (FakeMap.autoEvents) queueMicrotask(() => { if (!this.removed) { this.emit("render"); this.emit("idle"); } }); }
  getCanvas() { return this.canvas; }
  isStyleLoaded() { return this.styleLoaded; }
  areTilesLoaded() { return this.tilesLoaded; }
  isSourceLoaded() { return this.sourcesLoaded; }
  getSource(id: string) { return this.sources.get(id); }
  addSource(id: string, source: { data: unknown }) {
    const entry = { data: source.data, setData: vi.fn((data: unknown) => { entry.data = data; }),
      updateData: vi.fn((diff: { add: unknown[] }) => { const data = entry.data as { features: unknown[] }; entry.data = { ...data, features: [...data.features, ...diff.add] }; }) };
    this.sources.set(id, entry);
  }
  removeSource(id: string) { this.sources.delete(id); }
  getLayer(id: string) { return this.layers.get(id); }
  addLayer(layer: Record<string, unknown>) { this.layers.set(String(layer.id), layer); }
  removeLayer(id: string) { this.layers.delete(id); }
  queryRenderedFeatures() { return this.clickFeature ? [this.clickFeature] : []; }
  getBounds() { return { getWest: () => this.bounds.west, getEast: () => this.bounds.east, getSouth: () => this.bounds.south, getNorth: () => this.bounds.north }; }
  remove() { this.removed = true; FakeMap.lifecycle.push("map.remove"); }
  listenerCount() { return [...this.events.values()].reduce((count, handlers) => count + handlers.size, 0); }
  static reset() { FakeMap.instances = []; FakeMap.autoEvents = true; FakeMap.lifecycle = []; }
}
