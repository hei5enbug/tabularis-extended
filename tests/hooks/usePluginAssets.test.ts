import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { usePluginAssets } from "../../src/hooks/usePluginAssets";

describe("plugin asset", () => {
  beforeEach(() => { vi.mocked(invoke).mockReset(); });
  it("검증된asset바이트로BlobURL을생성한다", async () => {
    // Given
    vi.mocked(invoke).mockResolvedValue({ bytes: [97, 98], mime: "text/css" });
    const create = vi.fn().mockReturnValue("blob:fixture");
    vi.stubGlobal("URL", { createObjectURL: create, revokeObjectURL: vi.fn() });
    const { result } = renderHook(() => usePluginAssets("spatial"));
    // When
    const asset = await result.current.resolve("ui/style.css");
    // Then
    expect(asset.url).toBe("blob:fixture");
    expect(invoke).toHaveBeenCalledWith("read_plugin_asset", { pluginId: "spatial", filePath: "ui/style.css" });
    expect(create.mock.calls[0][0].type).toBe("text/css");
  });
  it("같은BlobURL을한번만해제한다", async () => {
    // Given
    vi.mocked(invoke).mockResolvedValue({ bytes: [], mime: "application/javascript" });
    const revoke = vi.fn();
    vi.stubGlobal("URL", { createObjectURL: vi.fn().mockReturnValue("blob:worker"), revokeObjectURL: revoke });
    const { result } = renderHook(() => usePluginAssets("spatial"));
    const asset = await result.current.resolve("ui/worker.js");
    const disposeTwice = () => { asset.dispose(); asset.dispose(); };
    // When
    disposeTwice();
    // Then
    expect(revoke).toHaveBeenCalledExactlyOnceWith("blob:worker");
  });
});
