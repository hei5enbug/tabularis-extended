import fs from 'node:fs';
import { deflateRawSync } from 'node:zlib';
import { fail } from './errors.mjs';
import { safeRelative } from './files.mjs';

const crcTable = Array.from({ length: 256 }, (_, index) => {
  let value = index;
  for (let bit = 0; bit < 8; bit += 1) value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
  return value >>> 0;
});

export function crc32(data) {
  let value = 0xffffffff;
  for (const byte of data) value = crcTable[(value ^ byte) & 255] ^ (value >>> 8);
  return (value ^ 0xffffffff) >>> 0;
}

export function writeZip(stage, output) {
  const descriptor = fs.openSync(output, 'wx', 0o600);
  const central = [];
  let offset = 0;
  function write(data) {
    if (offset + data.length > stage.limits.zipBytes || offset + data.length > 0xffffffff) fail('ZIP_TOO_LARGE');
    let written = 0;
    while (written < data.length) written += fs.writeSync(descriptor, data, written, data.length - written, null);
    offset += data.length;
  }
  try {
    const sorted = [...stage.entries.keys()].sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
    if (sorted.length > 65535) fail('ZIP_TOO_LARGE');
    for (const relative of sorted) {
      safeRelative(relative);
      const entry = stage.entries.get(relative);
      const name = Buffer.from(relative, 'utf8');
      if (name.length > 65535) fail('INVALID_PATH');
      const data = fs.readFileSync(entry.path);
      const compressed = deflateRawSync(data, { level: 9 });
      const crc = crc32(data);
      const localOffset = offset;
      const header = Buffer.alloc(30);
      header.writeUInt32LE(0x04034b50, 0);
      header.writeUInt16LE(20, 4);
      header.writeUInt16LE(0x800, 6);
      header.writeUInt16LE(8, 8);
      header.writeUInt16LE(33, 12);
      header.writeUInt32LE(crc, 14);
      header.writeUInt32LE(compressed.length, 18);
      header.writeUInt32LE(data.length, 22);
      header.writeUInt16LE(name.length, 26);
      write(header);
      write(name);
      write(compressed);
      const record = Buffer.alloc(46);
      record.writeUInt32LE(0x02014b50, 0);
      record.writeUInt16LE(0x314, 4);
      record.writeUInt16LE(20, 6);
      record.writeUInt16LE(0x800, 8);
      record.writeUInt16LE(8, 10);
      record.writeUInt16LE(33, 14);
      record.writeUInt32LE(crc, 16);
      record.writeUInt32LE(compressed.length, 20);
      record.writeUInt32LE(data.length, 24);
      record.writeUInt16LE(name.length, 28);
      record.writeUInt32LE(((0o100000 | entry.mode) << 16) >>> 0, 38);
      record.writeUInt32LE(localOffset, 42);
      central.push(Buffer.concat([record, name]));
    }
    const centralOffset = offset;
    for (const entry of central) write(entry);
    const centralBytes = offset - centralOffset;
    const end = Buffer.alloc(22);
    end.writeUInt32LE(0x06054b50, 0);
    end.writeUInt16LE(sorted.length, 8);
    end.writeUInt16LE(sorted.length, 10);
    end.writeUInt32LE(centralBytes, 12);
    end.writeUInt32LE(centralOffset, 16);
    write(end);
    fs.fsyncSync(descriptor);
    return { bytes: offset, files: sorted.length };
  } finally {
    fs.closeSync(descriptor);
  }
}
