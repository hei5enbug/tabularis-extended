import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { chmod, lstat, mkdir, mkdtemp, readFile, readdir, rename, rm, stat, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createZip, readZip } from "../../scripts/archive.mjs";
import { inspectPluginArchive, installArchive, verifyArchive } from "../../scripts/install.mjs";
import { packagePlugin } from "../../scripts/package.mjs";
import { preflightManifest } from "../../scripts/preflight.mjs";

const version = "1.0.0-beta.3+plus.1";
const profileId = "sqlserver-plus";
const installerScript = fileURLToPath(new URL("../../scripts/install.mjs", import.meta.url));
const packagerScript = fileURLToPath(new URL("../../scripts/package.mjs", import.meta.url));

function fixtureManifest() {
  return {
    id: profileId,
    name: "SQL Server Plus",
    version,
    min_runtime_version: "0.27.0",
    kind: "driver",
    engine: "sqlserver",
    executable: "sqlserver-plugin",
    readme: "README.md",
    capabilities: { sql_dialect: "mssql" },
    settings: [{ key: "max_pool_size" }],
    ui_extensions: [{ slot: "connection-modal.extra_fields", driver: profileId, module: "ui/dist/index.js" }],
    explain_parsers: [{ engine: "sqlserver", format: "sqlserver-showplan-xml", module: "explain/dist/index.iife.js" }],
  };
}

const cargoManifest = `[package]\nname = "sqlserver-plugin"\nversion = "${version}"\n\n[dependencies]\nserde = { version = "1" }\nversion = "0"\n`;

async function makeRepositoryFixture() {
  const root = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-package-"));
  const files = new Map([
    [".tabularium", JSON.stringify(fixtureManifest(), null, 2)],
    ["Cargo.toml", cargoManifest],
    ["ui/package.json", JSON.stringify({ version, private: true })],
    ["explain/package.json", JSON.stringify({ version, private: true })],
    ["README.md", "SQL Server Plus README"],
    ["LICENSE", "Apache license"],
    ["assets/screenshots/01.png", "screenshot"],
    ["ui/dist/index.js", "ui bundle"],
    ["explain/dist/index.iife.js", "parser bundle"],
  ]);
  for (const [relativePath, content] of files) {
    const fullPath = path.join(root, relativePath);
    await mkdir(path.dirname(fullPath), { recursive: true });
    await writeFile(fullPath, content);
  }
  const binaryPath = path.join(root, "sqlserver-plugin");
  await writeFile(binaryPath, "synthetic executable");
  await chmod(binaryPath, 0o755);
  return { root, binaryPath };
}

// Marks a promise as handled so a rejection that settles before its assertion runs is not reported as unhandled.
function handled(promise) {
  promise.catch(() => {});
  return promise;
}

async function rewriteJson(file, change) {
  const current = JSON.parse(await readFile(file, "utf8"));
  await writeFile(file, JSON.stringify(change(current)));
}

function archiveEntries({ omit = [], manifest = fixtureManifest(), extra = [] } = {}) {
  const files = [
    ["sqlserver-plugin", "synthetic executable", 0o755],
    [".tabularium", JSON.stringify(manifest), 0o644],
    ["README.md", "SQL Server Plus README", 0o644],
    ["LICENSE", "Apache license", 0o644],
    ["assets/screenshots/01.png", "screenshot", 0o644],
    ["ui/dist/index.js", "ui bundle", 0o644],
    ["explain/dist/index.iife.js", "parser bundle", 0o644],
    ...extra,
  ];
  return files.filter(([name]) => !omit.includes(name)).map(([name, data, mode]) => ({ name, data, mode }));
}

async function writeArchive(directory, entries = archiveEntries()) {
  const archivePath = path.join(directory, "sqlserver-plus-test.zip");
  const buffer = createZip(entries);
  await writeFile(archivePath, buffer);
  return {
    archivePath,
    digest: createHash("sha256").update(buffer).digest("hex"),
    buffer,
  };
}

test("manifest preflight는 SQL Server Plus 버전과 연결된 bundle을 확인합니다.", async () => {
  // given
  const { root } = await makeRepositoryFixture();

  // when
  const result = await preflightManifest({ root });

  // then
  assert.equal(result.manifest.id, profileId);
  assert.equal(result.manifest.version, version);
  assert.deepEqual(result.assetPaths.sort(), ["README.md", "explain/dist/index.iife.js", "ui/dist/index.js"]);
  await rm(root, { recursive: true, force: true });
});

