import fs from 'node:fs';
import path from 'node:path';
import { deflateRawSync } from 'node:zlib';
import crypto from 'node:crypto';
import { fail } from './errors.mjs';
import { readBounded, regularWithin, sha256 } from './files.mjs';

const table = Array.from({ length: 256 }, (_, index) => {
  let value = index;
  for (let bit = 0; bit < 8; bit += 1) value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
  return value >>> 0;
});

export function crc32(bytes) {
  let value = 0xffffffff;
  for (const byte of bytes) value = table[(value ^ byte) & 255] ^ (value >>> 8);
  return (value ^ 0xffffffff) >>> 0;
}

export function writeZip(stage, destination) {
  const fd = fs.openSync(destination, 'wx', 0o600);
  const hash = crypto.createHash('sha256');
  let offset = 0;
  const central = [];
  function write(bytes) {
    if (bytes.length > stage.limits.zipBytes - offset) fail('ZIP_TOO_LARGE');
    let written = 0;
    while (written < bytes.length) written += fs.writeSync(fd, bytes, written, bytes.length - written);
    hash.update(bytes);
    offset += bytes.length;
  }
  try {
    for (const entry of stage.list()) {
      const bytes = readBounded(regularWithin(stage.root, entry.path), stage.limits.fileBytes);
      if (bytes.length !== entry.bytes || sha256(bytes) !== entry.sha256) fail('SOURCE_CHANGED');
      const filename = Buffer.from(entry.path, 'utf8');
      if (filename.length > 65535) fail('INVALID_PATH');
      const compressed = deflateRawSync(bytes, { level: 9 });
      const crc = crc32(bytes);
      const localOffset = offset;
      const local = Buffer.alloc(30);
      local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(0x800, 6);
      local.writeUInt16LE(8, 8); local.writeUInt16LE(33, 12); local.writeUInt32LE(crc, 14);
      local.writeUInt32LE(compressed.length, 18); local.writeUInt32LE(bytes.length, 22); local.writeUInt16LE(filename.length, 26);
      write(local); write(filename); write(compressed);
      const record = Buffer.alloc(46);
      record.writeUInt32LE(0x02014b50, 0); record.writeUInt16LE(0x314, 4); record.writeUInt16LE(20, 6);
      record.writeUInt16LE(0x800, 8); record.writeUInt16LE(8, 10); record.writeUInt16LE(33, 14);
      record.writeUInt32LE(crc, 16); record.writeUInt32LE(compressed.length, 20); record.writeUInt32LE(bytes.length, 24);
      record.writeUInt16LE(filename.length, 28); record.writeUInt32LE((0o100644 << 16) >>> 0, 38); record.writeUInt32LE(localOffset, 42);
      central.push(record, filename);
    }
    const centralOffset = offset;
    for (const bytes of central) write(bytes);
    const end = Buffer.alloc(22);
    end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(stage.entries.size, 8); end.writeUInt16LE(stage.entries.size, 10);
    end.writeUInt32LE(offset - centralOffset, 12); end.writeUInt32LE(centralOffset, 16); write(end);
    fs.fsyncSync(fd);
    return { bytes: offset, sha256: hash.digest('hex') };
  } finally { fs.closeSync(fd); }
}

export function hashPublished(file, cap) {
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile() || stat.size > cap) fail('SOURCE_CHANGED');
    const hash = crypto.createHash('sha256');
    const chunk = Buffer.alloc(64 * 1024);
    let total = 0;
    for (;;) {
      const count = fs.readSync(fd, chunk, 0, Math.min(chunk.length, cap + 1 - total), null);
      if (!count) break;
      total += count;
      if (total > cap) fail('SOURCE_CHANGED');
      hash.update(chunk.subarray(0, count));
    }
    const final = fs.fstatSync(fd);
    if (total !== stat.size || final.size !== stat.size || final.mtimeMs !== stat.mtimeMs || final.ctimeMs !== stat.ctimeMs) fail('SOURCE_CHANGED');
    return { stat: final, sha256: hash.digest('hex') };
  } finally { fs.closeSync(fd); }
}
