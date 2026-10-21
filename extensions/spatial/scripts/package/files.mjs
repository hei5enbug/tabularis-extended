import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fail } from './errors.mjs';

export const DEFAULT_LIMITS = Object.freeze({
  bytes: 64 * 1024 * 1024,
  fileBytes: 16 * 1024 * 1024,
  files: 1024,
  jsonBytes: 1024 * 1024,
  licenseBytes: 1024 * 1024,
  dependencyRoots: 256,
  depth: 64,
  zipBytes: 128 * 1024 * 1024,
});

export function limitsFor(input = {}) {
  if (!input || typeof input !== 'object' || Array.isArray(input)) fail('INVALID_LIMITS');
  const limits = { ...DEFAULT_LIMITS };
  for (const [key, value] of Object.entries(input)) {
    if (!Object.hasOwn(DEFAULT_LIMITS, key) || !Number.isSafeInteger(value) || value < 1 || value > DEFAULT_LIMITS[key]) fail('INVALID_LIMITS');
    limits[key] = value;
  }
  return limits;
}

export function absolute(value) {
  if (typeof value !== 'string' || !path.isAbsolute(value) || value.includes('\0')) fail('INVALID_ARGUMENT');
  return path.resolve(value);
}

export function relative(value) {
  if (typeof value !== 'string' || !value || Buffer.from(value).toString('utf8') !== value || /[\\:\0]/.test(value) || path.posix.isAbsolute(value) || value.split('/').some(part => !part || part === '.' || part === '..')) fail('INVALID_PATH');
  return value;
}

export function directory(value) {
  const root = fs.realpathSync(absolute(value));
  if (!fs.statSync(root).isDirectory()) fail('INVALID_PATH');
  return root;
}

export function regularWithin(root, name) {
  const parts = relative(name).split('/');
  let current = root;
  for (let index = 0; index < parts.length; index += 1) {
    current = path.join(current, parts[index]);
    const stat = fs.lstatSync(current);
    if (stat.isSymbolicLink() || (index + 1 === parts.length ? !stat.isFile() : !stat.isDirectory())) fail('INVALID_FILE');
  }
  const canonical = fs.realpathSync(current);
  if (!canonical.startsWith(`${root}${path.sep}`)) fail('INVALID_FILE');
  return canonical;
}

export function sha256(bytes) {
  return crypto.createHash('sha256').update(bytes).digest('hex');
}

function parentSnapshot(file) {
  const parents = [];
  for (let current = path.dirname(file);; current = path.dirname(current)) {
    const stat = fs.lstatSync(current);
    if (!stat.isDirectory() || stat.isSymbolicLink()) fail('INVALID_FILE');
    parents.push({ path: current, dev: stat.dev, ino: stat.ino });
    if (current === path.dirname(current)) break;
  }
  return parents;
}

export function readBounded(file, cap, code = 'PACKAGE_TOO_LARGE') {
  const parents = parentSnapshot(file);
  const before = fs.lstatSync(file);
  if (!before.isFile() || before.isSymbolicLink()) fail('INVALID_FILE');
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try {
    const initial = fs.fstatSync(fd);
    if (!initial.isFile() || initial.ino !== before.ino || initial.dev !== before.dev) fail('SOURCE_CHANGED');
    if (!Number.isSafeInteger(initial.size) || initial.size > cap) fail(code);
    const bytes = Buffer.alloc(initial.size + 1);
    let length = 0;
    while (length < bytes.length) {
      const count = fs.readSync(fd, bytes, length, bytes.length - length, null);
      if (!count) break;
      length += count;
    }
    const verification = Buffer.alloc(Math.min(64 * 1024, length + 1));
    let verified = 0;
    while (verified <= length) {
      const count = fs.readSync(fd, verification, 0, Math.min(verification.length, length - verified + 1), verified);
      if (!count) {
        if (verified !== length) fail('SOURCE_CHANGED');
        break;
      }
      if (verified + count > length || !verification.subarray(0, count).equals(bytes.subarray(verified, verified + count))) fail('SOURCE_CHANGED');
      verified += count;
    }
    const final = fs.fstatSync(fd);
    const after = fs.lstatSync(file);
    if (length !== initial.size || final.size !== initial.size || final.mtimeMs !== initial.mtimeMs || final.ctimeMs !== initial.ctimeMs || !after.isFile() || after.isSymbolicLink() || after.ino !== initial.ino || after.dev !== initial.dev || after.size !== initial.size || after.mtimeMs !== initial.mtimeMs || after.ctimeMs !== initial.ctimeMs) fail('SOURCE_CHANGED');
    for (const parent of parents) {
      const stat = fs.lstatSync(parent.path);
      if (!stat.isDirectory() || stat.isSymbolicLink() || stat.dev !== parent.dev || stat.ino !== parent.ino) fail('SOURCE_CHANGED');
    }
    return bytes.subarray(0, length);
  } finally {
    fs.closeSync(fd);
  }
}

