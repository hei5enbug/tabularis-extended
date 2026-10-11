import { describe, it, expect, vi, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { UpdateProvider } from "../../src/contexts/UpdateProvider";
import { useUpdate } from "../../src/hooks/useUpdate";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ReactNode } from "react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
const wrapper = ({ children }: { children: ReactNode }) => <UpdateProvider>{children}</UpdateProvider>;
afterEach(() => { vi.clearAllMocks(); vi.useRealTimers(); });

describe("Extended 코어 업데이트", () => {
  it("시작 후 공식 코어 업데이트를 조회하거나 다운로드 이벤트를 구독하지 않습니다.", async () => {
    // given
    vi.useFakeTimers();
    const { result } = renderHook(() => useUpdate(), { wrapper });
    // when
    await act(() => vi.advanceTimersByTimeAsync(10_000));
    // then
    expect(invoke).not.toHaveBeenCalled();
    expect(listen).not.toHaveBeenCalled();
    expect(result.current.availableUpdate).toBeNull();
    expect(result.current.installationSource).toBe("extended-manual");
    expect(result.current.isUpToDate).toBe(false);
  });

  it.each(["checkForUpdates", "downloadAndInstall"] as const)("%s 호출도 공식 배포판으로 바꾸는 명령을 실행하지 않습니다.", async method => {
    // given
    const { result } = renderHook(() => useUpdate(), { wrapper });
    // when
    await act(async () => { await result.current[method](); });
    // then
    expect(invoke).not.toHaveBeenCalled();
    expect(result.current.updateInfo).toBeNull();
    expect(result.current.isDownloading).toBe(false);
  });
});
