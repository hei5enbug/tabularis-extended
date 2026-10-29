import { lstat, readFile, realpath } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { normalizeZipPath } from "./archive.mjs";

export const REQUIRED_ID = "sqlserver-plus";
export const REQUIRED_NAME = "SQL Server Plus";
export const REQUIRED_EXECUTABLE = "sqlserver-plugin";

// Connection authentication is a per-connection parameter owned by the UI
// extension; the plugin-level settings contract must never carry it.
const FORBIDDEN_SETTING_KEYS = new Set([
  "auth_mode",
  "auth_source",
  "tenant_id",
  "entra_object_id",
  "azure_cli_path",
  "integrated_auth",
]);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function parseJson(text, label) {
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new Error(`Could not parse ${label} JSON.`, { cause: error });
  }
}

function checkRelativeFilePath(value, label) {
  assert(typeof value === "string", `${label} path is missing.`);
  const normalized = normalizeZipPath(value);
  assert(normalized === value, `${label} path is not normalized.`);
  return normalized;
}

function isInside(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

export async function readRegularFileUnder(root, relativePath, label) {
  const physicalRoot = await realpath(root);
  const fullPath = path.resolve(physicalRoot, ...relativePath.split("/"));
  assert(isInside(physicalRoot, fullPath), `${label} path escapes the asset root.`);

  let current = physicalRoot;
  const segments = relativePath.split("/");
  for (const [index, segment] of segments.entries()) {
    current = path.join(current, segment);
    let stat;
    try {
      stat = await lstat(current);
    } catch (error) {
      throw new Error(`${label} file is missing: ${relativePath}`, { cause: error });
    }
    assert(!stat.isSymbolicLink(), `${label} must not use a symbolic link: ${relativePath}`);
    if (index < segments.length - 1) assert(stat.isDirectory(), `${label} parent path must be a directory: ${relativePath}`);
    else assert(stat.isFile(), `${label} must be a regular file: ${relativePath}`);
  }

  const physicalFile = await realpath(fullPath);
  assert(isInside(physicalRoot, physicalFile), `${label} resolves outside the asset root.`);
  return readFile(physicalFile);
}

function getCargoMetadata(text) {
  const section = text.match(/^\[package\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m)?.[1] ?? "";
  const name = section.match(/^name\s*=\s*"([^"]+)"\s*$/m)?.[1];
  const version = section.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  return { name, version };
}

// Identity rules shared by the repository manifest and the manifest inside a
// package archive, so a package can never carry what preflight rejects.
export function assertManifestIdentity(manifest, label) {
  assert(manifest !== null && typeof manifest === "object" && !Array.isArray(manifest), `${label} must be a JSON object.`);
  assert(manifest.id === REQUIRED_ID, `${label} id must be ${REQUIRED_ID}.`);
  assert(manifest.name === REQUIRED_NAME, `${label} name must be ${REQUIRED_NAME}.`);
  assert(manifest.kind === "driver", `${label} kind must be driver.`);
  assert(manifest.engine === "sqlserver", `${label} engine must be sqlserver.`);
  assert(typeof manifest.version === "string" && manifest.version.length > 0, `${label} version is missing.`);
  assert(typeof manifest.min_runtime_version === "string" && manifest.min_runtime_version.length > 0, `${label} min_runtime_version is missing.`);
  assert(manifest.executable === REQUIRED_EXECUTABLE, `${label} executable must be ${REQUIRED_EXECUTABLE}.`);
  assert(manifest.capabilities?.sql_dialect === "mssql", `${label} SQL dialect must be mssql.`);
  assert(Array.isArray(manifest.ui_extensions) && manifest.ui_extensions.length > 0, `${label} must declare at least one UI extension.`);
  for (const extension of manifest.ui_extensions) {
    assert(extension.driver === REQUIRED_ID, `Every ${label} UI extension driver must be ${REQUIRED_ID}.`);
  }
  assert(Array.isArray(manifest.explain_parsers) && manifest.explain_parsers.length > 0, `${label} must declare at least one EXPLAIN parser.`);
  for (const setting of manifest.settings ?? []) {
    assert(!FORBIDDEN_SETTING_KEYS.has(setting?.key), `Authentication field ${setting.key} belongs to per-connection params, not ${label} settings.`);
  }
}

export function manifestAssetPaths(manifest) {
  return [manifest.readme, ...manifest.ui_extensions.map(item => item.module), ...manifest.explain_parsers.map(item => item.module)];
}

export async function preflightManifest({ root = process.cwd(), assetRoot = root, metadataOnly = false, expectedVersion } = {}) {
  root = path.resolve(root);
  assetRoot = path.resolve(assetRoot);
  const [manifestBytes, cargoBytes, uiBytes, explainBytes] = await Promise.all([
    readRegularFileUnder(root, ".tabularium", ".tabularium"),
    readRegularFileUnder(root, "Cargo.toml", "Cargo.toml"),
    readRegularFileUnder(root, "ui/package.json", "ui/package.json"),
    readRegularFileUnder(root, "explain/package.json", "explain/package.json"),
  ]);
  const manifest = parseJson(manifestBytes.toString("utf8"), ".tabularium");
  const ui = parseJson(uiBytes.toString("utf8"), "ui/package.json");
  const explain = parseJson(explainBytes.toString("utf8"), "explain/package.json");
  const cargo = getCargoMetadata(cargoBytes.toString("utf8"));

  assertManifestIdentity(manifest, ".tabularium");
  assert(cargo.name === REQUIRED_EXECUTABLE, `Cargo package name must be ${REQUIRED_EXECUTABLE}.`);
  assert(typeof cargo.version === "string", "Cargo package version is missing.");
  assert(manifest.version === cargo.version, `.tabularium version ${manifest.version} differs from Cargo.toml (${cargo.version}).`);
  if (expectedVersion) assert(manifest.version === expectedVersion, `Release tag version ${expectedVersion} does not match the manifest.`);
  assert(ui.version === cargo.version, `UI package version must be ${cargo.version}.`);
  assert(explain.version === cargo.version, `EXPLAIN package version must be ${cargo.version}.`);
  assert(ui.private === true, "UI package must be private.");
  assert(explain.private === true, "EXPLAIN package must be private.");
  assert(!Object.hasOwn(explain, "publishConfig"), "EXPLAIN package must not have publishConfig.");

  const readme = checkRelativeFilePath(manifest.readme, "manifest README");
  for (const extension of manifest.ui_extensions) checkRelativeFilePath(extension.module, "UI extension module");
  for (const parser of manifest.explain_parsers) checkRelativeFilePath(parser.module, "EXPLAIN parser module");
  const localPaths = manifestAssetPaths({ ...manifest, readme });
  if (!metadataOnly) {
    for (const relativePath of new Set(localPaths)) {
      await readRegularFileUnder(assetRoot, relativePath, "Manifest-referenced asset");
    }
  }

  return { manifest, assetPaths: [...new Set(localPaths)] };
}

function parseArguments(argv) {
  const values = { root: process.cwd(), assetRoot: process.cwd(), metadataOnly: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--metadata-only") {
      values.metadataOnly = true;
    } else if (argument === "--expected-version" || argument === "--root" || argument === "--asset-root") {
      const value = argv[index + 1];
      if (!value || value.startsWith("--")) throw new Error(`${argument} requires a value.`);
      const key = { "--expected-version": "expectedVersion", "--root": "root", "--asset-root": "assetRoot" }[argument];
      values[key] = value;
      index += 1;
    } else {
      throw new Error(`Unknown argument: ${argument}`);
    }
  }
  return values;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  try {
    const { manifest } = await preflightManifest(parseArguments(process.argv.slice(2)));
    process.stdout.write(`preflight ok: ${manifest.id} ${manifest.version}, runtime >= ${manifest.min_runtime_version}, executable ${manifest.executable}\n`);
  } catch (error) {
    process.stderr.write(`Manifest preflight failed: ${error.message}\n`);
    process.exitCode = 1;
  }
}
