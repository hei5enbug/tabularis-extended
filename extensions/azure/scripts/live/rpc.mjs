import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { LiveError, absoluteFile, object } from './config.mjs';

const METHODS = new Set(['initialize', 'shutdown', 'cancel_request', 'get_columns', 'query_page', 'read_document', 'create_document', 'replace_document', 'delete_document', 'service_invalidate_auth']);
const FRAME = 16 * 1024 * 1024;
const LOG = 8 * 1024 * 1024;
const CODES = new Set(['AUTH_EXPIRED', 'AUTH_REQUIRED', 'INVALID_ARGUMENT', 'INVALID_PAGE_TOKEN', 'CURSOR_EXPIRED', 'DOCUMENT_ALREADY_EXISTS', 'DOCUMENT_NOT_FOUND', 'ETAG_CONFLICT', 'PARTITION_KEY_IMMUTABLE', 'PERMISSION_DENIED', 'WRITE_NOT_ALLOWED', 'RESOURCE_LIMIT', 'OUTCOME_UNKNOWN', 'CANCELLED', 'DEADLINE_EXCEEDED', 'UNSUPPORTED_OPERATION']);
export function safeFailure(value, write = false) {
  const code = CODES.has(value?.code) ? value.code : 'RPC_FAILURE';
  return new LiveError(code, value?.outcome === 'unknown' || code === 'OUTCOME_UNKNOWN' ? 'unknown' : write && value === undefined ? 'unknown' : 'not_applied');
}
export function privateWorkspace() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-c4-'));
  fs.chmodSync(root, 0o700);
  return root;
}
export class NativeRpc {
  constructor({ driver, spawnChild = spawn, requestTimeout = 120_000, closeTimeout = 5000 } = {}) {
    this.profile = privateWorkspace();
    this.pending = new Map(); this.frame = Buffer.alloc(0); this.logBytes = 0; this.closed = false; this.nextId = 0;
    this.child_exit_observed = false;
    this.childExit = new Promise(resolve => { this.resolveChildExit = resolve; });
    this.requestTimeout = Math.min(120_000, Math.max(1, requestTimeout)); this.closeTimeout = Math.min(5000, Math.max(1, closeTimeout));
    const entry = fileURLToPath(new URL('../../dist/index.js', import.meta.url));
    const executable = driver === undefined ? process.execPath : absoluteFile(driver);
    try {
      this.child = spawnChild(executable, driver === undefined ? [entry] : [], { shell: false, env: { PATH: '', HOME: this.profile, USERPROFILE: this.profile, TMPDIR: this.profile }, stdio: ['pipe', 'pipe', 'pipe'] });
    } catch { fs.rmSync(this.profile, { recursive: true, force: true }); throw new LiveError('DRIVER_START_FAILED'); }
    this.child.stdout.on('data', chunk => this.receive(chunk));
    this.child.stderr.on('data', chunk => { this.logBytes += chunk.length; if (this.logBytes > LOG) this.fail('RESOURCE_LIMIT'); });
    this.child.stdin.on('error', () => this.fail('DRIVER_EXITED'));
    this.child.on('error', () => this.fail('DRIVER_EXITED'));
    this.child.once('close', () => {
      this.child_exit_observed = true; this.resolveChildExit(); this.closed = true; this.rejectPending('DRIVER_EXITED');
      if (this.disposing) this.cleanupProfile();
    });
  }
  rejectPending(code) {
    for (const item of this.pending.values()) { clearTimeout(item.timer); item.signal?.removeEventListener('abort', item.abort); item.reject(new LiveError(code, item.write ? 'unknown' : 'not_applied')); }
    this.pending.clear();
  }
  fail(code) { this.rejectPending(code); this.closed = true; this.child.kill('SIGKILL'); }
  cleanupProfile() {
    if (!this.child_exit_observed) return;
    try { fs.rmSync(this.profile, { recursive: true, force: true }); } catch { this.cleanupFailed = true; }
  }
  async waitForChildExit() {
    if (this.child_exit_observed) return true;
    let timer;
    try { return await Promise.race([this.childExit.then(() => true), new Promise(resolve => { timer = setTimeout(() => resolve(false), this.closeTimeout); })]); }
    finally { clearTimeout(timer); }
  }
  receive(chunk) {
    let remaining = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    while (remaining.length) {
      const newline = remaining.indexOf(10);
      const piece = newline < 0 ? remaining : remaining.subarray(0, newline);
      if (this.frame.length + piece.length > FRAME) { this.fail('RESOURCE_LIMIT'); return; }
      this.frame = Buffer.concat([this.frame, piece]);
      if (newline < 0) return;
      let response;
      try {
        response = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(this.frame));
        if (!object(response) || response.jsonrpc !== '2.0' || !Object.hasOwn(response, 'id') || (Object.hasOwn(response, 'result') === Object.hasOwn(response, 'error'))) throw new Error();
      } catch { this.fail('INVALID_RPC_FRAME'); return; }
      this.frame = Buffer.alloc(0);
      const item = this.pending.get(response.id);
      if (item) {
        this.pending.delete(response.id); clearTimeout(item.timer); item.signal?.removeEventListener('abort', item.abort);
        if (response.error) item.reject(safeFailure(response.error.data, item.write));
        else item.resolve(response.result);
      }
      remaining = remaining.subarray(newline + 1);
    }
  }
  controlCancel(connectionId, requestId) {
    if (this.closed) return;
    try { this.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: `cancel-${++this.nextId}`, method: 'cancel_request', params: { connection_id: connectionId, request_id: requestId } })}\n`); } catch { this.fail('DRIVER_EXITED'); }
  }
  request(method, params, { signal, timeout = this.requestTimeout, write = false } = {}) {
    if (!METHODS.has(method)) return Promise.reject(new LiveError('UNSUPPORTED_OPERATION'));
    if (this.closed) return Promise.reject(new LiveError('DRIVER_EXITED'));
    if (signal?.aborted) return Promise.reject(new LiveError('CANCELLED'));
    if (this.pending.size >= 64) return Promise.reject(new LiveError('RESOURCE_LIMIT'));
    const id = ++this.nextId;
    let frame;
    try { frame = `${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`; } catch { return Promise.reject(new LiveError('INVALID_ARGUMENT')); }
    if (Buffer.byteLength(frame) > FRAME) return Promise.reject(new LiveError('RESOURCE_LIMIT'));
    return new Promise((resolve, reject) => {
      const stop = code => {
        const item = this.pending.get(id);
        if (!item) return;
        this.pending.delete(id); clearTimeout(item.timer); signal?.removeEventListener('abort', item.abort);
        const context = params?.driver_context;
        if (context) this.controlCancel(context.connection_id, context.request_id);
        reject(new LiveError(code, write ? 'unknown' : 'not_applied'));
      };
      const abort = () => stop('CANCELLED');
      const timer = setTimeout(() => stop('DEADLINE_EXCEEDED'), Math.max(1, Math.min(timeout, this.requestTimeout, 120_000)));
      this.pending.set(id, { resolve, reject, timer, abort, signal, write });
      signal?.addEventListener('abort', abort, { once: true });
      try { this.child.stdin.write(frame); } catch { this.fail('DRIVER_EXITED'); }
    });
  }
  async close() {
    if (this.disposing) return this.disposing;
    this.disposing = (async () => {
      try {
        if (!this.closed) {
          for (const item of this.pending.values()) item.abort();
          await this.request('shutdown', {}, { timeout: Math.min(this.closeTimeout, 3000) }).catch(() => {});
          this.child.stdin.end();
        }
        if (!await this.waitForChildExit()) {
          this.child.kill('SIGKILL');
          if (!await this.waitForChildExit()) throw new LiveError('DRIVER_EXITED', 'unknown');
        }
        this.cleanupProfile();
        if (this.cleanupFailed) throw new LiveError('DRIVER_EXITED', 'unknown');
      } finally { this.closed = true; this.rejectPending('DRIVER_EXITED'); this.cleanupProfile(); }
    })();
    return this.disposing;
  }
}
export function wire(config, auth, connectionId, input, { readOnly = true, invalidate = false, deadline = 120_000 } = {}) {
  const params = invalidate ? { driver: 'cosmos-nosql', connection_id: connectionId } : { driver: 'cosmos-nosql', database: config.database, extra: { endpoint: config.endpoint, database: config.database, auth_mode: config.auth_mode, ...(config.tenant_id ? { tenant_id: config.tenant_id, client_id: config.client_id } : {}) } };
  return { params, driver_context: { protocol_version: 1, connection_id: connectionId, request_id: randomUUID(), deadline_ms: deadline, read_only: readOnly, ...(invalidate ? {} : { auth }) }, input };
}