test("preflight는 asset root 밖으로 빠지는 상위 디렉터리 symlink를 거부합니다.", async () => {
  // given
  const { root } = await makeRepositoryFixture();
  const assetRoot = path.join(root, "assets");
  const externalRoot = path.join(root, "external");
  await mkdir(path.join(assetRoot, "explain/dist"), { recursive: true });
  await mkdir(path.join(externalRoot, "dist"), { recursive: true });
  await writeFile(path.join(assetRoot, "README.md"), "README");
  await writeFile(path.join(assetRoot, "explain/dist/index.iife.js"), "parser bundle");
  await writeFile(path.join(externalRoot, "dist/index.js"), "external bundle");
  await symlink(externalRoot, path.join(assetRoot, "ui"), "dir");

  // when
  const result = preflightManifest({ root, assetRoot });

  // then
  await assert.rejects(result, /must not use a symbolic link/);
  await rm(root, { recursive: true, force: true });
});

test("manifest preflight는 package 버전이 어긋나면 실패합니다.", async () => {
  // given
  const { root } = await makeRepositoryFixture();
  await writeFile(path.join(root, "ui/package.json"), JSON.stringify({ version: "1.0.0-beta.2", private: true }));

  // when
  const result = preflightManifest({ root });

  // then
  await assert.rejects(result, /UI package version/);
  await rm(root, { recursive: true, force: true });
});

test("재현 가능한 패키지는 정렬된 경로와 고정 실행 권한을 기록합니다.", async () => {
  // given
  const { root, binaryPath } = await makeRepositoryFixture();
  const firstPath = path.join(root, "out/first.zip");
  const secondPath = path.join(root, "out/second.zip");

  // when
  const first = await packagePlugin({ root, binaryPath, platform: "linux-x64", outputPath: firstPath });
  const second = await packagePlugin({ root, binaryPath, platform: "linux-x64", outputPath: secondPath });
  const firstArchive = await readFile(firstPath);
  const secondArchive = await readFile(secondPath);
  const entries = readZip(firstArchive);

  // then
  assert.deepEqual(firstArchive, secondArchive);
  assert.deepEqual([...entries.keys()], [...entries.keys()].sort());
  assert.equal(entries.get("sqlserver-plugin").mode & 0o111, 0o111);
  for (const required of [".tabularium", "ui/dist/index.js", "explain/dist/index.iife.js", "LICENSE", "README.md", "assets/screenshots/01.png"]) {
    assert.ok(entries.has(required), `Required package file is missing: ${required}`);
  }
  assert.equal((await readFile(first.sha256Path, "utf8")).split(" ")[0], first.sha256);
  assert.equal((await readFile(second.sha256Path, "utf8")).split(" ")[0], second.sha256);
  await rm(root, { recursive: true, force: true });
});

test("필수 UI bundle이 없으면 패키지 생성을 중단합니다.", async () => {
  // given
  const { root, binaryPath } = await makeRepositoryFixture();
  await rm(path.join(root, "ui/dist/index.js"));

  // when
  const result = packagePlugin({ root, binaryPath, platform: "linux-x64", outputPath: path.join(root, "missing.zip") });

  // then
  await assert.rejects(result, /Manifest-referenced asset file is missing/);
  await rm(root, { recursive: true, force: true });
});

test("installer는 기본 dry run에서 기존 profile 파일을 바꾸지 않습니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-dry-run-"));
  const profile = path.join(temporary, "profile");
  await mkdir(profile);
  const { archivePath, digest } = await writeArchive(temporary);

  // when
  const result = await installArchive({ archivePath, sha256: digest, profileDataDir: profile });

  // then
  assert.equal(result.applied, false);
  assert.equal(result.target, path.join(profile, "plugins", "drivers", profileId));
  await assert.rejects(lstat(path.join(profile, "plugins")), { code: "ENOENT" });
  await rm(temporary, { recursive: true, force: true });
});

