import fs from 'node:fs';
import { LiveError, closed, reject, uuidString } from './config.mjs';
import { resolveWireContext } from '../../dist/runtime/wire.js';

const SCOPE = 'https://cosmos.azure.com/.default';
export function validateCredential(value, config, now = Date.now(), deadline = 120_000) {
  const keys = value?.kind === 'account_key' ? ['kind', 'account_key', 'identity'] : ['kind', 'access_token', 'expires_at_ms', 'tenant_id', 'client_id', 'principal_id', 'scope', 'identity'];
  closed(value, keys, 'INVALID_CREDENTIAL');
  try {
    resolveWireContext({ jsonrpc: '2.0', id: 'validation', method: 'read_document', params: {
      params: { driver: 'cosmos-nosql', database: config.database, extra: { endpoint: config.endpoint, auth_mode: config.auth_mode, ...(config.tenant_id ? { tenant_id: config.tenant_id, client_id: config.client_id } : {}) } },
      driver_context: { protocol_version: 1, connection_id: 'validation', request_id: 'validation', deadline_ms: deadline, read_only: true, auth: value }, input: {},
    } });
  } catch { reject('INVALID_CREDENTIAL'); }
  if (typeof value.identity !== 'string' || /[\x00-\x1f\x7f]/.test(value.identity)) reject('INVALID_CREDENTIAL');
  if (config.auth_mode === 'account_key') {
    if (value.kind !== 'account_key') reject('INVALID_CREDENTIAL');
  } else if (value.kind !== 'entra_token' || value.scope !== SCOPE || value.expires_at_ms <= now + deadline + 300_000
    || value.tenant_id !== config.tenant_id || value.client_id !== config.client_id || !uuidString(value.principal_id)) reject('INVALID_CREDENTIAL');
  return value;
}
export function readCredentialFD(fd, config, { read = fs.readSync, now = Date.now, deadline = 120_000 } = {}) {
  if (!Number.isInteger(fd) || fd < 3 || fd > 255) reject('INVALID_CREDENTIAL');
  const bytes = Buffer.alloc(64 * 1024 + 1);
  let count = 0;
  try {
    while (count < bytes.length) {
      const size = read(fd, bytes, count, bytes.length - count, null);
      if (!Number.isInteger(size) || size < 0 || size > bytes.length - count) reject('INVALID_CREDENTIAL');
      if (size === 0) break;
      count += size;
    }
    if (count === 0 || count > 64 * 1024) reject('INVALID_CREDENTIAL');
    const value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, count)));
    return validateCredential(value, config, now(), deadline);
  } catch { throw new LiveError('INVALID_CREDENTIAL'); } finally { bytes.fill(0); }
}
export function releaseCredential(value) {
  if (value && typeof value === 'object') {
    if (Object.hasOwn(value, 'account_key')) value.account_key = '';
    if (Object.hasOwn(value, 'access_token')) value.access_token = '';
  }
}
