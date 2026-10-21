import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { randomUUID } from 'node:crypto';
import { LiveError } from '../../scripts/live/config.mjs';
import { stable, identity, hash, ordered, groupOracle } from '../../scripts/live/fixture.mjs';

export const CONFIG = { version: 1, dedicated_test_resource: true, endpoint: 'https://synthetic.documents.azure.com', database: 'fixture_db', container: 'tabularis_test_basic', hierarchical_container: 'tabularis_test_hpk', auth_mode: 'account_key' };
export const SECRET = 'SYNTHETIC_PASSWORD_TOKEN_CANARY';
export function auth() { return { owner: { kind: 'account_key', account_key: SECRET, identity: 'owner' }, readonly: { kind: 'account_key', account_key: `${SECRET}_readonly`, identity: 'readonly' } }; }
export function privateFixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-c4-synthetic-'));
  fs.chmodSync(root, 0o700); t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const config = path.join(root, 'config.json'); fs.writeFileSync(config, JSON.stringify(CONFIG), { mode: 0o600 });
  return { root, config };
}
export function capture(action) { try { return { value: action(), code: null }; } catch (error) { return { value: null, code: error.code }; } }
export function fdReader(text) {
  const bytes = Buffer.isBuffer(text) ? text : Buffer.from(text); let offset = 0;
  return (_fd, target, start, length) => { const count = Math.min(length, bytes.length - offset); bytes.copy(target, start, offset, offset + count); offset += count; return count; };
}
export function childFixture(handler = () => undefined) {
  const child = new EventEmitter(); child.stdout = new PassThrough(); child.stderr = new PassThrough(); child.stdin = new PassThrough();
  child.frames = []; child.kills = [];
  child.kill = signal => { child.kills.push(signal); queueMicrotask(() => child.emit('close', null, signal)); return true; };
  child.stdin.on('data', data => {
    const frame = JSON.parse(data.toString('utf8')); child.frames.push(frame);
    const response = handler(frame, child);
    if (response !== undefined) queueMicrotask(() => child.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id: frame.id, ...response })}\n`));
  });
  child.stdin.on('finish', () => queueMicrotask(() => child.emit('close', 0, null)));
  return child;
}

export function syntheticScenario(options = {}) {
  const state = { records: [], store: new Map(), processes: 0, closed: 0, etags: 0, creates: 0, deletes: [], emptyResumes: 0 };
  const key = (container, target) => stable({ container, identity: target });
  function document(document) { return { ...structuredClone(document), _rid: 'synthetic-rid', _self: 'synthetic-self', _attachments: 'attachments/', _etag: `synthetic-etag-${++state.etags}`, _ts: 1000 }; }
  function output(rows, mode, query, context, offset = 0, native = true) {
    const page = rows.slice(offset, offset + query.page_size);
    const snapshotRows = native ? page : rows;
    const cursor = native && offset + page.length < rows.length ? {
      binding: { sdk_version: '4.10.1', container_rid: 'synthetic-container', endpoint_identity: 'synthetic-endpoint', partition_hash: hash(null) },
      native_state: { version: 1, sdk_version: '4.10.1', connection_id: context.connection_id, auth_identity: context.auth.identity, auth_binding: 'synthetic-auth', endpoint_identity: 'synthetic-endpoint', database_rid: 'synthetic-db', container_rid: 'synthetic-container', partition_hash: hash(null), query_hash: hash(query), query: structuredClone(query), original_options: {}, continuation_token: String(offset + page.length), revision: randomUUID() },
      tail: [], columns: [], page_size: query.page_size, expires_at_ms: Date.now() + 900000,
    } : null;
    return { data: mode === 'documents' ? { kind: mode, documents: page } : { kind: mode, values: page }, snapshot: { sets: [{ kind: mode, columns: [], rows: snapshotRows }], revision: randomUUID(), limits: { truncated: false, reasons: [] }, allow_truncation: false, warnings: [] }, cursor, metrics: { elapsed_ms: 1, request_charge: 1, retry_count: 0 }, limits: { truncated: false, reasons: [] }, warnings: [], confirmed_cancelled: false, session_closed: false };
  }
  const factory = () => {
    state.processes += 1;
    return {
      async close() { state.closed += 1; state.records.push({ method: 'shutdown' }); },
      async request(method, params, requestOptions = {}) {
        state.records.push({ method, params: structuredClone(params), options: { write: requestOptions.write } });
        if (method === 'initialize') return { service_capabilities: { service_protocol: 1, documents_v1: true, query_page_v1: true, cancel_v1: true } };
        const context = params.driver_context; const input = params.input;
        const success = data => ({ protocol_version: 1, request_id: context.request_id, connection_id: context.connection_id, status: 'succeeded', data, error: null, metrics: { elapsed_ms: 1, request_charge: 1, retry_count: 0 } });
        const fail = code => ({ ...success(null), status: 'failed', error: { code, outcome: 'not_applied', retryable: false, details: null } });
        if (requestOptions.signal?.aborted) throw new LiveError('CANCELLED');
        if (context.auth?.kind === 'entra_token' && context.auth.expires_at_ms < Date.now()) return fail('AUTH_EXPIRED');
        if (method === 'get_columns') return success({ columns: [], partition_key_paths: input.table.table === CONFIG.hierarchical_container ? ['/tenant', '/region'] : options.badMetadata ? ['/wrong'] : ['/tenant'], partition_key_kind: input.table.table === CONFIG.hierarchical_container ? 'MultiHash' : 'Hash', partition_key_version: 2, system_key: false });
        if (method === 'service_invalidate_auth') return success({ invalidated: true });
        if (['create_document', 'replace_document', 'delete_document'].includes(method)) {
          if (context.read_only !== false) return fail('WRITE_NOT_ALLOWED');
          if (context.auth.identity === 'readonly' && !options.readonlyWrites) return fail('PERMISSION_DENIED');
        }
        if (method === 'create_document') {
          const target = { id: input.document.id, partition_key: input.partition_key };
          const index = key(input.container, target); if (state.store.has(index)) return fail('DOCUMENT_ALREADY_EXISTS');
          const stored = document(input.document); state.store.set(index, stored); state.creates += 1;
          if (options.foreignOwned && state.creates === 1) stored.run_id = 'foreign-owner';
          if (options.unknownCreate && state.creates === 1) throw new LiveError('OUTCOME_UNKNOWN', 'unknown');
          return success({ document: stored, identity: target, etag: stored._etag });
        }
        if (method === 'read_document') {
          const stored = state.store.get(key(input.container, input.identity));
          return stored ? success({ document: structuredClone(stored), identity: input.identity, etag: stored._etag }) : fail('DOCUMENT_NOT_FOUND');
        }
        if (method === 'replace_document') {
          const stored = state.store.get(key(input.container, input.identity)); if (!stored) return fail('DOCUMENT_NOT_FOUND');
          if (input.document.id !== input.identity.id || stable(identity(input.document, input.container === CONFIG.hierarchical_container).partition_key) !== stable(input.identity.partition_key)) return fail('PARTITION_KEY_IMMUTABLE');
          if (stored._etag !== input.if_match) return fail('ETAG_CONFLICT');
          const changed = document(input.document); state.store.set(key(input.container, input.identity), changed);
          return success({ document: changed, identity: input.identity, etag: changed._etag });
        }
        if (method === 'delete_document') {
          const index = key(input.container, input.identity); const stored = state.store.get(index);
          state.deletes.push(index); if (!stored) return fail('DOCUMENT_NOT_FOUND');
          if (stored._etag !== input.if_match) return fail('ETAG_CONFLICT');
          state.store.delete(index);
          if ((options.unknownDelete && state.deletes.length === 1) || (options.unknownCleanup && stored.id.endsWith('-item-00'))) throw new LiveError('OUTCOME_UNKNOWN', 'unknown');
          return success({ identity: input.identity, deleted: true });
        }
        if (method === 'query_page') {
          const query = input.query; const cursor = input.cursor_state;
          if (cursor && (cursor.native_state.query_hash !== hash(query) || cursor.native_state.connection_id !== context.connection_id || cursor.binding.sdk_version !== '4.10.1' || cursor.native_state.container_rid !== cursor.binding.container_rid)) return fail('INVALID_PAGE_TOKEN');
          if (cursor && options.validResumeFails) return fail('INVALID_PAGE_TOKEN');
          if (cursor && options.emptyFirstResume && state.emptyResumes === 0) {
            state.emptyResumes += 1;
            const empty = output([], query.result_mode ?? 'json_values', query, context);
            empty.cursor = structuredClone(cursor);
            return success(empty);
          }
          const run = query.parameters.find(parameter => parameter.name === '@run').value;
          const docs = ordered([...state.store.entries()].filter(([index, document]) => JSON.parse(index).container === query.container && document.run_id === run).map(([, document]) => document));
          let rows; let native = true;
          if (query.text.includes('GROUP BY')) { rows = groupOracle(docs); native = false; }
          else if (query.text.includes('COUNT(1)')) { rows = [docs.length]; native = false; }
          else if (query.text.includes('SUM(c.amount)')) { rows = [docs.reduce((sum, document) => sum + document.amount, 0)]; native = false; }
          else if (query.text.startsWith('SELECT c.id,c.status,c.nested')) rows = docs.filter(document => document.status === query.parameters.find(parameter => parameter.name === '@status').value).map(({ id, status, nested }) => ({ id, status, nested }));
          else if (query.text.startsWith('SELECT VALUE null ')) rows = docs.map(() => null);
          else if (query.text.startsWith('SELECT VALUE c.nested.list ')) rows = docs.map(document => document.nested.list);
          else if (query.text.startsWith('SELECT VALUE c.id ')) rows = docs.map(document => document.id);
          else if (query.text.startsWith('SELECT VALUE c.amount ')) rows = docs.map(document => document.amount);
          else rows = docs.map(document => ({ document, identity: identity(document), etag: document._etag }));
          if (options.materialized && query.result_mode === 'documents') native = false;
          return success(output(rows, query.result_mode ?? 'json_values', query, context, cursor ? Number(cursor.native_state.continuation_token) : 0, native));
        }
        throw new LiveError('UNSUPPORTED_OPERATION');
      },
    };
  };
  return { factory, state };
}
