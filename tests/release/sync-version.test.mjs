import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

test("포크 버전을 동기화하면서 원본 안내와 다른 Cargo 패키지는 보존한다", async t => {
  // given
  const root = await mkdtemp(path.join(os.tmpdir(), "extended-version-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(path.join(root, "src-tauri"));
  await mkdir(path.join(root, "src"));
  await writeFile(path.join(root, "package.json"), JSON.stringify({ name: "tabularis-extended", version: "0.27.0+extended.2" }));
  await writeFile(path.join(root, "src-tauri/tauri.conf.json"), JSON.stringify({ version: "0.27.0+extended.1", productName: "Tabularis Extended" }));
  await writeFile(path.join(root, "src-tauri/Cargo.toml"), '[package]\nname = "tabularis-extended"\nversion = "0.27.0+extended.1"\n');
  await writeFile(path.join(root, "src-tauri/Cargo.lock"), 'version = 4\n\n[[package]]\nname = "tabularis-extended"\nversion = "0.27.0+extended.1"\n\n[[package]]\nname = "other"\nversion = "0.27.0"\n');
  const original = "https://github.com/TabularisDB/tabularis/releases/download/v0.27.0/tabularis_0.27.0_x64.dmg\n";
  await writeFile(path.join(root, "README.upstream.md"), original);

  // when
  execFileSync(process.execPath, [fileURLToPath(new URL("../../scripts/sync-version.js", import.meta.url))], { cwd: root, stdio: "pipe" });

  // then
  assert.equal(JSON.parse(await readFile(path.join(root, "src-tauri/tauri.conf.json"), "utf8")).version, "0.27.0+extended.2");
  assert.match(await readFile(path.join(root, "src/version.ts"), "utf8"), /0\.27\.0\+extended\.2/);
  const lock = await readFile(path.join(root, "src-tauri/Cargo.lock"), "utf8");
  assert.match(lock, /name = "tabularis-extended"\nversion = "0\.27\.0\+extended\.2"/);
  assert.match(lock, /name = "other"\nversion = "0\.27\.0"/);
  assert.equal(await readFile(path.join(root, "README.upstream.md"), "utf8"), original);
});
