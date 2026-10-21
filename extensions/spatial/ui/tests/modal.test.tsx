import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ServiceRequest, ServiceResponse } from "@tabularis/plugin-api";
import { HostFixture, openFixture, applyThroughHost } from "./host";
import { FakeMap } from "./fake-map";
import { applyRequest, harness, mapFixture, pageFixture, resetHarness, response } from "./fixtures";
import { validateRequest } from "./schema";
import { WORLD, object } from "../src/models";

vi.mock("maplibre-gl", async () => ({ Map: (await import("./fake-map")).FakeMap, setWorkerUrl: vi.fn() }));
vi.mock("@tabularis/plugin-api", async () => {
  const { harness } = await import("./fixtures");
  return {
    usePluginService: () => harness.service, usePluginAssets: () => harness.assets,
    usePluginModal: () => ({ openModal: harness.openModal, closeModal: harness.closeModal }),
    usePluginTheme: () => ({ colors: null, isDark: true, themeId: "fixture", themeName: "fixture" }),
    usePluginToast: () => harness.toast, usePluginTranslation: () => (_key: string, options: { defaultValue: string }) => options.defaultValue,
  };
});
beforeEach(() => { resetHarness(); FakeMap.reset(); });
afterEach(() => { cleanup(); vi.useRealTimers(); document.head.querySelectorAll("link[rel=stylesheet]").forEach(link => link.remove()); });

const exportMetadata = { artifact_id: "fixture-artifact", bytes: 417, count: 1, checksum: "sha256:fixture", truncated: false };
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(complete => { resolve = complete; });
  return { promise, resolve };
}
function mockExport(reply: (request: ServiceRequest) => ServiceResponse | Promise<ServiceResponse>): void {
  const fallback = vi.mocked(harness.service.call).getMockImplementation()!;
  vi.mocked(harness.service.call).mockImplementation(async request => {
    if (request.operation !== "spatial.export") return fallback(request);
    harness.requests.push(structuredClone(request));
    return reply(request);
  });
}
async function clickExport(): Promise<void> {
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "GeoJSON export" })); });
}
async function unmountBeforeReply(view: ReturnType<typeof render>, reply: () => void): Promise<void> {
  await clickExport();
  view.unmount();
  await act(async () => { reply(); });
}
async function duplicateExportScenario(complete: () => void) {
  const buttons = screen.getAllByRole<HTMLButtonElement>("button", { name: "GeoJSON export" });
  await act(async () => { fireEvent.click(buttons[0]); fireEvent.click(buttons[1]); fireEvent.click(buttons[0]); });
  const disabledWhileSaving = buttons.map(button => button.disabled);
  const exportsWhileSaving = harness.requests.filter(request => request.operation === "spatial.export").length;
  const savesWhileSaving = vi.mocked(harness.service.saveArtifact).mock.calls.length;
  await act(async () => { complete(); });
  return { disabledWhileSaving, exportsWhileSaving, savesWhileSaving, disabledAfterSaving: buttons.map(button => button.disabled) };
}

