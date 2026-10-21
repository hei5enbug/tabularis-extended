import type { ServiceResponse } from "@tabularis/plugin-api";

export type Json = ServiceResponse["data"];
export interface Viewport { west: number; east: number; south: number; north: number; world?: boolean }
export interface TableRef { database: string | null; schema: string | null; table: string }
export interface RowIdentity { columns: string[]; values: Json[] }
export interface DisplayOptions { source_srid?: number | null; longitude_mode?: "preserve" | "shortest"; skip_invalid?: boolean }
export type LayerSource = DisplayOptions & (
  { kind: "query_result"; result_id: string; result_set_index: number; column_index: number } |
  { kind: "table"; table: TableRef; column: string; viewport: Viewport; srid_filter?: number | null }
);
export interface Style {
  point?: { color?: string; opacity?: number; radius?: number };
  line?: { color?: string; opacity?: number; width?: number };
  polygon?: { color?: string; opacity?: number };
}
export interface Basemap { style_url: string; attribution: string }
export interface LayerState { layer_id: string; connection_id: string; source: LayerSource; visible: boolean; style: Style; generation: number; result_references: { result_id: string; result_set_index: number }[] }
export interface MapState { map_id: string; name: string; version: number; viewport: Viewport; basemap: Basemap | null; layers: LayerState[]; selected_feature_refs: { layer_id: string; feature_id: string }[] }
export type Position = [number, number];
export type Geometry =
  { type: "Point"; coordinates: Position } |
  { type: "LineString" | "MultiPoint"; coordinates: Position[] } |
  { type: "Polygon" | "MultiLineString"; coordinates: Position[][] } |
  { type: "MultiPolygon"; coordinates: Position[][][] } |
  { type: "GeometryCollection"; geometries: Geometry[] };
export interface Feature { type: "Feature"; id: string; geometry: Geometry | null; properties: Record<string, Json> }
export interface RowReference { feature_id: string; identity: RowIdentity | null; snapshot_id?: string; row_ordinal: number }
export interface FeaturePage { features: Feature[]; row_references: RowReference[]; page: ServiceResponse["page"]; limits: ServiceResponse["limits"]; warnings: string[]; feature_errors: { index: number; code: string; message: string }[] }
export interface SpatialColumn { name: string; column_index: number; native_type: "geometry" | "geography" | null; srid: number | null; dimensions: string | null; srid_status?: "known" | "unknown" | "mixed"; primary_key?: string[]; spatial_index?: boolean }

export const WORLD: Viewport = { west: -180, east: 180, south: -90, north: 90, world: true };
export const CAMERA_LATITUDE = 85.05112878;
export const PAGE_LIMIT = { features: 1000, coordinates: 100_000, bytes: 8 * 1024 * 1024 };
export const LAYER_LIMIT = { features: 10_000, coordinates: 250_000, bytes: 32 * 1024 * 1024 };
export const DEFAULT_STYLE: Style = { point: { color: "#3b82f6", radius: 5, opacity: 0.9 }, line: { color: "#3b82f6", width: 2, opacity: 0.9 }, polygon: { color: "#3b82f6", opacity: 0.35 } };