test("CLI의 기본 실행은 profile에 파일을 쓰지 않고 설치 경로를 출력합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-cli-dry-run-"));
  const profile = path.join(temporary, "profile");
  await mkdir(profile);
  const { archivePath, digest } = await writeArchive(temporary);

  // when
  const result = spawnSync(process.execPath, [
    installerScript,
    "--archive", archivePath,
    "--sha256", digest,
    "--profile-data-dir", profile,
  ], { encoding: "utf8" });

  // then
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /Dry run/);
  assert.match(result.stdout, /plugins[\\/]drivers[\\/]sqlserver-plus/);
  await assert.rejects(lstat(path.join(profile, "plugins")), { code: "ENOENT" });
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 지정된 SQL Server Plus만 staging 후 교체합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-apply-"));
  const profile = path.join(temporary, "profile");
  const target = path.join(profile, "plugins", "drivers", profileId);
  const other = path.join(profile, "plugins", "drivers", "another-driver");
  await mkdir(target, { recursive: true });
  await mkdir(other, { recursive: true });
  await writeFile(path.join(target, "old.txt"), "old package");
  await writeFile(path.join(other, "preserve.txt"), "untouched");
  const { archivePath, digest } = await writeArchive(temporary);

  // when
  const result = await installArchive({ archivePath, sha256: digest, profileDataDir: profile, apply: true });

  // then
  assert.equal(result.applied, true);
  assert.ok(result.backupPath);
  assert.equal(await readFile(path.join(result.backupPath, "old.txt"), "utf8"), "old package");
  assert.equal((await readFile(path.join(target, "README.md"), "utf8")), "SQL Server Plus README");
  assert.equal((await stat(path.join(target, "sqlserver-plugin"))).mode & 0o111, 0o111);
  await assert.rejects(lstat(path.join(target, "old.txt")), { code: "ENOENT" });
  assert.equal(await readFile(path.join(other, "preserve.txt"), "utf8"), "untouched");
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 SHA-256 불일치 시 기존 plugin을 보존합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-hash-"));
  const profile = path.join(temporary, "profile");
  const target = path.join(profile, "plugins", "drivers", profileId);
  await mkdir(target, { recursive: true });
  await writeFile(path.join(target, "preserve.txt"), "existing");
  const { archivePath } = await writeArchive(temporary);

  // when
  const result = installArchive({ archivePath, sha256: "0".repeat(64), profileDataDir: profile, apply: true });

  // then
  await assert.rejects(result, /SHA-256 verification failed/);
  assert.equal(await readFile(path.join(target, "preserve.txt"), "utf8"), "existing");
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 manifest bundle이 빠진 archive를 적용하지 않습니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-bundle-"));
  const profile = path.join(temporary, "profile");
  await mkdir(profile);
  const { archivePath, digest } = await writeArchive(temporary, archiveEntries({ omit: ["explain/dist/index.iife.js"] }));

  // when
  const result = installArchive({ archivePath, sha256: digest, profileDataDir: profile, apply: true });

  // then
  await assert.rejects(result, /ZIP is missing a required file: explain\/dist\/index\.iife\.js/);
  await assert.rejects(lstat(path.join(profile, "plugins")), { code: "ENOENT" });
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 archive 경로 순회를 검증 전에 차단합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-traversal-"));
  const profile = path.join(temporary, "profile");
  await mkdir(profile);
  const fixture = await writeArchive(temporary);
  const malicious = Buffer.from(fixture.buffer);
  const safeName = Buffer.from(".tabularium");
  const traversalName = Buffer.from("../outside!");
  assert.equal(safeName.length, traversalName.length);
  let offset = 0;
  while ((offset = malicious.indexOf(safeName, offset)) !== -1) {
    traversalName.copy(malicious, offset);
    offset += traversalName.length;
  }
  await writeFile(fixture.archivePath, malicious);
  const digest = createHash("sha256").update(malicious).digest("hex");

  // when
  const result = installArchive({ archivePath: fixture.archivePath, sha256: digest, profileDataDir: profile, apply: true });

  // then
  await assert.rejects(result, /ZIP traversal or ambiguous paths/);
  await assert.rejects(lstat(path.join(temporary, "outside!")), { code: "ENOENT" });
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 ZIP symbolic link 항목을 거부합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-symlink-"));
  const fixture = await writeArchive(temporary);
  const malicious = Buffer.from(fixture.buffer);
  const signature = Buffer.from([0x50, 0x4b, 0x01, 0x02]);
  const central = malicious.indexOf(signature);
  assert.notEqual(central, -1);
  malicious.writeUInt32LE((0o120777 << 16) >>> 0, central + 38);

  // when
  let error;
  try {
    inspectPluginArchive(malicious);
  } catch (caught) {
    error = caught;
  }

  // then
  assert.match(error?.message ?? "", /symbolic link/);
  await rm(temporary, { recursive: true, force: true });
});

