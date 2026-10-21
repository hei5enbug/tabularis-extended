import type { UsePluginAssetsReturn } from "@tabularis/plugin-api";
import { SpatialUiError } from "./models";

export const CSS_ASSET = "ui/dist/style.css";
export const WORKER_ASSET = "ui/dist/maplibre-worker.js";
export interface MapAssets { workerUrl: string; dispose(): void }

export async function loadAssets(assets: UsePluginAssetsReturn, signal: AbortSignal): Promise<MapAssets> {
  const resolved = await Promise.allSettled([assets.resolve(CSS_ASSET), assets.resolve(WORKER_ASSET)]);
  const css = resolved[0].status === "fulfilled" ? resolved[0].value : null;
  const worker = resolved[1].status === "fulfilled" ? resolved[1].value : null;
  let link: HTMLLinkElement | null = null;
  let disposed = false;
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    link?.remove();
    worker?.dispose();
    css?.dispose();
  };
  if (!css || !worker || signal.aborted) { dispose(); throw new SpatialUiError("CAPABILITY_UNAVAILABLE", "지도 CSS 또는 worker asset을 사용할 수 없습니다."); }
  try {
    await new Promise<void>((resolve, reject) => {
      link = document.createElement("link");
      link.rel = "stylesheet";
      link.href = css.url;
      const timeout = setTimeout(() => settle(new SpatialUiError("APPLY_TIMEOUT", "지도 CSS 로드를 기다리는 시간이 초과되었습니다.")), 10000);
      const settle = (error?: Error) => {
        clearTimeout(timeout);
        signal.removeEventListener("abort", abort);
        if (link) { link.onload = null; link.onerror = null; }
        if (error) reject(error); else resolve();
      };
      const abort = () => settle(new DOMException("Cancelled", "AbortError"));
      link.onload = () => settle();
      link.onerror = () => settle(new SpatialUiError("CAPABILITY_UNAVAILABLE", "지도 CSS를 읽지 못했습니다."));
      signal.addEventListener("abort", abort, { once: true });
      document.head.append(link);
    });
    return { workerUrl: worker.url, dispose };
  } catch (error) { dispose(); throw error; }
}
