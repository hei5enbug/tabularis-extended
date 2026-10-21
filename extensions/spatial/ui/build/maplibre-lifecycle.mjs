import { createHash } from "node:crypto";
import { readFileSync, realpathSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const MAPLIBRE_VERSION = "6.11.2";
export const MAPLIBRE_SHA256 = "3f55566295583644617fe17d008a36c580414b8c71dd2e1fcff1309de6fdee5d";
export const RTL_MARKER = "this.url=null,this.dispatcher=fa()}_syncState(e){";
export const RTL_REPLACEMENT = "this.url=null}get dispatcher(){return fa()}_syncState(e){";
const moduleId = realpathSync(fileURLToPath(import.meta.resolve("maplibre-gl/dist/maplibre-gl.mjs"))).replaceAll("\\", "/");
const version = JSON.parse(readFileSync(join(dirname(dirname(moduleId)), "package.json"), "utf8")).version;

function fail(code) { throw Object.assign(new Error(code), { code }); }
export function rewriteRtlDispatcher(code) {
  const index = code.indexOf(RTL_MARKER);
  if (index < 0 || code.indexOf(RTL_MARKER, index + RTL_MARKER.length) >= 0) fail("MAPLIBRE_RTL_MARKER_MISMATCH");
  return code.slice(0, index) + RTL_REPLACEMENT + code.slice(index + RTL_MARKER.length);
}
export function transformMapLibreLifecycle(code, id, installed = { moduleId, version }) {
  if (id !== installed.moduleId) {
    if (/(?:^|[/\\])maplibre-gl[/\\]dist[/\\]maplibre-gl\.mjs(?:[?#].*)?$/.test(id)) fail("MAPLIBRE_MODULE_ID_MISMATCH");
    return code;
  }
  if (installed.version !== MAPLIBRE_VERSION) fail("MAPLIBRE_VERSION_MISMATCH");
  if (createHash("sha256").update(code).digest("hex") !== MAPLIBRE_SHA256) fail("MAPLIBRE_SOURCE_HASH_MISMATCH");
  return rewriteRtlDispatcher(code);
}
export function mapLibreLifecyclePlugin() {
  let transformed = false;
  return {
    name: "tabularis-maplibre-lifecycle",
    apply: "build",
    enforce: "pre",
    buildStart() { transformed = false; },
    transform(code, id) {
      const output = transformMapLibreLifecycle(code, id);
      if (id !== moduleId) return null;
      transformed = true;
      return { code: output, map: null };
    },
    generateBundle() { if (!transformed) fail("MAPLIBRE_MODULE_NOT_TRANSFORMED"); },
  };
}