test("패키지 교체 실패 시 기존 설치를 rollback합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-rollback-"));
  const profile = path.join(temporary, "profile");
  const target = path.join(profile, "plugins", "drivers", profileId);
  await mkdir(target, { recursive: true });
  await writeFile(path.join(target, "preserve.txt"), "old installation");
  const { archivePath, digest } = await writeArchive(temporary);
  let failActivation = true;
  const renameImpl = async (source, destination) => {
    if (failActivation && source.includes(`.${profileId}.stage-`) && destination === target) {
      failActivation = false;
      throw new Error("synthetic activation failure");
    }
    await rename(source, destination);
  };

  // when
  const result = installArchive({ archivePath, sha256: digest, profileDataDir: profile, apply: true, renameImpl });

  // then
  await assert.rejects(result, /synthetic activation failure/);
  assert.equal(await readFile(path.join(target, "preserve.txt"), "utf8"), "old installation");
  assert.deepEqual((await readdir(path.dirname(target))).filter(name => name.includes(".stage-") || name.includes(".backup-")), []);
  await rm(temporary, { recursive: true, force: true });
});

test("manifest preflight는 EXPLAIN package가 공개 배포 설정을 가지면 실패합니다.", async () => {
  // given
  const { root } = await makeRepositoryFixture();
  await rewriteJson(path.join(root, "explain/package.json"), current => ({ ...current, publishConfig: { access: "public" } }));
  const publicRoot = (await makeRepositoryFixture()).root;
  await rewriteJson(path.join(publicRoot, "explain/package.json"), current => ({ version: current.version }));

  // when
  const withPublishConfig = handled(preflightManifest({ root }));
  const notPrivate = handled(preflightManifest({ root: publicRoot }));

  // then
  await assert.rejects(withPublishConfig, /EXPLAIN package must not have publishConfig/);
  await assert.rejects(notPrivate, /EXPLAIN package must be private/);
  await rm(root, { recursive: true, force: true });
  await rm(publicRoot, { recursive: true, force: true });
});

test("manifest preflight는 Cargo 의존성의 version이 아니라 package 섹션의 version을 비교합니다.", async () => {
  // given
  const { root } = await makeRepositoryFixture();
  await writeFile(path.join(root, "Cargo.toml"), `[dependencies]\nversion = "0"\n\n[package]\nname = "sqlserver-plugin"\nversion = "9.9.9"\n`);

  // when
  const result = preflightManifest({ root });

  // then
  await assert.rejects(result, /differs from Cargo\.toml \(9\.9\.9\)/);
  await rm(root, { recursive: true, force: true });
});

test("manifest preflight는 package identity가 다르거나 plugin 설정에 인증 필드가 있으면 실패합니다.", async () => {
  // given
  const wrongId = (await makeRepositoryFixture()).root;
  await rewriteJson(path.join(wrongId, ".tabularium"), current => ({ ...current, id: "sqlserver" }));
  const wrongDriver = (await makeRepositoryFixture()).root;
  await rewriteJson(path.join(wrongDriver, ".tabularium"), current => ({
    ...current,
    ui_extensions: [{ ...current.ui_extensions[0], driver: "sqlserver" }],
  }));
  const authSetting = (await makeRepositoryFixture()).root;
  await rewriteJson(path.join(authSetting, ".tabularium"), current => ({ ...current, settings: [{ key: "tenant_id" }] }));

  // when
  const idResult = handled(preflightManifest({ root: wrongId }));
  const driverResult = handled(preflightManifest({ root: wrongDriver }));
  const settingResult = handled(preflightManifest({ root: authSetting }));

  // then
  await assert.rejects(idResult, /id must be sqlserver-plus/);
  await assert.rejects(driverResult, /UI extension driver must be sqlserver-plus/);
  await assert.rejects(settingResult, /belongs to per-connection params/);
  await Promise.all([wrongId, wrongDriver, authSetting].map(root => rm(root, { recursive: true, force: true })));
});

test("manifest preflight는 release tag 버전이 manifest와 다르면 실패합니다.", async () => {
  // given
  const { root } = await makeRepositoryFixture();

  // when
  const result = preflightManifest({ root, expectedVersion: "1.0.0" });

  // then
  await assert.rejects(result, /Release tag version 1\.0\.0 does not match/);
  await rm(root, { recursive: true, force: true });
});