describe("host modal과 common operations mock 검증", () => {
  it("active_connection없이_global_subscribe가_지도를_연다", async () => {
    // given
    const input = structuredClone(mapFixture);
    // when
    const actual = await openFixture(input);
    // then
    expect(actual.ack).toMatchObject({ gui_applied: true, rendered_version: input.version, state_version: input.version });
    expect(screen.getByRole("dialog", { name: "공간 지도" })).not.toBeNull();
    expect(harness.requests[0].connection_id).toBe("inactive-connection");
    expect(harness.service.subscribeMap).toHaveBeenCalledTimes(1);
    expect(FakeMap.instances).toHaveLength(1);
  });

  it("unmount는_subscribe와_map과_CSS_worker를_정리한다", async () => {
    // given
    const { view } = await openFixture();
    const map = FakeMap.instances[0];
    // when
    view.unmount();
    // then
    expect(harness.unsubscribe).toHaveBeenCalledTimes(1);
    expect(map.removed).toBe(true);
    expect(map.listenerCount()).toBe(0);
    expect(document.head.querySelector("link[rel=stylesheet]")).toBeNull();
  });

  it("명시_close는_resource_해제_후_true_ACK를_반환한다", async () => {
    // given
    await openFixture();
    // when
    const ack = await applyThroughHost(applyRequest("close", mapFixture, 2));
    // then
    expect(ack).toMatchObject({ gui_applied: true, rendered_version: mapFixture.version });
    expect(FakeMap.instances[0].removed).toBe(true);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.head.querySelector("link[rel=stylesheet]")).toBeNull();
  });

  it("열리지_않은_map의_명시_close도_자원없음을_확인한다", async () => {
    // given
    render(<HostFixture />);
    await waitFor(() => expect(harness.handler).not.toBeNull());
    // when
    const actual = await harness.handler!(applyRequest("close"));
    // then
    expect(actual.gui_applied).toBe(true);
    expect(harness.openModal).not.toHaveBeenCalled();
    expect(FakeMap.instances).toHaveLength(0);
  });

  it("저장은_expected_version을_보낸다", async () => {
    // given
    await openFixture();
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "저장" })); });
    // then
    expect(harness.requests.find(request => request.operation === "map.save")).toMatchObject({ map_id: mapFixture.map_id, expected_version: 1, input: {} });
    expect(screen.getAllByRole("status").some(status => status.textContent?.includes("저장했습니다"))).toBe(true);
  });

  it("불러오기는_map_get과_common_map_open만_사용한다", async () => {
    // given
    await openFixture();
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "저장된 지도 목록" })); });
    fireEvent.change(screen.getByLabelText("불러올 지도"), { target: { value: mapFixture.map_id } });
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "불러오기" })); });
    // then
    expect(harness.requests.slice(-2).map(request => request.operation)).toEqual(["map.get","map.open"]);
    expect(harness.requests.find(request => request.operation === "map.open")?.input).toEqual({ gui_instance_id: "fixture-gui" });
  });

  it("visibility는_공개_layer_update를_통해_변경한다", async () => {
    // given
    await openFixture();
    // when
    await act(async () => { fireEvent.click(screen.getByLabelText("표시")); });
    // then
    expect(harness.requests.find(request => request.operation === "map.layer.update")?.input).toMatchObject({ layer_id: "fixture-layer", visible: false });
    expect(harness.map.layers[0].generation).toBe(1);
    expect(harness.requests.filter(request => request.operation === "spatial.query_result")).toHaveLength(1);
  });

  it("점_선_면_style은_범위값을_common_layer_update로_보낸다", async () => {
    // given
    await openFixture();
    fireEvent.change(screen.getByLabelText("점 반경"), { target: { value: "40" } });
    fireEvent.change(screen.getByLabelText("선 굵기"), { target: { value: "20" } });
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "스타일 적용" })); });
    // then
    expect(harness.requests.find(request => request.operation === "map.layer.update")?.input).toMatchObject({ style: { point: { radius: 40 }, line: { width: 20 }, polygon: { color: "#3b82f6" } } });
    expect(harness.requests.every(request => validateRequest(request).valid)).toBe(true);
  });

  it("속성_selection은_returned_detail_cache를_사용하고_XSS를_실행하지_않는다", async () => {
    // given
    await openFixture();
    const selection = screen.getByLabelText("Feature 선택");
    // when
    await act(async () => { fireEvent.change(selection, { target: { value: pageFixture.features[0].id } }); });
    // then
    expect(harness.requests.find(request => request.operation === "spatial.feature")?.input).toEqual({ result_id: "display-cache", feature_id: pageFixture.features[0].id });
    expect(harness.requests.find(request => request.operation === "map.selection.set")?.input).toEqual({ feature_refs: [{ layer_id: "fixture-layer", feature_id: pageFixture.features[0].id }] });
    expect(document.querySelector("img")).toBeNull();
    expect(screen.getByLabelText("원본 속성").textContent).toContain("<img src=x onerror=alert(1)>");
    expect(screen.getByLabelText("원본 속성").textContent).toContain("AQEA-original+/==");
  });

  it("GeoJSON 내보내기는 명시한 연결의 artifact를 저장하고 메타데이터를 유지한다", async () => {
    // given
    await openFixture();
    // when
    await clickExport();
    // then
    expect(harness.requests.find(request => request.operation === "spatial.export")?.input).toMatchObject({ format: "geojson", filename: "spatial-layer.geojson", source: mapFixture.layers[0].source });
    expect(harness.requests.find(request => request.operation === "spatial.export")).toMatchObject({ connection_id: "inactive-connection", deadline_ms: 30000 });
    expect(harness.service.saveArtifact).toHaveBeenCalledExactlyOnceWith("fixture-artifact");
    expect(screen.getByLabelText("원본 속성").textContent).toContain("fixture-artifact");
    expect(screen.getByLabelText("원본 속성").textContent).toContain("sha256:fixture");
    expect(screen.getByText("GeoJSON 파일을 저장했습니다.")).not.toBeNull();
    expect(document.querySelector("a[download]")).toBeNull();
  });

  it("일부 데이터만 내보낸 파일은 저장 완료와 제한 사유를 함께 알린다", async () => {
    // given
    await openFixture();
    mockExport(request => ({ ...response(request, { ...exportMetadata, truncated: true }), limits: { truncated: true, reasons: ["bytes", "rows"] } }));
    // when
    await clickExport();
    // then
    expect(harness.service.saveArtifact).toHaveBeenCalledExactlyOnceWith("fixture-artifact");
    expect(screen.getByText("GeoJSON 파일을 저장했습니다. 일부 데이터만 포함되어 있습니다. (bytes, rows)")).not.toBeNull();
    expect(screen.getByLabelText("원본 속성").textContent).toContain('"truncated": true');
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("저장 대화상자 취소는 오류 없이 안내하고 내보내기를 반복하지 않는다", async () => {
    // given
    await openFixture();
    vi.mocked(harness.service.saveArtifact).mockResolvedValue(false);
    // when
    await clickExport();
    // then
    expect(screen.getByText("파일 저장을 취소했습니다.")).not.toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(harness.toast.showError).not.toHaveBeenCalled();
    expect(harness.requests.filter(request => request.operation === "spatial.export")).toHaveLength(1);
    expect(harness.service.saveArtifact).toHaveBeenCalledTimes(1);
    expect((screen.getByRole("button", { name: "GeoJSON export" }) as HTMLButtonElement).disabled).toBe(false);
    expect(screen.getByLabelText("원본 속성").textContent).toContain("fixture-artifact");
    expect(harness.requests.some(request => request.operation === "result.release" || request.operation === "result.get")).toBe(false);
  });

  it.each([
    ["객체가 아닌 응답", null],
    ["배열 응답", [exportMetadata]],
    ["식별자 누락", {}],
    ["숫자 식별자", { artifact_id: 17 }],
    ["빈 식별자", { artifact_id: "" }],
    ["상한을 넘은 식별자", { artifact_id: "a".repeat(129) }],
    ["NUL을 포함한 식별자", { artifact_id: "artifact\u0000id" }],
    ["제어 문자를 포함한 식별자", { artifact_id: "artifact\nid" }],
    ["DEL을 포함한 식별자", { artifact_id: "artifact\u007fid" }],
    ["C1 제어 문자를 포함한 식별자", { artifact_id: "artifact\u0085id" }],
  ])("저장 정보가 잘못되면 대화상자를 열지 않는다: %s", async (_label, metadata) => {
    // given
    await openFixture();
    mockExport(request => response(request, metadata));
    // when
    await clickExport();
    // then
    expect(harness.service.saveArtifact).not.toHaveBeenCalled();
    expect(screen.getByRole("alert").textContent).toContain("INVALID_ARGUMENT");
    expect(harness.toast.showError).toHaveBeenCalledTimes(1);
    expect(harness.requests.filter(request => request.operation === "spatial.export")).toHaveLength(1);
    expect((screen.getByRole("button", { name: "GeoJSON export" }) as HTMLButtonElement).disabled).toBe(false);
    expect(document.querySelector("a[download]")).toBeNull();
  });

  it("artifact 식별자는 허용된 최대 길이까지 그대로 전달한다", async () => {
    // given
    await openFixture();
    const artifactId = "a".repeat(128);
    mockExport(request => response(request, { ...exportMetadata, artifact_id: artifactId }));
    // when
    await clickExport();
    // then
    expect(harness.service.saveArtifact).toHaveBeenCalledExactlyOnceWith(artifactId);
    expect(screen.getByText("GeoJSON 파일을 저장했습니다.")).not.toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("native 저장 오류는 기존 오류 경로에 전달하고 자동 재시도하지 않는다", async () => {
    // given
    await openFixture();
    vi.mocked(harness.service.saveArtifact).mockRejectedValue(new Error("저장 fixture 오류"));
    // when
    await clickExport();
    // then
    expect(screen.getByRole("alert").textContent).toBe("저장 fixture 오류");
    expect(harness.toast.showError).toHaveBeenCalledExactlyOnceWith("저장 fixture 오류");
    expect(harness.requests.filter(request => request.operation === "spatial.export")).toHaveLength(1);
    expect(harness.service.saveArtifact).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("GeoJSON 파일을 저장했습니다.")).toBeNull();
    expect((screen.getByRole("button", { name: "GeoJSON export" }) as HTMLButtonElement).disabled).toBe(false);
    expect(screen.getByLabelText("원본 속성").textContent).toContain("fixture-artifact");
  });

  it("내보내기 서비스 오류는 저장 대화상자나 재내보내기를 실행하지 않는다", async () => {
    // given
    await openFixture();
    harness.failNext = "CAPABILITY_UNAVAILABLE";
    // when
    await clickExport();
    // then
    expect(harness.service.saveArtifact).not.toHaveBeenCalled();
    expect(screen.getByRole("alert").textContent).toContain("CAPABILITY_UNAVAILABLE");
    expect(harness.toast.showError).toHaveBeenCalledTimes(1);
    expect(harness.requests.filter(request => request.operation === "spatial.export")).toHaveLength(1);
    expect((screen.getByRole("button", { name: "GeoJSON export" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("모달을 닫은 뒤 내보내기 응답이 도착해도 저장 대화상자를 열지 않는다", async () => {
    // given
    const { view } = await openFixture();
    const pending = deferred<ServiceResponse>();
    let request!: ServiceRequest;
    mockExport(value => { request = value; return pending.promise; });
    // when
    await unmountBeforeReply(view, () => pending.resolve(response(request, exportMetadata)));
    // then
    expect(harness.requests.filter(value => value.operation === "spatial.export")).toHaveLength(1);
    expect(harness.service.saveArtifact).not.toHaveBeenCalled();
    expect(harness.toast.showError).not.toHaveBeenCalled();
    expect(view.container.childElementCount).toBe(0);
    expect(screen.queryByText("GeoJSON 파일을 저장했습니다.")).toBeNull();
  });

  it.each([["저장 완료", true], ["저장 취소", false]] as const)("모달을 닫은 뒤 native 응답이 도착해도 알림을 표시하지 않는다: %s", async (_label, saved) => {
    // given
    const { view } = await openFixture();
    const pending = deferred<boolean>();
    vi.mocked(harness.service.saveArtifact).mockReturnValue(pending.promise);
    // when
    await unmountBeforeReply(view, () => pending.resolve(saved));
    // then
    expect(harness.service.saveArtifact).toHaveBeenCalledExactlyOnceWith("fixture-artifact");
    expect(harness.requests.filter(request => request.operation === "spatial.export")).toHaveLength(1);
    expect(harness.toast.showError).not.toHaveBeenCalled();
    expect(view.container.childElementCount).toBe(0);
    expect(screen.queryByText("GeoJSON 파일을 저장했습니다.")).toBeNull();
    expect(screen.queryByText("파일 저장을 취소했습니다.")).toBeNull();
  });

  it("한 모달의 중복 클릭은 내보내기와 저장을 한 번만 실행하고 모든 버튼을 잠근다", async () => {
    // given
    const map = structuredClone(mapFixture);
    map.layers.push({ ...structuredClone(map.layers[0]), layer_id: "second-layer", connection_id: "other-explicit-connection" });
    harness.map = map;
    await openFixture(map);
    const pending = deferred<boolean>();
    vi.mocked(harness.service.saveArtifact).mockReturnValue(pending.promise);
    // when
    const actual = await duplicateExportScenario(() => pending.resolve(true));
    // then
    expect(actual.disabledWhileSaving).toEqual([true, true]);
    expect(actual.disabledAfterSaving).toEqual([false, false]);
    expect(actual.exportsWhileSaving).toBe(1);
    expect(actual.savesWhileSaving).toBe(1);
    expect(harness.requests.filter(request => request.operation === "spatial.export")).toHaveLength(1);
    expect(harness.service.saveArtifact).toHaveBeenCalledExactlyOnceWith("fixture-artifact");
    expect(screen.getByText("GeoJSON 파일을 저장했습니다.")).not.toBeNull();
  });

  it("외부_basemap은_네트워크와_attribution_확인_후_적용한다", async () => {
    // given
    await openFixture();
    fireEvent.change(screen.getByLabelText("HTTPS style URL"), { target: { value: "https://tiles.example/style.json" } });
    fireEvent.change(screen.getByLabelText("제공자 attribution"), { target: { value: "<b>제공자</b>" } });
    fireEvent.click(screen.getByLabelText("외부 네트워크와 제공자 라이선스 표시 확인"));
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Basemap 적용" })); });
    // then
    expect(harness.requests.find(request => request.operation === "map.update")?.input).toEqual({ basemap: { style_url: "https://tiles.example/style.json", attribution: "<b>제공자</b>" } });
    expect(document.querySelector(".spatial-attribution b")).toBeNull();
    expect(screen.getByText("<b>제공자</b>")).not.toBeNull();
    await waitFor(() => expect(FakeMap.instances[0].setStyle).toHaveBeenCalledWith("https://tiles.example/style.json"));
  });

  it("저장된 외부 배경지도는 창에서 동의하기 전까지 요청하지 않는다", async () => {
    // given
    const input = { ...structuredClone(mapFixture), basemap: { style_url: "https://tiles.example/style.json", attribution: "fixture" } };
    // when
    const actual = await openFixture(input);
    // then
    expect(actual.ack).toMatchObject({ gui_applied: false, rendered_version: null });
    expect(FakeMap.instances[0].setStyle).not.toHaveBeenCalledWith(input.basemap.style_url);
    expect(screen.getByText("외부 배경지도는 이 창에서 네트워크 사용을 확인하고 적용해야 표시됩니다.")).not.toBeNull();
  });

  it("외부 요청이 배경지도 URL을 바꾸면 기존 동의를 재사용하지 않는다", async () => {
    // given
    await openFixture();
    fireEvent.change(screen.getByLabelText("HTTPS style URL"), { target: { value: "https://tiles.example/style.json" } });
    fireEvent.change(screen.getByLabelText("제공자 attribution"), { target: { value: "fixture" } });
    fireEvent.click(screen.getByLabelText("외부 네트워크와 제공자 라이선스 표시 확인"));
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Basemap 적용" })); });
    await waitFor(() => expect(FakeMap.instances[0].setStyle).toHaveBeenCalledWith("https://tiles.example/style.json"));
    const changed = { ...structuredClone(harness.map), version: harness.map.version + 1, basemap: { style_url: "https://other.example/style.json", attribution: "other" } };
    FakeMap.instances[0].setStyle.mockClear();
    // when
    const actual = await applyThroughHost(applyRequest("update", changed, 20));
    // then
    expect(actual).toMatchObject({ gui_applied: false, rendered_version: null });
    expect(FakeMap.instances[0].setStyle).toHaveBeenCalledWith(expect.objectContaining({ sources: {} }));
    expect(FakeMap.instances[0].setStyle).not.toHaveBeenCalledWith(changed.basemap.style_url);
  });

  it("version_conflict는_reload후_사용자_재시도를_요청한다", async () => {
    // given
    await openFixture();
    harness.failNext = "VERSION_CONFLICT";
    fireEvent.change(screen.getByLabelText("지도 이름"), { target: { value: "변경" } });
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "이름 적용" })); });
    // then
    expect(harness.requests.slice(-2).map(request => request.operation)).toEqual(["map.update","map.get"]);
    expect(harness.requests.filter(request => request.operation === "map.update")).toHaveLength(1);
    expect(screen.getByRole("alert").textContent).toContain("다시 시도");
  });

  it("user_move는_150ms_debounce하며_query_SQL은_재실행하지_않는다", async () => {
    // given
    await openFixture();
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const map = FakeMap.instances[0];
    map.bounds = { west: 10, east: 20, south: -10, north: 10 };
    map.emit("moveend", { originalEvent: {} });
    map.bounds = { west: 20, east: 30, south: -5, north: 5 };
    map.emit("moveend", { originalEvent: {} });
    // when
    await act(async () => { await vi.advanceTimersByTimeAsync(150); });
    // then
    expect(harness.requests.filter(request => request.operation === "map.viewport.set")).toHaveLength(1);
    expect(harness.requests.find(request => request.operation === "map.viewport.set")?.input).toEqual({ viewport: { west: 20, east: 30, south: -5, north: 5, world: false } });
    expect(harness.requests.filter(request => request.operation === "spatial.query_result")).toHaveLength(1);
    expect(harness.requests.some(request => request.operation === "query.execute")).toBe(false);
  });

  it("table_viewport_변경만_새_범위를_조회한다", async () => {
    // given
    const map = structuredClone(mapFixture);
    map.layers[0].source = { kind: "table", table: { database: null, schema: "public", table: "roads" }, column: "g", viewport: { ...WORLD }, source_srid: 4326, srid_filter: 0 };
    harness.map = map;
    await openFixture(map);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    FakeMap.instances[0].bounds = { west: 10, east: 20, south: -5, north: 5 };
    FakeMap.instances[0].emit("moveend", { originalEvent: {} });
    // when
    await act(async () => { await vi.advanceTimersByTimeAsync(150); });
    // then
    expect(harness.requests.filter(request => request.operation === "spatial.table_query")).toHaveLength(2);
    expect(harness.requests.filter(request => request.operation === "spatial.table_query")[1].input).toMatchObject({ viewport: { west: 10, east: 20, south: -5, north: 5, world: false } });
    expect(harness.map.layers[0].generation).toBe(1);
  });

  it("접근성_label과_host_dialog를_유지한다", async () => {
    // given
    await openFixture();
    // when
    const actual = screen.getByRole("dialog", { name: "공간 지도" });
    // then
    expect(actual.getAttribute("aria-modal")).toBe("true");
    expect(screen.getByLabelText("MapLibre 지도")).not.toBeNull();
    expect(screen.getByRole("button", { name: "지도 닫기" })).not.toBeNull();
    expect(FakeMap.instances[0].canvas.getAttribute("aria-label")).toContain("방향키");
  });

  it("최신_테이블_행은_사용자의_명시_action과_typed_복합_PK로만_조회한다", async () => {
    // given
    const map = structuredClone(mapFixture);
    map.layers[0].source = { kind: "table", table: { database: null, schema: "public", table: "roads" }, column: "g", viewport: { ...WORLD }, source_srid: 4326, srid_filter: 0 };
    harness.map = map;
    harness.page.row_references[0].identity = { columns: ["tenant","id"], values: ["001",7] };
    await openFixture(map);
    await act(async () => { fireEvent.change(screen.getByLabelText("Feature 선택"), { target: { value: pageFixture.features[0].id } }); });
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "최신 행 별도 조회" })); });
    // then
    expect(harness.requests.filter(request => request.operation === "spatial.feature").map(request => request.input)).toEqual([{ result_id: "display-cache", feature_id: pageFixture.features[0].id }, { table: map.layers[0].source.table, identity: { columns: ["tenant","id"], values: ["001",7] } }]);
    expect(screen.getAllByRole("status").some(status => status.textContent?.includes("snapshot을 교체하지 않았습니다"))).toBe(true);
    expect(harness.page.features[0].properties.g).toEqual(pageFixture.features[0].properties.g);
  });

  it("worker_asset_누락은_false_ACK와_명확한_오류를_반환한다", async () => {
    // given
    harness.assets.resolve.mockRejectedValue(new Error("missing asset fixture"));
    render(<HostFixture />);
    await waitFor(() => expect(harness.handler).not.toBeNull());
    // when
    const actual = await applyThroughHost(applyRequest("open"));
    // then
    expect(actual).toMatchObject({ gui_applied: false, rendered_version: null });
    expect(FakeMap.instances).toHaveLength(0);
    expect(screen.getByRole("alert").textContent).toContain("asset");
  });

  it("Escape는_host_modal에_전달하고_map_자원을_해제한다", async () => {
    // given
    await openFixture();
    const input = screen.getByLabelText("지도 이름");
    input.focus();
    // when
    fireEvent.keyDown(input, { key: "Escape" });
    // then
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(harness.closeModal).toHaveBeenCalledTimes(1);
    expect(FakeMap.instances[0].removed).toBe(true);
  });
});

