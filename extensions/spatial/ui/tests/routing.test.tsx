import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import SpatialPlugin from "../src/index";

vi.mock("@tabularis/plugin-api", () => ({ usePluginService: () => ({}), usePluginAssets: () => ({}) }));
vi.mock("../src/MapToolbar", () => ({ MapToolbar: () => <div data-testid="service-toolbar" /> }));
vi.mock("../src/MapRenderer", () => ({ MapRenderer: () => <div data-testid="renderer" /> }));
vi.mock("../src/compat", () => ({ CompatToolbar: () => <div data-testid="basic-toolbar" /> }));

afterEach(cleanup);
const render_ = (driver: string | undefined, connectionId = "roads-connection") =>
  render(<SpatialPlugin pluginId="spatial" context={{ connectionId, driver }} />);

describe("공통 서비스 호스트의 드라이버 분기", () => {
  it("postgresql-plus 연결은 서비스 도구 모음을 보여줍니다.", () => {
    // given
    // when
    render_("postgresql-plus");
    // then
    expect(screen.queryByTestId("service-toolbar")).not.toBeNull();
    expect(screen.queryByTestId("basic-toolbar")).toBeNull();
  });

  it.each(["postgres", "postgresql"])("기존 %s 연결은 기본 지도 도구 모음으로 연결합니다.", driver => {
    // given
    // when
    render_(driver);
    // then
    expect(screen.queryByTestId("basic-toolbar")).not.toBeNull();
    expect(screen.queryByTestId("service-toolbar")).toBeNull();
  });

  it("다른 드라이버 연결에는 도구 모음을 보여주지 않습니다.", () => {
    // given
    // when
    render_("sqlite");
    // then
    expect(screen.queryByTestId("service-toolbar")).toBeNull();
    expect(screen.queryByTestId("basic-toolbar")).toBeNull();
  });

  it("연결이 없는 슬롯은 지도 렌더러를 보여줍니다.", () => {
    // given
    // when
    render_(undefined, "");
    // then
    expect(screen.queryByTestId("renderer")).not.toBeNull();
  });
});
