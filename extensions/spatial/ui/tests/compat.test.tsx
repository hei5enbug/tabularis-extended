import { useState, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import SpatialPlugin from "../src/index";
import { CompatMapModal, CompatToolbar } from "../src/compat";
import type { SlotComponentProps } from "@tabularis/plugin-api";
import { FakeMap } from "./fake-map";

type QueryResult = { columns: string[]; rows: unknown[][] };

const compatHarness = vi.hoisted(() => ({
  connection: { connectionId: "roads-connection", driver: "postgres", schema: "public" },
  extensionSchema: "public",
  workerUrl: "",
  executeQuery: vi.fn<(sql: string) => Promise<QueryResult>>(),
  modal: { openModal: vi.fn(), closeModal: vi.fn() },
}));

async function queryResult(sql: string): Promise<QueryResult> {
  if (sql.includes("pg_catalog.pg_extension")) return { columns: ["extension_schema"], rows: [[compatHarness.extensionSchema]] };
  if (sql.includes("geometry_columns")) return { columns: ["column_name", "spatial_type", "srid", "coord_dimension"], rows: [["geom", "geometry", 4326, 2]] };
  return { columns: ["__geojson", "__source_srid", "__properties"], rows: [[JSON.stringify({ type: "Point", coordinates: [1, 2] }), 4326, { label: "A" }]] };
}

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
  compatHarness.extensionSchema = "public";
  compatHarness.workerUrl = "";
  compatHarness.executeQuery.mockReset().mockImplementation(queryResult);
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
    render(<ModalHost />);
    // when
    fireEvent.click(screen.getByRole("button", { name: "Map" }));
    // then
    expect(screen.getByRole("dialog", { name: "기본 지도 모달" })).not.toBeNull();
    expect(screen.getByText(/현재 테이블을 새로 조회합니다/)).not.toBeNull();
    expect(compatHarness.modal.openModal).toHaveBeenCalledWith(expect.objectContaining({ title: "PostGIS 기본 지도", size: "xl" }));
  });

  it("PostgreSQL이 아니면 기본 지도 버튼을 숨깁니다.", () => {
    // given
    const context = { ...slotContext, driver: "sqlite" };
    // when
    render(<SpatialPlugin context={context} pluginId="spatial" />);
    // then
    expect(screen.queryByRole("button", { name: "Map" })).toBeNull();
  });

  it("테이블 정보가 없어도 PostgreSQL 연결에는 기본 지도 버튼을 표시합니다.", () => {
    // given
    const context = { ...slotContext, tableName: null };
    // when
    render(<SpatialPlugin context={context} pluginId="spatial" />);
    // then
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
    await waitFor(() => expect(compatHarness.executeQuery).toHaveBeenCalledTimes(3));
    expect(screen.getByText(/현재 테이블을 새로 조회합니다/)).not.toBeNull();
    expect(compatHarness.executeQuery.mock.calls[0][0]).toContain("pg_catalog.pg_extension");
    expect(compatHarness.executeQuery.mock.calls[1][0]).toContain("f_table_schema = 'public'");
    expect(compatHarness.executeQuery.mock.calls[1][0]).toContain("f_table_name = 'roads'");
    expect(compatHarness.executeQuery.mock.calls[2][0]).toContain('FROM "public"."roads" AS "t"');
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

  it("연결이 바뀐 뒤 늦게 도착한 확장 스키마 응답으로 카탈로그를 조회하지 않습니다.", async () => {
    // given
    let resolve!: (value: { columns: string[]; rows: unknown[][] }) => void;
    compatHarness.executeQuery.mockReturnValueOnce(new Promise<{ columns: string[]; rows: unknown[][] }>(complete => { resolve = complete; }));
    const view = render(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    await waitFor(() => expect(compatHarness.executeQuery).toHaveBeenCalledTimes(1));
    compatHarness.connection = { connectionId: "another-connection", driver: "postgres", schema: "public" };
    view.rerender(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    // when
    await act(async () => { resolve({ columns: ["extension_schema"], rows: [["public"]] }); });
    // then
    expect(screen.queryByRole("option", { name: /geom/ })).toBeNull();
    expect(compatHarness.executeQuery).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("alert").textContent).toContain("현재 활성 연결과 다릅니다");
    expect(screen.getByRole("status").textContent).not.toContain("조회하는 중");
  });

  it.each([
    { extensionSchema: "public", quoted: '"public"' },
    { extensionSchema: "postgis_ext", quoted: '"postgis_ext"' },
    { extensionSchema: 'post"gis\'; DROP SCHEMA public; --', quoted: '"post""gis\'; DROP SCHEMA public; --"' },
  ])("$extensionSchema 확장의 카탈로그와 함수로 사용자 테이블을 표시합니다.", async ({ extensionSchema, quoted }) => {
    // given
    compatHarness.extensionSchema = extensionSchema;
    // when
    render(<CompatMapModal schema="city" table="roads" connectionId="roads-connection" />);
    // then
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("1개 feature"));
    expect(compatHarness.executeQuery).toHaveBeenCalledTimes(3);
    expect(compatHarness.executeQuery.mock.calls[0][0]).toContain("pg_catalog.pg_extension");
    expect(compatHarness.executeQuery.mock.calls[1][0]).toContain(`FROM ${quoted}.geometry_columns`);
    expect(compatHarness.executeQuery.mock.calls[1][0]).toContain(`FROM ${quoted}.geography_columns`);
    expect(compatHarness.executeQuery.mock.calls[1][0]).toContain("f_table_schema = 'city'");
    expect(compatHarness.executeQuery.mock.calls[2][0]).toContain(`${quoted}.st_transform(`);
    expect(compatHarness.executeQuery.mock.calls[2][0]).toContain(`::${quoted}.geometry`);
    expect(compatHarness.executeQuery.mock.calls[2][0]).toContain('FROM "city"."roads" AS "t"');
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it.each([
    { reason: "PostGIS가 없으면", rows: [], message: "PostGIS 확장이 설치되어 있지 않습니다" },
    { reason: "확장 스키마가 여러 개면", rows: [["public"], ["postgis_ext"]], message: "유효한 스키마 하나가 필요합니다" },
    { reason: "확장 스키마가 비어 있으면", rows: [[""]], message: "유효한 스키마 하나가 필요합니다" },
  ])("$reason 오류를 알리고 후속 쿼리를 실행하지 않습니다.", async ({ rows, message }) => {
    // given
    compatHarness.executeQuery.mockResolvedValueOnce({ columns: ["extension_schema"], rows });
    // when
    render(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    // then
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain(message));
    expect(compatHarness.executeQuery).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("status").textContent).not.toContain("조회하는 중");
    expect((screen.getByRole("button", { name: "조회" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("확장 스키마 조회가 실패하면 public 카탈로그로 재시도하지 않습니다.", async () => {
    // given
    compatHarness.executeQuery.mockRejectedValueOnce(new Error("extension catalog unavailable"));
    // when
    render(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    // then
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("extension catalog unavailable"));
    expect(compatHarness.executeQuery).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("option", { name: /geom/ })).toBeNull();
  });

  it.each([
    { change: "연결", connectionId: "another-connection", schema: "public", table: "roads" },
    { change: "스키마", connectionId: "roads-connection", schema: "archive", table: "roads" },
    { change: "테이블", connectionId: "roads-connection", schema: "public", table: "parks" },
  ])("$change 변경 뒤 늦은 확장 스키마 응답이 현재 조회를 덮어쓰지 않습니다.", async ({ connectionId, schema, table }) => {
    // given
    let resolve!: (value: QueryResult) => void;
    compatHarness.executeQuery.mockReturnValueOnce(new Promise<QueryResult>(complete => { resolve = complete; }));
    const view = render(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    await waitFor(() => expect(compatHarness.executeQuery).toHaveBeenCalledTimes(1));
    compatHarness.connection = { connectionId, driver: "postgres", schema };
    compatHarness.extensionSchema = "current_ext";
    view.rerender(<CompatMapModal schema={schema} table={table} connectionId={connectionId} />);
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("1개 feature"));
    // when
    await act(async () => { resolve({ columns: ["extension_schema"], rows: [["stale_ext"]] }); });
    // then
    expect(compatHarness.executeQuery).toHaveBeenCalledTimes(4);
    expect(compatHarness.executeQuery.mock.calls[2][0]).toContain('FROM "current_ext".geometry_columns');
    expect(compatHarness.executeQuery.mock.calls[3][0]).toContain(`FROM "${schema}"."${table}" AS "t"`);
    expect(compatHarness.executeQuery.mock.calls[3][0]).toContain('"current_ext".st_transform(');
    expect(screen.getByRole("option", { name: /geom/ })).not.toBeNull();
    expect(screen.getByRole("status").textContent).toContain("1개 feature");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("테이블 변경 뒤 늦은 카탈로그 응답으로 이전 공간 열을 선택하지 않습니다.", async () => {
    // given
    let resolve!: (value: QueryResult) => void;
    const pending = new Promise<QueryResult>(complete => { resolve = complete; });
    compatHarness.executeQuery.mockImplementation(sql => sql.includes("geometry_columns") && sql.includes("f_table_name = 'roads'") ? pending : queryResult(sql));
    const view = render(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    await waitFor(() => expect(compatHarness.executeQuery).toHaveBeenCalledTimes(2));
    view.rerender(<CompatMapModal schema="public" table="parks" connectionId="roads-connection" />);
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("1개 feature"));
    // when
    await act(async () => { resolve({ columns: [], rows: [["stale_geom", "geometry", 4326, 2]] }); });
    // then
    expect(compatHarness.executeQuery).toHaveBeenCalledTimes(5);
    expect(compatHarness.executeQuery.mock.calls[4][0]).toContain('FROM "public"."parks" AS "t"');
    expect(screen.queryByRole("option", { name: /stale_geom/ })).toBeNull();
    expect(screen.getByRole("status").textContent).toContain("1개 feature");
  });

  it.each([
    { change: "스키마", schema: "archive", table: "roads" },
    { change: "테이블", schema: "public", table: "parks" },
  ])("$change 변경 시 이전 열로 새 테이블을 조회하지 않고 선택 상태를 초기화합니다.", async ({ schema, table }) => {
    // given
    const view = render(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("1개 feature"));
    const map = FakeMap.instances[0];
    act(() => map.emit("load"));
    map.clickFeature = { properties: { __compatFeatureId: "compat:1" } };
    act(() => map.emit("click", { point: [0, 0] }));
    fireEvent.change(screen.getByLabelText("SRID 0 행의 원본 SRID"), { target: { value: "3857" } });
    compatHarness.executeQuery.mockReturnValueOnce(new Promise<QueryResult>(() => {}));
    // when
    view.rerender(<CompatMapModal schema={schema} table={table} connectionId="roads-connection" />);
    // then
    expect(compatHarness.executeQuery).toHaveBeenCalledTimes(4);
    expect(compatHarness.executeQuery.mock.calls[3][0]).toContain("pg_catalog.pg_extension");
    expect(screen.queryByRole("option", { name: /geom/ })).toBeNull();
    expect((screen.getByLabelText("SRID 0 행의 원본 SRID") as HTMLInputElement).value).toBe("");
    expect(screen.getByRole("region", { name: "선택한 feature 속성" }).textContent).toContain("지도에서 feature를 선택하세요");
    expect((screen.getByRole("button", { name: "GeoJSON 다운로드" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "조회" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("연결 변경 뒤 늦은 테이블 응답을 버리고 조회 중 상태를 해제합니다.", async () => {
    // given
    let resolve!: (value: QueryResult) => void;
    const pending = new Promise<QueryResult>(complete => { resolve = complete; });
    compatHarness.executeQuery.mockImplementation(sql => sql.includes('AS "__geojson"') ? pending : queryResult(sql));
    const view = render(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    await waitFor(() => expect(compatHarness.executeQuery).toHaveBeenCalledTimes(3));
    compatHarness.connection = { connectionId: "another-connection", driver: "postgres", schema: "public" };
    view.rerender(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    // when
    await act(async () => { resolve({ columns: [], rows: [[JSON.stringify({ type: "Point", coordinates: [1, 2] }), 4326, { label: "stale" }]] }); });
    // then
    expect(compatHarness.executeQuery).toHaveBeenCalledTimes(3);
    expect(screen.getByRole("status").textContent).toContain("0개 feature");
    expect(screen.getByRole("status").textContent).not.toContain("조회하는 중");
    expect(screen.getByRole("alert").textContent).toContain("현재 활성 연결과 다릅니다");
    expect((screen.getByRole("button", { name: "GeoJSON 다운로드" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("SRID 0 행에 양의 SRID를 입력하면 조건부 설정 후 새로 표시합니다.", async () => {
    // given
    compatHarness.executeQuery.mockImplementation(async (sql: string): Promise<{ columns: string[]; rows: unknown[][] }> => {
      if (sql.includes("pg_catalog.pg_extension")) return { columns: ["extension_schema"], rows: [["public"]] };
      if (sql.includes("geometry_columns")) return { columns: [], rows: [["geom", "geometry", 0, 2]] };
      if (sql.includes("st_setsrid")) return { columns: [], rows: [[JSON.stringify({ type: "Point", coordinates: [1, 2] }), 0, { label: "zero" }]] };
      return { columns: [], rows: [[null, 0, { label: "zero" }]] };
    });
    render(<CompatMapModal schema="public" table="roads" connectionId="roads-connection" />);
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("카탈로그 SRID가 0 또는 unknown"));
    fireEvent.change(screen.getByLabelText("SRID 0 행의 원본 SRID"), { target: { value: "4326" } });
    // when
    fireEvent.click(screen.getByRole("button", { name: "조회" }));
    // then
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("1개 feature"));
    expect(compatHarness.executeQuery).toHaveBeenCalledTimes(3);
    expect(compatHarness.executeQuery.mock.calls[2][0]).toContain('WHEN "public".st_srid("t"."geom"::"public".geometry) = 0 THEN "public".st_setsrid');
  });
});
