import { useEffect, useRef, useState, type FormEvent, type MouseEvent } from "react";
import { Map as LibreMap, getWorkerUrl, setWorkerUrl, type GeoJSONSource, type MapMouseEvent } from "maplibre-gl";
import { usePluginConnection, usePluginModal, usePluginQuery, type SlotComponentProps } from "@tabularis/plugin-api";
import workerSource from "virtual:tabularis-map-worker";
import mapLibreCss from "maplibre-gl/dist/maplibre-gl.css?inline";
import spatialCss from "./styles.css?inline";
import { cameraBounds, countGeometry, type Feature, type Geometry, type Viewport } from "./models";
import { buildSpatialColumnsQuery, buildTableQuery, parseSpatialColumns, parseTableRows, type CompatSpatialColumn } from "./compat-query";

let workerUrl: string | null = null;
let previousWorkerUrl: string | null = null;
let workerUsers = 0;

function acquireWorker(): () => void {
  if (!workerUrl) {
    previousWorkerUrl = getWorkerUrl();
    workerUrl = URL.createObjectURL(new Blob([workerSource], { type: "text/javascript" }));
    setWorkerUrl(workerUrl);
  }
  workerUsers++;
  let released = false;
  return () => {
    if (released) return;
    released = true;
    workerUsers--;
    if (workerUsers === 0 && workerUrl) {
      const ownedUrl = workerUrl;
      if (getWorkerUrl() === ownedUrl && previousWorkerUrl !== null) setWorkerUrl(previousWorkerUrl);
      URL.revokeObjectURL(ownedUrl);
      workerUrl = null;
      previousWorkerUrl = null;
    }
  };
}

function tableSchema(contextSchema: string | null | undefined, activeSchema: string | null): string {
  return contextSchema || activeSchema || "public";
}

export function CompatToolbar(props: SlotComponentProps) {
  const active = usePluginConnection();
  const modal = usePluginModal();
  const [error, setError] = useState("");
  const driver = props.context.driver ?? active.driver;
  const connectionId = props.context.connectionId ?? active.connectionId;
  if (typeof driver !== "string" || !["postgres", "postgresql"].includes(driver.toLowerCase()) || !connectionId) return null;
  const activeForSlot = active.connectionId === connectionId;
  const contextTable = typeof props.context.tableName === "string" ? props.context.tableName : "";
  const open = () => {
    setError("");
    if (active.connectionId !== connectionId) {
      setError("이 테이블을 연 연결이 현재 활성 연결과 다릅니다. 해당 연결을 다시 선택한 뒤 여세요.");
      return;
    }
    modal.openModal({
      title: "PostGIS 기본 지도",
      size: "xl",
      content: <><style>{`${mapLibreCss}\n${spatialCss}`}</style>{contextTable
        ? <CompatMapModal schema={tableSchema(props.context.schema, active.schema)} table={contextTable} connectionId={connectionId} />
        : <CompatTablePicker initialSchema={tableSchema(undefined, active.schema)} connectionId={connectionId} />}</>,
    });
  };
  return <div className="spatial-toolbar">
    <button type="button" disabled={!activeForSlot} onClick={open}>Map</button>
    {!activeForSlot ? <span role="status">테이블 연결을 활성화하면 지도를 열 수 있습니다.</span> : null}
    {error ? <span role="alert">{error}</span> : null}
  </div>;
}

function CompatTablePicker({ initialSchema, connectionId }: { initialSchema: string; connectionId: string }) {
  const active = usePluginConnection();
  const [schema, setSchema] = useState(initialSchema);
  const [table, setTable] = useState("");
  const [target, setTarget] = useState<{ schema: string; table: string } | null>(null);
  const [error, setError] = useState("");
  const connectionChanged = active.connectionId !== connectionId;
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (active.connectionId !== connectionId) {
      setError("활성 연결이 바뀌어 지도 조회를 시작할 수 없습니다. 테이블을 열 연결을 다시 선택해 주세요.");
      return;
    }
    const selectedSchema = schema.trim();
    const selectedTable = table.trim();
    if (!selectedSchema || !selectedTable) {
      setError("스키마와 테이블을 입력하세요.");
      return;
    }
    setError("");
    setTarget({ schema: selectedSchema, table: selectedTable });
  };
  if (target) return <CompatMapModal schema={target.schema} table={target.table} connectionId={connectionId} />;
  return <section className="spatial-map-modal" aria-label="지도 테이블 선택">
    <h2>PostGIS 기본 지도</h2>
    <p>지도에 표시할 스키마와 테이블을 입력하세요.</p>
    <form aria-label="지도 테이블" onSubmit={submit}>
      <label>스키마<input aria-label="스키마" value={schema} onChange={event => setSchema(event.target.value)} required /></label>
      <label>테이블<input aria-label="테이블" value={table} onChange={event => setTable(event.target.value)} required /></label>
      <button type="submit" disabled={connectionChanged || !schema.trim() || !table.trim()}>지도 열기</button>
    </form>
    {connectionChanged ? <p role="alert">활성 연결이 바뀌어 지도 조회를 시작할 수 없습니다. 테이블을 열 연결을 다시 선택해 주세요.</p> : error ? <p role="alert">{error}</p> : null}
  </section>;
}

