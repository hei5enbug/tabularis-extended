import { createHash } from "node:crypto";
import { lstat, mkdir, readFile, readdir, rename, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createZip, normalizeZipPath } from "./archive.mjs";
import { REQUIRED_EXECUTABLE, preflightManifest, readRegularFileUnder } from "./preflight.mjs";

const PLATFORMS = new Set(["linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64", "win-x64"]);
const FIXED_SOURCES = [".tabularium", "README.md", "LICENSE", "ui/dist/index.js", "explain/dist/index.iife.js"];
const ASSETS_DIRECTORY = "assets";

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function readBinary(binaryPath, windows) {
  const fullPath = path.resolve(binaryPath);
  const stat = await lstat(fullPath);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error("The executable must be a regular file, not a symbolic link.");
  if (windows && !fullPath.toLowerCase().endsWith(".exe")) throw new Error("Windows packages require an .exe executable.");
  if (!windows && (stat.mode & 0o111) === 0) throw new Error("The Unix executable does not have execute permission.");
  return readFile(fullPath);
}

async function listAssetFiles(root, directory = ASSETS_DIRECTORY) {
  let children;
  try {
    children = await readdir(path.join(root, ...directory.split("/")), { withFileTypes: true });
  } catch (error) {
    if (error.code === "ENOENT" && directory === ASSETS_DIRECTORY) return [];
    throw error;
  }
  const files = [];
  for (const child of children) {
    const relativePath = normalizeZipPath(`${directory}/${child.name}`);
    assert(!child.isSymbolicLink(), `Package assets must not use a symbolic link: ${relativePath}`);
    if (child.isDirectory()) files.push(...await listAssetFiles(root, relativePath));
    else if (child.isFile()) files.push(relativePath);
    else throw new Error(`Package assets must be regular files: ${relativePath}`);
  }
  return files.sort();
}

async function removeReplaceable(destination) {
  let stat;
  try {
    stat = await lstat(destination);
  } catch (error) {
    if (error.code === "ENOENT") return;
    throw error;
  }
  assert(stat.isFile() && !stat.isSymbolicLink(), `Package output path is not a regular file: ${destination}`);
  await rm(destination);
}

async function assertAbsent(destination) {
  try {
    await lstat(destination);
  } catch (error) {
    if (error.code === "ENOENT") return;
    throw error;
  }
  throw new Error(`Package output path already exists: ${destination}`);
}

export async function packagePlugin({ root = process.cwd(), binaryPath, platform, outputPath, replace = false }) {
  root = path.resolve(root);
  if (!PLATFORMS.has(platform)) throw new Error(`Unsupported platform: ${platform}`);
  if (!binaryPath || !outputPath) throw new Error("binaryPath and outputPath are required.");

  const windows = platform === "win-x64";
  const binaryName = windows ? `${REQUIRED_EXECUTABLE}.exe` : REQUIRED_EXECUTABLE;
  const binary = await readBinary(binaryPath, windows);
  const { assetPaths } = await preflightManifest({ root, assetRoot: root });
  const sources = [...new Set([...FIXED_SOURCES, ...assetPaths, ...await listAssetFiles(root)])];

  const entries = [{ name: binaryName, data: binary, mode: 0o755 }];
  for (const sourcePath of sources) {
    entries.push({
      name: sourcePath,
      data: await readRegularFileUnder(root, sourcePath, "Required package file"),
      mode: 0o644,
    });
  }

  const archive = createZip(entries);
  const output = path.resolve(outputPath);
  const sidecar = `${output}.sha256`;
  await mkdir(path.dirname(output), { recursive: true });
  const digest = createHash("sha256").update(archive).digest("hex");
  const temporary = `${output}.tmp-${process.pid}`;
  const sidecarTemporary = `${sidecar}.tmp-${process.pid}`;
  if (replace) await Promise.all([removeReplaceable(output), removeReplaceable(sidecar)]);
  for (const destination of [output, sidecar, temporary, sidecarTemporary]) await assertAbsent(destination);
  let archiveCommitted = false;
  try {
    await writeFile(temporary, archive, { flag: "wx", mode: 0o644 });
    await writeFile(sidecarTemporary, `${digest}  ${path.basename(output)}\n`, { flag: "wx", mode: 0o644 });
    await rename(temporary, output);
    archiveCommitted = true;
    await rename(sidecarTemporary, sidecar);
  } catch (error) {
    await Promise.allSettled([rm(temporary, { force: true }), rm(sidecarTemporary, { force: true })]);
    if (archiveCommitted) await rm(output, { force: true }).catch(() => {});
    throw error;
  }
  return { archivePath: output, sha256Path: sidecar, sha256: digest, entries: entries.map(entry => entry.name).sort() };
}

function parseArguments(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--replace") {
      result.replace = true;
      continue;
    }
    if (!["--root", "--binary", "--platform", "--output"].includes(argument)) {
      throw new Error(`Unknown argument: ${argument}`);
    }
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`${argument} requires a value.`);
    result[argument.slice(2)] = value;
    index += 1;
  }
  return result;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  try {
    const options = parseArguments(process.argv.slice(2));
    const result = await packagePlugin({
      root: options.root ?? process.cwd(),
      binaryPath: options.binary,
      platform: options.platform,
      outputPath: options.output,
      replace: options.replace === true,
    });
    process.stdout.write(`Packaged ${result.archivePath}\nSHA-256: ${result.sha256Path}\n`);
  } catch (error) {
    process.stderr.write(`Package creation failed: ${error.message}\n`);
    process.exitCode = 1;
  }
}
