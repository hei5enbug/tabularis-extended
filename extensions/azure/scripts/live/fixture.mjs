import fs from 'node:fs';
import { randomUUID, createHash } from 'node:crypto';
import { LiveError } from './config.mjs';

function freeze(value) { if (value && typeof value === 'object') { Object.values(value).forEach(freeze); Object.freeze(value); } return value; }
export const DOCUMENTS = freeze(JSON.parse(fs.readFileSync(new URL('../../tests/live/fixtures/documents.json', import.meta.url), 'utf8')));
if (DOCUMENTS.length !== 37 || new Set(DOCUMENTS.map(document => document.id)).size !== 37) throw new LiveError('INVALID_FIXTURE');
export function materialize(runId = randomUUID()) {
  return DOCUMENTS.map(document => ({ ...structuredClone(document), id: `${runId}-${document.id}`, tenant: `${runId}:${document.tenant}`, run_id: runId }));
}
export function stable(value) {
  if (Array.isArray(value)) return JSON.stringify(value.map(item => JSON.parse(stable(item))));
  if (value && typeof value === 'object') return JSON.stringify(Object.fromEntries(Object.keys(value).sort().map(key => [key, JSON.parse(stable(value[key]))])));
  return JSON.stringify(value);
}
export function hash(value) { return createHash('sha256').update(typeof value === 'string' ? value : stable(value)).digest('hex'); }
export function userDocument(document) { return Object.fromEntries(Object.entries(document).filter(([key]) => !['_rid', '_self', '_etag', '_attachments', '_ts'].includes(key))); }
export function component(value) { return value === undefined ? { type: 'undefined' } : value === null ? { type: 'null' } : { type: typeof value, value }; }
export function identity(document, hierarchical = false) { return { id: document.id, partition_key: [component(document.tenant), ...(hierarchical ? [component(document.region)] : [])] }; }
export function ordered(documents) { return [...documents].sort((left, right) => left.sort - right.sort || (left.id < right.id ? -1 : left.id > right.id ? 1 : 0)); }
export function groupOracle(documents) {
  const groups = new Map();
  for (const document of documents) {
    const group = groups.get(document.status) ?? { status: document.status, count: 0, total: 0 };
    group.count += 1; group.total += document.amount; groups.set(document.status, group);
  }
  return [...groups.values()].sort((a, b) => a.status.localeCompare(b.status, 'en'));
}
