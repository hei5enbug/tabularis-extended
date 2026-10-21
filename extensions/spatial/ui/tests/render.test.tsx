import { beforeEach, describe, expect, it, vi } from "vitest";
import { waitFor } from "@testing-library/react";
import { MapEngine } from "../src/render";
import { LayerDataStore } from "../src/data";
import { FakeMap } from "./fake-map";
import { harness, mapFixture, pageFixture, resetHarness } from "./fixtures";

vi.mock("maplibre-gl", async () => ({ Map: (await import("./fake-map")).FakeMap, setWorkerUrl: vi.fn() }));
beforeEach(() => { resetHarness(); FakeMap.reset(); });
function engine() {
  const dispose = vi.fn(() => FakeMap.lifecycle.push("asset.dispose"));
  const move = vi.fn();
  const select = vi.fn();
  const store = new LayerDataStore(harness.service);
  const engine = new MapEngine(document.createElement("div"), { workerUrl: "blob:fixture-worker", dispose }, store, move, select, "#101010");
  return { engine, dispose, move, select, store, map: FakeMap.instances[0] };
}

describe("MapLibre 이벤트 ACK fence mock 검증", () => {
  it("style_source_tiles와_render_idle_전에는_완료하지_않는다", async () => {
    // given
    FakeMap.autoEvents = false;
    const fixture = engine();
    fixture.map.sourcesLoaded = false;
    let complete = false;
    const pending = fixture.engine.apply(mapFixture, new AbortController().signal).then(() => { complete = true; });
    await waitFor(() => expect(fixture.map.sources.size).toBe(1));
    fixture.map.emit("render");
    fixture.map.emit("idle");
    await Promise.resolve();
    const before = complete;
    fixture.map.sourcesLoaded = true;
    // when
    fixture.map.emit("idle");
    // then
    await expect(pending).resolves.toBeUndefined();
    expect(complete).toBe(true);
    expect(before).toBe(false);
    expect(fixture.map.events.get("render")?.size).toBe(0);
    expect(fixture.map.events.get("idle")?.size).toBe(0);
  });

  it("source에는_최소_ID만_넣고_원본_properties는_보존한다", async () => {
    // given
    const fixture = engine();
    // when
    await fixture.engine.apply(mapFixture, new AbortController().signal);
    // then
    expect(fixture.map.sources.get("spatial:fixture-layer")?.data).toMatchObject({ type: "FeatureCollection", features: [{ properties: { layer_id: "fixture-layer", feature_id: "original-snapshot:2:19" } }] });
    expect(JSON.stringify(fixture.map.sources.get("spatial:fixture-layer")?.data)).not.toContain("AQEA-original");
    expect(fixture.store.get("fixture-layer")?.page.features[0].properties.g).toMatchObject({ dimensions: "XYZM", value: "AQEA-original+/==" });
  });

  it("programmatic_camera는_viewport_저장_피드백을_만들지_않는다", async () => {
    // given
    const fixture = engine();
    // when
    await fixture.engine.apply(mapFixture, new AbortController().signal);
    // then
    expect(fixture.move).not.toHaveBeenCalled();
    expect(fixture.map.fitBounds).toHaveBeenCalledWith([[-180,-85.05112878],[180,85.05112878]], expect.any(Object));
  });

  it("사용자_이동만_dateline_viewport로_전달한다", async () => {
    // given
    const fixture = engine();
    fixture.map.bounds = { west: 179, east: 181, south: -10, north: 10 };
    // when
    fixture.map.emit("moveend", { originalEvent: {} });
    // then
    expect(fixture.move).toHaveBeenCalledWith({ west: 179, east: -179, south: -10, north: 10, world: false });
  });

  it("dispose는_map_remove_이후_assets와_모든_listener를_해제한다", async () => {
    // given
    const fixture = engine();
    await fixture.engine.apply(mapFixture, new AbortController().signal);
    // when
    fixture.engine.dispose();
    // then
    expect(FakeMap.lifecycle).toEqual(["map.remove","asset.dispose"]);
    expect(fixture.map.listenerCount()).toBe(0);
    expect(fixture.dispose).toHaveBeenCalledTimes(1);
  });

  it("render_error는_성공_완료로_반환하지_않는다", async () => {
    // given
    FakeMap.autoEvents = false;
    const fixture = engine();
    const pending = fixture.engine.apply(mapFixture, new AbortController().signal);
    await waitFor(() => expect(fixture.map.sources.size).toBe(1));
    // when
    fixture.map.emit("error", { error: { message: "WebGL failed fixture" } });
    // then
    await expect(pending).rejects.toThrow("WebGL failed fixture");
    expect(fixture.map.events.get("error")?.size).toBe(0);
  });
});

async function applyPresentationChanges(fixture: ReturnType<typeof engine>) {
  const state = structuredClone(mapFixture);
  state.viewport = { west: -10, east: 10, south: -10, north: 10 };
  await fixture.engine.apply(state, new AbortController().signal);
  state.selected_feature_refs = [{ layer_id: state.layers[0].layer_id, feature_id: "original-snapshot:2:19" }];
  await fixture.engine.apply(state, new AbortController().signal);
  state.layers[0].style.line = { width: 7 };
  state.layers[0].visible = false;
  await fixture.engine.apply(state, new AbortController().signal);
}