export function CompatMapModal({ schema, table, connectionId }: { schema: string; table: string; connectionId: string }) {
  const query = usePluginQuery();
  const active = usePluginConnection();
  const modal = usePluginModal();
  const [columns, setColumns] = useState<CompatSpatialColumn[]>([]);
  const [selectedName, setSelectedName] = useState("");
  const [columnsConnectionId, setColumnsConnectionId] = useState("");
  const [sourceSridDraft, setSourceSridDraft] = useState("");
  const [appliedSourceSrid, setAppliedSourceSrid] = useState<number | null>(null);
  const [queryRevision, setQueryRevision] = useState(0);
  const [features, setFeatures] = useState<Feature[]>([]);
  const [selectedProperties, setSelectedProperties] = useState<Record<string, unknown> | null>(null);
  const [loading, setLoading] = useState(false);
  const [truncated, setTruncated] = useState(false);
  const [error, setError] = useState("");
  const [mapReady, setMapReady] = useState(false);
  const container = useRef<HTMLDivElement>(null);
  const map = useRef<LibreMap | null>(null);
  const requestId = useRef(0);
  const alive = useRef(false);
  const connectionRef = useRef(active.connectionId);
  const executeQueryRef = useRef(query.executeQuery);
  const featuresRef = useRef(features);
  const mapReadyRef = useRef(mapReady);
  connectionRef.current = active.connectionId;
  executeQueryRef.current = query.executeQuery;
  featuresRef.current = features;
  mapReadyRef.current = mapReady;

  const current = (id: number): boolean => alive.current && requestId.current === id && connectionRef.current === connectionId;
  const guardedQuery = async (sql: string, id: number): Promise<{ rows: unknown[][]; truncated: boolean }> => {
    if (!current(id)) throw new DOMException("The active connection changed.", "AbortError");
    const result = await executeQueryRef.current(sql) as { rows: unknown[][]; truncated?: boolean; pagination?: { has_more?: boolean } };
    if (!current(id)) throw new DOMException("The active connection changed.", "AbortError");
    if (!result || !Array.isArray(result.rows)) throw new Error("PostgreSQL query returned an invalid row set.");
    return { rows: result.rows, truncated: result.truncated === true || result.pagination?.has_more === true };
  };

  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; requestId.current++; };
  }, []);

  useEffect(() => {
    const id = ++requestId.current;
    setColumns([]);
    setSelectedName("");
    setColumnsConnectionId("");
    setFeatures([]);
    setTruncated(false);
    setSourceSridDraft("");
    setAppliedSourceSrid(null);
    setQueryRevision(0);
    setError("");
    if (active.connectionId !== connectionId) {
      setError("이 테이블 연결이 현재 활성 연결과 다릅니다. 테이블을 연 연결을 활성화해 주세요.");
      return () => { if (requestId.current === id) requestId.current++; };
    }
    setLoading(true);
    void guardedQuery(buildSpatialColumnsQuery(schema, table), id).then(({ rows }) => {
      if (!current(id)) return;
      const found = parseSpatialColumns(rows);
      setColumns(found);
      setSelectedName(found[0]?.name ?? "");
      setColumnsConnectionId(connectionId);
      if (found.length === 0) setError("선택한 테이블에서 geometry 또는 geography 열을 찾지 못했습니다.");
    }).catch(failure => {
      if (current(id)) setError(messageOf(failure));
    }).finally(() => {
      if (current(id)) setLoading(false);
    });
    return () => { if (requestId.current === id) requestId.current++; };
  }, [active.connectionId, connectionId, schema, table]);

  const selectedColumn = columns.find(column => column.name === selectedName) ?? null;
  useEffect(() => {
    if (!selectedColumn || columnsConnectionId !== connectionId) return;
    const id = ++requestId.current;
    setFeatures([]);
    setSelectedProperties(null);
    setTruncated(false);
    setError("");
    if (connectionRef.current !== connectionId) {
      setError("이 테이블 연결이 현재 활성 연결과 다릅니다. 테이블을 연 연결을 활성화해 주세요.");
      return () => { if (requestId.current === id) requestId.current++; };
    }
    if ((selectedColumn.srid === null || selectedColumn.srid === 0) && appliedSourceSrid === null) {
      setError("카탈로그 SRID가 0 또는 unknown입니다. 원본 SRID를 입력하고 조회를 누르세요.");
      return () => { if (requestId.current === id) requestId.current++; };
    }
    setLoading(true);
    const sql = buildTableQuery({ schema, table, column: selectedColumn.name, ...(appliedSourceSrid === null ? {} : { sourceSrid: appliedSourceSrid }) });
    void guardedQuery(sql, id).then(({ rows, truncated: hostTruncated }) => {
      if (!current(id)) return;
      const page = parseTableRows(rows, appliedSourceSrid ?? undefined);
      setFeatures(page.features);
      setTruncated(page.truncated || hostTruncated);
    }).catch(failure => {
      if (current(id)) setError(messageOf(failure));
    }).finally(() => {
      if (current(id)) setLoading(false);
    });
    return () => { if (requestId.current === id) requestId.current++; };
  }, [appliedSourceSrid, columnsConnectionId, connectionId, queryRevision, schema, selectedColumn, table]);

  const runTableQuery = () => {
    if (!selectedColumn) return;
    const input = sourceSridDraft.trim();
    const parsed = input ? Number(input) : null;
    if (parsed !== null && (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > 999_999)) {
      setError("SRID는 1부터 999999 사이의 양의 정수여야 합니다.");
      return;
    }
    if ((selectedColumn.srid === null || selectedColumn.srid === 0) && parsed === null) {
      setError("카탈로그 SRID가 0 또는 unknown입니다. 양의 원본 SRID를 입력하세요.");
      return;
    }
    setError("");
    setAppliedSourceSrid(parsed);
    setQueryRevision(value => value + 1);
  };

  useEffect(() => {
    const element = container.current;
    if (!element) return;
    let releaseWorker: (() => void) | null = null;
    let instance: LibreMap | null = null;
    const ready = () => setMapReady(true);
    const failed = (event: { error: { message: string } }) => setError(event.error.message || "MapLibre 지도를 만들지 못했습니다.");
    const clicked = (event: MapMouseEvent) => {
      if (!instance || !mapReadyRef.current) return;
      const hit = instance.queryRenderedFeatures(event.point, { layers: ["compat-polygons", "compat-lines", "compat-points"] })[0];
      const featureId = hit?.properties?.__compatFeatureId;
      const feature = typeof featureId === "string" ? featuresRef.current.find(value => value.id === featureId) : undefined;
      setSelectedProperties(feature?.properties ?? null);
    };
    try {
      releaseWorker = acquireWorker();
      instance = new LibreMap({
        container: element,
        style: { version: 8, sources: {}, layers: [{ id: "compat-background", type: "background", paint: { "background-color": "#e5e7eb" } }] },
        attributionControl: false,
        renderWorldCopies: false,
      });
      map.current = instance;
      instance.on("load", ready);
      instance.on("error", failed);
      instance.on("click", clicked);
    } catch (failure) {
      releaseWorker?.();
      setError(messageOf(failure));
    }
    return () => {
      if (instance) {
        instance.off("load", ready);
        instance.off("error", failed);
        instance.off("click", clicked);
        try { instance.remove(); }
        finally { releaseWorker?.(); }
      } else {
        releaseWorker?.();
      }
      map.current = null;
      setMapReady(false);
    };
  }, []);
  useEffect(() => {
    const instance = map.current;
    if (!instance || !mapReady) return;
    const display = displayFeatures(features);
    const source = instance.getSource("compat-source") as GeoJSONSource | undefined;
    if (source) source.setData({ type: "FeatureCollection", features: display });
    else {
      instance.addSource("compat-source", { type: "geojson", data: { type: "FeatureCollection", features: display } });
      instance.addLayer({ id: "compat-polygons", type: "fill", source: "compat-source", paint: { "fill-color": "#3b82f6", "fill-opacity": 0.35 } });
      instance.addLayer({ id: "compat-lines", type: "line", source: "compat-source", paint: { "line-color": "#1d4ed8", "line-width": 2 } });
      instance.addLayer({ id: "compat-points", type: "circle", source: "compat-source", paint: { "circle-color": "#1d4ed8", "circle-radius": 5 } });
    }
    const extent = bounds(features);
    if (extent) instance.fitBounds(cameraBounds(extent), { padding: 36, duration: 0, maxZoom: 16 });
  }, [features, mapReady]);

  const download = (event: MouseEvent<HTMLButtonElement>) => {
    event.preventDefault();
    const content = JSON.stringify({ type: "FeatureCollection", features }, null, 2);
    const url = URL.createObjectURL(new Blob([content], { type: "application/geo+json" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = `${safeFilename(table)}.geojson`;
    link.click();
    URL.revokeObjectURL(url);
  };

  const close = () => modal.closeModal();
  return <section className="spatial-map-modal" aria-label="기본 PostGIS 지도">
    <div className="spatial-map-actions">
      <label>공간 열<select aria-label="공간 열" value={selectedName} onChange={event => { setSelectedName(event.target.value); setSourceSridDraft(""); setAppliedSourceSrid(null); setQueryRevision(0); }} disabled={!columns.length || loading}>
        {columns.length === 0 ? <option value="">열 없음</option> : null}
        {columns.map(column => <option key={`${column.type}:${column.name}`} value={column.name}>{column.name} ({column.type}, SRID {column.srid ?? "unknown"})</option>)}
      </select></label>
      <label>SRID 0 행의 원본 SRID<input aria-label="SRID 0 행의 원본 SRID" inputMode="numeric" value={sourceSridDraft} onChange={event => setSourceSridDraft(event.target.value)} placeholder="필요할 때만 입력" /></label>
      <button type="button" disabled={!features.length} onClick={download}>GeoJSON 다운로드</button>
      <button type="button" disabled={!selectedColumn || loading} onClick={runTableQuery}>조회</button>
      <button type="button" onClick={close}>닫기</button>
    </div>
    <p>현재 테이블을 새로 조회합니다. 이 기본 지도는 최대 1,000개 feature, 100,000개 좌표, 8 MiB를 표시합니다. 결과 snapshot 지도, 저장 레이어, MCP 지도 제어는 제공하지 않습니다.</p>
    <p role="status" aria-live="polite">{loading ? "PostGIS 테이블을 조회하는 중입니다." : `${features.length}개 feature · ${features.reduce((total, feature) => total + (feature.geometry ? countGeometry(feature.geometry) : 0), 0)}개 좌표${truncated ? " · 상한에 도달해 결과를 잘랐습니다." : ""}`}</p>
    {error ? <p role="alert">{error}</p> : null}
    <div ref={container} className="spatial-map-canvas" aria-label="MapLibre 지도" />
    <section aria-label="선택한 feature 속성"><h3>선택한 feature 속성</h3><pre>{selectedProperties ? JSON.stringify(selectedProperties, null, 2) : "지도에서 feature를 선택하세요."}</pre></section>
  </section>;
}

function displayFeatures(features: Feature[]) {
  const display: { type: "Feature"; id: string; geometry: Exclude<Geometry, { type: "GeometryCollection" }>; properties: Record<string, unknown> }[] = [];
  const visit = (geometry: Geometry, feature: Feature) => {
    if (geometry.type === "GeometryCollection") {
      for (const child of geometry.geometries) visit(child, feature);
    } else {
      display.push({ type: "Feature", id: `${feature.id}:${display.length}`, geometry, properties: { ...feature.properties, __compatFeatureId: feature.id } });
    }
  };
  for (const feature of features) if (feature.geometry) visit(feature.geometry, feature);
  return display;
}

function bounds(features: Feature[]): Viewport | null {
  const coordinates: [number, number][] = [];
  const collect = (geometry: Geometry) => {
    if (geometry.type === "GeometryCollection") { for (const child of geometry.geometries) collect(child); return; }
    const visit = (value: unknown) => {
      if (!Array.isArray(value)) return;
      if (value.length >= 2 && typeof value[0] === "number" && typeof value[1] === "number") coordinates.push([value[0], value[1]]);
      else for (const child of value) visit(child);
    };
    visit(geometry.coordinates);
  };
  for (const feature of features) if (feature.geometry) collect(feature.geometry);
  if (!coordinates.length) return null;
  const longitudes = [...new Set(coordinates.map(([longitude]) => (longitude + 360) % 360))].sort((a, b) => a - b);
  let largestGap = -1;
  let westIndex = 0;
  for (let index = 0; index < longitudes.length; index++) {
    const next = index + 1 < longitudes.length ? longitudes[index + 1] : longitudes[0] + 360;
    const gap = next - longitudes[index];
    if (gap > largestGap) { largestGap = gap; westIndex = (index + 1) % longitudes.length; }
  }
  const west = normalizeLongitude(longitudes[westIndex]);
  const east = normalizeLongitude(longitudes[(westIndex + longitudes.length - 1) % longitudes.length]);
  const latitudes = coordinates.map(([, latitude]) => latitude);
  const south = latitudes.reduce((minimum, value) => Math.min(minimum, value), 90);
  const north = latitudes.reduce((maximum, value) => Math.max(maximum, value), -90);
  return { west, east, south, north, world: 360 - largestGap >= 359.999 };
}

function normalizeLongitude(value: number): number {
  return ((value + 180) % 360 + 360) % 360 - 180;
}

function safeFilename(value: string): string {
  return value.replace(/[\u0000-\u001f\u007f/\\:*?"<>|]/g, "_").slice(0, 100) || "spatial-table";
}

function messageOf(failure: unknown): string {
  return failure instanceof Error ? failure.message : String(failure);
}
