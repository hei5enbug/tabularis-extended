import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { chmod, lstat, mkdir, mkdtemp, readFile, rename, rm, stat, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createZip, readZip } from "../../scripts/archive.mjs";
import { installArchive, inspectPluginArchive } from "../../scripts/install.mjs";
import { packagePlugin } from "../../scripts/package.mjs";
import { preflightManifest } from "../../scripts/preflight.mjs";

const version = "1.0.0-beta.3+plus.1";
const profileId = "sqlserver-plus";

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
    homepage: "https://github.com/hei5enbug/tabularis-sqlserver-plus",
    documentation_url: "https://github.com/hei5enbug/tabularis-sqlserver-plus#readme",
    icon: "https://raw.githubusercontent.com/hei5enbug/tabularis-sqlserver-plus/main/sqlserver-icon.svg",
    support: { issues_url: "https://github.com/hei5enbug/tabularis-sqlserver-plus/issues" },
    capabilities: { sql_dialect: "mssql" },
    ui_extensions: [{ driver: profileId, module: "ui/dist/index.js" }],
    explain_parsers: [{ module: "explain/dist/index.iife.js" }],
  };
}

async function makeRepositoryFixture() {
  const root = await mkdtemp(path.join(os.tmpdir(), "sqlserver-plus-package-"));
  const files = new Map([
    [".tabularium", JSON.stringify(fixtureManifest(), null, 2)],
    ["Cargo.toml", `[package]\nname = "sqlserver-plugin"\nversion = "${version}"\n`],
    ["ui/package.json", JSON.stringify({ version, private: true })],
    ["explain/package.json", JSON.stringify({ version, private: true, devDependencies: { vitest: "4.1.11" } })],
    ["README.md", "SQL Server Plus README"],
    ["LICENSE", "Apache license"],
    ["vendor/mssql-tds-preview/LICENSE", "MIT license"],
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

function archiveEntries({ omit = [], manifest = fixtureManifest(), extra = [] } = {}) {
  const files = [
    ["sqlserver-plugin", "synthetic executable", 0o755],
    [".tabularium", JSON.stringify(manifest), 0o644],
    ["README.md", "SQL Server Plus README", 0o644],
    ["LICENSE", "Apache license", 0o644],
    ["ui/dist/index.js", "ui bundle", 0o644],
    ["explain/dist/index.iife.js", "parser bundle", 0o644],
    ["licenses/mssql-tds-preview-MIT.txt", "MIT license", 0o644],
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
  await writeFile(path.join(root, "ui/package.json"), JSON.stringify({ version: "1.0.0-beta.3", private: true }));

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
  for (const required of [".tabularium", "ui/dist/index.js", "explain/dist/index.iife.js", "LICENSE", "README.md", "licenses/mssql-tds-preview-MIT.txt"]) {
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
  const installer = fileURLToPath(new URL("../../scripts/install.mjs", import.meta.url));

  // when
  const result = spawnSync(process.execPath, [
    installer,
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
  assert.deepEqual((await (await import("node:fs/promises")).readdir(path.dirname(target))).filter(name => name.includes(".stage-") || name.includes(".backup-")), []);
  await rm(temporary, { recursive: true, force: true });
});
