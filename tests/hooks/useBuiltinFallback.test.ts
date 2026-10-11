import { describe, expect, it, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { useDrivers } from "../../src/hooks/useDrivers";
import { BUILTIN_DRIVER_IDS } from "../../src/utils/connections";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => { throw new Error("fixture registry unavailable"); }) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("../../src/hooks/useSettings", () => ({ useSettings: () => ({ settings: {} }) }));

describe("Extended 내장 드라이버 대체 목록", () => {
  it("레지스트리를 읽지 못하면 MySQL과 SQLite만 내장 드라이버로 제공합니다.", async () => {
    // given
    const hook = () => useDrivers();
    // when
    const { result } = renderHook(hook);
    // then
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.drivers.map(driver => driver.id)).toEqual(["mysql", "sqlite"]);
    expect(BUILTIN_DRIVER_IDS).toEqual(["mysql", "sqlite"]);
  });
});
