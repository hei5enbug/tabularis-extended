import type { JsonValue } from "@tabularis/service-contracts/types";
import type { RpcRequestContext } from "../runtime/contracts.js";
import { DriverError, serviceResponse } from "../runtime/errors.js";
import { MAX_FRAME_BYTES } from "../runtime/framing.js";
import { MAX_PAGE_BYTES, PAGE_ENVELOPE_MARGIN, MAX_SPOOL_BYTES, MAX_SPOOL_ROWS, type DocumentEntry, type QueryData, type QueryDriverOutput, type ResultSnapshot } from "./contracts.js";

export function queryData(rows: JsonValue[], mode: "json_values" | "documents"): QueryData {
  return mode === "documents" ? { kind: mode, documents: rows as unknown as DocumentEntry[] } : { kind: mode, values: rows };
}
export function publicPage(rows: JsonValue[], mode: "json_values" | "documents", pageSize: number, context: RpcRequestContext): { rows: JsonValue[]; tail: JsonValue[] } {
  const empty = serviceResponse(context, queryData([], mode) as unknown as JsonValue);
  const overhead = Buffer.byteLength(JSON.stringify(empty), "utf8") + PAGE_ENVELOPE_MARGIN;
  let bytes = overhead;
  let end = 0;
  for (const row of rows) {
    const size = Buffer.byteLength(JSON.stringify(row), "utf8");
    if (overhead + size > MAX_PAGE_BYTES) throw new DriverError("DOCUMENT_TOO_LARGE", "One query value exceeds the public response size limit.", "not_applied");
    if (end >= pageSize || bytes + size + (end ? 1 : 0) > MAX_PAGE_BYTES) break;
    bytes += size + (end ? 1 : 0);
    end++;
  }
  return { rows: rows.slice(0, end), tail: rows.slice(end) };
}
export function boundRows(rows: JsonValue[]): void {
  if (rows.length > MAX_SPOOL_ROWS || Buffer.byteLength(JSON.stringify(rows), "utf8") > MAX_SPOOL_BYTES - PAGE_ENVELOPE_MARGIN) throw new DriverError("RESOURCE_LIMIT", "The completed query spool exceeds its row or byte limit.", "not_applied");
}
export function snapshot(rows: JsonValue[], mode: "json_values" | "documents", revision: string): ResultSnapshot {
  boundRows(rows);
  return { sets: [{ kind: mode, columns: [], rows }], revision, limits: { truncated: false, reasons: [] }, allow_truncation: false, warnings: ["cosmos_snapshot_consistency_not_guaranteed"] };
}
export function output(rows: JsonValue[], full: JsonValue[], mode: "json_values" | "documents", revision: string, context: RpcRequestContext, cursor: QueryDriverOutput["cursor"]): QueryDriverOutput {
  return { data: queryData(rows, mode), snapshot: snapshot(full, mode, revision), cursor, metrics: context.metrics!.snapshot(), limits: { truncated: false, reasons: [] }, warnings: ["cosmos_snapshot_consistency_not_guaranteed"], confirmed_cancelled: false, session_closed: false };
}
export function boundResumeInput(query: unknown, state: unknown, context: RpcRequestContext): void {
  const settings = context.connection;
  const frame = { jsonrpc: "2.0", id: context.request_id, method: "query_page", params: { params: { driver: "cosmos-nosql", database: settings?.database, extra: settings }, driver_context: { protocol_version: 1, connection_id: context.connection_id, request_id: context.request_id, deadline_ms: context.deadline_ms, read_only: context.read_only ?? true, auth: context.auth, ...(context.session_handle === undefined ? {} : { session_handle: context.session_handle }) }, input: { query, cursor_state: state } } };
  if (Buffer.byteLength(JSON.stringify(frame), "utf8") > MAX_FRAME_BYTES - PAGE_ENVELOPE_MARGIN) throw new DriverError("RESOURCE_LIMIT", "The native cursor cannot fit in a bounded driver request frame.", "not_applied");
}
export function boundRpcOutput(value: QueryDriverOutput, context: RpcRequestContext): void {
  const frame = { jsonrpc: "2.0", id: context.request_id, result: serviceResponse(context, value as unknown as JsonValue) };
  if (Buffer.byteLength(JSON.stringify(frame), "utf8") > MAX_FRAME_BYTES - PAGE_ENVELOPE_MARGIN) throw new DriverError("RESOURCE_LIMIT", "The completed query output exceeds the driver response frame limit.", "not_applied");
}
