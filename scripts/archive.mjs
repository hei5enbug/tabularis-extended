import { inflateRawSync } from "node:zlib";

const UTF8_FLAG = 0x0800;
const MAX_ENTRIES = 128;
const MAX_FILE_SIZE = 256 * 1024 * 1024;
const MAX_TOTAL_SIZE = 512 * 1024 * 1024;

const CRC_TABLE = Uint32Array.from({ length: 256 }, (_, index) => {
  let value = index;
  for (let bit = 0; bit < 8; bit += 1) {
    value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
  }
  return value >>> 0;
});

export function normalizeZipPath(value) {
  if (typeof value !== "string" || value.length === 0 || value.includes("\0")) {
    throw new Error("ZIP entry path is empty or invalid.");
  }
  if (value.startsWith("/") || value.startsWith("\\") || /^[A-Za-z]:/.test(value) || value.includes("\\")) {
    throw new Error(`ZIP absolute or platform-specific paths are not allowed: ${value}`);
  }
  const segments = value.split("/");
  if (segments.some(segment => segment === "" || segment === "." || segment === ".." || segment.endsWith(".") || segment.endsWith(" ") || segment.includes(":"))) {
    throw new Error(`ZIP traversal or ambiguous paths are not allowed: ${value}`);
  }
  return value;
}

export function crc32(data) {
  let value = 0xffffffff;
  for (const byte of data) value = CRC_TABLE[(value ^ byte) & 0xff] ^ (value >>> 8);
  return (value ^ 0xffffffff) >>> 0;
}

export function createZip(entries) {
  if (!Array.isArray(entries) || entries.length === 0 || entries.length > MAX_ENTRIES) {
    throw new Error("ZIP must contain between 1 and 128 files.");
  }

  const normalized = entries.map(entry => {
    const name = normalizeZipPath(entry.name);
    const data = Buffer.isBuffer(entry.data) ? entry.data : Buffer.from(entry.data);
    const mode = entry.mode ?? 0o644;
    if ((mode & 0o170000) !== 0 && (mode & 0o170000) !== 0o100000) {
      throw new Error(`Only regular files may be added to the ZIP: ${name}`);
    }
    if (data.length > MAX_FILE_SIZE) throw new Error(`ZIP entry exceeds the allowed size: ${name}`);
    return { name, data, mode: mode & 0o777 };
  }).sort((left, right) => Buffer.compare(Buffer.from(left.name, "utf8"), Buffer.from(right.name, "utf8")));

  const seen = new Set();
  for (const entry of normalized) {
    const key = entry.name.toLowerCase();
    if (seen.has(key)) throw new Error(`Duplicate ZIP path: ${entry.name}`);
    seen.add(key);
  }

  let totalSize = 0;
  let localOffset = 0;
  const localParts = [];
  const centralParts = [];

  for (const entry of normalized) {
    const name = Buffer.from(entry.name, "utf8");
    if (name.length > 0xffff) throw new Error(`ZIP path is too long: ${entry.name}`);
    totalSize += entry.data.length;
    if (totalSize > MAX_TOTAL_SIZE) throw new Error("ZIP exceeds the allowed total size.");
    const checksum = crc32(entry.data);

    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(UTF8_FLAG, 6);
    local.writeUInt16LE(0, 8);
    local.writeUInt16LE(0, 10);
    local.writeUInt16LE(0x0021, 12); // 1980-01-01, ZIP's minimum DOS date.
    local.writeUInt32LE(checksum, 14);
    local.writeUInt32LE(entry.data.length, 18);
    local.writeUInt32LE(entry.data.length, 22);
    local.writeUInt16LE(name.length, 26);
    local.writeUInt16LE(0, 28);
    localParts.push(local, name, entry.data);

    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE((3 << 8) | 20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(UTF8_FLAG, 8);
    central.writeUInt16LE(0, 10);
    central.writeUInt16LE(0, 12);
    central.writeUInt16LE(0x0021, 14);
    central.writeUInt32LE(checksum, 16);
    central.writeUInt32LE(entry.data.length, 20);
    central.writeUInt32LE(entry.data.length, 24);
    central.writeUInt16LE(name.length, 28);
    central.writeUInt16LE(0, 30);
    central.writeUInt16LE(0, 32);
    central.writeUInt16LE(0, 34);
    central.writeUInt16LE(0, 36);
    central.writeUInt32LE(((0o100000 | entry.mode) << 16) >>> 0, 38);
    central.writeUInt32LE(localOffset, 42);
    centralParts.push(central, name);
    localOffset += local.length + name.length + entry.data.length;
  }

  const centralOffset = localOffset;
  const centralSize = centralParts.reduce((sum, part) => sum + part.length, 0);
  if (centralOffset + centralSize > 0xffffffff) throw new Error("ZIP64 is not supported.");

  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(0, 4);
  end.writeUInt16LE(0, 6);
  end.writeUInt16LE(normalized.length, 8);
  end.writeUInt16LE(normalized.length, 10);
  end.writeUInt32LE(centralSize, 12);
  end.writeUInt32LE(centralOffset, 16);
  end.writeUInt16LE(0, 20);

  return Buffer.concat([...localParts, ...centralParts, end]);
}

function decodeUtf8(bytes) {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error("ZIP path is not valid UTF-8.");
  }
}

