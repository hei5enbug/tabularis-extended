import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type CSSProperties } from "react";
import { usePluginAssets, usePluginModal, usePluginService, usePluginTheme, usePluginToast, usePluginTranslation } from "@tabularis/plugin-api";
import { loadAssets } from "./assets";
import { LayerDataStore } from "./data";
import { MapEngine } from "./render";
import { MapSession } from "./session";
import { call, errorText, isAborted, mapResponse, type Operation } from "./service";
import { WORLD, SpatialUiError, object, parseMap, validateBasemap, type Json, type LayerState, type MapState, type Style, type Viewport } from "./models";
import type { ServiceRequest } from "@tabularis/plugin-api";

export function MapModal({ session, pluginId }: { session: MapSession; pluginId: string }) {
  session.markPresented();
  const service = usePluginService();
  const assets = usePluginAssets(pluginId);
  const modal = usePluginModal();
  const theme = usePluginTheme();
  const toast = usePluginToast();
  const t = usePluginTranslation(pluginId);
  const state = useSyncExternalStore(session.subscribe, session.getSnapshot);
  const data = useMemo(() => new LayerDataStore(service), [service]);
  const container = useRef<HTMLDivElement>(null);
  const engine = useRef<MapEngine | null>(null);
  const actions = useRef(new Set<AbortController>());
  const alive = useRef(true);
  const exportBusy = useRef(false);
  const viewportTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const viewportController = useRef<AbortController | null>(null);
  const detailController = useRef<AbortController | null>(null);
  const [ready, setReady] = useState(0);
  const [revision, setRevision] = useState(0);
  const [loading, setLoading] = useState(true);
  const [exporting, setExporting] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [detail, setDetail] = useState<Json | null>(null);
  const [selected, setSelected] = useState<{ layerId: string; featureId: string } | null>(null);
  const [savedMaps, setSavedMaps] = useState<MapState[]>([]);
  const [loadId, setLoadId] = useState("");
  const [name, setName] = useState(state.map.name);
  const [basemapUrl, setBasemapUrl] = useState(state.map.basemap?.style_url ?? "");
  const [attribution, setAttribution] = useState(state.map.basemap?.attribution ?? "");
  const [networkAccepted, setNetworkAccepted] = useState(false);
  const [approvedBasemapUrl, setApprovedBasemapUrl] = useState<string | null>(null);
  const [guiInstanceId, setGuiInstanceId] = useState<string | null>(null);

  const managed = async <T,>(action: (signal: AbortSignal) => Promise<T>): Promise<T> => {
    const controller = new AbortController();
    actions.current.add(controller);
    try { return await action(controller.signal); }
    finally { actions.current.delete(controller); }
  };
  const report = (failure: unknown) => {
    if (!isAborted(failure) && alive.current) { const text = errorText(failure); setError(text); void toast.showError(text); }
  };
  const mutation = async <O extends Operation,>(operation: O, input: ServiceRequest<O>["input"], signal?: AbortSignal, connectionId?: string) => {
    const map = session.getSnapshot().map;
    try {
      const response = await call(service, operation, input, { map_id: map.map_id, expected_version: map.version, ...(connectionId ? { connection_id: connectionId } : {}) }, signal);
      if (alive.current) session.updateLocal(mapResponse(response));
      return response;
    } catch (failure) {
      if (failure instanceof SpatialUiError && failure.code === "VERSION_CONFLICT") {
        const current = mapResponse(await call(service, "map.get", {}, { map_id: map.map_id }, signal));
        if (alive.current) session.updateLocal(current);
        throw new SpatialUiError("VERSION_CONFLICT", "다른 요청이 지도를 변경했습니다. 현재 상태를 불러왔으니 다시 시도해 주세요.");
      }
      throw failure;
    }
  };
  const move = (viewport: Viewport) => {
    if (viewportTimer.current) clearTimeout(viewportTimer.current);
    viewportController.current?.abort();
    engine.current?.cancel();
    const controller = new AbortController();
    viewportController.current = controller;
    viewportTimer.current = setTimeout(() => {
      viewportTimer.current = null;
      void mutation("map.viewport.set", { viewport }, controller.signal).catch(report);
    }, 150);
  };
  const select = (layerId: string, featureId: string) => {
    detailController.current?.abort();
    const controller = new AbortController();
    detailController.current = controller;
    setSelected({ layerId, featureId });
    const layerData = data.get(layerId);
    const original = layerData?.page.features.find(feature => feature.id === featureId)?.properties ?? null;
    setDetail(original);
    void mutation("map.selection.set", { feature_refs: [{ layer_id: layerId, feature_id: featureId }] }, controller.signal).then(async () => {
      const layer = session.getSnapshot().map.layers.find(value => value.layer_id === layerId);
      const resultId = layerData?.featureResultIds.get(featureId);
      if (!layer || !resultId || controller.signal.aborted) return;
      const response = await call(service, "spatial.feature", { result_id: resultId, feature_id: featureId }, { connection_id: layer.connection_id }, controller.signal);
      if (!controller.signal.aborted && alive.current) setDetail(response.data);
    }).catch(report);
  };
  const moveRef = useRef(move);
  const selectRef = useRef(select);
  moveRef.current = move;
  selectRef.current = select;

  useEffect(() => {
    alive.current = true;
    if (session.isClosed()) { session.unmounted(); return () => session.unmounted(); }
    session.markMounted();
    const controller = new AbortController();
    void loadAssets(assets, controller.signal).then(resources => {
      if (controller.signal.aborted || !container.current) { resources.dispose(); return; }
      try {
        engine.current = new MapEngine(container.current, resources, data, viewport => moveRef.current(viewport), (layerId, featureId) => selectRef.current(layerId, featureId), theme.colors?.bg.base ?? "#111827");
        setReady(value => value + 1);
      } catch (failure) { resources.dispose(); throw failure; }
    }).catch(failure => { if (!controller.signal.aborted) { report(failure); session.rendered(session.getSnapshot().serial, false); setLoading(false); } });
    return () => {
      alive.current = false;
      controller.abort();
      if (viewportTimer.current) clearTimeout(viewportTimer.current);
      viewportController.current?.abort();
      detailController.current?.abort();
      for (const action of actions.current) action.abort();
      actions.current.clear();
      engine.current?.dispose();
      engine.current = null;
      data.dispose();
      session.unmounted();
    };
  }, [assets, data, session]);

  useEffect(() => {
    if (!ready || !engine.current) return;
    const controller = new AbortController();
    const serial = state.serial;
    setLoading(true);
    setError("");
    void engine.current.apply(state.map, controller.signal, approvedBasemapUrl).then(() => {
      if (!controller.signal.aborted && alive.current) { session.rendered(serial, !state.map.basemap || state.map.basemap.style_url === approvedBasemapUrl); setRevision(value => value + 1); setLoading(false); }
    }).catch(failure => {
      if (!controller.signal.aborted && alive.current && session.getSnapshot().serial === serial) { report(failure); session.rendered(serial, false); setLoading(false); }
    });
    return () => controller.abort();
  }, [state, ready, session, approvedBasemapUrl]);

  useEffect(() => {
    let active = true;
    const connectionId = state.map.layers[0]?.connection_id;
    if (connectionId) void service.capabilities(connectionId).then(value => { if (active) setGuiInstanceId(value.gui_instance_id ?? null); }).catch(() => { if (active) setGuiInstanceId(null); });
    return () => { active = false; };
  }, [service, state.map.layers[0]?.connection_id]);

  const more = (layer: LayerState) => void managed(async signal => {
    await data.load(layer, signal, true);
    const current = session.getSnapshot().map;
    if (current.layers.find(value => value.layer_id === layer.layer_id)?.generation !== layer.generation) return;
    await engine.current?.refresh(current, signal);
    if (alive.current) setRevision(value => value + 1);
  }).catch(report);
  const latest = () => void managed(async signal => {
    if (!selected) return;
    const layer = state.map.layers.find(value => value.layer_id === selected.layerId);
    const identity = data.get(selected.layerId)?.page.row_references.find(reference => reference.feature_id === selected.featureId)?.identity;
    if (layer?.source.kind !== "table" || !identity) throw new SpatialUiError("UNSUPPORTED_OPERATION", "최신 행 조회에는 테이블과 지원되는 PK가 필요합니다.");
    const response = await call(service, "spatial.feature", { table: layer.source.table, identity }, { connection_id: layer.connection_id }, signal);
    if (alive.current) { setDetail(response.data); setNotice("사용자가 요청한 최신 행입니다. 기존 snapshot을 교체하지 않았습니다."); }
  }).catch(report);
  const list = () => void managed(async signal => {
    const response = await call(service, "map.list", {}, {}, signal);
    const maps = object(response.data).maps;
    if (!Array.isArray(maps)) throw new SpatialUiError("INVALID_ARGUMENT", "지도 목록을 읽지 못했습니다.");
    if (alive.current) setSavedMaps(maps.map(parseMap));
  }).catch(report);
  const load = () => void managed(async signal => {
    if (!guiInstanceId) throw new SpatialUiError("GUI_UNAVAILABLE", "지도를 열 GUI가 없습니다.");
    const map = mapResponse(await call(service, "map.get", {}, { map_id: loadId }, signal));
    await call(service, "map.open", { gui_instance_id: guiInstanceId }, { map_id: map.map_id }, signal);
  }).catch(report);
  const basemap = () => void managed(async signal => {
    const next = basemapUrl.trim() ? { style_url: basemapUrl.trim(), attribution } : null;
    if (next) { validateBasemap(next); if (!networkAccepted) throw new SpatialUiError("INVALID_ARGUMENT", "외부 tile/font/sprite 네트워크와 제공자 라이선스 표시를 확인하세요."); }
    await mutation("map.update", { basemap: next }, signal);
    if (alive.current && !signal.aborted) setApprovedBasemapUrl(next?.style_url ?? null);
  }).catch(report);
  const exportLayer = (layer: LayerState) => {
    if (exportBusy.current || !alive.current) return;
    exportBusy.current = true;
    setExporting(true);
    void managed(async signal => {
      try {
        const response = await call(service, "spatial.export", { source: layer.source, format: "geojson", filename: "spatial-layer.geojson" }, { connection_id: layer.connection_id }, signal);
        if (signal.aborted || !alive.current) return;
        setDetail(response.data);
        const metadata = object(response.data);
        const artifactId = metadata.artifact_id;
        if (typeof artifactId !== "string" || artifactId.length === 0 || artifactId.length > 128 || /[\u0000-\u001f\u007f-\u009f]/.test(artifactId)) throw new SpatialUiError("INVALID_ARGUMENT", "내보낸 파일의 저장 정보를 확인할 수 없습니다.");
        const saved = await service.saveArtifact(artifactId);
        if (signal.aborted || !alive.current) return;
        const truncated = response.limits.truncated || metadata.truncated === true;
        setNotice(saved ? `GeoJSON 파일을 저장했습니다.${truncated ? ` 일부 데이터만 포함되어 있습니다.${response.limits.reasons.length ? ` (${response.limits.reasons.join(", ")})` : ""}` : ""}` : "파일 저장을 취소했습니다.");
      } finally {
        exportBusy.current = false;
        if (alive.current) setExporting(false);
      }
    }).catch(report);
  };
  const close = () => void managed(async signal => {
    if (guiInstanceId) await call(service, "map.close", { gui_instance_id: guiInstanceId }, { map_id: state.map.map_id }, signal);
    else modal.closeModal();
  }).catch(report);
  const style: CSSProperties = { "--spatial-bg": theme.colors?.bg.base ?? "#111827", "--spatial-panel": theme.colors?.bg.elevated ?? "#1f2937", "--spatial-text": theme.colors?.text.primary ?? "#f9fafb", "--spatial-border": theme.colors?.border.default ?? "#4b5563", "--spatial-accent": theme.colors?.accent.primary ?? "#3b82f6" } as CSSProperties;
  void revision;

  return <section className="spatial-map-modal" style={style} aria-label={state.map.name}>
    <div className="spatial-map-actions">
      <label>지도 이름<input value={name} onChange={event => setName(event.target.value)} /></label>
      <button type="button" onClick={() => void managed(signal => mutation("map.update", { name }, signal)).catch(report)}>이름 적용</button>
      <button type="button" onClick={() => void managed(async signal => { await mutation("map.save", {}, signal); if (alive.current) setNotice("지도를 저장했습니다."); }).catch(report)}>{t("map.save", { defaultValue: "저장" })}</button>
      <button type="button" onClick={list}>저장된 지도 목록</button>
      <label>불러올 지도<select value={loadId} onChange={event => setLoadId(event.target.value)}><option value="">지도 선택</option>{savedMaps.map(map => <option key={map.map_id} value={map.map_id}>{map.name}</option>)}</select></label>
      <button type="button" disabled={!loadId} onClick={load}>불러오기</button>
      <button type="button" onClick={() => void managed(signal => mutation("map.viewport.set", { viewport: WORLD }, signal)).catch(report)}>전세계 범위</button>
      <button type="button" onClick={close}>지도 닫기</button>
    </div>
    {error ? <p role="alert">{error}</p> : null}
    {notice ? <p role="status">{notice}</p> : null}
    <p role="status" aria-live="polite">{loading ? "지도와 공간 source를 불러오는 중입니다." : "지도 렌더 작업이 끝났습니다."}</p>
    <div className="spatial-map-body">
      <div ref={container} className="spatial-map-canvas" aria-label="MapLibre 지도" />
      <aside aria-label="레이어와 속성" className="spatial-map-sidebar">
        {state.map.layers.map(layer => <LayerPanel key={layer.layer_id} layer={layer} data={data} exporting={exporting} onMore={() => more(layer)} onExport={() => exportLayer(layer)} onSelect={featureId => select(layer.layer_id, featureId)} onUpdate={(visible, style) => void managed(signal => mutation("map.layer.update", { layer_id: layer.layer_id, ...(visible == null ? {} : { visible }), ...(style ? { style } : {}) }, signal)).catch(report)} onRemove={() => void managed(signal => mutation("map.layer.remove", { layer_id: layer.layer_id }, signal)).catch(report)} />)}
        <details><summary>Basemap</summary><p>기본 배경은 네트워크를 사용하지 않습니다. 외부 style은 tile, font, sprite 요청을 보낼 수 있습니다.</p>
          {state.map.basemap && state.map.basemap.style_url !== approvedBasemapUrl ? <p role="status">외부 배경지도는 이 창에서 네트워크 사용을 확인하고 적용해야 표시됩니다.</p> : null}
          <label>HTTPS style URL<input value={basemapUrl} onChange={event => { setBasemapUrl(event.target.value); setNetworkAccepted(false); }} placeholder="비워 두면 단색 배경" /></label>
          <label>제공자 attribution<input value={attribution} onChange={event => setAttribution(event.target.value)} /></label>
          <label><input type="checkbox" checked={networkAccepted} onChange={event => { setNetworkAccepted(event.target.checked); if (!event.target.checked) setApprovedBasemapUrl(null); }} />외부 네트워크와 제공자 라이선스 표시 확인</label>
          <button type="button" onClick={basemap}>Basemap 적용</button>
        </details>
        {state.map.basemap ? <p className="spatial-attribution">{state.map.basemap.attribution}</p> : null}
        <section aria-label="원본 속성"><h3>원본 속성</h3><p>EWKB, Z/M, SRID는 표시 좌표와 별도로 보존됩니다.</p><pre>{detail === null ? "Feature를 선택하세요." : JSON.stringify(detail, null, 2)}</pre><button type="button" disabled={!selected} onClick={latest}>최신 행 별도 조회</button></section>
      </aside>
    </div>
  </section>;
}