test("metadata-only preflight는 빌드 산출물이 없어도 manifest 정합성만 확인합니다.", async () => {
  // given
  const { root } = await makeRepositoryFixture();
  await rm(path.join(root, "ui/dist"), { recursive: true });

  // when
  const metadata = await preflightManifest({ root, metadataOnly: true });
  const full = handled(preflightManifest({ root }));

  // then
  assert.equal(metadata.manifest.id, profileId);
  await assert.rejects(full, /Manifest-referenced asset file is missing/);
  await rm(root, { recursive: true, force: true });
});

test("패키지는 설치 검증을 통과하고 assets와 manifest bundle을 그대로 담습니다.", async () => {
  // given
  const { root, binaryPath } = await makeRepositoryFixture();
  const output = path.join(root, "out", `${profileId}-linux-x64.zip`);

  // when
  const packaged = await packagePlugin({ root, binaryPath, platform: "linux-x64", outputPath: output });
  const inspected = await verifyArchive({ archivePath: output, sha256: packaged.sha256Path });

  // then
  assert.equal(inspected.binaryName, "sqlserver-plugin");
  assert.deepEqual([...inspected.entries.keys()].sort(), packaged.entries);
  assert.equal(inspected.entries.get("assets/screenshots/01.png").data.toString(), "screenshot");
  assert.equal(await readFile(`${output}.sha256`, "utf8"), `${packaged.sha256}  ${profileId}-linux-x64.zip\n`);
  await rm(root, { recursive: true, force: true });
});

test("Windows 패키지는 .exe 실행 파일을 요구하고 exe 이름으로 기록합니다.", async () => {
  // given
  const { root, binaryPath } = await makeRepositoryFixture();
  const exePath = path.join(root, "sqlserver-plugin.exe");
  await writeFile(exePath, "synthetic windows executable");

  // when
  const rejected = handled(packagePlugin({ root, binaryPath, platform: "win-x64", outputPath: path.join(root, "out/bad.zip") }));
  const packaged = await packagePlugin({ root, binaryPath: exePath, platform: "win-x64", outputPath: path.join(root, "out/win.zip") });

  // then
  await assert.rejects(rejected, /require an \.exe executable/);
  assert.ok(packaged.entries.includes("sqlserver-plugin.exe"));
  assert.ok(!packaged.entries.includes("sqlserver-plugin"));
  await rm(root, { recursive: true, force: true });
});

test("패키지 생성은 지원하지 않는 platform과 실행 권한이 없는 binary를 거부합니다.", async () => {
  // given
  const { root, binaryPath } = await makeRepositoryFixture();
  await chmod(binaryPath, 0o644);

  // when
  const unsupported = handled(packagePlugin({ root, binaryPath, platform: "win-arm64", outputPath: path.join(root, "a.zip") }));
  const notExecutable = handled(packagePlugin({ root, binaryPath, platform: "linux-x64", outputPath: path.join(root, "b.zip") }));

  // then
  await assert.rejects(unsupported, /Unsupported platform/);
  if (process.platform !== "win32") await assert.rejects(notExecutable, /execute permission/);
  await rm(root, { recursive: true, force: true });
});

test("패키지 생성은 symlink 실행 파일과 symlink asset을 거부합니다.", async (context) => {
  // given
  const { root, binaryPath } = await makeRepositoryFixture();
  const linkedBinary = path.join(root, "linked-plugin");
  const outside = path.join(root, "outside.png");
  await writeFile(outside, "outside");
  try {
    await symlink(binaryPath, linkedBinary);
  } catch (error) {
    context.skip(`symbolic links are unavailable: ${error.code}`);
    await rm(root, { recursive: true, force: true });
    return;
  }
  await symlink(outside, path.join(root, "assets/screenshots/linked.png"));

  // when
  const binaryResult = handled(packagePlugin({ root, binaryPath: linkedBinary, platform: "linux-x64", outputPath: path.join(root, "a.zip") }));
  const assetResult = handled(packagePlugin({ root, binaryPath, platform: "linux-x64", outputPath: path.join(root, "b.zip") }));

  // then
  await assert.rejects(binaryResult, /regular file, not a symbolic link/);
  await assert.rejects(assetResult, /must not use a symbolic link/);
  await assert.rejects(lstat(path.join(root, "b.zip")), { code: "ENOENT" });
  await rm(root, { recursive: true, force: true });
});