function optionPage(offset: number, count: number, more: boolean) {
  const page = structuredClone(pageFixture);
  page.features = Array.from({ length: count }, (_, index) => ({ ...structuredClone(pageFixture.features[0]), id: `original-snapshot:2:${offset + index}` }));
  page.row_references = page.features.map((feature, index) => ({ feature_id: feature.id, identity: null, snapshot_id: "original-snapshot", row_ordinal: offset + index }));
  page.page = { next_token: more ? "options-next" : null, has_more: more, resume_mode: more ? "materialized" : "none" };
  return page;
}
function mockOptionPages() {
  const pages = [optionPage(0, 500, true), optionPage(500, 500, false)];
  const fallback = vi.mocked(harness.service.call).getMockImplementation()!;
  vi.mocked(harness.service.call).mockImplementation(async request => {
    if (request.operation !== "spatial.query_result") return fallback(request);
    harness.requests.push(structuredClone(request));
    const page = pages[object(request.input).next_token ? 1 : 0];
    return { ...response(request, structuredClone(page), "display-cache"), page: page.page };
  });
}
async function navigateFeatureOptions() {
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Feature 다음 페이지" })); });
  const nextFirst = (screen.getByLabelText("Feature 선택") as HTMLSelectElement).options[1].value;
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Feature 이전 페이지" })); });
  const previousFirst = (screen.getByLabelText("Feature 선택") as HTMLSelectElement).options[1].value;
  await act(async () => { fireEvent.change(screen.getByLabelText("Feature 목록 페이지"), { target: { value: "5" } }); });
  const select = screen.getByLabelText("Feature 선택") as HTMLSelectElement;
  const last = select.options[100].value;
  await act(async () => { fireEvent.change(select, { target: { value: last } }); });
  return { nextFirst, previousFirst, last, optionCount: select.options.length };
}
async function reloadFeatureOptions() {
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "더 보기" })); });
  await waitFor(() => expect(screen.getByText(/1000 features/)).not.toBeNull());
  const retained = (screen.getByLabelText("Feature 목록 페이지") as HTMLInputElement).value;
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "스타일 적용" })); });
  const generationReset = (screen.getByLabelText("Feature 목록 페이지") as HTMLInputElement).value;
  await act(async () => { fireEvent.change(screen.getByLabelText("Feature 목록 페이지"), { target: { value: "3" } }); });
  const changed = structuredClone(harness.map);
  changed.version++;
  changed.layers[0].source = { kind: "query_result", result_id: "original-snapshot", result_set_index: 2, column_index: 5, skip_invalid: false, longitude_mode: "preserve" };
  await applyThroughHost(applyRequest("update", changed, 20));
  return { retained, generationReset, sourceReset: (screen.getByLabelText("Feature 목록 페이지") as HTMLInputElement).value };
}

