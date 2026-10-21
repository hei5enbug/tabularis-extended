import { Ajv } from 'ajv';
import requestSchema from '@tabularis/service-contracts/schema/v1/request.json';
import responseSchema from '@tabularis/service-contracts/schema/v1/response.json';
import type { JsonValue, ServiceRequest, ServiceResponse } from '@tabularis/service-contracts/types';
import type { DocumentEntry, SavedConnection } from '../src/models';

export const connection: SavedConnection = { connection_id: 'cosmos-a', name: '첫 번째 계정', driver: 'cosmos-nosql', settings: { extra: { endpoint: 'https://example.documents.azure.com', auth_mode: 'account_key', database: 'db' } }, version: 4, allow_writes: true };
export const containerMetadata = { columns: [{ name: 'id', data_type: 'string' }], partition_key_paths: ['/tenant', '/nested/region', '/optional'], partition_key_kind: 'MultiHash', partition_key_version: 2, system_key: false };
export const original: DocumentEntry = { document: { id: 'doc-1', tenant: '서울', nested: { region: null }, date: '2026-10-03T00:00:00Z', array: [1, null, { child: true }], _custom: '보존', _etag: 'etag-1', _rid: 'rid-1', _ts: 1 }, identity: { id: 'doc-1', partition_key: [{ type: 'string', value: '서울' }, { type: 'null' }, { type: 'undefined' }] }, etag: 'etag-1' };
const ajv = new Ajv({ allErrors: true, strict: false, coerceTypes: false, useDefaults: false, removeAdditional: false });
const validateRequest = ajv.compile(requestSchema);
const validateResponse = ajv.compile(responseSchema);
export function json(value: unknown): JsonValue { return value as JsonValue; }
export function response(data: unknown, patch: Partial<ServiceResponse> = {}): ServiceResponse {
  return { protocol_version: 1, request_id: 'fixture', connection_id: connection.connection_id, status: 'succeeded', job_id: null, result_id: null, data: json(data), page: { next_token: null, has_more: false, resume_mode: 'none' }, limits: { truncated: false, reasons: [] }, metrics: { elapsed_ms: 1, request_charge: 2.5, retry_count: 0 }, warnings: [], error: null, ...patch };
}
export function checked(request: ServiceRequest): void {
  if (!validateRequest(request)) throw new Error(`Invalid fixture request: ${JSON.stringify(validateRequest.errors)}`);
}
export function checkedResponse(value: ServiceResponse): ServiceResponse {
  if (!validateResponse(value)) throw new Error(`Invalid fixture response: ${JSON.stringify(validateResponse.errors)}`);
  return value;
}
export function failure(code: NonNullable<ServiceResponse['error']>['code'], status: ServiceResponse['status'] = 'failed'): ServiceResponse {
  return response(null, { status, error: { code, message: '서버 오류', retryable: false, outcome: status === 'outcome_unknown' ? 'unknown' : 'not_applied', details: null } });
}