export class SpatialUiError extends Error {
  constructor(public code: string, message: string) { super(message); this.name = "SpatialUiError"; }
}
export function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new SpatialUiError("INVALID_ARGUMENT", "잘못된 서비스 응답입니다.");
  return value as Record<string, unknown>;
}
export function safeInteger(value: unknown): value is number { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0; }
export function validateViewport(viewport: Viewport): void {
  if (![viewport.west, viewport.east, viewport.south, viewport.north].every(Number.isFinite) || viewport.west < -180 || viewport.west > 180 || viewport.east < -180 || viewport.east > 180 || viewport.south < -90 || viewport.north > 90 || viewport.south > viewport.north || (viewport.world !== undefined && typeof viewport.world !== "boolean")) throw new SpatialUiError("INVALID_ARGUMENT", "WGS84 지도 범위를 확인하세요.");
}
export function cameraBounds(viewport: Viewport): [[number, number], [number, number]] {
  validateViewport(viewport);
  const west = viewport.world ? -180 : viewport.west;
  const east = viewport.world ? 180 : viewport.west > viewport.east ? viewport.east + 360 : viewport.east;
  return [[west, Math.max(-CAMERA_LATITUDE, Math.min(CAMERA_LATITUDE, viewport.south))], [east, Math.max(-CAMERA_LATITUDE, Math.min(CAMERA_LATITUDE, viewport.north))]];
}
export function validateBasemap(basemap: Basemap): void {
  let url: URL;
  try { url = new URL(basemap.style_url); } catch { throw new SpatialUiError("INVALID_ARGUMENT", "HTTPS basemap URL을 입력하세요."); }
  if (url.protocol !== "https:" || url.username || url.password || /[\s\\\u0000-\u001f\u007f]/u.test(basemap.style_url) || basemap.style_url.length > 2048 || basemap.attribution.length > 4096) throw new SpatialUiError("INVALID_ARGUMENT", "Basemap은 사용자 정보 없는 HTTPS URL이어야 합니다.");
}
export function parseMap(value: unknown): MapState {
  const map = object(value);
  if (typeof map.map_id !== "string" || !map.map_id || typeof map.name !== "string" || !safeInteger(map.version) || !Array.isArray(map.layers) || !Array.isArray(map.selected_feature_refs)) throw new SpatialUiError("INVALID_ARGUMENT", "지도 상태를 확인할 수 없습니다.");
  const result = map as unknown as MapState;
  validateViewport(result.viewport);
  if (result.basemap) validateBasemap(result.basemap);
  const ids = new Set<string>();
  for (const layer of result.layers) {
    if (!layer.layer_id || !layer.connection_id || !safeInteger(layer.generation) || ids.has(layer.layer_id)) throw new SpatialUiError("INVALID_ARGUMENT", "레이어 상태가 유효하지 않습니다.");
    ids.add(layer.layer_id);
    validateSource(layer.source);
  }
  return result;
}
export function validateSource(source: LayerSource): void {
  if (source.source_srid != null && (!safeInteger(source.source_srid) || source.source_srid === 0)) throw new SpatialUiError("INVALID_ARGUMENT", "Source SRID는 양의 정수여야 합니다.");
  if (source.kind === "query_result") {
    if (!source.result_id || !safeInteger(source.result_set_index) || !safeInteger(source.column_index)) throw new SpatialUiError("INVALID_ARGUMENT", "결과 snapshot과 열 index를 확인하세요.");
  } else if (source.kind === "table") {
    if (!source.table.table || !source.column || (source.srid_filter != null && (!safeInteger(source.srid_filter) || source.srid_filter > 999999))) throw new SpatialUiError("INVALID_ARGUMENT", "Table SRID filter를 확인하세요.");
    validateViewport(source.viewport);
  } else throw new SpatialUiError("INVALID_ARGUMENT", "지도 source가 유효하지 않습니다.");
}
export function countGeometry(value: unknown, depth = 1): number {
  if (value === null) return 0;
  if (depth > 128) throw new SpatialUiError("INVALID_GEOMETRY", "공간 중첩은 128단계까지 지원합니다.");
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new SpatialUiError("INVALID_GEOMETRY", "공간 객체가 유효하지 않습니다.");
  const geometry = value as Record<string, unknown>;
  const position = (input: unknown): number => {
    if (!Array.isArray(input) || input.length !== 2 || !input.every(item => typeof item === "number" && Number.isFinite(item)) || input[0] < -180 || input[0] > 180 || input[1] < -90 || input[1] > 90) throw new SpatialUiError("INVALID_GEOMETRY", "표시 좌표는 유한한 WGS84 XY여야 합니다.");
    return 1;
  };
  const nested = (input: unknown, level: number): number => {
    if (!Array.isArray(input)) throw new SpatialUiError("INVALID_GEOMETRY", "좌표 배열이 유효하지 않습니다.");
    return level === 0 ? position(input) : input.reduce<number>((sum, child) => sum + nested(child, level - 1), 0);
  };
  const line = (input: unknown): number => {
    if (!Array.isArray(input) || input.length === 1) throw new SpatialUiError("INVALID_GEOMETRY", "LineString은 두 좌표 이상이 필요합니다.");
    return nested(input, 1);
  };
  const polygon = (input: unknown): number => {
    if (!Array.isArray(input)) throw new SpatialUiError("INVALID_GEOMETRY", "Polygon ring이 유효하지 않습니다.");
    for (const ring of input) if (!Array.isArray(ring) || ring.length < 4 || JSON.stringify(ring[0]) !== JSON.stringify(ring[ring.length - 1])) throw new SpatialUiError("INVALID_GEOMETRY", "Polygon ring은 닫힌 네 좌표 이상이어야 합니다.");
    return nested(input, 2);
  };
  switch (geometry.type) {
    case "Point": return Array.isArray(geometry.coordinates) && geometry.coordinates.length === 0 ? 0 : nested(geometry.coordinates, 0);
    case "LineString": return line(geometry.coordinates);
    case "MultiPoint": return nested(geometry.coordinates, 1);
    case "Polygon": return polygon(geometry.coordinates);
    case "MultiLineString": if (Array.isArray(geometry.coordinates)) return geometry.coordinates.reduce<number>((sum, child) => sum + line(child), 0); break;
    case "MultiPolygon": if (Array.isArray(geometry.coordinates)) return geometry.coordinates.reduce<number>((sum, child) => sum + polygon(child), 0); break;
    case "GeometryCollection": if (Array.isArray(geometry.geometries) && geometry.geometries.every(child => child !== null)) return geometry.geometries.reduce<number>((sum, child) => sum + countGeometry(child, depth + 1), 0); break;
    default: throw new SpatialUiError("UNSUPPORTED_TYPE", "이 공간 형식은 지도에서 지원하지 않습니다.");
  }
  throw new SpatialUiError("INVALID_GEOMETRY", "GeometryCollection이 유효하지 않습니다.");
}
export function pageBytes(page: FeaturePage): number { return new TextEncoder().encode(JSON.stringify(page)).byteLength; }
export function parsePage(value: unknown): FeaturePage {
  const input = object(structuredClone(value));
  if (!Array.isArray(input.features) || !Array.isArray(input.row_references) || !Array.isArray(input.warnings) || !Array.isArray(input.feature_errors)) throw new SpatialUiError("INVALID_ARGUMENT", "공간 page가 유효하지 않습니다.");
  const page = input as unknown as FeaturePage;
  if (page.features.length !== page.row_references.length || page.features.length > PAGE_LIMIT.features || pageBytes(page) > PAGE_LIMIT.bytes) throw new SpatialUiError("RESOURCE_LIMIT", "공간 page 상한을 초과했습니다.");
  let coordinates = 0;
  const ids = new Set<string>();
  for (let index = 0; index < page.features.length; index++) {
    const feature = page.features[index];
    if (feature.type !== "Feature" || !feature.id || ids.has(feature.id) || page.row_references[index].feature_id !== feature.id || !safeInteger(page.row_references[index].row_ordinal)) throw new SpatialUiError("INVALID_ARGUMENT", "Feature와 row reference가 일치하지 않습니다.");
    ids.add(feature.id);
    object(feature.properties);
    coordinates += countGeometry(feature.geometry);
    if (feature.geometry && countGeometry(feature.geometry) === 0) feature.geometry = null;
  }
  if (coordinates > PAGE_LIMIT.coordinates) throw new SpatialUiError("RESOURCE_LIMIT", "공간 좌표 상한을 초과했습니다.");
  if (!page.page || !page.limits || page.page.has_more !== (page.page.next_token !== null) || (page.page.has_more && page.page.resume_mode === "none")) throw new SpatialUiError("INVALID_ARGUMENT", "공간 continuation이 유효하지 않습니다.");
  return page;
}
