#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

export const STABLE_TAG = /^v\d+\.\d+\.\d+$/;
export const FORK_VERSION = /^\d+\.\d+\.\d+\+extended\.\d+$/;

const FORK_NAME = "tabularis-extended";
const FORK_PRODUCT = "Tabularis Extended";
const FORK_IDENTIFIER = "io.github.hei5enbug.tabularis.extended";
const FORBIDDEN_NAME = new RegExp(["Data", "Grip"].join(""), "i");

export function git(cwd, args, { allowFail = false } = {}) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", maxBuffer: 1 << 30 });
  if (result.error) throw result.error;
  if (result.status !== 0 && !allowFail) {
    throw new Error(`git ${args.join(" ")} failed:\n${(result.stderr || result.stdout).trim()}`);
  }
  return { status: result.status, stdout: result.stdout, stderr: result.stderr };
}

export function compareTags(a, b) {
  const left = a.slice(1).split(".").map(Number);
  const right = b.slice(1).split(".").map(Number);
  for (let index = 0; index < 3; index += 1) {
    if (left[index] !== right[index]) return left[index] - right[index];
  }
  return 0;
}

export function newestMergedStableTag(root, allowed = null) {
  const tags = git(root, ["tag", "--merged", "HEAD", "--list", "v*"]).stdout
    .split("\n")
    .filter((tag) => STABLE_TAG.test(tag) && (!allowed || allowed.has(tag)));
  return tags.sort(compareTags).at(-1) ?? null;
}

function packageRange(text) {
  const header = /^\[package\][^\n]*\n/m.exec(text);
  if (!header) return null;
  const start = header.index + header[0].length;
  const next = text.slice(start).search(/^\[/m);
  return [start, next < 0 ? text.length : start + next];
}

function replaceFirstJsonVersion(text, version, file) {
  const updated = text.replace(/("version"\s*:\s*)"[^"]*"/, `$1"${version}"`);
  if (JSON.parse(updated).version !== version) throw new Error(`${file}: top-level version not found`);
  return updated;
}

export const versionSources = [
  {
    file: "package.json",
    read: (text) => JSON.parse(text).version,
    write: (text, version) => replaceFirstJsonVersion(text, version, "package.json"),
  },
  {
    file: "src-tauri/tauri.conf.json",
    read: (text) => JSON.parse(text).version,
    write: (text, version) => replaceFirstJsonVersion(text, version, "src-tauri/tauri.conf.json"),
  },
  {
    file: "src-tauri/Cargo.toml",
    read: (text) => {
      const range = packageRange(text);
      return range ? /^\s*version\s*=\s*"([^"]*)"/m.exec(text.slice(...range))?.[1] : undefined;
    },
    write: (text, version) => {
      const range = packageRange(text);
      if (!range) throw new Error("src-tauri/Cargo.toml has no [package] section");
      const body = text.slice(...range);
      if (!/^\s*version\s*=\s*"[^"]*"/m.test(body)) throw new Error("src-tauri/Cargo.toml has no package version");
      return text.slice(0, range[0]) + body.replace(/^(\s*version\s*=\s*)"[^"]*"/m, `$1"${version}"`) + text.slice(range[1]);
    },
  },
  {
    file: "src-tauri/Cargo.lock",
    read: (text) => /\[\[package\]\]\r?\nname = "tabularis-extended"\r?\nversion = "([^"]*)"/.exec(text)?.[1],
    write: (text, version) => {
      const pattern = /(\[\[package\]\]\r?\nname = "tabularis-extended"\r?\nversion = )"[^"]*"/;
      if (!pattern.test(text)) throw new Error("src-tauri/Cargo.lock has no tabularis-extended entry");
      return text.replace(pattern, `$1"${version}"`);
    },
  },
  {
    file: "src/version.ts",
    read: (text) => /APP_VERSION\s*=\s*"([^"]*)"/.exec(text)?.[1],
    write: (text, version) => {
      if (!/APP_VERSION\s*=\s*"[^"]*"/.test(text)) throw new Error("src/version.ts has no APP_VERSION");
      return text.replace(/(APP_VERSION\s*=\s*)"[^"]*"/, `$1"${version}"`);
    },
  },
];

const rustConstants = [
  ["src-tauri/src/paths.rs", "APP_DIR_NAME"],
  ["src-tauri/src/keychain_utils.rs", "SERVICE_NAME"],
  ["src-tauri/src/plugins/deep_link.rs", "DEEP_LINK_SCHEME"],
  ["src-tauri/src/mcp/install.rs", "MCP_SERVER_KEY"],
];

function readText(root, file, problems) {
  const path = join(root, file);
  if (!existsSync(path)) {
    problems.push(`${file}: file is missing`);
    return null;
  }
  return readFileSync(path, "utf8");
}

function readJson(root, file, problems) {
  const text = readText(root, file, problems);
  if (text === null) return null;
  try {
    return JSON.parse(text);
  } catch (error) {
    problems.push(`${file}: invalid JSON (${error.message})`);
    return null;
  }
}

