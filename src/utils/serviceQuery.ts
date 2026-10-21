import type { JsonObject, Operation, OperationInputs, ServiceRequest, ServiceResponse } from "@tabularis/service-contracts";
import type { QueryResult, ServiceResultReference } from "../types/editor";
import type { DriverCapabilities, PluginManifest } from "../types/plugins";
import type { SlotContext } from "../types/pluginSlots";

export interface QueryService {
  call(request: ServiceRequest, options?: { signal?: AbortSignal }): Promise<ServiceResponse>;
  executeWrite(request: ServiceRequest<"query.execute">, options?: { signal?: AbortSignal }): Promise<ServiceResponse>;
  capabilities(connectionId: string): Promise<{ service_protocol?: number; operations: readonly string[] }>;
}

export function serviceRequest<O extends Operation>(connectionId: string | null, operation: O, input: OperationInputs[O], sessionId: string | null = null, expectedVersion: number | null = null): ServiceRequest<O> {
  return { protocol_version: 1, operation, request_id: crypto.randomUUID(), connection_id: connectionId, session_id: sessionId, map_id: null, expected_version: expectedVersion, deadline_ms: 30000, input };
}

export function serviceObject(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The service response is invalid.");
  return value as Record<string, unknown>;
}

export function requireServiceSuccess(response: ServiceResponse): ServiceResponse {
  if (response.status !== "succeeded" || response.error) throw new Error(`${response.error?.code ?? response.status}: ${response.error?.message ?? "The request did not succeed."}`);
  return response;
}

export function servicePageSize(value?: number): number {
  return Math.min(1000, Math.max(1, Number.isFinite(value) && value! > 0 ? Math.floor(value!) : 1000));
}

export async function supportsServiceQuery(service: QueryService, connectionId: string): Promise<boolean> {
  const capabilities = await service.capabilities(connectionId);
  if (!Array.isArray(capabilities.operations)) throw new Error("The service capabilities are invalid.");
  return capabilities.service_protocol === 1 && capabilities.operations.includes("query.execute");
}

export function snapshotResult(response: ServiceResponse, context: { connectionId: string; driverId: string; generation: number; pageSize: number }, previous?: ServiceResultReference, page = 1): QueryResult {
  requireServiceSuccess(response);
  const data = serviceObject(response.data);
  const kind = data.kind;
  if (kind !== "tabular" && kind !== "documents" && kind !== "json_values") throw new Error("The service result kind is invalid.");
  const resultId = response.result_id ?? previous?.resultId;
  const revision = data.result_revision;
  const offset = data.offset;
  const setIndex = data.result_set_index;
  const setCount = data.result_set_count;
  if (!resultId || typeof revision !== "string" || !revision || !Number.isSafeInteger(offset) || (offset as number) < 0 || !Number.isSafeInteger(setIndex) || (setIndex as number) < 0 || !Number.isSafeInteger(setCount) || (setCount as number) < 1 || (setIndex as number) >= (setCount as number)) throw new Error("The immutable result reference is invalid.");
  if (previous && (resultId !== previous.resultId || revision !== previous.resultRevision || setIndex !== previous.resultSetIndex)) throw new Error("The immutable result reference changed.");
  const columnMetadata = kind === "tabular" ? (Array.isArray(data.columns) ? data.columns.map(serviceObject) : null) : [];
  if (!columnMetadata || columnMetadata.some((column, index) => typeof column.name !== "string" || column.column_index !== index)) throw new Error("The result columns are invalid.");
  const rawRows = kind === "tabular" ? data.rows : kind === "documents" ? data.documents : data.values;
  if (!Array.isArray(rawRows) || rawRows.length > 1000 || (kind === "tabular" && rawRows.some((row) => !Array.isArray(row) || row.length !== columnMetadata.length))) throw new Error("The result rows are invalid.");
  const rows = kind === "tabular" ? structuredClone(rawRows) as unknown[][] : rawRows.map((row) => [structuredClone(row)]);
  if (response.page.has_more && !rows.length && response.page.resume_mode === "materialized") throw new Error("The result page made no progress.");
  const offsets = previous?.pageSize === context.pageSize ? [...previous.pageOffsets] : [0];
  offsets[page - 1] = offset as number;
  if (rows.length > 0) offsets[page] = (offset as number) + rows.length;
  const origin: Record<string, unknown> | null = data.origin == null ? null : serviceObject(data.origin);
  const service: ServiceResultReference = {
    resultId, resultSetIndex: setIndex as number, resultGeneration: context.generation, resultRowOffset: offset as number,
    connectionId: context.connectionId, driverId: context.driverId, resultRevision: revision, resultSetCount: setCount as number,
    pageOffsets: offsets, pageSize: context.pageSize, nextToken: response.page.next_token, resumeMode: response.page.resume_mode,
    kind, columnMetadata: structuredClone(columnMetadata), warnings: [...response.warnings], limitReasons: [...response.limits.reasons],
    ...(origin && typeof origin.database === "string" && typeof origin.container === "string" ? { origin: { database: origin.database, container: origin.container } } : {}),
  };
  const affected = data.affected_rows;
  return { columns: kind === "tabular" ? columnMetadata.map((column) => column.name as string) : [kind === "documents" ? "document" : "value"], rows,
    affected_rows: typeof affected === "number" && Number.isSafeInteger(affected) && affected >= 0 ? affected : null,
    truncated: response.limits.truncated, pagination: { page, page_size: context.pageSize, total_rows: null, has_more: response.page.has_more }, service };
}

