import { useEffect, useRef, useState } from "react";
import { usePluginService, usePluginToast, usePluginTranslation, type ServiceCapabilities, type SlotComponentProps } from "@tabularis/plugin-api";
import { call, errorText, isAborted, mapResponse } from "./service";
import { DEFAULT_STYLE, WORLD, SpatialUiError, object, parseMap, safeInteger, type LayerSource, type MapState, type SpatialColumn } from "./models";

export function MapToolbar({ context, pluginId }: SlotComponentProps) {
  const service = usePluginService();
  const toast = usePluginToast();
  const t = usePluginTranslation(pluginId);
  const [capabilities, setCapabilities] = useState<ServiceCapabilities | null>(null);
  const [expanded, setExpanded] = useState(false);
  const snapshotAvailable = Boolean(context.resultId && safeInteger(context.resultSetIndex));
  const [mode, setMode] = useState<"query_result" | "table">(context.resultId ? "query_result" : "table");
  const [columns, setColumns] = useState<SpatialColumn[]>([]);
  const [columnIndex, setColumnIndex] = useState<number | null>(null);
  const [sourceSrid, setSourceSrid] = useState("");
  const [sridFilter, setSridFilter] = useState("");
  const [longitudeMode, setLongitudeMode] = useState<"preserve" | "shortest">("preserve");
  const [skipInvalid, setSkipInvalid] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [maps, setMaps] = useState<MapState[]>([]);
  const [targetMap, setTargetMap] = useState("");
  const pending = useRef<AbortController | null>(null);
  const connectionId = context.connectionId ?? "";

  useEffect(() => {
    let active = true;
    setCapabilities(null);
    void service.capabilities(connectionId).then(value => { if (active) setCapabilities(value); }).catch(error => { if (active) setError(errorText(error)); });
    return () => { active = false; pending.current?.abort(); };
  }, [service, connectionId]);

  useEffect(() => {
    if (!expanded) return;
    const controller = new AbortController();
    pending.current?.abort();
    pending.current = controller;
    setColumns([]);
    setColumnIndex(null);
    setError("");
    setBusy(true);
    const load = async () => {
      if (mode === "query_result") {
        if (!snapshotAvailable) throw new SpatialUiError("RESULT_EXPIRED", "결과 snapshot과 result set index가 필요합니다.");
        const response = await call(service, "result.get", { result_id: context.resultId!, result_set_index: context.resultSetIndex!, offset: 0, limit: 1 }, { connection_id: connectionId }, controller.signal);
        return object(response.data).columns;
      }
      if (!context.tableName) throw new SpatialUiError("INVALID_ARGUMENT", "테이블 이름이 필요합니다.");
      const response = await call(service, "spatial.columns", { table: { database: null, schema: context.schema ?? null, table: context.tableName } }, { connection_id: connectionId }, controller.signal);
      return object(response.data).columns;
    };
    void load().then(value => {
      if (!Array.isArray(value)) throw new SpatialUiError("INVALID_ARGUMENT", "공간 열 metadata가 없습니다.");
      const spatial = value.filter((column): column is SpatialColumn => Boolean(column && typeof column === "object" && (column as SpatialColumn).name && safeInteger((column as SpatialColumn).column_index) && ["geometry", "geography"].includes((column as SpatialColumn).native_type ?? "")));
      if (!controller.signal.aborted) { setColumns(spatial); setColumnIndex(spatial[0]?.column_index ?? null); }
    }).catch(error => { if (!isAborted(error) && !controller.signal.aborted) setError(errorText(error)); }).finally(() => { if (!controller.signal.aborted) setBusy(false); });
    return () => controller.abort();
  }, [service, expanded, mode, connectionId, context.resultId, context.resultSetIndex, context.resultGeneration, context.tableName, context.schema, snapshotAvailable]);

  const available = capabilities?.spatial_v1 === true && capabilities.operations.includes("map.open");
  const open = async () => {
    const controller = new AbortController();
    pending.current?.abort();
    pending.current = controller;
    setBusy(true);
    setError("");
    try {
      if (!capabilities?.gui_instance_id) throw new SpatialUiError("GUI_UNAVAILABLE", "지도 renderer가 연결된 GUI를 찾지 못했습니다.");
      const column = columns.find(value => value.column_index === columnIndex);
      if (!column) throw new SpatialUiError("INVALID_ARGUMENT", "공간 열을 선택하세요.");
      const parseSrid = (value: string, zero: boolean) => {
        if (!value.trim()) return undefined;
        const srid = Number(value);
        if (!safeInteger(srid) || srid < (zero ? 0 : 1) || srid > 999999) throw new SpatialUiError("INVALID_ARGUMENT", "SRID는 허용 범위의 정수여야 합니다.");
        return srid;
      };
      const source_srid = parseSrid(sourceSrid, false);
      const srid_filter = parseSrid(sridFilter, true);
      if (column.srid != null && source_srid != null && source_srid !== column.srid) throw new SpatialUiError("INVALID_ARGUMENT", "입력 SRID가 알려진 원본 SRID와 다릅니다.");
      if (mode === "table" && column.srid != null && srid_filter != null && srid_filter !== column.srid) throw new SpatialUiError("INVALID_ARGUMENT", "SRID filter가 알려진 원본 SRID와 다릅니다.");
      if (mode === "table" && column.srid_status !== "known" && (source_srid == null || srid_filter == null)) throw new SpatialUiError("SRID_REQUIRED", "Unknown/mixed 테이블은 source SRID와 원본 SRID filter가 모두 필요합니다.");
      const options = { ...(source_srid == null ? {} : { source_srid }), longitude_mode: longitudeMode, skip_invalid: skipInvalid };
      const source: LayerSource = mode === "query_result"
        ? { kind: "query_result", result_id: context.resultId!, result_set_index: context.resultSetIndex!, column_index: column.column_index, ...options }
        : { kind: "table", table: { database: null, schema: context.schema ?? null, table: context.tableName! }, column: column.name, viewport: { ...WORLD }, ...(srid_filter == null ? {} : { srid_filter }), ...options };
      const created = targetMap
        ? mapResponse(await call(service, "map.get", {}, { map_id: targetMap }, controller.signal))
        : mapResponse(await call(service, "map.create", { name: `${column.name} 지도` }, {}, controller.signal));
      const added = mapResponse(await call(service, "map.layer.add", { source, style: DEFAULT_STYLE }, { connection_id: connectionId, map_id: created.map_id, expected_version: created.version }, controller.signal));
      const opened = await call(service, "map.open", { gui_instance_id: capabilities.gui_instance_id }, { map_id: added.map_id }, controller.signal);
      if (object(opened.data).gui_applied !== true) throw new SpatialUiError("APPLY_TIMEOUT", "지도 상태는 저장되었지만 GUI에 적용되지 않았습니다.");
      if (!controller.signal.aborted) setExpanded(false);
    } catch (error) {
      if (!isAborted(error) && !controller.signal.aborted) { setError(errorText(error)); void toast.showError(errorText(error)); }
    } finally { if (!controller.signal.aborted) setBusy(false); }
  };
  const listMaps = async () => {
    const controller = new AbortController();
    pending.current?.abort();
    pending.current = controller;
    try {
      const response = await call(service, "map.list", {}, {}, controller.signal);
      const values = object(response.data).maps;
      if (!Array.isArray(values)) throw new SpatialUiError("INVALID_ARGUMENT", "지도 목록을 읽지 못했습니다.");
      if (!controller.signal.aborted) setMaps(values.map(parseMap));
    } catch (failure) { if (!isAborted(failure) && !controller.signal.aborted) setError(errorText(failure)); }
  };

  return <div className="spatial-toolbar">
    <button type="button" disabled={!available || busy} onClick={() => setExpanded(value => !value)} aria-expanded={expanded}>{t("map.button", { defaultValue: "지도" })}</button>
    {!capabilities || !available ? <span role="status">외부 PostgreSQL 플러그인을 설치하고 연결을 복제한 뒤 공간 capability를 확인하세요. 기존 연결 driver는 유지됩니다.</span> : null}
    {expanded ? <section aria-label="지도 source 선택" className="spatial-source-picker">
      <fieldset><legend>지도 데이터</legend>
        <label><input type="radio" name="spatial-source" checked={mode === "query_result"} disabled={!snapshotAvailable || busy} onChange={() => setMode("query_result")} />결과 snapshot 지도 — SQL을 다시 실행하지 않습니다</label>
        <label><input type="radio" name="spatial-source" checked={mode === "table"} disabled={!context.tableName || busy} onChange={() => setMode("table")} />테이블 viewport 지도 — 지도 이동 시 범위를 다시 조회합니다</label>
      </fieldset>
      <label>공간 열<select value={columnIndex ?? ""} disabled={busy} onChange={event => setColumnIndex(Number(event.target.value))}><option value="" disabled>열 선택</option>{columns.map(column => <option key={column.column_index} value={column.column_index}>{column.name} (열 {column.column_index + 1}, {column.native_type}, SRID {column.srid ?? "unknown"})</option>)}</select></label>
      <label>추가할 지도<select value={targetMap} onChange={event => setTargetMap(event.target.value)}><option value="">새 지도</option>{maps.map(map => <option key={map.map_id} value={map.map_id}>{map.name}</option>)}</select></label><button type="button" disabled={busy} onClick={() => void listMaps()}>기존 지도 목록</button>
      <label>원본 source SRID<input inputMode="numeric" value={sourceSrid} onChange={event => setSourceSrid(event.target.value)} placeholder="SRID 없는 값에만 명시 입력" /></label>
      {mode === "table" ? <label>원본 SRID filter<input inputMode="numeric" value={sridFilter} onChange={event => setSridFilter(event.target.value)} placeholder="unknown 행은 0" /></label> : null}
      <label>날짜변경선<select value={longitudeMode} onChange={event => setLongitudeMode(event.target.value as typeof longitudeMode)}><option value="preserve">원본 경도 순서 보존</option><option value="shortest">명시적으로 최단 경로 표시</option></select></label>
      <label><input type="checkbox" checked={skipInvalid} onChange={event => setSkipInvalid(event.target.checked)} />표시할 수 없는 feature를 건너뛰고 이유 표시</label>
      <p>원본 geometry/geography와 EWKB는 보존됩니다. Geography 테이블은 현재 전세계 범위만 조회할 수 있습니다.</p>
      {!busy && columns.length === 0 ? <p role="status">공간 metadata가 있는 열이 없습니다.</p> : null}
      <button type="button" disabled={busy || columnIndex === null} onClick={() => void open()}>지도 열기</button>
      <button type="button" onClick={() => { pending.current?.abort(); setExpanded(false); setBusy(false); }}>취소</button>
      {error ? <p role="alert">{error}</p> : null}
    </section> : null}
  </div>;
}
