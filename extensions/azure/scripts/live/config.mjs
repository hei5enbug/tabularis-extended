import fs from 'node:fs';
import path from 'node:path';
import { validateEndpoint } from '../../dist/connection/settings.js';
import { assertOwnerAcl } from './windows-acl.mjs';

export class LiveError extends Error {
  constructor(code, outcome = 'not_applied') { super(code); this.code = code; this.outcome = outcome; }
}
export function reject(code = 'INVALID_CONFIG') { throw new LiveError(code); }
export function object(value) { return value !== null && typeof value === 'object' && !Array.isArray(value); }
export function closed(value, keys, code = 'INVALID_CONFIG') {
  if (!object(value) || Object.keys(value).some(key => !keys.includes(key))) reject(code);
}
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export function uuidString(value) { return typeof value === 'string' && uuid.test(value); }
function resource(value) { return typeof value === 'string' && value.length >= 1 && value.length <= 255 && !/[\x00-\x1f\x7f/\\?#]/.test(value); }
export function validateConfig(value) {
  closed(value, ['version', 'dedicated_test_resource', 'endpoint', 'database', 'container', 'hierarchical_container', 'auth_mode', 'tenant_id', 'client_id']);
  if (value.version !== 1 || value.dedicated_test_resource !== true || !resource(value.database) || /^(?:master|system)$/i.test(value.database)
    || !resource(value.container) || !resource(value.hierarchical_container) || !value.container.startsWith('tabularis_test_')
    || !value.hierarchical_container.startsWith('tabularis_test_') || value.container === value.hierarchical_container
    || !['account_key', 'entra_user', 'entra_service_principal'].includes(value.auth_mode)) reject();
  let endpoint;
  try { endpoint = validateEndpoint(value.endpoint); } catch { reject(); }
  if (value.auth_mode !== 'account_key' && (!uuidString(value.tenant_id) || !uuidString(value.client_id))) reject();
  if ((value.tenant_id !== undefined && !uuidString(value.tenant_id)) || (value.client_id !== undefined && !uuidString(value.client_id))) reject();
  return { ...value, endpoint };
}
export function absoluteFile(value, code = 'INVALID_ARGUMENT') {
  if (typeof value !== 'string' || !path.isAbsolute(value) || /[\x00-\x1f\x7f]/.test(value)) reject(code);
  return value;
}
export function privateFile(file, maxBytes = 64 * 1024) {
  absoluteFile(file, 'INVALID_CONFIG');
  let descriptor;
  try {
    assertOwnerAcl(file);
    const before = fs.lstatSync(file);
    if (!before.isFile() || before.isSymbolicLink() || !Number.isSafeInteger(before.size) || before.size > maxBytes
      || (process.platform !== 'win32' && ((before.mode & 0o077) !== 0 || before.uid !== process.getuid()))) reject();
    descriptor = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    const stat = fs.fstatSync(descriptor);
    if (!stat.isFile() || stat.ino !== before.ino || stat.dev !== before.dev || stat.size !== before.size) reject();
    const bytes = Buffer.alloc(stat.size + 1);
    let count = 0;
    while (count < bytes.length) {
      const read = fs.readSync(descriptor, bytes, count, bytes.length - count, null);
      if (read === 0) break;
      count += read;
    }
    const after = fs.lstatSync(file);
    if (count !== stat.size || after.isSymbolicLink() || after.ino !== stat.ino || after.dev !== stat.dev || fs.fstatSync(descriptor).size !== stat.size) reject();
    assertOwnerAcl(file);
    return bytes.subarray(0, count);
  } catch { reject(); } finally { if (descriptor !== undefined) fs.closeSync(descriptor); }
}
export function readConfig(file) {
  try { return validateConfig(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(privateFile(file)))); }
  catch { reject(); }
}
export function parseArguments(args) {
  const options = {};
  const flags = new Set(['--allow-live', '--allow-fixture-writes']);
  const names = new Map([['--config', 'config'], ['--credential-fd', 'credentialFd'], ['--readonly-credential-fd', 'readonlyCredentialFd'], ['--driver', 'driver'], ['--report', 'report']]);
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    const key = names.get(arg) ?? (flags.has(arg) ? arg : null);
    if (!key || Object.hasOwn(options, key)) reject('INVALID_ARGUMENT');
    if (flags.has(arg)) options[key] = true;
    else {
      const value = args[++index];
      if (typeof value !== 'string' || value.startsWith('--')) reject('INVALID_ARGUMENT');
      options[key] = value;
    }
  }
  if (options['--allow-live'] !== true || options['--allow-fixture-writes'] !== true || !options.config) reject('EXPLICIT_AUTHORIZATION_REQUIRED');
  absoluteFile(options.config);
  for (const key of ['credentialFd', 'readonlyCredentialFd']) {
    if (!/^(?:[3-9]|[1-9][0-9]|1[0-9]{2}|2[0-4][0-9]|25[0-5])$/.test(options[key] ?? '')) reject('INVALID_ARGUMENT');
    options[key] = Number(options[key]);
  }
  if (options.credentialFd === options.readonlyCredentialFd) reject('INVALID_ARGUMENT');
  if (options.driver !== undefined) absoluteFile(options.driver);
  if (options.report !== undefined) absoluteFile(options.report);
  return options;
}