test("패키지 생성은 기존 출력 파일을 덮어쓰지 않고 replace에서만 교체합니다.", async () => {
  // given
  const { root, binaryPath } = await makeRepositoryFixture();
  const output = path.join(root, "out/package.zip");
  await packagePlugin({ root, binaryPath, platform: "linux-x64", outputPath: output });
  await writeFile(binaryPath, "rebuilt executable");

  // when
  const kept = await packagePlugin({ root, binaryPath, platform: "linux-x64", outputPath: output }).catch(error => error);
  const replaced = await packagePlugin({ root, binaryPath, platform: "linux-x64", outputPath: output, replace: true });

  // then
  assert.match(kept.message, /Package output path already exists/);
  const entries = readZip(await readFile(output));
  assert.equal(entries.get("sqlserver-plugin").data.toString(), "rebuilt executable");
  assert.equal((await readFile(replaced.sha256Path, "utf8")).split(" ")[0], replaced.sha256);
  assert.deepEqual((await readdir(path.dirname(output))).sort(), ["package.zip", "package.zip.sha256"]);
  await rm(root, { recursive: true, force: true });
});

test("패키지 CLI는 --replace 없이 기존 출력이 있으면 실패하고 있으면 교체합니다.", async () => {
  // given
  const { root, binaryPath } = await makeRepositoryFixture();
  const output = path.join(root, "out/cli.zip");
  const args = [packagerScript, "--root", root, "--binary", binaryPath, "--platform", "linux-x64", "--output", output];

  // when
  const first = spawnSync(process.execPath, args, { encoding: "utf8" });
  const second = spawnSync(process.execPath, args, { encoding: "utf8" });
  const third = spawnSync(process.execPath, [...args, "--replace"], { encoding: "utf8" });

  // then
  assert.equal(first.status, 0, first.stderr);
  assert.equal(second.status, 1);
  assert.match(second.stderr, /Package output path already exists/);
  assert.equal(third.status, 0, third.stderr);
  await rm(root, { recursive: true, force: true });
});

test("installer는 허용 목록에 없는 파일이 든 archive를 거부합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-extra-"));
  const profile = path.join(temporary, "profile");
  await mkdir(profile);
  const { archivePath, digest } = await writeArchive(temporary, archiveEntries({ extra: [["extra.sh", "#!/bin/sh", 0o755]] }));

  // when
  const result = installArchive({ archivePath, sha256: digest, profileDataDir: profile, apply: true });

  // then
  await assert.rejects(result, /ZIP contains a disallowed file: extra\.sh/);
  await assert.rejects(lstat(path.join(profile, "plugins")), { code: "ENOENT" });
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 다른 plugin id의 manifest를 가진 archive를 거부합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-identity-"));
  const { buffer } = await writeArchive(temporary, archiveEntries({ manifest: { ...fixtureManifest(), id: "sqlserver" } }));

  // when
  const inspect = () => inspectPluginArchive(buffer);

  // then
  assert.throws(inspect, /ZIP manifest id must be sqlserver-plus/);
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 profile 디렉터리가 없으면 어떤 경로도 만들지 않습니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-missing-"));
  const { archivePath, digest } = await writeArchive(temporary);
  const profile = path.join(temporary, "absent-profile");

  // when
  const result = installArchive({ archivePath, sha256: digest, profileDataDir: profile, apply: true });

  // then
  await assert.rejects(result, /profile data directory must exist/);
  await assert.rejects(lstat(profile), { code: "ENOENT" });
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 상대 경로 profile과 profile/plugins-root 동시 지정을 거부합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-arguments-"));
  const { archivePath, digest } = await writeArchive(temporary);

  // when
  const relative = handled(installArchive({ archivePath, sha256: digest, profileDataDir: "relative/profile" }));
  const both = handled(installArchive({ archivePath, sha256: digest, profileDataDir: temporary, pluginsRoot: path.join(temporary, "plugins") }));
  const neither = handled(installArchive({ archivePath, sha256: digest }));

  // then
  await assert.rejects(relative, /absolute --profile-data-dir/);
  await assert.rejects(both, /Exactly one of/);
  await assert.rejects(neither, /Exactly one of/);
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 plugins-root를 지정하면 그 아래 drivers 디렉터리에만 설치합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-plugins-root-"));
  const pluginsRoot = path.join(temporary, "custom-plugins");
  const { archivePath, digest } = await writeArchive(temporary);

  // when
  const dryRun = await installArchive({ archivePath, sha256: digest, pluginsRoot });
  const applied = await installArchive({ archivePath, sha256: digest, pluginsRoot, apply: true });

  // then
  assert.equal(dryRun.applied, false);
  assert.equal(applied.target, path.join(pluginsRoot, "drivers", profileId));
  assert.equal(await readFile(path.join(applied.target, "assets/screenshots/01.png"), "utf8"), "screenshot");
  assert.equal(applied.backupPath, null);
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 plugins 경로 중간이 symlink이면 설치를 거부합니다.", async (context) => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-linked-root-"));
  const profile = path.join(temporary, "profile");
  const elsewhere = path.join(temporary, "elsewhere");
  await mkdir(profile);
  await mkdir(elsewhere);
  try {
    await symlink(elsewhere, path.join(profile, "plugins"), "dir");
  } catch (error) {
    context.skip(`symbolic links are unavailable: ${error.code}`);
    await rm(temporary, { recursive: true, force: true });
    return;
  }
  const { archivePath, digest } = await writeArchive(temporary);

  // when
  const result = installArchive({ archivePath, sha256: digest, profileDataDir: profile, apply: true });

  // then
  await assert.rejects(result, /must be a regular directory/);
  assert.deepEqual(await readdir(elsewhere), []);
  await rm(temporary, { recursive: true, force: true });
});

