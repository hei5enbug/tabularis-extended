import { createHash, randomUUID } from "node:crypto";
import { lstat, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readZip } from "./archive.mjs";
import { REQUIRED_ID, REQUIRED_MIN_RUNTIME, REQUIRED_VERSION } from "./preflight.mjs";

const REQUIRED_FILES = [
  ".tabularium",
  "README.md",
  "LICENSE",
  "ui/dist/index.js",
  "explain/dist/index.iife.js",
  "licenses/mssql-tds-preview-MIT.txt",
];

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
  assert(manifest.id === REQUIRED_ID, `ZIP manifest id must be ${REQUIRED_ID}.`);
  assert(manifest.name === "SQL Server Plus", "ZIP manifest name must be SQL Server Plus.");
  assert(manifest.version === REQUIRED_VERSION, `ZIP manifest version must be ${REQUIRED_VERSION}.`);
  assert(manifest.min_runtime_version === REQUIRED_MIN_RUNTIME, `ZIP minimum runtime must be ${REQUIRED_MIN_RUNTIME}.`);
  assert(manifest.kind === "driver" && manifest.engine === "sqlserver", "ZIP manifest driver type is invalid.");
  assert(manifest.executable === "sqlserver-plugin", "ZIP manifest executable is invalid.");
  assert(manifest.capabilities?.sql_dialect === "mssql", "ZIP manifest SQL dialect must be mssql.");

  const referencedPaths = [manifest.readme, ...(manifest.ui_extensions ?? []).map(item => item.module), ...(manifest.explain_parsers ?? []).map(item => item.module)];
  assert(typeof manifest.readme === "string", "ZIP manifest README path is missing.");
  assert(Array.isArray(manifest.ui_extensions) && manifest.ui_extensions.length > 0, "ZIP manifest has no UI extension.");
  assert(Array.isArray(manifest.explain_parsers) && manifest.explain_parsers.length > 0, "ZIP manifest has no EXPLAIN parser.");
  for (const extension of manifest.ui_extensions) {
    assert(extension.driver === REQUIRED_ID, "ZIP manifest UI extension driver ID does not match.");
  }
  for (const relativePath of referencedPaths) {
    assert(typeof relativePath === "string" && !relativePath.startsWith("/") && !relativePath.includes("\\"), "ZIP manifest contains an invalid asset path.");
    assert(entries.has(relativePath), `ZIP manifest references a missing file: ${relativePath}`);
  }
  return manifest;
}

export function inspectPluginArchive(buffer) {
  const entries = readZip(buffer);
  const binaryNames = ["sqlserver-plugin", "sqlserver-plugin.exe"].filter(name => entries.has(name));
  assert(binaryNames.length === 1, "ZIP must contain exactly one sqlserver-plugin or sqlserver-plugin.exe executable.");
  const allowed = new Set([...REQUIRED_FILES, ...binaryNames]);
  for (const name of entries.keys()) assert(allowed.has(name), `ZIP contains a disallowed file: ${name}`);
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

export async function installArchive({ archivePath, sha256, profileDataDir, apply = false, renameImpl = rename }) {
  assert(typeof archivePath === "string" && archivePath.length > 0, "--archive is required.");
  assert(typeof sha256 === "string" && sha256.length > 0, "--sha256 is required.");
  assert(typeof profileDataDir === "string" && path.isAbsolute(profileDataDir), "An absolute --profile-data-dir is required.");

  const archive = await readRegularInput(archivePath, "Plugin archive");
  const digest = createHash("sha256").update(archive).digest("hex");
  assert(digest === await expectedDigest(sha256), "Plugin archive SHA-256 verification failed.");
  const inspected = inspectPluginArchive(archive);

  const profileRoot = path.resolve(profileDataDir);
  assert(await directoryState(profileRoot) === "directory", "The profile data directory must exist.");
  const pluginsRoot = path.join(profileRoot, "plugins");
  const driversRoot = path.join(pluginsRoot, "drivers");
  const target = path.join(driversRoot, REQUIRED_ID);
  const pluginsState = await directoryState(pluginsRoot);
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

  await ensureDirectory(pluginsRoot);
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
  const result = { apply: false };
  const keys = {
    "--archive": "archivePath",
    "--sha256": "sha256",
    "--profile-data-dir": "profileDataDir",
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--apply") {
      result.apply = true;
      continue;
    }
    if (!["--archive", "--sha256", "--profile-data-dir"].includes(argument)) {
      throw new Error(`Unknown argument: ${argument}`);
    }
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`${argument} requires a value.`);
    result[keys[argument]] = value;
    index += 1;
  }
  return result;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  try {
    const result = await installArchive(parseArguments(process.argv.slice(2)));
    const action = result.applied ? "Installation complete" : "Dry run (no files changed)";
    process.stdout.write(`${action}: ${result.id} ${result.version}\nTarget: ${result.target}\n`);
    if (result.backupPath) process.stdout.write(`Previous package backup: ${result.backupPath}\n`);
    if (!result.applied) process.stdout.write("Add --apply to apply this verified installation.\n");
  } catch (error) {
    process.stderr.write(`Installation validation failed: ${error.message}\n`);
    process.exitCode = 1;
  }
}
