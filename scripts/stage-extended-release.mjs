import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { copyFile, lstat, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { fingerprintSource } from "./companion-source.mjs";

const sha256 = bytes => createHash("sha256").update(bytes).digest("hex");
const extensions = { "darwin-arm64": ".dmg", "darwin-x64": ".dmg", "linux-x64": ".deb", "win-x64": ".exe" };

export async function stageRelease({ root, bundleRoot, output, companionArchive, platform }) {
  if (![root, bundleRoot, output, companionArchive].every(value => typeof value === "string" && path.isAbsolute(value))) {
    throw new Error("Release paths must be absolute.");
  }
  if (!extensions[platform]) throw new Error("Unsupported release platform.");
  const pkg = JSON.parse(await readFile(path.join(root, "package.json"), "utf8"));
  const config = JSON.parse(await readFile(path.join(root, "src-tauri/tauri.conf.json"), "utf8"));
  const cargo = await readFile(path.join(root, "src-tauri/Cargo.toml"), "utf8");
  const frontend = await readFile(path.join(root, "src/version.ts"), "utf8");
  const locks = await readFile(path.join(root, "integration/companions.json"));
  const pin = JSON.parse(locks).sqlserverPlus.platforms[platform];
  if (pkg.name !== "tabularis-extended" || config.productName !== "Tabularis Extended"
    || config.identifier !== "io.github.hei5enbug.tabularis.extended" || config.version !== pkg.version
    || cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1] !== pkg.version
    || frontend.match(/APP_VERSION\s*=\s*["']([^"']+)["']/)?.[1] !== pkg.version
    || config.bundle.createUpdaterArtifacts !== false || config.plugins.updater.endpoints.length !== 0) {
    throw new Error("Fork identity, version, or updater configuration is invalid.");
  }
  if (!pin || sha256(await readFile(companionArchive)) !== pin.sha256) throw new Error("Companion archive does not match the compiled pin.");
  const bundles = [];
  async function walk(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const fullPath = path.join(directory, entry.name);
      if (entry.isSymbolicLink()) throw new Error("Release bundles cannot contain symbolic links.");
      if (entry.isDirectory() && !entry.name.endsWith(".app")) await walk(fullPath);
      else if (entry.isFile() && entry.name.endsWith(extensions[platform])) bundles.push(fullPath);
    }
  }
  if (!(await lstat(bundleRoot)).isDirectory()) throw new Error("Bundle root must be a directory.");
  await walk(bundleRoot);
  if (bundles.length !== 1) throw new Error("Expected exactly one native installer bundle.");
  const sourceTreeSha256 = await fingerprintSource(root, { allowedSymlinks: {
    ".agents/skills": "../.claude/skills", "CLAUDE.md": "AGENTS.md",
  } });
  let sourceCommit = null;
  try {
    sourceCommit = execFileSync("git", ["rev-parse", "--verify", "HEAD"], { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {}
  const sourceDirty = execFileSync("git", ["status", "--porcelain"], { cwd: root, encoding: "utf8" }).trim().length > 0;
  await mkdir(output, { recursive: false });
  const assets = [];
  for (const file of [...bundles, companionArchive]) {
    const name = path.basename(file);
    if (assets.some(asset => asset.name === name)) throw new Error("Release asset names collide.");
    const stat = await lstat(file);
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error("Release assets must be regular files.");
    await copyFile(file, path.join(output, name));
    const bytes = await readFile(path.join(output, name));
    const digest = sha256(bytes);
    assets.push({ name, sha256: digest, bytes: bytes.length });
    const sidecar = Buffer.from(`${digest}  ${name}\n`);
    await writeFile(path.join(output, `${name}.sha256`), sidecar, { flag: "wx" });
    assets.push({ name: `${name}.sha256`, sha256: sha256(sidecar), bytes: sidecar.length });
  }
  await writeFile(path.join(output, "companions.json"), locks, { flag: "wx" });
  assets.push({ name: "companions.json", sha256: sha256(locks), bytes: locks.length });
  const receipt = { schemaVersion: 1, version: pkg.version, platform, sourceCommit, sourceDirty, sourceTreeSha256,
    signed: false, notarized: false, automaticUpdates: false, assets };
  const receiptBytes = Buffer.from(`${JSON.stringify(receipt, null, 2)}\n`);
  await writeFile(path.join(output, "release.json"), receiptBytes, { flag: "wx" });
  const hashes = [...assets, { name: "release.json", sha256: sha256(receiptBytes) }];
  await writeFile(path.join(output, "SHA256SUMS"), hashes.map(asset => `${asset.sha256}  ${asset.name}\n`).join(""), { flag: "wx" });
  return receipt;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const keys = { "--root": "root", "--bundle-root": "bundleRoot", "--output": "output", "--companion-archive": "companionArchive", "--platform": "platform" };
    const options = {};
    for (let index = 2; index < process.argv.length; index += 2) {
      const key = keys[process.argv[index]];
      const value = process.argv[index + 1];
      if (!key || !value || value.startsWith("--") || Object.hasOwn(options, key)) throw new Error("Invalid arguments.");
      options[key] = value;
    }
    const receipt = await stageRelease(options);
    process.stdout.write(`Staged Tabularis Extended ${receipt.version} (${receipt.platform}).\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
