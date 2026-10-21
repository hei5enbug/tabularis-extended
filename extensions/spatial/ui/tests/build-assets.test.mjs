// @vitest-environment node
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { build, parseAst } from "vite";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Script } from "node:vm";
import { fileURLToPath } from "node:url";

let directory;
let worker;
beforeAll(async () => {
  directory = await mkdtemp(join(tmpdir(), "tabularis-t0c-worker-"));
  await build({ root: fileURLToPath(new URL("..", import.meta.url)), configFile: fileURLToPath(new URL("../vite.config.ts", import.meta.url)), build: { outDir: directory, emptyOutDir: true }, logLevel: "silent" });
  worker = await readFile(join(directory, "maplibre-worker.js"), "utf8");
}, 30000);
afterAll(async () => { if (directory) await rm(directory, { recursive: true, force: true }); });

function workerSyntax(code) {
  const script = new Script(code, { filename: "maplibre-worker.js" });
  const nodes = [parseAst(code)];
  const imports = [];
  while (nodes.length) {
    const node = nodes.pop();
    if (!node || typeof node !== "object") continue;
    if (node.type === "ImportDeclaration" || node.type === "ExportAllDeclaration" || node.type === "ExportNamedDeclaration" || node.type === "ExportDefaultDeclaration") imports.push(node.type);
    for (const value of Object.values(node)) if (Array.isArray(value)) nodes.push(...value); else if (value && typeof value === "object") nodes.push(value);
  }
  return { script, imports };
}

describe("공간 UI production asset", () => {
  it("실제 빌드는 UI와 CSS 및 단일 worker 파일을 함께 출력한다", async () => {
    // given
    const output = directory;
    // when
    const actual = await readdir(output);
    // then
    expect(actual.sort()).toEqual(["index.js", "maplibre-worker.js", "style.css"]);
  });

  it("번들 worker는 static import와 export 없이 standalone script 문법을 만족한다", () => {
    // given
    const source = worker;
    // when
    const actual = workerSyntax(source);
    // then
    expect(actual.script).toBeInstanceOf(Script);
    expect(actual.imports).toEqual([]);
    expect(source).not.toContain("./maplibre-gl-shared.mjs");
    expect(source).toContain("MapLibre GL JS");
    expect(source).toContain("@license 3-Clause BSD");
  });
});
