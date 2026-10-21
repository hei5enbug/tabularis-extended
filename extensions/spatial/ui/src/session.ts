import type { PluginMapApplyAck, PluginMapApplyRequest } from "@tabularis/plugin-api";
import { parseMap, safeInteger, SpatialUiError, object, type MapState } from "./models";

export function failure(request: PluginMapApplyRequest): PluginMapApplyAck { return { state_version: request.state_version, rendered_version: null, generation: request.generation, gui_applied: false }; }
export function parseApply(request: PluginMapApplyRequest): { action: "open" | "update" | "close"; map: MapState } {
  const state = object(request.state);
  if (!safeInteger(request.state_version) || !safeInteger(request.generation) || !["open", "update", "close"].includes(String(state.action))) throw new SpatialUiError("INVALID_ARGUMENT", "지도 apply 요청이 유효하지 않습니다.");
  const map = parseMap(state.map);
  if (request.map_id !== map.map_id || request.state_version !== map.version) throw new SpatialUiError("INVALID_ARGUMENT", "지도 apply version이 일치하지 않습니다.");
  return { action: state.action as "open" | "update" | "close", map };
}
export interface RenderState { map: MapState; generation: number; serial: number }

export class MapSession {
  private listeners = new Set<() => void>();
  private pending: { request: PluginMapApplyRequest; resolve: (ack: PluginMapApplyAck) => void } | null = null;
  private closed = false;
  private externalGeneration = -1;
  private mounted = false;
  private presented = false;
  private resolveDisposed!: () => void;
  readonly whenDisposed = new Promise<void>(resolve => { this.resolveDisposed = resolve; });
  private snapshot: RenderState;
  constructor(map: MapState) { this.snapshot = { map, generation: 0, serial: 0 }; }
  subscribe = (listener: () => void): (() => void) => { this.listeners.add(listener); return () => this.listeners.delete(listener); };
  getSnapshot = (): RenderState => this.snapshot;
  isClosed(): boolean { return this.closed; }
  markPresented(): void { this.presented = true; }
  markMounted(): void { this.mounted = true; }
  unmounted(): void { this.mounted = false; queueMicrotask(() => { if (!this.mounted) { this.close(); this.resolveDisposed(); } }); }

  canApply(request: PluginMapApplyRequest, map: MapState): boolean {
    return !this.closed && request.state_version >= this.snapshot.map.version && request.generation >= this.externalGeneration && !map.layers.some(layer => {
      const previous = this.snapshot.map.layers.find(value => value.layer_id === layer.layer_id);
      return previous && layer.generation < previous.generation;
    });
  }

  apply(request: PluginMapApplyRequest, map: MapState): Promise<PluginMapApplyAck> {
    if (!this.canApply(request, map)) return Promise.resolve(failure(request));
    this.rejectPending();
    this.externalGeneration = request.generation;
    const promise = new Promise<PluginMapApplyAck>(resolve => { this.pending = { request, resolve }; });
    this.snapshot = { map, generation: request.generation, serial: this.snapshot.serial + 1 };
    this.notify();
    return promise;
  }
  updateLocal(map: MapState): void {
    if (this.closed || map.version < this.snapshot.map.version) return;
    this.rejectPending();
    this.snapshot = { map, generation: this.snapshot.generation, serial: this.snapshot.serial + 1 };
    this.notify();
  }
  rendered(serial: number, success: boolean): void {
    if (this.closed || serial !== this.snapshot.serial || !this.pending) return;
    const { request, resolve } = this.pending;
    this.pending = null;
    resolve(success ? { state_version: request.state_version, rendered_version: request.state_version, generation: request.generation, gui_applied: true } : failure(request));
  }
  close(): void { this.closed = true; this.rejectPending(); this.listeners.clear(); if (!this.mounted && !this.presented) this.resolveDisposed(); }
  private rejectPending(): void { if (this.pending) this.pending.resolve(failure(this.pending.request)); this.pending = null; }
  private notify(): void { for (const listener of this.listeners) listener(); }
}
