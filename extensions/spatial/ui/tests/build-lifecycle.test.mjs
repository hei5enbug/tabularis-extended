// @vitest-environment node
import { createHash } from "node:crypto";
import { readFileSync, realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { MAPLIBRE_SHA256, MAPLIBRE_VERSION, RTL_MARKER, RTL_REPLACEMENT, mapLibreLifecyclePlugin, rewriteRtlDispatcher, transformMapLibreLifecycle } from "../build/maplibre-lifecycle.mjs";

const moduleId = realpathSync(fileURLToPath(import.meta.resolve("maplibre-gl/dist/maplibre-gl.mjs"))).replaceAll("\\", "/");
const source = readFileSync(moduleId, "utf8");
function rejected(cases) {
  return cases.map(([code, id, installed]) => { try { transformMapLibreLifecycle(code, id, installed); return null; } catch (error) { return error.code; } });
}
function lazyScenario() {
  let factoryCalls = 0, creations = 0, broadcastCalls = 0, broadcastArgs;
  let singleton;
  const factory = () => { factoryCalls++; return singleton ??= (() => { creations++; return { broadcast(...args) { broadcastCalls++; broadcastArgs = args; return "same-result"; } }; })(); };
  const original = `class RTL{constructor(){this.status="unavailable",${RTL_MARKER}this.status=e;return this.dispatcher.broadcast("SRPS",{pluginStatus:e})}}`;
  const Constructor = Function("fa", `return (${rewriteRtlDispatcher(original)})`)(factory);
  const instance = new Constructor();
  const eagerCalls = factoryCalls;
  const first = instance.dispatcher;
  const firstAccessCalls = factoryCalls;
  const result = instance._syncState("loaded");
  return { eagerCalls, firstAccessCalls, creations, broadcastCalls, broadcastArgs, result, firstIsSame: first === singleton, status: instance.status };
}
function pluginScenario(plugin) {
  return { transformed: plugin.transform(source, moduleId), other: plugin.transform("own", "/own.mjs"), complete: plugin.generateBundle() };
}

describe("고정 MapLibre RTL dispatcher 수명 변환", () => {
  it("실제 고정 모듈에서 marker 한 곳만 바꾸고 나머지 byte를 보존한다", () => {
    // given
    const index = source.indexOf(RTL_MARKER);
    const expected = source.slice(0, index) + RTL_REPLACEMENT + source.slice(index + RTL_MARKER.length);
    const license = source.match(/^\/\*\*[\s\S]*?\*\//)?.[0];
    // when
    const actual = transformMapLibreLifecycle(source, moduleId);
    // then
    expect(createHash("sha256").update(source).digest("hex")).toBe(MAPLIBRE_SHA256);
    expect(actual).toBe(expected);
    expect(actual.startsWith(license)).toBe(true);
    expect(actual).not.toContain(RTL_MARKER);
    expect(actual.split(RTL_REPLACEMENT)).toHaveLength(2);
  });

  it("생성자는 dispatcher를 만들지 않고 첫 실제 접근이 원래 singleton을 얻는다", () => {
    // given
    const scenario = lazyScenario;
    // when
    const actual = scenario();
    // then
    expect(actual.eagerCalls).toBe(0);
    expect(actual.firstAccessCalls).toBe(1);
    expect(actual.creations).toBe(1);
    expect(actual.firstIsSame).toBe(true);
  });

  it("RTL 상태 동기화의 broadcast 인자와 반환값을 보존한다", () => {
    // given
    const scenario = lazyScenario;
    // when
    const actual = scenario();
    // then
    expect(actual.broadcastCalls).toBe(1);
    expect(actual.broadcastArgs).toEqual(["SRPS", { pluginStatus: "loaded" }]);
    expect(actual.result).toBe("same-result");
    expect(actual.status).toBe("loaded");
  });

  it("버전과 원문 SHA가 바뀌면 production 변환을 거부한다", () => {
    // given
    const inputs = [[source, moduleId, { moduleId, version: "6.11.3" }], [source + " ", moduleId, { moduleId, version: MAPLIBRE_VERSION }]];
    // when
    const actual = rejected(inputs);
    // then
    expect(actual).toEqual(["MAPLIBRE_VERSION_MISMATCH", "MAPLIBRE_SOURCE_HASH_MISMATCH"]);
  });

  it("다른 MapLibre 설치 경로와 query가 붙은 module id는 거부한다", () => {
    // given
    const inputs = [[source, "/other/maplibre-gl/dist/maplibre-gl.mjs", { moduleId, version: MAPLIBRE_VERSION }], [source, moduleId + "?changed", { moduleId, version: MAPLIBRE_VERSION }]];
    // when
    const actual = rejected(inputs);
    // then
    expect(actual).toEqual(["MAPLIBRE_MODULE_ID_MISMATCH", "MAPLIBRE_MODULE_ID_MISMATCH"]);
  });

  it("marker가 없거나 두 개이면 순수 치환도 거부한다", () => {
    // given
    const inputs = ["no marker", RTL_MARKER + RTL_MARKER];
    // when
    const actual = inputs.map(code => { try { rewriteRtlDispatcher(code); return null; } catch (error) { return error.code; } });
    // then
    expect(actual).toEqual(["MAPLIBRE_RTL_MARKER_MISMATCH", "MAPLIBRE_RTL_MARKER_MISMATCH"]);
  });

  it("원문 marker의 변경도 SHA fence에서 거부한다", () => {
    // given
    const changed = source.replace(RTL_MARKER, RTL_REPLACEMENT);
    // when
    const actual = rejected([[changed, moduleId, { moduleId, version: MAPLIBRE_VERSION }]]);
    // then
    expect(actual).toEqual(["MAPLIBRE_SOURCE_HASH_MISMATCH"]);
  });

  it("다른 모듈과 standalone worker는 원문 그대로 통과한다", () => {
    // given
    const inputs = [["own module", "/own/module.mjs"], ["own worker", moduleId.replace("maplibre-gl.mjs", "maplibre-gl-worker.mjs")]];
    // when
    const actual = inputs.map(([code, id]) => transformMapLibreLifecycle(code, id));
    // then
    expect(actual).toEqual(["own module", "own worker"]);
  });

  it("plugin은 build 전에 고정 모듈 하나에만 변환을 등록한다", () => {
    // given
    const plugin = mapLibreLifecyclePlugin();
    plugin.buildStart();
    // when
    const actual = pluginScenario(plugin);
    // then
    expect(plugin.apply).toBe("build");
    expect(plugin.enforce).toBe("pre");
    expect(actual.transformed.code).toContain(RTL_REPLACEMENT);
    expect(actual.other).toBeNull();
    expect(actual.complete).toBeUndefined();
  });

  it("고정 모듈을 변환하지 않은 빌드는 성공하지 않는다", () => {
    // given
    const plugin = mapLibreLifecyclePlugin(); plugin.buildStart();
    // when
    const actual = (() => { try { plugin.generateBundle(); return null; } catch (error) { return error.code; } })();
    // then
    expect(actual).toBe("MAPLIBRE_MODULE_NOT_TRANSFORMED");
  });
});
