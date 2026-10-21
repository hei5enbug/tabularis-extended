import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fail } from './errors.mjs';

export function safeRelative(value) {
  if (typeof value !== 'string' || value.length === 0 || value.includes('\\') || value.includes(':') || value.includes('\0') || value.startsWith('/') || value.split('/').some(part => part === '' || part === '.' || part === '..')) fail('INVALID_PATH');
  return value;
}

export function absolute(value) {
  if (typeof value !== 'string' || !path.isAbsolute(value) || value.includes('\0')) fail('INVALID_PATH');
  return value;
}

export function regularWithin(root, relative) {
  safeRelative(relative);
  let current = fs.realpathSync(root);
  const canonicalRoot = current;
  const parts = relative.split('/');
  for (let index = 0; index < parts.length; index += 1) {
    current = path.join(current, parts[index]);
    const stat = fs.lstatSync(current);
    if (stat.isSymbolicLink() || (index < parts.length - 1 ? !stat.isDirectory() : !stat.isFile())) fail('INVALID_FILE');
  }
  const canonical = fs.realpathSync(current);
  if (!canonical.startsWith(`${canonicalRoot}${path.sep}`)) fail('INVALID_FILE');
  return canonical;
}

export function sha256(data) {
  return createHash('sha256').update(data).digest('hex');
}

export function hashFile(file) {
  const descriptor = fs.openSync(file, 'r');
  const hash = createHash('sha256');
  const buffer = Buffer.alloc(1024 * 1024);
  try {
    let count;
    while ((count = fs.readSync(descriptor, buffer, 0, buffer.length, null)) !== 0) hash.update(buffer.subarray(0, count));
    return hash.digest('hex');
  } finally {
    fs.closeSync(descriptor);
  }
}

export const MAX_JSON_BYTES = 1024 * 1024;

export function readBoundedRegular(file, maxBytes, errorCode) {
  const before = fs.lstatSync(file);
  if (!before.isFile() || before.isSymbolicLink()) fail(errorCode);
  const descriptor = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
  try {
    const stat = fs.fstatSync(descriptor);
    if (!stat.isFile() || stat.ino !== before.ino || stat.dev !== before.dev || !Number.isSafeInteger(stat.size) || stat.size > maxBytes) fail(errorCode);
    const buffer = Buffer.alloc(stat.size + 1);
    let bytes = 0;
    while (bytes < buffer.length) {
      const count = fs.readSync(descriptor, buffer, bytes, buffer.length - bytes, null);
      if (count === 0) break;
      bytes += count;
    }
    const current = fs.lstatSync(file);
    if (bytes > stat.size || bytes > maxBytes || current.isSymbolicLink() || current.ino !== stat.ino || current.dev !== stat.dev || fs.fstatSync(descriptor).size !== stat.size) fail(errorCode);
    return buffer.subarray(0, bytes);
  } finally {
    fs.closeSync(descriptor);
  }
}

export function readJSON(file) {
  try {
    return JSON.parse(readBoundedRegular(file, MAX_JSON_BYTES, 'INVALID_JSON').toString('utf8'));
  } catch {
    fail('INVALID_JSON');
  }
}

export function boundedDirectoryEntries(root, limit) {
  const directory = fs.opendirSync(root);
  const names = [];
  try {
    let entry;
    while ((entry = directory.readSync()) !== null) {
      if (names.length >= limit) fail('PACKAGE_TOO_LARGE');
      names.push(entry.name);
    }
    return names.sort();
  } finally {
    directory.closeSync();
  }
}

export class Stage {
  constructor(root, limits) {
    this.root = root;
    this.limits = limits;
    this.entries = new Map();
    this.bytes = 0;
    this.visits = 0;
  }

  add(relative, data, mode = 0o644) {
    safeRelative(relative);
    if (this.entries.has(relative)) fail('DUPLICATE_PATH');
    const bytes = Buffer.isBuffer(data) ? data : Buffer.from(data);
    if (bytes.length > this.limits.fileBytes) fail('FILE_TOO_LARGE');
    if (this.entries.size >= this.limits.files || this.bytes + bytes.length > this.limits.bytes || !Number.isSafeInteger(this.bytes + bytes.length)) fail('PACKAGE_TOO_LARGE');
    const target = path.join(this.root, ...relative.split('/'));
    fs.mkdirSync(path.dirname(target), { recursive: true, mode: 0o700 });
    fs.writeFileSync(target, bytes, { flag: 'wx', mode: 0o600 });
    this.entries.set(relative, { path: target, size: bytes.length, mode, sha256: sha256(bytes) });
    this.bytes += bytes.length;
  }

  copy(file, relative, mode = 0o644) {
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.isSymbolicLink()) fail('INVALID_FILE');
    if (this.entries.size >= this.limits.files || stat.size > this.limits.fileBytes || this.bytes + stat.size > this.limits.bytes) fail('PACKAGE_TOO_LARGE');
    this.add(relative, readBoundedRegular(file, Math.min(this.limits.fileBytes, this.limits.bytes - this.bytes), 'PACKAGE_TOO_LARGE'), mode);
  }

  copyTree(root, prefix, exclude = () => false) {
    const visit = (directory, relative = '', depth = 0) => {
      if (depth > this.limits.depth) fail('GRAPH_TOO_LARGE');
      for (const name of boundedDirectoryEntries(directory, this.limits.files * 2 - this.visits)) {
        if (++this.visits > this.limits.files * 2) fail('PACKAGE_TOO_LARGE');
        const child = relative ? `${relative}/${name}` : name;
        if (exclude(child, name)) continue;
        safeRelative(child);
        const file = path.join(directory, name);
        const stat = fs.lstatSync(file);
        if (stat.isSymbolicLink()) fail('SYMLINK_CONTENT');
        if (stat.isDirectory()) visit(file, child, depth + 1);
        else if (stat.isFile()) this.copy(file, `${prefix}/${child}`);
        else fail('INVALID_FILE');
      }
    };
    visit(fs.realpathSync(root));
  }
}
