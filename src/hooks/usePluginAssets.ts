import { useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";

export function usePluginAssets(pluginId: string) {
  return useMemo(() => ({
    async resolve(filePath: string): Promise<{ url: string; dispose(): void }> {
      const asset = await invoke<{ bytes: number[]; mime: string }>("read_plugin_asset", { pluginId, filePath });
      const url = URL.createObjectURL(new Blob([new Uint8Array(asset.bytes)], { type: asset.mime }));
      let disposed = false;
      return { url, dispose() { if (!disposed) { disposed = true; URL.revokeObjectURL(url); } } };
    },
  }), [pluginId]);
}