test("verifyArchive는 sidecar 파일의 digest로 archive를 검증합니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-sidecar-"));
  const { archivePath, digest } = await writeArchive(temporary);
  const sidecar = `${archivePath}.sha256`;
  await writeFile(sidecar, `${digest}  ${path.basename(archivePath)}\n`);
  const wrong = path.join(temporary, "wrong.sha256");
  await writeFile(wrong, `${"1".repeat(64)}  ${path.basename(archivePath)}\n`);
  const malformed = path.join(temporary, "malformed.sha256");
  await writeFile(malformed, "not a digest\n");

  // when
  const verified = await verifyArchive({ archivePath, sha256: sidecar });
  const mismatch = handled(verifyArchive({ archivePath, sha256: wrong }));
  const invalid = handled(verifyArchive({ archivePath, sha256: malformed }));

  // then
  assert.equal(verified.manifest.id, profileId);
  await assert.rejects(mismatch, /SHA-256 verification failed/);
  await assert.rejects(invalid, /64-character SHA-256 digest/);
  await rm(temporary, { recursive: true, force: true });
});

test("CLI의 --verify-only는 profile 없이 archive만 검증하고 --apply와 함께 쓸 수 없습니다.", async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-verify-only-"));
  const { archivePath, digest } = await writeArchive(temporary);

  // when
  const verified = spawnSync(process.execPath, [installerScript, "--verify-only", "--archive", archivePath, "--sha256", digest], { encoding: "utf8" });
  const conflicting = spawnSync(process.execPath, [installerScript, "--verify-only", "--apply", "--archive", archivePath, "--sha256", digest], { encoding: "utf8" });

  // then
  assert.equal(verified.status, 0, verified.stderr);
  assert.match(verified.stdout, /verified .*: sqlserver-plus /);
  assert.equal(conflicting.status, 1);
  assert.match(conflicting.stderr, /cannot be combined with --apply/);
  await rm(temporary, { recursive: true, force: true });
});

test("installer는 설치 직전 대상 디렉터리 상태가 바뀌어도 기존 plugin 백업을 복구합니다.", { skip: process.platform === "win32" }, async () => {
  // given
  const temporary = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-restore-"));
  const profile = path.join(temporary, "profile");
  const target = path.join(profile, "plugins", "drivers", profileId);
  await mkdir(target, { recursive: true });
  await writeFile(path.join(target, "preserve.txt"), "old installation");
  const { archivePath, digest } = await writeArchive(temporary);
  const renameImpl = async (source, destination) => {
    await rename(source, destination);
    if (source === target) await mkdir(target);
  };

  // when
  const result = installArchive({ archivePath, sha256: digest, profileDataDir: profile, apply: true, renameImpl });

  // then
  await assert.rejects(result, /target plugin directory changed during installation/);
  const siblings = await readdir(path.dirname(target));
  assert.deepEqual(siblings.filter(name => name.includes(".stage-")), []);
  assert.equal((await stat(target)).isDirectory(), true);
  await rm(temporary, { recursive: true, force: true });
});
