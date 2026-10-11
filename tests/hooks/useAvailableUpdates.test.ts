import { describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { useAvailableUpdates } from "../../src/hooks/useAvailableUpdates";

vi.mock("../../src/hooks/usePluginRegistry", () => ({ usePluginRegistry: () => ({ updates: [{ name: "Plugin One", installed_version: "1.0", latest_version: "2.0" }, { name: "Plugin Two", installed_version: "1.0", latest_version: "1.1" }] }) }));

describe("Extended 업데이트 배지", () => {
  it("코어 업데이트는 제외하고 플러그인 업데이트 수와 상세를 유지합니다.", () => {
    // given
    const hook = () => useAvailableUpdates();
    // when
    const { result } = renderHook(hook);
    // then
    expect(result.current.coreCount).toBe(0);
    expect(result.current.pluginCount).toBe(2);
    expect(result.current.totalCount).toBe(2);
    expect(result.current.pluginDetails).toEqual(["Plugin One 1.0 → 2.0", "Plugin Two 1.0 → 1.1"]);
  });
});