export function readJSON(root, name, limits) {
  const bytes = readBounded(regularWithin(root, name), Math.min(limits.jsonBytes, limits.fileBytes), 'INVALID_JSON');
  let value;
  const text = bytes.toString('utf8');
  if (!Buffer.from(text).equals(bytes)) fail('INVALID_JSON');
  try { value = JSON.parse(text); } catch { fail('INVALID_JSON'); }
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail('INVALID_JSON');
  return { bytes, value };
}

export function entriesBounded(root, cap) {
  const entries = [];
  const dir = fs.opendirSync(root);
  try {
    for (;;) {
      const entry = dir.readSync();
      if (!entry) break;
      if (entries.length >= cap) fail('PACKAGE_TOO_LARGE');
      entries.push(entry);
    }
  } finally {
    dir.closeSync();
  }
  return entries.sort((a, b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name)));
}

export class Stage {
  constructor(root, limits) {
    this.root = directory(root);
    this.limits = limits;
    this.entries = new Map();
    this.sources = [];
    this.bytes = 0;
  }

  check(size, count = 1) {
    if (!Number.isSafeInteger(size) || size < 0 || size > this.limits.fileBytes || this.entries.size + count > this.limits.files || size > this.limits.bytes - this.bytes) fail('PACKAGE_TOO_LARGE');
  }

  add(name, bytes) {
    relative(name);
    if (this.entries.has(name)) fail('DUPLICATE_PATH');
    this.check(bytes.length);
    const file = path.join(this.root, ...name.split('/'));
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    const fd = fs.openSync(file, 'wx', 0o600);
    try {
      fs.writeFileSync(fd, bytes);
      fs.fsyncSync(fd);
    } finally { fs.closeSync(fd); }
    const entry = { path: name, bytes: bytes.length, sha256: sha256(bytes) };
    this.entries.set(name, entry);
    this.bytes += bytes.length;
    return entry;
  }

  copy(root, source, destination, cap = this.limits.fileBytes) {
    const file = regularWithin(root, source);
    const stat = fs.lstatSync(file);
    this.check(stat.size);
    const bytes = readBounded(file, Math.min(cap, this.limits.fileBytes));
    this.track(root, source, bytes, Math.min(cap, this.limits.fileBytes));
    return this.add(destination, bytes);
  }

  track(root, source, bytes, cap) {
    this.trackHash(root, source, bytes.length, sha256(bytes), cap);
  }

  trackHash(root, source, bytes, hash, cap) {
    this.sources.push({ root, source, bytes, sha256: hash, cap });
  }

  verifySources() {
    for (const source of this.sources) {
      const bytes = readBounded(regularWithin(source.root, source.source), source.cap);
      if (bytes.length !== source.bytes || sha256(bytes) !== source.sha256) fail('SOURCE_CHANGED');
    }
  }

  list() {
    return [...this.entries.values()].sort((a, b) => Buffer.compare(Buffer.from(a.path), Buffer.from(b.path)));
  }
}
