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
  return { ok: false, code: error instanceof PackageError ? error.code : 'PACKAGE_FAILED' };
}
