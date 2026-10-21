import fs from 'node:fs';
import path from 'node:path';
import { PackageError, fail } from './errors.mjs';
import { absolute, directory, limitsFor, readJSON, Stage } from './files.mjs';
import { HOST_FLOOR, MAPLIBRE_VERSION, validatedManifest } from './manifest.mjs';
import { copyLicenses, discoverLicenses } from './licenses.mjs';
import { hashPublished, writeZip } from './zip.mjs';

function absent(file) {
  try { fs.lstatSync(file); fail('OUTPUT_EXISTS'); } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
}

function publish(next, destination) {
  try { fs.linkSync(next, destination); } catch (error) {
    if (error.code === 'EEXIST') fail('OUTPUT_EXISTS');
    throw error;
  }
}

function rollbackOwned(file, inode, expected, limits) {
  try {
    const current = fs.lstatSync(file);
    if (!current.isFile() || current.isSymbolicLink() || current.dev !== inode.dev || current.ino !== inode.ino) return false;
    const hashed = hashPublished(file, limits.zipBytes);
    const latest = fs.lstatSync(file);
    if (hashed.sha256 !== expected || latest.ino !== inode.ino || latest.dev !== inode.dev || latest.mtimeMs !== hashed.stat.mtimeMs || latest.ctimeMs !== hashed.stat.ctimeMs) return false;
    fs.unlinkSync(file);
    return true;
  } catch { return false; }
}

export function packageBundle({ source, output, limits: requestedLimits } = {}) {
  const limits = limitsFor(requestedLimits);
  let temporary;
  let temporaryInode;
  let zipPublished = false;
  let checksumPublished = false;
  let result;
  let failure;
  let zipPath;
  let zipInfo;
  let inode;
  try {
    const root = directory(source);
    const destination = absolute(output);
    const parent = directory(path.dirname(destination));
    zipPath = path.join(parent, path.basename(destination));
    if (!path.basename(destination) || path.basename(destination) === '.' || path.basename(destination) === '..') fail('INVALID_ARGUMENT');
    absent(zipPath); absent(`${zipPath}.sha256`);
    temporary = fs.mkdtempSync(path.join(parent, '.tabularis-spatial-package-'));
    fs.chmodSync(temporary, 0o700);
    temporaryInode = fs.lstatSync(temporary);
    const stageRoot = path.join(temporary, 'stage');
    fs.mkdirSync(stageRoot, { mode: 0o700 });
    const stage = new Stage(stageRoot, limits);
    const manifest = readJSON(root, 'manifest.json', limits);
    validatedManifest(manifest.value);
    stage.track(root, 'manifest.json', manifest.bytes, Math.min(limits.jsonBytes, limits.fileBytes));
    stage.add('.tabularium', manifest.bytes);
    if (fs.lstatSync(path.join(root, 'LICENSE')).size === 0) fail('LICENSE_MISSING');
    stage.copy(root, 'LICENSE', 'LICENSE', limits.licenseBytes);
    for (const name of ['index.js', 'style.css', 'maplibre-worker.js']) stage.copy(root, `ui/dist/${name}`, `ui/dist/${name}`);
    const uiManifest = readJSON(path.join(root, 'ui'), 'package.json', limits);
    stage.track(path.join(root, 'ui'), 'package.json', uiManifest.bytes, Math.min(limits.jsonBytes, limits.fileBytes));
    const nodes = discoverLicenses(root, limits);
    for (const node of nodes) stage.trackHash(node.root, 'package.json', node.packageBytes, node.packageHash, Math.min(limits.jsonBytes, limits.fileBytes));
    const notices = copyLicenses(nodes, stage);
    const noticeBytes = Buffer.from(`${JSON.stringify({ packages: notices }, null, 2)}\n`);
    if (noticeBytes.length > limits.jsonBytes) fail('PACKAGE_TOO_LARGE');
    stage.add('licenses/third-party-notices.json', noticeBytes);
    const releaseBytes = Buffer.from(`${JSON.stringify({
      platform: 'any', arch: 'any', maplibre_version: MAPLIBRE_VERSION,
      min_runtime_version: HOST_FLOOR, service_protocol: 1, manifest_version: manifest.value.version,
      files: stage.list(),
    }, null, 2)}\n`);
    if (releaseBytes.length > limits.jsonBytes) fail('PACKAGE_TOO_LARGE');
    stage.add('release.json', releaseBytes);
    const nextZip = path.join(temporary, 'bundle.zip');
    zipInfo = writeZip(stage, nextZip);
    stage.verifySources();
    inode = fs.lstatSync(nextZip);
    const nextChecksum = path.join(temporary, 'bundle.sha256');
    const fd = fs.openSync(nextChecksum, 'wx', 0o600);
    try { fs.writeFileSync(fd, `${zipInfo.sha256}\n`); fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
    publish(nextZip, zipPath);
    zipPublished = true;
    publish(nextChecksum, `${zipPath}.sha256`);
    checksumPublished = true;
    if (process.platform !== 'win32') {
      const parentFD = fs.openSync(parent, fs.constants.O_RDONLY);
      try { fs.fsyncSync(parentFD); } finally { fs.closeSync(parentFD); }
    }
    result = { ok: true, sha256: zipInfo.sha256, bytes: zipInfo.bytes, files: stage.entries.size, dependency_roots: nodes.length, license_packages: notices.length, platform: 'any', arch: 'any' };
  } catch (error) {
    failure = error instanceof PackageError ? error : new PackageError('PACKAGE_FAILED');
    if (zipPublished && checksumPublished) failure = new PackageError('PARTIAL_PUBLICATION_APPLIED');
    if (zipPublished && !checksumPublished && !rollbackOwned(zipPath, inode, zipInfo.sha256, limits)) failure = new PackageError('PARTIAL_PUBLICATION_APPLIED');
  } finally {
    if (temporary) {
      try {
        const current = fs.lstatSync(temporary);
        if (!current.isDirectory() || current.isSymbolicLink() || current.dev !== temporaryInode.dev || current.ino !== temporaryInode.ino) fail('CLEANUP_FAILED');
        fs.rmSync(temporary, { recursive: true, force: true });
      } catch {
        if (!failure) failure = new PackageError(zipPublished ? 'PARTIAL_PUBLICATION_APPLIED' : 'CLEANUP_FAILED');
      }
    }
  }
  if (failure) throw failure;
  return result;
}