export function readZip(buffer) {
  if (!Buffer.isBuffer(buffer)) buffer = Buffer.from(buffer);
  const minimumEnd = Math.max(0, buffer.length - 0xffff - 22);
  const endOffset = buffer.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  if (endOffset < minimumEnd || endOffset < 0 || endOffset + 22 > buffer.length) {
    throw new Error("ZIP end record was not found.");
  }

  const commentLength = buffer.readUInt16LE(endOffset + 20);
  if (endOffset + 22 + commentLength !== buffer.length) throw new Error("Unexpected data follows the ZIP end record.");
  const disk = buffer.readUInt16LE(endOffset + 4);
  const centralDisk = buffer.readUInt16LE(endOffset + 6);
  const diskEntries = buffer.readUInt16LE(endOffset + 8);
  const entryCount = buffer.readUInt16LE(endOffset + 10);
  const centralSize = buffer.readUInt32LE(endOffset + 12);
  const centralOffset = buffer.readUInt32LE(endOffset + 16);
  if (disk !== 0 || centralDisk !== 0 || diskEntries !== entryCount || entryCount === 0 || entryCount > MAX_ENTRIES) {
    throw new Error("Multi-disk or unsupported ZIP format.");
  }
  if (centralOffset + centralSize !== endOffset) throw new Error("ZIP central directory range is invalid.");

  const entries = new Map();
  const caseFolded = new Set();
  const localOffsets = new Set();
  const localRanges = [];
  let cursor = centralOffset;
  let totalSize = 0;

  for (let index = 0; index < entryCount; index += 1) {
    if (cursor + 46 > endOffset || buffer.readUInt32LE(cursor) !== 0x02014b50) {
      throw new Error("ZIP central directory entry is invalid.");
    }
    const madeBy = buffer.readUInt16LE(cursor + 4) >>> 8;
    const flags = buffer.readUInt16LE(cursor + 8);
    const method = buffer.readUInt16LE(cursor + 10);
    const expectedCrc = buffer.readUInt32LE(cursor + 16);
    const compressedSize = buffer.readUInt32LE(cursor + 20);
    const uncompressedSize = buffer.readUInt32LE(cursor + 24);
    const nameLength = buffer.readUInt16LE(cursor + 28);
    const extraLength = buffer.readUInt16LE(cursor + 30);
    const entryCommentLength = buffer.readUInt16LE(cursor + 32);
    const startDisk = buffer.readUInt16LE(cursor + 34);
    const externalAttributes = buffer.readUInt32LE(cursor + 38);
    const localOffset = buffer.readUInt32LE(cursor + 42);
    const recordEnd = cursor + 46 + nameLength + extraLength + entryCommentLength;
    if (recordEnd > endOffset || nameLength === 0) throw new Error("ZIP path field is truncated.");
    if (flags !== UTF8_FLAG) {
      throw new Error("Encrypted or unsupported ZIP flags are not allowed.");
    }
    if (extraLength !== 0 || entryCommentLength !== 0) throw new Error("ZIP extra fields and entry comments are not supported.");
    if (method !== 0 && method !== 8) throw new Error("Unsupported ZIP compression method.");
    if (startDisk !== 0 || compressedSize === 0xffffffff || uncompressedSize === 0xffffffff || localOffset === 0xffffffff) {
      throw new Error("ZIP64 and multi-disk ZIP archives are not supported.");
    }

    const name = normalizeZipPath(decodeUtf8(buffer.subarray(cursor + 46, cursor + 46 + nameLength)));
    if (name.endsWith("/")) throw new Error(`ZIP directory entries are not allowed: ${name}`);
    const foldedName = name.toLowerCase();
    if (caseFolded.has(foldedName)) throw new Error(`Duplicate ZIP paths, including case-only duplicates, are not allowed: ${name}`);
    caseFolded.add(foldedName);
    const unixMode = madeBy === 3 ? externalAttributes >>> 16 : 0;
    const fileType = unixMode & 0o170000;
    if (fileType === 0o120000) throw new Error(`ZIP symbolic links are not allowed: ${name}`);
    if (fileType !== 0 && fileType !== 0o100000) throw new Error(`Non-file ZIP entries are not allowed: ${name}`);
    if (uncompressedSize > MAX_FILE_SIZE) throw new Error(`ZIP entry exceeds the allowed size: ${name}`);
    totalSize += uncompressedSize;
    if (totalSize > MAX_TOTAL_SIZE) throw new Error("ZIP exceeds the allowed total size.");

    if (localOffset + 30 > centralOffset || buffer.readUInt32LE(localOffset) !== 0x04034b50) {
      throw new Error(`ZIP local header is invalid: ${name}`);
    }
    if (localOffsets.has(localOffset)) throw new Error(`Multiple paths share a ZIP local entry: ${name}`);
    localOffsets.add(localOffset);
    const localFlags = buffer.readUInt16LE(localOffset + 6);
    const localMethod = buffer.readUInt16LE(localOffset + 8);
    const localCrc = buffer.readUInt32LE(localOffset + 14);
    const localCompressedSize = buffer.readUInt32LE(localOffset + 18);
    const localUncompressedSize = buffer.readUInt32LE(localOffset + 22);
    const localNameLength = buffer.readUInt16LE(localOffset + 26);
    const localExtraLength = buffer.readUInt16LE(localOffset + 28);
    const localNameStart = localOffset + 30;
    const dataStart = localNameStart + localNameLength + localExtraLength;
    const dataEnd = dataStart + compressedSize;
    if (localFlags !== flags || localMethod !== method || localNameLength !== nameLength || localExtraLength !== 0 || localCrc !== expectedCrc || localCompressedSize !== compressedSize || localUncompressedSize !== uncompressedSize || dataEnd > centralOffset) {
      throw new Error(`ZIP local entry boundary is invalid: ${name}`);
    }
    const localName = decodeUtf8(buffer.subarray(localNameStart, localNameStart + localNameLength));
    if (localName !== name) throw new Error(`ZIP central and local paths do not match: ${name}`);
    const compressed = buffer.subarray(dataStart, dataEnd);
    const localEnd = dataEnd;
    if (localRanges.some(([start, end]) => localOffset < end && start < localEnd)) {
      throw new Error(`ZIP local entries overlap: ${name}`);
    }
    localRanges.push([localOffset, localEnd]);
    let data;
    try {
      data = method === 0 ? Buffer.from(compressed) : inflateRawSync(compressed, { maxOutputLength: MAX_FILE_SIZE });
    } catch (error) {
      throw new Error(`Could not decompress ZIP entry: ${name}`, { cause: error });
    }
    if (data.length !== uncompressedSize || crc32(data) !== expectedCrc) {
      throw new Error(`ZIP entry size or CRC does not match: ${name}`);
    }
    entries.set(name, { data, mode: unixMode & 0o777 });
    cursor = recordEnd;
  }

  if (cursor !== endOffset) throw new Error("Unexpected entry in ZIP central directory.");
  return entries;
}