function LayerPanel({ layer, data, exporting, onMore, onExport, onSelect, onUpdate, onRemove }: { layer: LayerState; data: LayerDataStore; exporting: boolean; onMore(): void; onExport(): void; onSelect(id: string): void; onUpdate(visible?: boolean, style?: Style): void; onRemove(): void }) {
  const values = data.get(layer.layer_id);
  const [featurePage, setFeaturePage] = useState(0);
  const featureSourceKey = JSON.stringify([layer.connection_id, layer.source]);
  useEffect(() => { setFeaturePage(0); }, [featureSourceKey, layer.generation]);
  const featureCount = values?.page.features.length ?? 0;
  const pageCount = Math.max(1, Math.ceil(featureCount / 100));
  const currentPage = Math.min(featurePage, pageCount - 1);
  const featureOffset = currentPage * 100;
  const listedFeatures = values?.page.features.slice(featureOffset, featureOffset + 100) ?? [];
  const moveFeaturePage = (page: number) => { if (Number.isSafeInteger(page)) setFeaturePage(Math.max(0, Math.min(page, pageCount - 1))); };
  const [color, setColor] = useState(layer.style.point?.color?.slice(0, 7) ?? "#3b82f6");
  const [opacity, setOpacity] = useState(layer.style.polygon?.opacity ?? 0.35);
  const [radius, setRadius] = useState(layer.style.point?.radius ?? 5);
  const [width, setWidth] = useState(layer.style.line?.width ?? 2);
  const styleKey = JSON.stringify(layer.style);
  useEffect(() => { setColor(layer.style.point?.color?.slice(0, 7) ?? "#3b82f6"); setOpacity(layer.style.polygon?.opacity ?? 0.35); setRadius(layer.style.point?.radius ?? 5); setWidth(layer.style.line?.width ?? 2); }, [styleKey]);
  return <section className="spatial-layer" aria-label={`레이어 ${layer.layer_id}`}>
    <h3>{layer.source.kind === "query_result" ? "결과 snapshot" : `${layer.source.table.table}.${layer.source.column}`}</h3>
    <p>{values?.page.features.length ?? 0} features · {values?.coordinates ?? 0} coordinates · generation {layer.generation}</p>
    <label><input type="checkbox" checked={layer.visible} onChange={event => onUpdate(event.target.checked)} />표시</label>
    <label>색상<input type="color" value={color} onChange={event => setColor(event.target.value)} /></label>
    <label>불투명도<input type="number" min="0" max="1" step="0.05" value={opacity} onChange={event => setOpacity(Number(event.target.value))} /></label>
    <label>점 반경<input type="number" min="1" max="40" value={radius} onChange={event => setRadius(Number(event.target.value))} /></label>
    <label>선 굵기<input type="number" min="1" max="20" value={width} onChange={event => setWidth(Number(event.target.value))} /></label>
    <button type="button" onClick={() => onUpdate(undefined, { point: { color, opacity, radius }, line: { color, opacity, width }, polygon: { color, opacity } })}>스타일 적용</button>
    <button type="button" onClick={onRemove}>레이어 삭제</button>
    <button type="button" disabled={!values?.page.page.has_more} onClick={onMore}>더 보기</button>
    <button type="button" disabled={exporting} onClick={onExport}>GeoJSON export</button>
    {values?.page.limits.truncated ? <p role="status">truncated: {values.page.limits.reasons.join(", ")}</p> : null}
    {values?.page.warnings.map(warning => <p key={warning}>{warning}</p>)}
    {values?.page.feature_errors.map((failure, index) => <p key={index} role="status">행 {failure.index}: {failure.code} — {failure.message}</p>)}
    <p aria-live="polite">Feature 목록 {featureCount ? featureOffset + 1 : 0}–{featureOffset + listedFeatures.length} / {featureCount}개</p>
    <button type="button" disabled={currentPage === 0} onClick={() => moveFeaturePage(currentPage - 1)}>Feature 이전 페이지</button>
    <label>Feature 목록 페이지<input type="number" min="1" max={pageCount} disabled={!featureCount} value={currentPage + 1} onChange={event => moveFeaturePage(event.target.valueAsNumber - 1)} /></label>
    <span> / {pageCount} 페이지</span>
    <button type="button" disabled={currentPage + 1 >= pageCount} onClick={() => moveFeaturePage(currentPage + 1)}>Feature 다음 페이지</button>
    <label>Feature 선택<select value="" onChange={event => { if (event.target.value) onSelect(event.target.value); }}><option value="">Feature 선택</option>{listedFeatures.map(feature => <option key={feature.id} value={feature.id}>{feature.id}{feature.geometry === null ? " (NULL/EMPTY)" : ""}</option>)}</select></label>
  </section>;
}
