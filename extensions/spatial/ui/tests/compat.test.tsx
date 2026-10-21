import { useState, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import SpatialPlugin from "../src/index";
import { CompatMapModal, CompatToolbar } from "../src/compat";
import type { SlotComponentProps } from "@tabularis/plugin-api";
import { FakeMap } from "./fake-map";

const compatHarness = vi.hoisted(() => ({
  connection: { connectionId: "roads-connection", driver: "postgres", schema: "public" },
  workerUrl: "",
  executeQuery: vi.fn(async (sql: string): Promise<{ columns: string[]; rows: unknown[][] }> => sql.includes("geometry_columns")
    ? { columns: ["column_name", "spatial_type", "srid", "coord_dimension"], rows: [["geom", "geometry", 4326, 2]] }
    : { columns: ["__geojson", "__source_srid", "__properties"], rows: [[JSON.stringify({ type: "Point", coordinates: [1, 2] }), 4326, { label: "A" }]] }),
  modal: { openModal: vi.fn(), closeModal: vi.fn() },
}));

vi.mock("@tabularis/plugin-api", () => ({
  usePluginConnection: () => compatHarness.connection,
  usePluginModal: () => compatHarness.modal,
  usePluginQuery: () => ({ executeQuery: compatHarness.executeQuery, loading: false, error: null }),
  usePluginService: undefined,
  usePluginAssets: undefined,
  usePluginTheme: undefined,
  usePluginToast: undefined,
  usePluginTranslation: undefined,
}));

vi.mock("maplibre-gl", async importOriginal => {
  const actual = await importOriginal<typeof import("maplibre-gl")>();
  const { FakeMap: TestMap } = await import("./fake-map");
  return {
    ...actual,
    Map: TestMap,
    getWorkerUrl: () => compatHarness.workerUrl,
    setWorkerUrl: (value: string) => { compatHarness.workerUrl = value; },
  };
});

beforeEach(() => {
  compatHarness.connection = { connectionId: "roads-connection", driver: "postgres", schema: "public" };
  compatHarness.workerUrl = "";
  compatHarness.executeQuery.mockReset().mockImplementation(async (sql: string): Promise<{ columns: string[]; rows: unknown[][] }> => sql.includes("geometry_columns")
    ? { columns: ["column_name", "spatial_type", "srid", "coord_dimension"], rows: [["geom", "geometry", 4326, 2]] }
    : { columns: ["__geojson", "__source_srid", "__properties"], rows: [[JSON.stringify({ type: "Point", coordinates: [1, 2] }), 4326, { label: "A" }]] });
  compatHarness.modal.openModal.mockReset();
  compatHarness.modal.closeModal.mockReset();
  FakeMap.reset();
  Object.defineProperty(URL, "createObjectURL", { configurable: true, value: vi.fn(() => "blob:compat-worker") });
  Object.defineProperty(URL, "revokeObjectURL", { configurable: true, value: vi.fn() });
});

afterEach(() => {
  cleanup();
  document.head.querySelectorAll("link[rel=stylesheet]").forEach(link => link.remove());
  Reflect.deleteProperty(URL, "createObjectURL");
  Reflect.deleteProperty(URL, "revokeObjectURL");
});

const slotContext = { connectionId: "roads-connection", driver: "postgres", tableName: "roads", schema: "public" };
const slotProps: SlotComponentProps = { context: slotContext, pluginId: "spatial" };

function ModalHost({ context = slotContext }: { context?: SlotComponentProps["context"] }) {
  const [content, setContent] = useState<ReactNode>(null);
  compatHarness.modal.openModal.mockImplementation(options => setContent(options.content));
  return <><SpatialPlugin context={context} pluginId="spatial" />{content ? <div key={compatHarness.connection.connectionId} role="dialog" aria-label="기본 지도 모달">{content}</div> : null}</>;
}

describe("기본 PostgreSQL 지도 호환 UI", () => {
  it("기존 plugin API에서는 지원하는 테이블에 기본 지도 버튼을 표시합니다.", () => {
    // given
    const view = render(<ModalHost />);
    // when
    fireEvent.click(screen.getByRole("button", { name: "Map" }));
    // then
    expect(screen.getByRole("dialog", { name: "기본 지도 모달" })).not.toBeNull();
    expect(screen.getByText(/현재 테이블을 새로 조회합니다/)).not.toBeNull();
    expect(compatHarness.modal.openModal).toHaveBeenCalledWith(expect.objectContaining({ title: "PostGIS 기본 지도", size: "xl" }));
    view.unmount();
  });

  it("PostgreSQL이 아니면 버튼을 숨기고 테이블 정보가 없어도 표시합니다.", () => {
    // given
    const view = render(<SpatialPlugin context={{ ...slotContext, driver: "sqlite" }} pluginId="spatial" />);
    // when
    const button = screen.queryByRole("button", { name: "Map" });
    // then
    expect(button).toBeNull();
    view.rerender(<SpatialPlugin context={{ ...slotContext, tableName: null }} pluginId="spatial" />);
    expect(screen.queryByRole("button", { name: "Map" })).not.toBeNull();
  });

  it("빈 슬롯에서 테이블을 입력하면 기존 지도 모달이 해당 테이블을 조회합니다.", async () => {
    // given
    render(<ModalHost context={{}} />);
    fireEvent.click(screen.getByRole("button", { name: "Map" }));
    fireEvent.change(screen.getByLabelText("스키마"), { target: { value: " public " } });
    fireEvent.change(screen.getByLabelText("테이블"), { target: { value: " roads " } });
    // when
    fireEvent.submit(screen.getByRole("form", { name: "지도 테이블" }));
    // then
    await waitFor(() => expect(compatHarness.executeQuery).toHaveBeenCalledTimes(2));
    expect(screen.getByText(/현재 테이블을 새로 조회합니다/)).not.toBeNull();
    const queries = compatHarness.executeQuery.mock.calls.map(([sql]) => sql);
    expect(queries[0]).toContain("f_table_schema = 'public'");
    expect(queries[0]).toContain("f_table_name = 'roads'");
    expect(queries[1]).toContain('FROM "public"."roads" AS "t"');
  });

  it("테이블 입력 중 활성 연결이 바뀌면 제출을 거부합니다.", () => {
    // given
    const view = render(<ModalHost context={{}} />);
    fireEvent.click(screen.getByRole("button", { name: "Map" }));
    fireEvent.change(screen.getByLabelText("테이블"), { target: { value: "roads" } });
    compatHarness.connection = { connectionId: "another-connection", driver: "postgres", schema: "public" };
    view.rerender(<ModalHost context={{}} />);
    // when
    fireEvent.submit(screen.getByRole("form", { name: "지도 테이블" }));
    // then
    expect((screen.getByRole("button", { name: "지도 열기" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole("alert").textContent).toContain("활성 연결이 바뀌어 지도 조회를 시작할 수 없습니다");
    expect(compatHarness.executeQuery).not.toHaveBeenCalled();
  });

  it("활성 연결이 테이블 연결과 다르면 모달이나 쿼리를 열지 않습니다.", () => {
    // given
    compatHarness.connection = { connectionId: "another-connection", driver: "postgres", schema: "public" };
    render(<CompatToolbar {...slotProps} />);
    // when
    const button = screen.getByRole("button", { name: "Map" }) as HTMLButtonElement;
    // then
    expect(button.disabled).toBe(true);
    expect(screen.getByRole("status").textContent).toContain("연결을 활성화");
    expect(compatHarness.executeQuery).not.toHaveBeenCalled();
    expect(compatHarness.modal.openModal).not.toHaveBeenCalled();
  });

  it("연결이 바뀐 뒤 늦게 도착한 카탈로그 응답을 버립니다.", async () => {
    // given
    let resolve!: (value: { columns: string[]; rows: unknown[][] }) => void;
    compatHarness.executeQuery.mockReturnValueOnce(new Promise<{ columns: string[]; rows: unknown[][] }>(complete => { resolve = complete; }));
    const view = render(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    await waitFor(() => expect(compatHarness.executeQuery).toHaveBeenCalledTimes(1));
    // when
    compatHarness.connection = { connectionId: "another-connection", driver: "postgres", schema: "public" };
    view.rerender(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    await act(async () => { resolve({ columns: ["column_name", "spatial_type", "srid", "coord_dimension"], rows: [["geom", "geometry", 4326, 2]] }); });
    // then
    expect(screen.queryByRole("option", { name: /geom/ })).toBeNull();
    expect(compatHarness.executeQuery).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("alert").textContent).toContain("현재 활성 연결과 다릅니다");
    view.unmount();
  });

  it("SRID 0 행에 양의 SRID를 입력하면 조건부 설정 후 새로 표시합니다.", async () => {
    // given
    compatHarness.executeQuery.mockImplementation(async (sql: string): Promise<{ columns: string[]; rows: unknown[][] }> => {
      if (sql.includes("geometry_columns")) return { columns: [], rows: [["geom", "geometry", 0, 2]] };
      if (sql.includes("ST_SetSRID")) return { columns: [], rows: [[JSON.stringify({ type: "Point", coordinates: [1, 2] }), 0, { label: "zero" }]] };
      return { columns: [], rows: [[null, 0, { label: "zero" }]] };
    });
    render(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("카탈로그 SRID가 0 또는 unknown"));
    // when
    fireEvent.change(screen.getByLabelText("SRID 0 행의 원본 SRID"), { target: { value: "4326" } });
    fireEvent.click(screen.getByRole("button", { name: "조회" }));
    // then
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("1개 feature"));
    const queries = compatHarness.executeQuery.mock.calls.map(([sql]) => sql);
    expect(queries).toHaveLength(2);
    expect(queries[1]).toContain("WHEN ST_SRID(\"t\".\"geom\"::geometry) = 0 THEN ST_SetSRID");
  });
});
