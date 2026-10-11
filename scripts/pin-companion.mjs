import { createHash, randomUUID } from "node:crypto";
import { lstat, readFile, rename, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { fingerprintSource } from "./companion-source.mjs";

const platforms = new Set(["darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64", "win-x64"]);
const digest = bytes => createHash("sha256").update(bytes).digest("hex");

export async function pinCompanion({ sourceRoot, archivePath, platform, lockPath }) {
  if (![sourceRoot, archivePath, lockPath].every(value => typeof value === "string" && path.isAbsolute(value))) {
    throw new Error("Source, archive, and lock paths must be absolute.");
  }
  if (!platforms.has(platform)) throw new Error("Unsupported companion platform.");
  const lock = JSON.parse(await readFile(lockPath, "utf8"));
  const pin = lock.sqlserverPlus;
  if (lock.schemaVersion !== 1 || !/^[a-f0-9]{64}$/.test(pin?.sourceSha256 ?? "")) {
    throw new Error("Invalid companion source pin.");
  }
  if (await fingerprintSource(sourceRoot) !== pin.sourceSha256) throw new Error("Companion source fingerprint mismatch.");
  const stat = await lstat(archivePath);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 512 * 1024 * 1024) throw new Error("Invalid companion archive.");
  const bytes = await readFile(archivePath);
  const { inspectPluginArchive } = await import(pathToFileURL(path.join(sourceRoot, "scripts/install.mjs")).href);
  const { entries, manifest, binaryName } = inspectPluginArchive(bytes);
  if (manifest.id !== "sqlserver-plus" || manifest.version !== pin.version) throw new Error("Companion identity mismatch.");
  if ((platform === "win-x64") !== binaryName.endsWith(".exe")) throw new Error("Companion executable platform mismatch.");
  const files = Object.fromEntries([...entries].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)
    .map(([name, entry]) => [name, digest(entry.data)]));
  const artifact = { archive: path.basename(archivePath), sha256: digest(bytes), bytes: bytes.length, files };
  if (await fingerprintSource(sourceRoot) !== pin.sourceSha256) throw new Error("Companion source changed during verification.");
  pin.platforms = { ...pin.platforms, [platform]: artifact };
  const temporary = `${lockPath}.${randomUUID()}.tmp`;
  try {
    await writeFile(temporary, `${JSON.stringify(lock, null, 2)}\n`, { flag: "wx" });
    await rename(temporary, lockPath);
  } finally {
    await rm(temporary, { force: true });
  }
  return artifact;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const options = {};
    const keys = { "--source-root": "sourceRoot", "--archive": "archivePath", "--platform": "platform", "--lock": "lockPath" };
    for (let index = 2; index < process.argv.length; index += 2) {
      const key = keys[process.argv[index]];
      const value = process.argv[index + 1];
      if (!key || !value || value.startsWith("--") || Object.hasOwn(options, key)) throw new Error("Invalid arguments.");
      options[key] = value;
    }
    const artifact = await pinCompanion(options);
    process.stdout.write(`Verified companion archive: ${artifact.sha256}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
