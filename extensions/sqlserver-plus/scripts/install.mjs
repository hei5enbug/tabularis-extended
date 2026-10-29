import { createHash, randomUUID } from "node:crypto";
import { lstat, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readZip } from "./archive.mjs";
import { REQUIRED_EXECUTABLE, REQUIRED_ID, assertManifestIdentity, manifestAssetPaths } from "./preflight.mjs";

const REQUIRED_FILES = [
  ".tabularium",
  "README.md",
  "LICENSE",
  "ui/dist/index.js",
  "explain/dist/index.iife.js",
];
const BINARY_NAMES = [REQUIRED_EXECUTABLE, `${REQUIRED_EXECUTABLE}.exe`];
const ASSET_PREFIX = "assets/";

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function parseManifest(entries) {
  const manifestEntry = entries.get(".tabularium");
  assert(manifestEntry, "ZIP is missing its .tabularium manifest.");
  let manifest;
  try {
    manifest = JSON.parse(manifestEntry.data.toString("utf8"));
  } catch (error) {
    throw new Error("ZIP .tabularium manifest contains invalid JSON.", { cause: error });
  }
  assertManifestIdentity(manifest, "ZIP manifest");
  assert(typeof manifest.readme === "string", "ZIP manifest README path is missing.");
  for (const relativePath of manifestAssetPaths(manifest)) {
    assert(typeof relativePath === "string" && !relativePath.startsWith("/") && !relativePath.includes("\\"), "ZIP manifest contains an invalid asset path.");
    assert(entries.has(relativePath), `ZIP manifest references a missing file: ${relativePath}`);
  }
  return manifest;
}

export function inspectPluginArchive(buffer) {
  const entries = readZip(buffer);
  const binaryNames = BINARY_NAMES.filter(name => entries.has(name));
  assert(binaryNames.length === 1, `ZIP must contain exactly one ${BINARY_NAMES[0]} or ${BINARY_NAMES[1]} executable.`);
  const allowed = new Set([...REQUIRED_FILES, ...binaryNames]);
  for (const name of entries.keys()) {
    assert(allowed.has(name) || name.startsWith(ASSET_PREFIX), `ZIP contains a disallowed file: ${name}`);
  }
  for (const name of REQUIRED_FILES) assert(entries.has(name), `ZIP is missing a required file: ${name}`);
  const manifest = parseManifest(entries);
  return { entries, manifest, binaryName: binaryNames[0] };
}

async function readRegularInput(filePath, label) {
  const fullPath = path.resolve(filePath);
  const stat = await lstat(fullPath);
  assert(stat.isFile() && !stat.isSymbolicLink(), `${label} must be a regular file, not a symbolic link.`);
  return readFile(fullPath);
}

async function expectedDigest(value) {
  if (/^[a-f0-9]{64}$/i.test(value)) return value.toLowerCase();
  const sidecar = (await readRegularInput(value, "SHA-256 sidecar")).toString("utf8").trim();
  const match = sidecar.match(/^([a-f0-9]{64})(?:\s+\*?.+)?$/i);
  assert(match, "SHA-256 sidecar must contain a 64-character SHA-256 digest.");
  return match[1].toLowerCase();
}

export async function verifyArchive({ archivePath, sha256 }) {
  assert(typeof archivePath === "string" && archivePath.length > 0, "--archive is required.");
  assert(typeof sha256 === "string" && sha256.length > 0, "--sha256 is required.");
  const archive = await readRegularInput(archivePath, "Plugin archive");
  const digest = createHash("sha256").update(archive).digest("hex");
  assert(digest === await expectedDigest(sha256), "Plugin archive SHA-256 verification failed.");
  return inspectPluginArchive(archive);
}

async function directoryState(directoryPath) {
  try {
    const stat = await lstat(directoryPath);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`Install path component must be a regular directory: ${directoryPath}`);
    return "directory";
  } catch (error) {
    if (error.code === "ENOENT") return "missing";
    throw error;
  }
}

async function ensureDirectory(directoryPath) {
  const state = await directoryState(directoryPath);
  if (state === "missing") await mkdir(directoryPath);
}

function resolvePluginsRoot({ profileDataDir, pluginsRoot }) {
  assert((profileDataDir === undefined) !== (pluginsRoot === undefined), "Exactly one of --profile-data-dir or --plugins-root is required.");
  if (profileDataDir !== undefined) {
    assert(typeof profileDataDir === "string" && path.isAbsolute(profileDataDir), "An absolute --profile-data-dir is required.");
    const profileRoot = path.resolve(profileDataDir);
    return { anchor: profileRoot, anchorLabel: "The profile data directory", pluginsRoot: path.join(profileRoot, "plugins") };
  }
  assert(typeof pluginsRoot === "string" && path.isAbsolute(pluginsRoot), "An absolute --plugins-root is required.");
  const root = path.resolve(pluginsRoot);
  return { anchor: path.dirname(root), anchorLabel: "The parent of the plugins root", pluginsRoot: root };
}

