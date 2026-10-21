import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { PackageError, boundedLimits, fail } from './errors.mjs';
import { Stage, absolute, hashFile, regularWithin, sha256 } from './files.mjs';
import { materializeProductionGraph } from './graph.mjs';
import { validatedManifest, uiStyle, requiredAsset } from './manifest.mjs';
import { MIN_HOST_VERSION, NODE_PINS, NODE_VERSION, SDK_VERSION } from './pins.mjs';
import { extractPinnedRuntime } from './runtime.mjs';
import { writeZip } from './zip.mjs';

export { PackageError } from './errors.mjs';
export { extractPinnedRuntime } from './runtime.mjs';
export { NODE_PINS, NODE_VERSION, SDK_VERSION, MIN_HOST_VERSION } from './pins.mjs';

function privateWrite(file, data) {
  const descriptor = fs.openSync(file, 'wx', 0o600);
  try {
    fs.writeFileSync(descriptor, data);
    fs.fsyncSync(descriptor);
  } finally {
    fs.closeSync(descriptor);
  }
}

function publish(next, output, replace) {
  if (replace) fs.renameSync(next, output);
  else fs.linkSync(next, output);
}

export function packageBundle({ platform, arch, source, launcher, runtimeArchive, output, tar, replace = false, expectedRuntimePin, limits: requestedLimits } = {}) {
  let ownedStage;
  let ownedNext;
  let zipPublished = false;
  let checksumPublished = false;
  let publishedIdentity;
  let publishedDigest;
  let primaryError;
  try {
    if (!NODE_PINS[`${platform}-${arch}`]) fail('UNSUPPORTED_PLATFORM');
    for (const value of [source, launcher, runtimeArchive, output]) absolute(value);
    if (typeof replace !== 'boolean') fail('INVALID_ARGUMENT');
    const limits = boundedLimits(requestedLimits);
    const checksumOutput = `${output}.sha256`;
    if (!replace && (fs.existsSync(output) || fs.existsSync(checksumOutput))) fail('OUTPUT_EXISTS');
    if (!fs.statSync(source).isDirectory() || !fs.lstatSync(launcher).isFile() || fs.lstatSync(launcher).isSymbolicLink()) fail('INVALID_FILE');
    const manifest = validatedManifest(source, platform);
    const runtime = extractPinnedRuntime({ platform, arch, archive: runtimeArchive, tar, expectedPin: expectedRuntimePin });
    ownedStage = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-azure-package-'));
    fs.chmodSync(ownedStage, 0o700);
    ownedNext = fs.mkdtempSync(path.join(path.dirname(output), '.tabularis-package-next-'));
    fs.chmodSync(ownedNext, 0o700);
    const stage = new Stage(ownedStage, limits);
    stage.add('.tabularium', `${JSON.stringify(manifest, null, 2)}\n`);
    stage.add('package.json', '{"type":"module"}\n');
    stage.copy(requiredAsset(source, 'LICENSE'), 'LICENSE');
    stage.copy(regularWithin(source, 'dist/index.js'), 'dist/driver.mjs');
    stage.copyTree(path.join(source, 'dist'), 'dist', relative => relative === 'index.js');
    stage.copy(launcher, platform === 'win32' ? 'cosmos-nosql.exe' : 'cosmos-nosql', 0o755);
    stage.add(runtime.runtime_path, runtime.node, 0o755);
    runtime.node = null;
    stage.add('licenses/node/LICENSE', runtime.license);
    stage.copy(requiredAsset(source, 'ui/dist/index.js'), 'ui/dist/index.js');
    stage.copy(uiStyle(source), 'ui/dist/style.css');
    const graph = materializeProductionGraph(stage, source);
    if (!graph.packages.some(item => item.path === 'node_modules/@azure/cosmos' && item.version === SDK_VERSION)) fail('SDK_VERSION_MISMATCH');
    if (!graph.packages.some(item => item.path === 'node_modules/@tabularis/service-contracts')) fail('DEPENDENCY_MISSING');
    const notices = [{ name: 'node', version: NODE_VERSION, sha256: sha256(runtime.license) }, ...graph.notices];
    stage.add('licenses/third-party-notices.json', `${JSON.stringify(notices, null, 2)}\n`);
    const files = [...stage.entries.entries()].sort(([a], [b]) => Buffer.compare(Buffer.from(a), Buffer.from(b))).map(([relative, file]) => ({ path: relative, bytes: file.size, sha256: file.sha256 }));
    stage.add('release.json', `${JSON.stringify({ node: NODE_VERSION, sdk: SDK_VERSION, service_protocol: 1, min_runtime_version: MIN_HOST_VERSION, platform, arch, runtime_archive_sha256: runtime.archive_sha256, files }, null, 2)}\n`);
    const nextZip = path.join(ownedNext, 'bundle.zip');
    const zip = writeZip(stage, nextZip);
    const digest = hashFile(nextZip);
    const nextChecksum = path.join(ownedNext, 'bundle.sha256');
    privateWrite(nextChecksum, `${digest}\n`);
    const nextIdentity = fs.statSync(nextZip, { bigint: true });
    publish(nextZip, output, replace);
    zipPublished = true;
    publishedIdentity = nextIdentity;
    publishedDigest = digest;
    publish(nextChecksum, checksumOutput, replace);
    checksumPublished = true;
    if (process.platform !== 'win32') {
      const directory = fs.openSync(path.dirname(output), 'r');
      try { fs.fsyncSync(directory); } finally { fs.closeSync(directory); }
    }
    return { sha256: digest, bytes: zip.bytes, files: zip.files, uncompressed_bytes: stage.bytes, node: NODE_VERSION, sdk: SDK_VERSION, platform, arch };
  } catch (error) {
    let failure = error instanceof PackageError ? error : new PackageError(error?.code === 'EEXIST' ? 'OUTPUT_EXISTS' : 'PACKAGE_FAILED');
    if (zipPublished) {
      if (replace || checksumPublished) failure = new PackageError('PARTIAL_PUBLICATION_APPLIED');
      else {
        try {
          const current = fs.lstatSync(output, { bigint: true });
          if (!current.isFile() || current.ino !== publishedIdentity.ino || current.dev !== publishedIdentity.dev || hashFile(output) !== publishedDigest) fail('PARTIAL_PUBLICATION_APPLIED');
          const confirmed = fs.lstatSync(output, { bigint: true });
          if (!confirmed.isFile() || confirmed.ino !== current.ino || confirmed.dev !== current.dev || confirmed.size !== current.size) fail('PARTIAL_PUBLICATION_APPLIED');
          fs.unlinkSync(output);
        } catch {
          failure = new PackageError('PARTIAL_PUBLICATION_APPLIED');
        }
      }
    }
    primaryError = failure;
    throw failure;
  } finally {
    let cleanupFailed = false;
    for (const owned of [ownedStage, ownedNext]) {
      if (!owned) continue;
      try { fs.rmSync(owned, { recursive: true, force: true }); }
      catch { cleanupFailed = true; }
    }
    if (cleanupFailed && !primaryError) fail(zipPublished ? 'PACKAGE_CLEANUP_FAILED_APPLIED' : 'PACKAGE_CLEANUP_FAILED');
  }
}
