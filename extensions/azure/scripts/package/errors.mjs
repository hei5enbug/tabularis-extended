export class PackageError extends Error {
  constructor(code) {
    super(code);
    this.name = 'PackageError';
    this.code = code;
  }
}

export function fail(code) {
  throw new PackageError(code);
}

export function publicError(error) {
  return error instanceof PackageError ? error.code : 'PACKAGE_FAILED';
}

export function boundedLimits(limits = {}) {
  const defaults = { packages: 4096, depth: 128, files: 50000, bytes: 512 * 1024 * 1024, fileBytes: 256 * 1024 * 1024, zipBytes: 2 * 1024 * 1024 * 1024 };
  const result = {};
  for (const [key, value] of Object.entries(defaults)) {
    const limit = limits[key] ?? value;
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > value) fail('INVALID_LIMIT');
    result[key] = limit;
  }
  if (Object.keys(limits).some(key => !(key in defaults))) fail('INVALID_LIMIT');
  return result;
}
