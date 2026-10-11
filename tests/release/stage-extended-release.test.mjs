import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { access, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { stageRelease } from "../../scripts/stage-extended-release.mjs";

const sha256 = bytes => createHash("sha256").update(bytes).digest("hex");

async function fixture(t) {
  const temporary = await mkdtemp(path.join(os.tmpdir(), "extended-release-"));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const root = path.join(temporary, "source");
  await mkdir(root);
  execFileSync("git", ["init", "--quiet", root]);
  for (const name of ["src", "src-tauri", "integration"]) await mkdir(path.join(root, name));
  const version = "0.27.0+extended.1";
  const config = { productName: "Tabularis Extended", version, identifier: "io.github.hei5enbug.tabularis.extended",
    bundle: { createUpdaterArtifacts: false }, plugins: { updater: { endpoints: [] } } };
  await writeFile(path.join(root, "package.json"), JSON.stringify({ name: "tabularis-extended", version }));
  await writeFile(path.join(root, "src-tauri/tauri.conf.json"), JSON.stringify(config));
  await writeFile(path.join(root, "src-tauri/Cargo.toml"), `[package]\nversion = "${version}"\n`);
  await writeFile(path.join(root, "src/version.ts"), `export const APP_VERSION = "${version}";\n`);
  const bundleRoot = path.join(temporary, "bundle");
  await mkdir(bundleRoot);
  await writeFile(path.join(bundleRoot, "Tabularis Extended.dmg"), "synthetic app bundle");
  const companionArchive = path.join(temporary, "sqlserver-plus-darwin-arm64.zip");
  await writeFile(companionArchive, "synthetic verified companion");
  await writeFile(path.join(root, "integration/companions.json"), JSON.stringify({ sqlserverPlus: {
    platforms: { "darwin-arm64": { sha256: sha256(await readFile(companionArchive)) } },
  } }));
  return { root, bundleRoot, companionArchive, output: path.join(temporary, "release"), platform: "darwin-arm64", config };
}

test("릴리스 설치 파일과 동반 플러그인의 실제 바이트를 영수증과 체크섬에 기록한다", async t => {
  // given
  const input = await fixture(t);
  // when
  const receipt = await stageRelease(input);
  const sums = await readFile(path.join(input.output, "SHA256SUMS"), "utf8");
  // then
  assert.equal(receipt.sourceDirty, true);
  assert.equal(receipt.signed, false);
  assert.equal(receipt.automaticUpdates, false);
  assert.match(receipt.sourceTreeSha256, /^[a-f0-9]{64}$/);
  const archiveName = path.basename(input.companionArchive);
  assert.equal(await readFile(path.join(input.output, `${archiveName}.sha256`), "utf8"),
    `${sha256(await readFile(input.companionArchive))}  ${archiveName}\n`);
  for (const line of sums.trim().split("\n")) {
    const [expected, name] = line.split("  ");
    assert.equal(sha256(await readFile(path.join(input.output, name))), expected);
  }
});

test("검증 후 변조된 동반 플러그인은 출력 디렉터리를 만들기 전에 거부한다", async t => {
  // given
  const input = await fixture(t);
  await writeFile(input.companionArchive, "tampered companion");
  // when
  const result = stageRelease(input);
  // then
  await assert.rejects(result, /compiled pin/);
  await assert.rejects(access(input.output));
});

test("공식 업데이트 주소나 버전 불일치가 있는 빌드를 릴리스로 묶지 않는다", async t => {
  // given
  const input = await fixture(t);
  input.config.plugins.updater.endpoints = ["https://example.invalid/upstream"];
  await writeFile(path.join(input.root, "src-tauri/tauri.conf.json"), JSON.stringify(input.config));
  // when
  const withUpdater = stageRelease(input);
  // then
  await assert.rejects(withUpdater, /updater configuration/);
  input.config.plugins.updater.endpoints = [];
  await writeFile(path.join(input.root, "src-tauri/tauri.conf.json"), JSON.stringify(input.config));
  await writeFile(path.join(input.root, "src/version.ts"), "export const APP_VERSION = '0.26.0';\n");
  await assert.rejects(stageRelease(input), /version/);
});

test("이전 빌드가 섞인 디렉터리와 이미 존재하는 출력 경로를 거부한다", async t => {
  // given
  const input = await fixture(t);
  const oldBundle = path.join(input.bundleRoot, "Old.dmg");
  await writeFile(oldBundle, "old bundle");
  // when
  const ambiguous = stageRelease(input);
  // then
  await assert.rejects(ambiguous, /exactly one/);
  await rm(oldBundle);
  await mkdir(input.output);
  await writeFile(path.join(input.output, "keep"), "existing output");
  await assert.rejects(stageRelease(input), { code: "EEXIST" });
  assert.equal(await readFile(path.join(input.output, "keep"), "utf8"), "existing output");
});

test("기존 저장소 내부 링크를 기록하고 다른 대상으로 바뀐 링크를 거부한다", async t => {
  // given
  const input = await fixture(t);
  await writeFile(path.join(input.root, "AGENTS.md"), "synthetic instructions");
  await symlink("AGENTS.md", path.join(input.root, "CLAUDE.md"));
  await mkdir(path.join(input.root, ".agents"));
  await mkdir(path.join(input.root, ".claude/skills"), { recursive: true });
  await writeFile(path.join(input.root, ".claude/skills/fixture.txt"), "synthetic skill");
  await symlink("../.claude/skills", path.join(input.root, ".agents/skills"));
  // when
  const receipt = await stageRelease(input);
  // then
  assert.match(receipt.sourceTreeSha256, /^[a-f0-9]{64}$/);
  await rm(path.join(input.root, "CLAUDE.md"));
  await symlink("package.json", path.join(input.root, "CLAUDE.md"));
  await assert.rejects(stageRelease({ ...input, output: `${input.output}-changed` }), /allowed target/);
});
