import fs from 'node:fs';
import path from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { NODE_PINS, NODE_VERSION } from '../package/pins.mjs';

function fail(code) { const error = new Error(code); error.code = code; throw error; }
export function archiveName(key) {
  if (!Object.hasOwn(NODE_PINS, key)) fail('CAPABILITY_UNAVAILABLE');
  const split = key.lastIndexOf('-');
  const platform = key.slice(0, split);
  const arch = key.slice(split + 1);
  return `node-v${NODE_VERSION}-${platform === 'win32' ? 'win' : platform}-${arch}.${platform === 'win32' ? 'zip' : 'tar.gz'}`;
}
export function digestFile(file) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 128 * 1024 * 1024) fail('INVALID_ARCHIVE');
  const descriptor = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
  const hash = createHash('sha256');
  const buffer = Buffer.alloc(1024 * 1024);
  try {
    let count;
    while ((count = fs.readSync(descriptor, buffer, 0, buffer.length, null)) > 0) hash.update(buffer.subarray(0, count));
    return hash.digest('hex');
  } finally { fs.closeSync(descriptor); }
}
export async function ensureArchive(cache, key, { fetcher = fetch } = {}) {
  if (!path.isAbsolute(cache)) fail('INVALID_PATH');
  const filename = archiveName(key);
  const target = path.join(cache, filename);
  fs.mkdirSync(cache, { recursive: true });
  if (fs.existsSync(target)) {
    if (digestFile(target) !== NODE_PINS[key]) fail('ARCHIVE_HASH_MISMATCH');
    return target;
  }
  const temporary = path.join(cache, `.${filename}.${randomUUID()}`);
  let descriptor;
  try {
    const response = await fetcher(`https://nodejs.org/dist/v${NODE_VERSION}/${filename}`, { redirect: 'error', signal: AbortSignal.timeout(180_000) });
    if (!response.ok || !response.body) fail('DOWNLOAD_FAILED');
    descriptor = fs.openSync(temporary, 'wx', 0o600);
    let bytes = 0;
    const hash = createHash('sha256');
    for await (const chunk of response.body) {
      bytes += chunk.length;
      if (bytes > 128 * 1024 * 1024) fail('INVALID_ARCHIVE');
      hash.update(chunk);
      fs.writeSync(descriptor, chunk);
    }
    if (hash.digest('hex') !== NODE_PINS[key]) fail('ARCHIVE_HASH_MISMATCH');
    fs.fsyncSync(descriptor);
    fs.closeSync(descriptor);
    descriptor = undefined;
    fs.linkSync(temporary, target);
    return target;
  } finally {
    if (descriptor !== undefined) fs.closeSync(descriptor);
    fs.rmSync(temporary, { force: true });
  }
}
export async function prepareArchives(cache, options) {
  const archives = {};
  for (const key of Object.keys(NODE_PINS).sort()) archives[key] = await ensureArchive(cache, key, options);
  return archives;
}
