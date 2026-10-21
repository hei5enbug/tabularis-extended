import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MapToolbar } from "../src/MapToolbar";
import { capabilities, harness, resetHarness } from "./fixtures";
import { validateRequest } from "./schema";

vi.mock("@tabularis/plugin-api", async () => {
  const { harness } = await import("./fixtures");
  return { usePluginService: () => harness.service, usePluginToast: () => harness.toast, usePluginTranslation: () => (_key: string, options: { defaultValue: string }) => options.defaultValue };
});
beforeEach(resetHarness);
afterEach(cleanup);
const context = { connectionId: "toolbar-connection", tableName: "roads", schema: "public", driver: "postgres", resultId: "query-snapshot", resultSetIndex: 2, resultGeneration: 7 };
async function expandedToolbar() {
  const view = render(<MapToolbar pluginId="fixture" context={context} />);
  await waitFor(() => expect((screen.getByRole("button", { name: "지도" }) as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(screen.getByRole("button", { name: "지도" }));
  await waitFor(() => expect((screen.getByRole("button", { name: "지도 열기" }) as HTMLButtonElement).disabled).toBe(false));
  return view;
}

describe("MapToolbar common service", () => {
  it("snapshot_set와_duplicate_column_index로_지도를_만든다", async () => {
    // given
    await expandedToolbar();
    fireEvent.change(screen.getByLabelText("공간 열"), { target: { value: "5" } });
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "지도 열기" })); });
    // then
    await waitFor(() => expect(harness.requests.map(request => request.operation)).toEqual(["result.get","map.create","map.layer.add","map.open"]));
    expect(harness.requests[0].input).toMatchObject({ result_id: "query-snapshot", result_set_index: 2, offset: 0, limit: 1 });
    expect(harness.requests[2].input).toMatchObject({ source: { kind: "query_result", result_id: "query-snapshot", result_set_index: 2, column_index: 5, skip_invalid: false } });
    expect(harness.requests.every(request => validateRequest(request).valid)).toBe(true);
    expect(harness.requests.some(request => ["query.execute", "connection.update", "connection.create"].includes(request.operation))).toBe(false);
    expect(harness.openModal).not.toHaveBeenCalled();
  });

  it("all_NULL_공간_열도_metadata로_선택할_수_있다", async () => {
    // given
    await expandedToolbar();
    // when
    fireEvent.change(screen.getByLabelText("공간 열"), { target: { value: "5" } });
    // then
    expect((screen.getByLabelText("공간 열") as HTMLSelectElement).value).toBe("5");
    expect(screen.getByRole("option", { name: "g (열 6, geography, SRID unknown)" })).not.toBeNull();
  });

  it("capability가_없으면_disabled_Map과_외부_plugin_안내를_보인다", async () => {
    // given
    vi.mocked(harness.service.capabilities).mockResolvedValue({ ...capabilities, spatial_v1: false });
    // when
    render(<MapToolbar pluginId="fixture" context={context} />);
    // then
    await waitFor(() => expect((screen.getByRole("button", { name: "지도" }) as HTMLButtonElement).disabled).toBe(true));
    expect(screen.getByRole("status").textContent).toContain("외부 PostgreSQL 플러그인");
    expect(harness.requests).toHaveLength(0);
  });

  it("GUI_instance가_없으면_modal_우회나_map_create를_하지_않는다", async () => {
    // given
    vi.mocked(harness.service.capabilities).mockResolvedValue({ ...capabilities, gui_instance_id: undefined });
    await expandedToolbar();
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "지도 열기" })); });
    // then
    expect(screen.getByRole("alert").textContent).toContain("GUI_UNAVAILABLE");
    expect(harness.requests.map(request => request.operation)).toEqual(["result.get"]);
    expect(harness.openModal).not.toHaveBeenCalled();
  });

  it("mixed_table은_source_srid와_filter를_명시해야_한다", async () => {
    // given
    await expandedToolbar();
    fireEvent.click(screen.getByLabelText("테이블 viewport 지도 — 지도 이동 시 범위를 다시 조회합니다"));
    await waitFor(() => expect(screen.getByRole("option", { name: "g (열 3, geometry, SRID unknown)" })).not.toBeNull());
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "지도 열기" })); });
    // then
    expect(screen.getByRole("alert").textContent).toContain("SRID_REQUIRED");
    expect(harness.requests.some(request => request.operation === "map.create")).toBe(false);
  });

  it("명시_table_source는_원본_filter와_world_viewport를_보낸다", async () => {
    // given
    await expandedToolbar();
    fireEvent.click(screen.getByLabelText("테이블 viewport 지도 — 지도 이동 시 범위를 다시 조회합니다"));
    await waitFor(() => expect((screen.getByRole("button", { name: "지도 열기" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.change(screen.getByLabelText("원본 source SRID"), { target: { value: "4326" } });
    fireEvent.change(screen.getByLabelText("원본 SRID filter"), { target: { value: "0" } });
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "지도 열기" })); });
    // then
    expect(harness.requests.find(request => request.operation === "map.layer.add")?.input).toMatchObject({ source: { kind: "table", source_srid: 4326, srid_filter: 0, viewport: { world: true, south: -90, north: 90 } } });
    expect(harness.requests.some(request => request.operation === "query.execute")).toBe(false);
  });

  it("알려진_srid와_충돌하는_override는_생성하지_않는다", async () => {
    // given
    await expandedToolbar();
    fireEvent.change(screen.getByLabelText("원본 source SRID"), { target: { value: "3857" } });
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "지도 열기" })); });
    // then
    expect(screen.getByRole("alert").textContent).toContain("알려진 원본 SRID");
    expect(harness.requests.some(request => request.operation === "map.create")).toBe(false);
  });

  it("GUI_apply_false는_지도_성공으로_숨기지_않는다", async () => {
    // given
    harness.openGuiApplied = false;
    await expandedToolbar();
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "지도 열기" })); });
    // then
    expect(screen.getByRole("alert").textContent).toContain("APPLY_TIMEOUT");
    expect(harness.openModal).not.toHaveBeenCalled();
  });

  it("기존_지도에_추가할_때는_새_map_create를_하지_않는다", async () => {
    // given
    await expandedToolbar();
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "기존 지도 목록" })); });
    fireEvent.change(screen.getByLabelText("추가할 지도"), { target: { value: "fixture-map" } });
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "지도 열기" })); });
    // then
    expect(harness.requests.map(request => request.operation)).toEqual(["result.get","map.list","map.get","map.layer.add","map.open"]);
    expect(harness.requests[3]).toMatchObject({ connection_id: "toolbar-connection", map_id: "fixture-map", expected_version: 1 });
    expect(harness.map.layers).toHaveLength(2);
    expect(harness.requests.every(request => validateRequest(request).valid)).toBe(true);
  });

  it("snapshot_index_누락은_table로_자동_대체하지_않는다", async () => {
    // given
    const missing = { ...context, resultSetIndex: undefined };
    render(<MapToolbar pluginId="fixture" context={missing} />);
    await waitFor(() => expect((screen.getByRole("button", { name: "지도" }) as HTMLButtonElement).disabled).toBe(false));
    // when
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "지도" })); });
    // then
    expect(screen.getByRole("alert").textContent).toContain("RESULT_EXPIRED");
    expect(harness.requests).toHaveLength(0);
    expect((screen.getByLabelText("결과 snapshot 지도 — SQL을 다시 실행하지 않습니다") as HTMLInputElement).checked).toBe(true);
  });
});