async function refreshChangedSources(fixture: ReturnType<typeof engine>) {
  harness.page.features[0].id = "original-snapshot:2:20";
  harness.page.row_references[0].feature_id = "original-snapshot:2:20";
  harness.page.row_references[0].row_ordinal = 20;
  harness.page.page = { next_token: null, has_more: false, resume_mode: "none" };
  await fixture.store.load(mapFixture.layers[0], new AbortController().signal, true);
  const source = fixture.map.sources.get("spatial:fixture-layer")!;
  await fixture.engine.refresh(mapFixture, new AbortController().signal);
  const appended = source.data;
  const style = { ...mapFixture, basemap: { style_url: "https://tiles.example/style.json", attribution: "fixture" } };
  await fixture.engine.apply(style, new AbortController().signal, style.basemap.style_url);
  const styled = fixture.map.sources.get("spatial:fixture-layer")!.data;
  await fixture.engine.apply({ ...style, layers: [] }, new AbortController().signal, style.basemap.style_url);
  const removed = fixture.map.sources.size;
  await fixture.engine.apply(style, new AbortController().signal, style.basemap.style_url);
  return { source, appended, styled, removed, restored: fixture.map.sources.get("spatial:fixture-layer")!.data };
}

describe("불변 GeoJSON source 재사용", () => {
  it("viewport와 선택 및 스타일과 표시 변경은 데이터 재전송 없이 적용한다", async () => {
    // given
    const fixture = engine();
    await fixture.engine.apply(mapFixture, new AbortController().signal);
    const source = fixture.map.sources.get("spatial:fixture-layer")!;
    // when
    await applyPresentationChanges(fixture);
    // then
    expect(source.setData).not.toHaveBeenCalled();
    expect(fixture.map.fitBounds).toHaveBeenCalledTimes(2);
    expect(fixture.map.setPaintProperty).toHaveBeenCalledWith("spatial:fixture-layer:line", "line-width", 7);
    expect(fixture.map.setPaintProperty).toHaveBeenCalledWith("spatial:fixture-layer:point", "circle-color", ["case", ["in", ["get", "feature_id"], ["literal", ["original-snapshot:2:19"]]], "#f59e0b", "#3b82f6"]);
    expect(fixture.map.setLayoutProperty).toHaveBeenCalledWith("spatial:fixture-layer:point", "visibility", "none");
    expect(harness.requests.filter(request => request.operation === "spatial.query_result")).toHaveLength(1);
  });

  it("새 데이터와 스타일 교체 및 삭제 뒤 재추가는 source를 다시 게시한다", async () => {
    // given
    harness.page.page = { next_token: "render-next", has_more: true, resume_mode: "materialized" };
    const fixture = engine();
    await fixture.engine.apply(mapFixture, new AbortController().signal);
    // when
    const actual = await refreshChangedSources(fixture);
    // then
    expect(actual.source.setData).not.toHaveBeenCalled();
    expect(actual.source.updateData).toHaveBeenCalledExactlyOnceWith({ add: [{ type: "Feature", id: "original-snapshot:2:20:1", geometry: pageFixture.features[0].geometry, properties: { feature_id: "original-snapshot:2:20", layer_id: "fixture-layer" } }] });
    expect(actual.appended).toMatchObject({ features: [{ properties: { feature_id: "original-snapshot:2:19" } }, { properties: { feature_id: "original-snapshot:2:20" } }] });
    expect(actual.styled).toEqual(actual.appended);
    expect(actual.removed).toBe(0);
    expect(actual.restored).toMatchObject({ features: [{ properties: { feature_id: "original-snapshot:2:20" } }] });
    expect(harness.requests.some(request => request.operation === "query.execute")).toBe(false);
  });
});

describe("외부 배경지도의 네트워크 동의", () => {
  it.each([null, "https://other.example/style.json"])("현재 URL에 동의하지 않으면 외부 스타일을 요청하지 않는다: %s", async approved => {
    // given
    const fixture = engine();
    const state = { ...mapFixture, basemap: { style_url: "https://tiles.example/style.json", attribution: "fixture" } };
    // when
    await fixture.engine.apply(state, new AbortController().signal, approved);
    // then
    expect(fixture.map.setStyle).toHaveBeenCalledWith(expect.objectContaining({ sources: {} }));
    expect(fixture.map.setStyle).not.toHaveBeenCalledWith(state.basemap.style_url);
    expect(fixture.map.sources.size).toBe(1);
  });

  it("현재 URL에 동의하면 외부 스타일을 적용한다", async () => {
    // given
    const fixture = engine();
    const state = { ...mapFixture, basemap: { style_url: "https://tiles.example/style.json", attribution: "fixture" } };
    // when
    await fixture.engine.apply(state, new AbortController().signal, state.basemap.style_url);
    // then
    expect(fixture.map.setStyle).toHaveBeenCalledExactlyOnceWith(state.basemap.style_url);
  });

  it("동의를 철회하면 단색 배경으로 돌아간다", async () => {
    // given
    const fixture = engine();
    const state = { ...mapFixture, basemap: { style_url: "https://tiles.example/style.json", attribution: "fixture" } };
    await fixture.engine.apply(state, new AbortController().signal, state.basemap.style_url);
    fixture.map.setStyle.mockClear();
    // when
    await fixture.engine.apply(state, new AbortController().signal);
    // then
    expect(fixture.map.setStyle).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ sources: {} }));
    expect(fixture.map.sources.size).toBe(1);
  });
});
