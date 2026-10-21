import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { absolute, hashFile, safeRelative } from './files.mjs';
import { PackageError, fail } from './errors.mjs';
import { NODE_PINS, NODE_VERSION } from './pins.mjs';

export function defaultTar() {
  if (process.platform === 'darwin') return '/usr/bin/tar';
  if (process.platform === 'linux') return '/usr/bin/bsdtar';
  if (process.platform === 'win32') {
    const root = process.env.SystemRoot;
    if (!root || !path.isAbsolute(root)) fail('UNSUPPORTED_ARCHIVE_PARSER');
    return path.join(root, 'System32', 'tar.exe');
  }
  fail('UNSUPPORTED_ARCHIVE_PARSER');
}

function runTar(tar, archive, args, maxBuffer) {
  const descriptor = fs.openSync(archive, 'r');
  try {
    const result = spawnSync(tar, args, { stdio: [descriptor, 'pipe', 'pipe'], shell: false, windowsHide: true, encoding: null, maxBuffer, timeout: 60000 });
    if (result.error || result.status !== 0 || result.signal) fail('UNSUPPORTED_ARCHIVE_PARSER');
    return result.stdout;
  } finally {
    fs.closeSync(descriptor);
  }
}

function extract({ platform, arch, archive, tar = defaultTar(), expectedPin = NODE_PINS[`${platform}-${arch}`] }) {
  const key = `${platform}-${arch}`;
  if (!NODE_PINS[key] || !/^[a-f0-9]{64}$/.test(expectedPin ?? '')) fail('UNSUPPORTED_PLATFORM');
  absolute(archive);
  absolute(tar);
  if (!fs.lstatSync(archive).isFile() || fs.lstatSync(archive).isSymbolicLink()) fail('INVALID_ARCHIVE');
  if (hashFile(archive) !== expectedPin) fail('RUNTIME_HASH_MISMATCH');
  if (!fs.statSync(tar).isFile()) fail('UNSUPPORTED_ARCHIVE_PARSER');
  const root = `node-v${NODE_VERSION}-${platform === 'win32' ? 'win' : platform}-${arch}`;
  const nodeMember = `${root}/${platform === 'win32' ? 'node.exe' : 'bin/node'}`;
  const licenseMember = `${root}/LICENSE`;
  const list = runTar(tar, archive, ['-tf', '-'], 16 * 1024 * 1024).toString('utf8').split(/\r?\n/).filter(Boolean).map(item => item.endsWith('/') ? item.slice(0, -1) : item);
  for (const member of list) safeRelative(member);
  for (const member of [nodeMember, licenseMember]) {
    if (list.filter(value => value === member).length !== 1) fail('INVALID_ARCHIVE');
    const detail = runTar(tar, archive, ['-tvf', '-', member], 1024 * 1024).toString('utf8').trim().split(/\r?\n/);
    if (detail.length !== 1 || !detail[0].startsWith('-')) fail('INVALID_ARCHIVE');
  }
  const node = runTar(tar, archive, ['-xOf', '-', nodeMember], 256 * 1024 * 1024);
  const license = runTar(tar, archive, ['-xOf', '-', licenseMember], 8 * 1024 * 1024);
  if (node.length === 0 || license.length === 0) fail('INVALID_ARCHIVE');
  return { node, license, archive_sha256: expectedPin, runtime_path: platform === 'win32' ? 'runtime/node.exe' : 'runtime/bin/node' };
}

export function extractPinnedRuntime(options) {
  try { return extract(options); }
  catch (error) {
    if (error instanceof PackageError) throw error;
    fail('INVALID_ARCHIVE');
  }
}
