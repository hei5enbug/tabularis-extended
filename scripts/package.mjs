import { createHash } from "node:crypto";
import { lstat, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createZip } from "./archive.mjs";
import { preflightManifest, readRegularFileUnder } from "./preflight.mjs";

const PLATFORMS = new Set(["linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64", "win-x64"]);

async function readBinary(binaryPath, windows) {
  const fullPath = path.resolve(binaryPath);
  const stat = await lstat(fullPath);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error("The executable must be a regular file, not a symbolic link.");
  if (windows && !fullPath.toLowerCase().endsWith(".exe")) throw new Error("Windows packages require an .exe executable.");
  if (!windows && (stat.mode & 0o111) === 0) throw new Error("The Unix executable does not have execute permission.");
  return readFile(fullPath);
}

export async function packagePlugin({ root = process.cwd(), binaryPath, platform, outputPath }) {
  root = path.resolve(root);
  if (!PLATFORMS.has(platform)) throw new Error(`Unsupported platform: ${platform}`);
  if (!binaryPath || !outputPath) throw new Error("binaryPath and outputPath are required.");

  const windows = platform === "win-x64";
  const binaryName = windows ? "sqlserver-plugin.exe" : "sqlserver-plugin";
  const binary = await readBinary(binaryPath, windows);
  const { assetPaths } = await preflightManifest({ root, assetRoot: root });
  const requiredSources = [
    [".tabularium", ".tabularium", 0o644],
    ["README.md", "README.md", 0o644],
    ["LICENSE", "LICENSE", 0o644],
    ["ui/dist/index.js", "ui/dist/index.js", 0o644],
    ["explain/dist/index.iife.js", "explain/dist/index.iife.js", 0o644],
    ["vendor/mssql-tds-preview/LICENSE", "licenses/mssql-tds-preview-MIT.txt", 0o644],
  ];
  for (const [manifestPath, archivePath] of assetPaths.map(value => [value, value, 0o644])) {
    if (!requiredSources.some(([, requiredPath]) => requiredPath === archivePath)) {
      requiredSources.push([manifestPath, archivePath, 0o644]);
    }
  }

  const entries = [{ name: binaryName, data: binary, mode: 0o755 }];
  for (const [sourcePath, archivePath, mode] of requiredSources) {
    entries.push({
      name: archivePath,
      data: await readRegularFileUnder(root, sourcePath, "Required package file"),
      mode,
    });
  }

  const archive = createZip(entries);
  const output = path.resolve(outputPath);
  const sidecar = `${output}.sha256`;
  await mkdir(path.dirname(output), { recursive: true });
  const digest = createHash("sha256").update(archive).digest("hex");
  const temporary = `${output}.tmp-${process.pid}`;
  const sidecarTemporary = `${sidecar}.tmp-${process.pid}`;
  for (const destination of [output, sidecar, temporary, sidecarTemporary]) {
    try {
      await lstat(destination);
      throw new Error(`Package output path already exists: ${destination}`);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
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
    if (!["--root", "--binary", "--platform", "--output"].includes(argument)) {
      throw new Error(`Unknown argument: ${argument}`);
    }
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`${argument} requires a value.`);
    result[argument.slice(2).replaceAll("-", "")] = value;
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
    });
    process.stdout.write(`Package created: ${result.archivePath}\nSHA-256: ${result.sha256Path}\n`);
  } catch (error) {
    process.stderr.write(`Package creation failed: ${error.message}\n`);
    process.exitCode = 1;
  }
}