export async function installArchive({ archivePath, sha256, profileDataDir, pluginsRoot, apply = false, renameImpl = rename }) {
  const inspected = await verifyArchive({ archivePath, sha256 });
  const resolved = resolvePluginsRoot({ profileDataDir, pluginsRoot });

  assert(await directoryState(resolved.anchor) === "directory", `${resolved.anchorLabel} must exist.`);
  const driversRoot = path.join(resolved.pluginsRoot, "drivers");
  const target = path.join(driversRoot, REQUIRED_ID);
  const pluginsState = await directoryState(resolved.pluginsRoot);
  const driversState = pluginsState === "directory" ? await directoryState(driversRoot) : "missing";
  const targetState = driversState === "directory" ? await directoryState(target) : "missing";
  const summary = {
    id: inspected.manifest.id,
    version: inspected.manifest.version,
    target,
    files: [...inspected.entries.keys()].sort(),
    applied: false,
    backupPath: null,
  };
  if (!apply) return summary;

  await ensureDirectory(resolved.pluginsRoot);
  await ensureDirectory(driversRoot);
  const stage = await mkdtemp(path.join(driversRoot, `.${REQUIRED_ID}.stage-`));
  let stageExists = true;
  let backup = null;
  let existingMoved = false;
  try {
    for (const [relativePath, entry] of inspected.entries) {
      const destination = path.resolve(stage, ...relativePath.split("/"));
      const relative = path.relative(stage, destination);
      assert(relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative), "ZIP entry path escapes the staging root.");
      await mkdir(path.dirname(destination), { recursive: true });
      const mode = relativePath === inspected.binaryName ? 0o755 : 0o644;
      await writeFile(destination, entry.data, { flag: "wx", mode });
    }

    if (targetState === "directory") {
      backup = path.join(driversRoot, `.${REQUIRED_ID}.backup-${process.pid}-${randomUUID()}`);
      await renameImpl(target, backup);
      existingMoved = true;
      summary.backupPath = backup;
    }
    assert(await directoryState(target) === "missing", "The target plugin directory changed during installation.");
    await renameImpl(stage, target);
    stageExists = false;
    summary.applied = true;
    return summary;
  } catch (error) {
    if (stageExists) await rm(stage, { recursive: true, force: true }).catch(() => {});
    if (existingMoved && backup) {
      try {
        await renameImpl(backup, target);
        backup = null;
      } catch (rollbackError) {
        throw new Error(`Installation and rollback both failed. The backup was retained at: ${backup}`, { cause: new AggregateError([error, rollbackError]) });
      }
    }
    throw error;
  }
}

function parseArguments(argv) {
  const result = { apply: false, verifyOnly: false };
  const keys = {
    "--archive": "archivePath",
    "--sha256": "sha256",
    "--profile-data-dir": "profileDataDir",
    "--plugins-root": "pluginsRoot",
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--apply") {
      result.apply = true;
      continue;
    }
    if (argument === "--verify-only") {
      result.verifyOnly = true;
      continue;
    }
    if (!Object.hasOwn(keys, argument)) throw new Error(`Unknown argument: ${argument}`);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`${argument} requires a value.`);
    result[keys[argument]] = value;
    index += 1;
  }
  assert(!(result.verifyOnly && result.apply), "--verify-only cannot be combined with --apply.");
  return result;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  try {
    const { verifyOnly, ...options } = parseArguments(process.argv.slice(2));
    if (verifyOnly) {
      const { manifest } = await verifyArchive(options);
      process.stdout.write(`verified ${options.archivePath}: ${manifest.id} ${manifest.version}\n`);
    } else {
      const result = await installArchive(options);
      const action = result.applied ? "Installation complete" : "Dry run (no files changed)";
      process.stdout.write(`${action}: ${result.id} ${result.version}\nTarget: ${result.target}\n`);
      if (result.backupPath) process.stdout.write(`Previous package backup: ${result.backupPath}\n`);
      if (!result.applied) process.stdout.write("Add --apply to apply this verified installation.\n");
    }
  } catch (error) {
    process.stderr.write(`Installation validation failed: ${error.message}\n`);
    process.exitCode = 1;
  }
}
