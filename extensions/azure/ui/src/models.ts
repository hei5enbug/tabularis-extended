import type { AuthMode, DocumentIdentity, JsonObject, JsonValue, PartitionKeyComponent, QueryParameter, ServiceResponse } from '@tabularis/service-contracts/types';

export interface SavedConnection {
  connection_id: string; name: string; driver: string; settings: JsonObject; version: number; allow_writes: boolean;
}
export interface ConnectionFields { endpoint: string; auth_mode: AuthMode; auth_source?: "oauth" | "azure_cli"; tenant_id: string; client_id: string; database: string }
export interface ContainerMetadata {
  columns: JsonValue[]; partition_key_paths: string[]; partition_key_kind: string; partition_key_version: number; system_key: boolean;
}
export interface DocumentEntry { document: JsonObject; identity: DocumentIdentity; etag: string }
export interface Origin { database: string; container: string }
export interface ResultData { kind: 'documents' | 'json_values'; documents: DocumentEntry[]; values: JsonValue[]; origin: Origin | null }
export interface SnapshotRef { result_id: string; result_set_index: number; row_ordinal: number; generation: number }
export const SYSTEM_FIELDS = ['_rid', '_self', '_etag', '_attachments', '_ts'] as const;

export class UiError extends Error { constructor(public code: string, public response?: ServiceResponse) { super(code); } }
export function object(value: unknown): value is JsonObject { return !!value && typeof value === 'object' && !Array.isArray(value); }
export function safeIndex(value: unknown): value is number { return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0; }
export function connections(data: JsonValue): SavedConnection[] {
  if (!object(data) || !Array.isArray(data.connections)) throw new UiError('INVALID_RESPONSE');
  return data.connections.filter((item): item is JsonObject => object(item) && item.driver === 'cosmos-nosql').map(savedConnection);
}
export function savedConnection(value: JsonValue): SavedConnection {
  if (!object(value) || typeof value.connection_id !== 'string' || !value.connection_id || typeof value.name !== 'string' || value.driver !== 'cosmos-nosql' || !object(value.settings) || !safeIndex(value.version) || typeof value.allow_writes !== 'boolean') throw new UiError('INVALID_RESPONSE');
  return value as unknown as SavedConnection;
}
export function connectionFields(settings: JsonObject): ConnectionFields {
  const extra = object(settings.extra) ? settings.extra : {};
  const field = (name: string) => typeof extra[name] === 'string' ? extra[name] as string : typeof settings[name] === 'string' ? settings[name] as string : '';
  const mode = field('auth_mode');
  return { endpoint: field('endpoint') || field('host'), auth_mode: mode === 'entra_user' || mode === 'entra_service_principal' ? mode : 'account_key', ...(field('auth_source') ? { auth_source: field('auth_source') as 'oauth' | 'azure_cli' } : {}), tenant_id: field('tenant_id'), client_id: field('client_id'), database: field('database') };
}
export function publicEndpoint(endpoint: string): string {
  let url: URL;
  try { url = new URL(endpoint); } catch { throw new UiError('INVALID_ENDPOINT'); }
  if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash || (url.port && url.port !== '443') || url.pathname !== '/' || !/^[a-z0-9][a-z0-9-]*\.documents\.azure\.com$/i.test(url.hostname)) throw new UiError('INVALID_ENDPOINT');
  return url.origin;
}
export function settingsFromFields(fields: ConnectionFields, previous: JsonObject = {}): JsonObject {
  const endpoint = publicEndpoint(fields.endpoint);
  if (fields.auth_mode !== 'account_key' && (!fields.tenant_id.trim() || !fields.client_id.trim())) throw new UiError('MISSING_AUTH_FIELDS');
  return { ...previous, extra: { ...(object(previous.extra) ? previous.extra : {}), ...fields, endpoint } };
}
export function metadata(value: JsonValue): ContainerMetadata {
  if (!object(value) || !Array.isArray(value.columns) || !Array.isArray(value.partition_key_paths) || value.partition_key_paths.length < 1 || value.partition_key_paths.length > 3 || !value.partition_key_paths.every(path => typeof path === 'string' && path.startsWith('/')) || typeof value.partition_key_kind !== 'string' || !safeIndex(value.partition_key_version) || typeof value.system_key !== 'boolean') throw new UiError('METADATA_UNAVAILABLE');
  const result = value as unknown as ContainerMetadata;
  result.partition_key_paths.forEach(pathParts);
  return result;
}
export function pathParts(path: string): string[] {
  const parts: string[] = [];
  let offset = 0;
  while (offset < path.length) {
    if (path[offset] !== '/') throw new UiError('UNSUPPORTED_PARTITION_PATH');
    offset += 1;
    if (offset === path.length) break;
    const quote = path[offset];
    if (quote === '"' || quote === "'") {
      const start = ++offset;
      while (offset < path.length && (path[offset] !== quote || path[offset - 1] === '\\')) offset += 1;
      if (offset === path.length) throw new UiError('UNSUPPORTED_PARTITION_PATH');
      parts.push(path.slice(start, offset++));
    } else {
      const end = path.indexOf('/', offset);
      parts.push(path.slice(offset, end < 0 ? undefined : end).trim());
      offset = end < 0 ? path.length : end;
    }
  }
  if (!parts.length || parts.some(part => !part)) throw new UiError('UNSUPPORTED_PARTITION_PATH');
  return parts;
}
export function samePartition(left: PartitionKeyComponent[], right: PartitionKeyComponent[]): boolean {
  return left.length === right.length && left.every((part, index) => {
    const other = right[index];
    return part.type === other?.type && (!('value' in part) || (!!other && 'value' in other && part.value === other.value));
  });
}
export function component(value: unknown, exists = true): PartitionKeyComponent {
  if (!exists) return { type: 'undefined' };
  if (value === null) return { type: 'null' };
  if (typeof value === 'string') return { type: 'string', value };
  if (typeof value === 'boolean') return { type: 'boolean', value };
  if (typeof value === 'number' && Number.isFinite(value) && (!Number.isInteger(value) || Number.isSafeInteger(value))) return { type: 'number', value };
  throw new UiError('INVALID_PARTITION_KEY');
}
export function partitionFromDocument(document: JsonObject, meta: ContainerMetadata): PartitionKeyComponent[] {
  return meta.partition_key_paths.map(path => {
    let value: JsonValue = document;
    for (const part of pathParts(path)) {
      if (!value || typeof value !== 'object' || !Object.hasOwn(value, part)) return component(undefined, false);
      value = (value as JsonObject)[part];
    }
    return component(value);
  });
}
export function validatePartition(key: PartitionKeyComponent[], meta: ContainerMetadata, partial = false): void {
  if (!key.length || key.length > meta.partition_key_paths.length || (!partial && key.length !== meta.partition_key_paths.length)) throw new UiError('INCOMPLETE_PARTITION_KEY');
  key.forEach(item => {
    if (!object(item) || !['null', 'undefined', 'string', 'number', 'boolean'].includes(item.type as string)) throw new UiError('INVALID_PARTITION_KEY');
    if (item.type === 'null' || item.type === 'undefined') {
      if (Object.keys(item).length !== 1) throw new UiError('INVALID_PARTITION_KEY');
    } else if (Object.keys(item).length !== 2 || component(item.value).type !== item.type) throw new UiError('INVALID_PARTITION_KEY');
  });
}
export function validateId(id: unknown): asserts id is string {
  if (typeof id !== 'string' || !id || /[\\/#?\u0000-\u001f\u007f]/.test(id) || new TextEncoder().encode(id).length > 1023) throw new UiError('INVALID_DOCUMENT_ID');
}
export function entry(value: JsonValue): DocumentEntry {
  if (!object(value) || !object(value.document) || !object(value.identity) || typeof value.etag !== 'string' || !value.etag || !Array.isArray(value.identity.partition_key)) throw new UiError('INVALID_DOCUMENT_ENTRY');
  validateId(value.identity.id);
  if (value.document.id !== value.identity.id) throw new UiError('INVALID_DOCUMENT_ENTRY');
  const key = value.identity.partition_key as unknown as PartitionKeyComponent[];
  validatePartition(key, { columns: [], partition_key_paths: key.map(() => '/key'), partition_key_kind: 'Hash', partition_key_version: 1, system_key: false });
  return value as unknown as DocumentEntry;
}
export function resultData(value: JsonValue): ResultData {
  if (!object(value)) throw new UiError('INVALID_RESPONSE');
  const origin = object(value.origin) && typeof value.origin.database === 'string' && value.origin.database && typeof value.origin.container === 'string' && value.origin.container ? value.origin as unknown as Origin : null;
  if (value.kind === 'documents' && Array.isArray(value.documents)) return { kind: 'documents', documents: value.documents.map(entry), values: [], origin };
  if (value.kind === 'json_values' && Array.isArray(value.values)) return { kind: 'json_values', values: value.values, documents: [], origin };
  if (value.kind === 'tabular' && Array.isArray(value.rows)) return { kind: 'json_values', values: value.rows, documents: [], origin: null };
  throw new UiError('INVALID_RESPONSE');
}
export function parseJson(text: string): JsonValue {
  const outsideStrings = text.replace(/"(?:\\.|[^"\\])*"/g, '""');
  for (const match of outsideStrings.matchAll(/-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/g)) {
    const number = Number(match[0]);
    if (!Number.isFinite(number) || (Number.isInteger(number) && !Number.isSafeInteger(number))) throw new UiError('UNSAFE_NUMBER');
  }
  try { return JSON.parse(text) as JsonValue; } catch { throw new UiError('INVALID_JSON'); }
}
export function documentPayload(text: string, meta: ContainerMetadata, original?: DocumentEntry): JsonObject {
  if (meta.system_key) throw new UiError('SYSTEM_PARTITION_KEY');
  const value = parseJson(text);
  if (!object(value)) throw new UiError('DOCUMENT_OBJECT_REQUIRED');
  validateId(value.id);
  const key = partitionFromDocument(value, meta);
  validatePartition(key, meta);
  if (original && (value.id !== original.identity.id || !samePartition(key, original.identity.partition_key))) throw new UiError('IDENTITY_READ_ONLY');
  for (const name of SYSTEM_FIELDS) {
    if (Object.hasOwn(value, name) && (!original || JSON.stringify(value[name]) !== JSON.stringify(original.document[name]))) throw new UiError('SYSTEM_FIELD_READ_ONLY');
  }
  const clean = Object.fromEntries(Object.entries(value).filter(([name]) => !(SYSTEM_FIELDS as readonly string[]).includes(name))) as JsonObject;
  if (new TextEncoder().encode(JSON.stringify(clean)).length > 2 * 1024 * 1024) throw new UiError('DOCUMENT_TOO_LARGE');
  return clean;
}
export function parameters(text: string): QueryParameter[] {
  const value = parseJson(text);
  if (!Array.isArray(value)) throw new UiError('INVALID_PARAMETERS');
  const names = new Set<string>();
  return value.map(param => {
    if (!object(param) || typeof param.name !== 'string' || !/^@[A-Za-z_][A-Za-z0-9_]*$/.test(param.name) || !Object.hasOwn(param, 'value') || Object.keys(param).length !== 2 || names.has(param.name)) throw new UiError('INVALID_PARAMETERS');
    names.add(param.name);
    return { name: param.name, value: param.value };
  });
}
export function snapshotRef(context: Record<string, unknown>): SnapshotRef | null {
  if (context.isInsertion || typeof context.resultId !== 'string' || !context.resultId || !safeIndex(context.resultSetIndex) || !safeIndex(context.resultGeneration) || !safeIndex(context.resultRowOrdinal)) return null;
  return { result_id: context.resultId, result_set_index: context.resultSetIndex, row_ordinal: context.resultRowOrdinal, generation: context.resultGeneration };
}
export function challengeUrl(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  try {
    const url = new URL(value);
    return url.protocol === 'https:' && url.hostname === 'microsoft.com' && url.pathname === '/devicelogin' && (!url.port || url.port === '443') && !url.username && !url.password && !url.search && !url.hash && !/[?#@]/.test(value) ? url.href : null;
  } catch { return null; }
}
export function errorText(error: unknown): string {
  const code = error instanceof UiError ? error.code : 'REQUEST_FAILED';
  const messages: Record<string, string> = {
    CANCELLED: '작업이 취소되었습니다.', STALE_RESPONSE: '선택이 변경되어 이전 결과를 적용하지 않았습니다.',
    OUTCOME_UNKNOWN: '작업 결과를 확인할 수 없습니다. 다시 전송하지 말고 최신 문서를 조회하세요.',
    ETAG_CONFLICT: '문서가 변경되었습니다. 최신 조회 버튼으로 확인한 뒤 다시 결정하세요.',
    VERSION_CONFLICT: '연결 설정이 변경되었습니다. 최신 설정을 확인하고 다시 저장하세요.',
    METADATA_UNAVAILABLE: '파티션 키 정보를 확인할 수 없어 문서 쓰기를 사용할 수 없습니다.',
    IDENTITY_READ_ONLY: '문서 ID와 파티션 키는 변경할 수 없습니다.', SYSTEM_FIELD_READ_ONLY: '시스템 속성은 변경할 수 없습니다.',
    SYSTEM_PARTITION_KEY: '시스템 파티션 키 컨테이너는 문서 쓰기를 지원하지 않습니다.',
    INVALID_ENDPOINT: 'Azure Cosmos DB for NoSQL의 HTTPS 계정 주소를 입력하세요.', MISSING_AUTH_FIELDS: '테넌트 ID와 클라이언트 ID를 입력하세요.',
    UNSAFE_NUMBER: '정확하게 표현할 수 없는 숫자는 사용할 수 없습니다.', INVALID_JSON: '올바른 JSON을 입력하세요.',
    INCOMPLETE_PARTITION_KEY: '파티션 키의 모든 구성 요소를 순서대로 입력하세요.', CAPABILITY_UNAVAILABLE: '이 연결에서는 문서 서비스를 사용할 수 없습니다.',
  };
  const message = messages[code] || `작업을 완료하지 못했습니다. (${code})`;
  if (!(error instanceof UiError) || !error.response) return message;
  const outcomes = { not_started: '시작되지 않음', not_applied: '적용되지 않음', applied: '적용됨', unknown: '확인 불가' };
  const statuses: Record<string, string> = { failed: '실패', cancelled: '취소', interrupted: '중단', outcome_unknown: '결과 불명' };
  return `${message} 상태: ${statuses[error.response.status] || '미완료'} · 결과: ${error.response.error ? outcomes[error.response.error.outcome] : '확인 불가'}`;
}
