import { lstat, readFile, realpath } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { normalizeZipPath } from "./archive.mjs";

export const REQUIRED_ID = "sqlserver-plus";
export const REQUIRED_VERSION = "1.0.0-beta.3+plus.1";
export const REQUIRED_MIN_RUNTIME = "0.27.0";

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

export async function readRegularFileUnder(root, relativePath, label) {
  const physicalRoot = await realpath(root);
  const fullPath = path.resolve(physicalRoot, ...relativePath.split("/"));
  const relative = path.relative(physicalRoot, fullPath);
  assert(relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative), `${label} path escapes the asset root.`);

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
  const physicalRelative = path.relative(physicalRoot, physicalFile);
  assert(physicalRelative !== ".." && !physicalRelative.startsWith(`..${path.sep}`) && !path.isAbsolute(physicalRelative), `${label} resolves outside the asset root.`);
  return readFile(physicalFile);
}

function getCargoMetadata(text) {
  const name = text.match(/^name\s*=\s*"([^"]+)"\s*$/m)?.[1];
  const version = text.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  return { name, version };
}

function collectUrls(value, urls = []) {
  if (typeof value === "string" && /^https?:\/\//i.test(value)) urls.push(value);
  else if (Array.isArray(value)) value.forEach(item => collectUrls(item, urls));
  else if (value && typeof value === "object") Object.values(value).forEach(item => collectUrls(item, urls));
  return urls;
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
  const manifestText = manifestBytes.toString("utf8");
  const cargoText = cargoBytes.toString("utf8");
  const uiText = uiBytes.toString("utf8");
  const explainText = explainBytes.toString("utf8");
  const manifest = parseJson(manifestText, ".tabularium");
  const ui = parseJson(uiText, "ui/package.json");
  const explain = parseJson(explainText, "explain/package.json");
  const cargo = getCargoMetadata(cargoText);

  assert(manifest.id === REQUIRED_ID, `.tabularium id must be ${REQUIRED_ID}.`);
  assert(manifest.name === "SQL Server Plus", ".tabularium name must be SQL Server Plus.");
  assert(manifest.kind === "driver", ".tabularium kind must be driver.");
  assert(manifest.engine === "sqlserver", ".tabularium engine must be sqlserver.");
  assert(manifest.version === REQUIRED_VERSION, `.tabularium version must be ${REQUIRED_VERSION}.`);
  if (expectedVersion) assert(manifest.version === expectedVersion, `Release tag version ${expectedVersion} does not match the manifest.`);
  assert(manifest.min_runtime_version === REQUIRED_MIN_RUNTIME, `min_runtime_version must be ${REQUIRED_MIN_RUNTIME}.`);
  assert(manifest.executable === "sqlserver-plugin", "Manifest executable must be sqlserver-plugin.");
  assert(manifest.capabilities?.sql_dialect === "mssql", "SQL dialect must be mssql.");

  assert(cargo.name === "sqlserver-plugin", "Cargo package name must be sqlserver-plugin.");
  assert(cargo.version === REQUIRED_VERSION, `Cargo version must be ${REQUIRED_VERSION}.`);
  assert(ui.version === REQUIRED_VERSION, `UI package version must be ${REQUIRED_VERSION}.`);
  assert(explain.version === REQUIRED_VERSION, `EXPLAIN package version must be ${REQUIRED_VERSION}.`);
  assert(ui.private === true, "UI package must be private.");
  assert(explain.private === true, "EXPLAIN package must be private.");
  assert(!Object.hasOwn(explain, "publishConfig"), "EXPLAIN package must not have publishConfig.");
  assert(explain.devDependencies?.vitest === "4.1.11", "EXPLAIN Vitest must use the security-fixed version 4.1.11.");

  const uiExtensions = manifest.ui_extensions;
  assert(Array.isArray(uiExtensions) && uiExtensions.length > 0, "Manifest must declare at least one UI extension.");
  for (const extension of uiExtensions) {
    assert(extension.driver === REQUIRED_ID, "Every UI extension driver must be sqlserver-plus.");
    checkRelativeFilePath(extension.module, "UI extension module");
  }
  const parsers = manifest.explain_parsers;
  assert(Array.isArray(parsers) && parsers.length > 0, "Manifest must declare at least one EXPLAIN parser.");
  for (const parser of parsers) checkRelativeFilePath(parser.module, "EXPLAIN parser module");
  const readme = checkRelativeFilePath(manifest.readme, "manifest README");

  const localPaths = [readme, ...uiExtensions.map(item => item.module), ...parsers.map(item => item.module)];
  if (!metadataOnly) {
    for (const relativePath of new Set(localPaths)) {
      await readRegularFileUnder(assetRoot, relativePath, "Manifest-referenced asset");
    }
  }

  const { $schema: _schemaUrl, ...publicManifest } = manifest;
  const forkUrls = collectUrls(publicManifest);
  assert(forkUrls.length > 0, "Manifest must contain fork URLs.");
  assert(forkUrls.every(url => url.includes("github.com/hei5enbug/tabularis-sqlserver-plus") || url.includes("raw.githubusercontent.com/hei5enbug/tabularis-sqlserver-plus")), "Manifest URLs must point to hei5enbug/tabularis-sqlserver-plus.");

  return { manifest, assetPaths: [...new Set(localPaths)] };
}

function parseArguments(argv) {
  const values = { root: process.cwd(), assetRoot: process.cwd(), metadataOnly: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--metadata-only") {
      values.metadataOnly = true;
    } else if (argument === "--expected-version") {
      const value = argv[index + 1];
      if (!value || value.startsWith("--")) throw new Error(`${argument} requires a value.`);
      values.expectedVersion = value;
      index += 1;
    } else if (argument === "--root" || argument === "--asset-root") {
      const value = argv[index + 1];
      if (!value || value.startsWith("--")) throw new Error(`${argument} requires a value.`);
      values[argument === "--root" ? "root" : "assetRoot"] = value;
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
    process.stdout.write(`Manifest preflight passed: ${manifest.id} ${manifest.version}\n`);
  } catch (error) {
    process.stderr.write(`Manifest preflight failed: ${error.message}\n`);
    process.exitCode = 1;
  }
}