describe("Feature 선택 목록 페이지", () => {
  it("백 개 목록의 이전과 다음 및 숫자 이동은 모든 원본 식별자에 접근한다", async () => {
    // given
    mockOptionPages();
    await openFixture();
    // when
    const actual = await navigateFeatureOptions();
    // then
    expect(actual).toEqual({ nextFirst: "original-snapshot:2:100", previousFirst: "original-snapshot:2:0", last: "original-snapshot:2:499", optionCount: 101 });
    expect(screen.getByText("Feature 목록 401–500 / 500개")).not.toBeNull();
    expect(harness.requests.find(request => request.operation === "map.selection.set")?.input).toEqual({ feature_refs: [{ layer_id: "fixture-layer", feature_id: "original-snapshot:2:499" }] });
    expect(harness.requests.filter(request => request.operation === "spatial.query_result")).toHaveLength(1);
    expect(FakeMap.instances[0].sources.get("spatial:fixture-layer")?.data).toMatchObject({ features: expect.arrayContaining([{ type: "Feature", id: "original-snapshot:2:499:499", geometry: pageFixture.features[0].geometry, properties: { feature_id: "original-snapshot:2:499", layer_id: "fixture-layer" } }]) });
  });

  it("더 보기는 목록 페이지를 유지하고 generation이나 source 변경은 처음으로 돌아간다", async () => {
    // given
    mockOptionPages();
    await openFixture();
    await act(async () => { fireEvent.change(screen.getByLabelText("Feature 목록 페이지"), { target: { value: "3" } }); });
    // when
    const actual = await reloadFeatureOptions();
    // then
    expect(actual).toEqual({ retained: "3", generationReset: "1", sourceReset: "1" });
    expect(harness.requests.filter(request => request.operation === "spatial.query_result")).toHaveLength(3);
    expect(harness.requests.some(request => request.operation === "query.execute")).toBe(false);
  });
});