export async function readSnapshotPage(service: QueryService, result: QueryResult, page: number, size = result.service?.pageSize, signal?: AbortSignal): Promise<QueryResult> {
  const ref = result.service;
  if (!ref || !Number.isSafeInteger(page) || page < 1) throw new Error("The snapshot page is invalid.");
  const pageSize = servicePageSize(size);
  const offset = pageSize !== ref.pageSize ? (page === 1 ? 0 : undefined) : ref.pageOffsets[page - 1];
  if (offset === undefined) throw new Error("Read the next snapshot page before jumping ahead.");
  if (ref.resumeMode === "native" && page === (result.pagination?.page ?? 1) + 1) throw new Error("Use the Cosmos workspace to continue this query.");
  const request = serviceRequest(ref.connectionId, "result.get", { result_id: ref.resultId, result_set_index: ref.resultSetIndex, offset, limit: pageSize });
  const response = await service.call(request, { signal });
  return snapshotResult(response, { connectionId: ref.connectionId, driverId: ref.driverId, generation: ref.resultGeneration, pageSize }, ref, page);
}

export async function readAllSnapshotRows(service: QueryService, result: QueryResult, signal?: AbortSignal): Promise<QueryResult> {
  let page = await readSnapshotPage(service, result, 1, 1000, signal);
  const rows: unknown[][] = [];
  let bytes = new TextEncoder().encode(JSON.stringify({ columns: page.columns, service: page.service })).length;
  let bounded = false;
  for (let requests = 0; requests < 64; requests++) {
    for (const row of page.rows) {
      const rowBytes = new TextEncoder().encode(JSON.stringify(row)).length;
      if (rows.length >= 10000 || bytes + rowBytes > 32 * 1024 * 1024) { bounded = true; break; }
      rows.push(row); bytes += rowBytes;
    }
    if (bounded || !page.pagination?.has_more) return { ...page, rows, truncated: bounded || page.truncated, service: page.service && { ...page.service, limitReasons: bounded ? [...page.service.limitReasons, "COPY_LIMIT"] : page.service.limitReasons } };
    page = await readSnapshotPage(service, page, (page.pagination?.page ?? 1) + 1, 1000, signal);
  }
  return { ...page, rows, truncated: true, service: page.service && { ...page.service, limitReasons: [...page.service.limitReasons, "COPY_REQUEST_LIMIT"] } };
}

export function snapshotSlotContext(ref?: ServiceResultReference, originalRowIndex?: number, isInsertion = false): SlotContext {
  if (!ref || isInsertion) return {};
  const ordinal = originalRowIndex === undefined ? undefined : ref.resultRowOffset + originalRowIndex;
  return { connectionId: ref.connectionId, driver: ref.driverId, resultId: ref.resultId, resultSetIndex: ref.resultSetIndex, resultGeneration: ref.resultGeneration,
    ...(ordinal !== undefined && Number.isSafeInteger(ordinal) && ordinal >= 0 ? { rowIndex: ordinal, resultRowOrdinal: ordinal } : {}) };
}

export function connectionWriteState(response: ServiceResponse, connectionId: string): { allowWrites: boolean; version: number } {
  const data = serviceObject(requireServiceSuccess(response).data);
  if (!Array.isArray(data.connections)) throw new Error("The connection permissions are unavailable.");
  const connection = data.connections.map(serviceObject).find((item) => item.connection_id === connectionId);
  if (!connection || typeof connection.allow_writes !== "boolean" || !Number.isSafeInteger(connection.version)) throw new Error("The connection permissions are unavailable.");
  return { allowWrites: connection.allow_writes, version: connection.version as number };
}

export function permissionPatch(allowWrites: boolean): JsonObject { return { allow_writes: allowWrites }; }

export function orderedServicePrimaryKey(columns: { name: string; is_pk: boolean; primary_key_position?: number | null }[]): string[] | null {
  const keys = columns.filter((column) => column.is_pk);
  if (!keys.length) return null;
  if (keys.length === 1 && keys[0].primary_key_position == null) return [keys[0].name];
  const sorted = [...keys].sort((left, right) => (left.primary_key_position ?? 0) - (right.primary_key_position ?? 0));
  if (new Set(keys.map((column) => column.name)).size !== keys.length || sorted.some((column, index) => column.primary_key_position !== index + 1)) return null;
  return sorted.map((column) => column.name);
}

export function isCurrentSnapshot(expected: ServiceResultReference, actual: ServiceResultReference | undefined, connectionId: string): boolean {
  return !!actual && connectionId === expected.connectionId && actual.connectionId === expected.connectionId && actual.resultId === expected.resultId && actual.resultSetIndex === expected.resultSetIndex && actual.resultGeneration === expected.resultGeneration && actual.resultRevision === expected.resultRevision;
}

export function queryUsesSnapshot(intent: "run" | "page", result: QueryResult | null): boolean { return intent === "page" && !!result?.service; }

export function captureQueryCapabilities(drivers: readonly Pick<PluginManifest, "id" | "capabilities">[], driverId: string, connectionId: string, activeConnectionId: string | null, activeCapabilities: DriverCapabilities | null): DriverCapabilities {
  const driver = drivers.find((driver) => driver.id === driverId);
  if (!driver) throw new Error("The saved connection driver is unavailable.");
  return connectionId === activeConnectionId && activeCapabilities ? activeCapabilities : driver.capabilities;
}