function checkVersions(root, problems) {
  const found = [];
  for (const source of versionSources) {
    const text = readText(root, source.file, problems);
    if (text === null) continue;
    let version;
    try {
      version = source.read(text);
    } catch (error) {
      problems.push(`${source.file}: cannot read the version (${error.message})`);
      continue;
    }
    if (!version) problems.push(`${source.file}: no version found`);
    else if (!FORK_VERSION.test(version)) problems.push(`${source.file}: version "${version}" must look like X.Y.Z+extended.N`);
    else found.push([source.file, version]);
  }
  if (new Set(found.map(([, version]) => version)).size > 1) {
    problems.push(`versions differ: ${found.map(([file, version]) => `${file}=${version}`).join(", ")}`);
  }
}

function checkConfigs(root, problems) {
  const pkg = readJson(root, "package.json", problems);
  if (pkg && pkg.name !== FORK_NAME) problems.push(`package.json: name is "${pkg.name}", expected "${FORK_NAME}"`);

  const tauri = readJson(root, "src-tauri/tauri.conf.json", problems);
  if (!tauri) return;
  const file = "src-tauri/tauri.conf.json";
  if (tauri.productName !== FORK_PRODUCT) problems.push(`${file}: productName is "${tauri.productName}", expected "${FORK_PRODUCT}"`);
  if (tauri.identifier !== FORK_IDENTIFIER) problems.push(`${file}: identifier is "${tauri.identifier}", expected "${FORK_IDENTIFIER}"`);
  if (tauri.bundle?.createUpdaterArtifacts !== false) problems.push(`${file}: bundle.createUpdaterArtifacts must be false`);
  const endpoints = tauri.plugins?.updater?.endpoints;
  if (endpoints !== undefined && !(Array.isArray(endpoints) && endpoints.length === 0)) {
    problems.push(`${file}: plugins.updater.endpoints must be empty`);
  }
  const schemes = tauri.plugins?.["deep-link"]?.desktop?.schemes;
  if (!Array.isArray(schemes) || schemes.length !== 1 || schemes[0] !== FORK_NAME) {
    problems.push(`${file}: plugins.deep-link.desktop.schemes must be only ["${FORK_NAME}"], found ${JSON.stringify(schemes)}`);
  }
}

function checkRust(root, problems) {
  for (const [file, name] of rustConstants) {
    const text = readText(root, file, problems);
    if (text === null) continue;
    const value = new RegExp(`const\\s+${name}\\s*:\\s*&str\\s*=\\s*"([^"]*)"`).exec(text)?.[1];
    if (value !== FORK_NAME) problems.push(`${file}: ${name} is ${value === undefined ? "missing" : `"${value}"`}, expected "${FORK_NAME}"`);
  }
  const updater = readText(root, "src-tauri/src/updater.rs", problems);
  if (updater !== null && !/OFFICIAL_UPDATE_FEED_ENABLED\s*:\s*bool\s*=\s*false\s*;/.test(updater)) {
    problems.push("src-tauri/src/updater.rs: OFFICIAL_UPDATE_FEED_ENABLED must stay false");
  }
  if (!existsSync(join(root, "src-tauri/linux/tabularis-extended.desktop"))) {
    problems.push("src-tauri/linux/tabularis-extended.desktop: file is missing");
  }
  if (existsSync(join(root, "src-tauri/linux/tabularis.desktop"))) {
    problems.push("src-tauri/linux/tabularis.desktop: upstream desktop file must stay renamed to tabularis-extended.desktop");
  }
}

function checkForbiddenName(root, problems) {
  const base = newestMergedStableTag(root);
  if (!base) {
    problems.push("no upstream release tag (vX.Y.Z) is merged into HEAD; fetch the upstream tags first (see UPSTREAM_SYNC.md)");
    return;
  }
  const diff = git(root, ["diff", "-U0", "--no-color", "--no-ext-diff", `${base}...HEAD`]).stdout;
  const hits = [];
  let file = "";
  for (const line of diff.split("\n")) {
    if (line.startsWith("+++ ")) file = line.slice(4).replace(/^b\//, "");
    else if (line.startsWith("+") && FORBIDDEN_NAME.test(line)) hits.push(`${file}: ${line.slice(1).trim().slice(0, 100)}`);
  }
  for (const hit of hits.slice(0, 20)) problems.push(`added after ${base} and names the other IDE product: ${hit}`);
  if (hits.length > 20) problems.push(`... and ${hits.length - 20} more lines naming the other IDE product`);
}

export function collectProblems(root) {
  const problems = [];
  checkVersions(root, problems);
  checkConfigs(root, problems);
  checkRust(root, problems);
  checkForbiddenName(root, problems);
  return problems;
}

function main() {
  const root = git(process.cwd(), ["rev-parse", "--show-toplevel"]).stdout.trim();
  const problems = collectProblems(root);
  if (problems.length === 0) {
    console.log("Fork identity is intact.");
    return;
  }
  console.error("Fork identity check failed:");
  for (const problem of problems) console.error(`  - ${problem}`);
  process.exit(1);
}

if (process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))) main();
