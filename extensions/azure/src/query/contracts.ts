import type { DocumentIdentity, JsonObject, JsonValue, Limits, Metrics, PartitionKeyComponent, QueryParameter } from "@tabularis/service-contracts/types";
import type { RpcHandler, RpcRequestContext } from "../runtime/contracts.js";

export const COSMOS_SDK_VERSION = "4.10.1";
export const MAX_PAGE_BYTES = 8 * 1024 * 1024;
export const PAGE_ENVELOPE_MARGIN = 64 * 1024;
export const MAX_SPOOL_BYTES = 32 * 1024 * 1024;
export const MAX_SPOOL_ROWS = 10_000;
export const CURSOR_TTL_MS = 15 * 60 * 1000;
export interface QueryInput {
  language?: "cosmos_sql";
  database: string;
  container: string;
  text: string;
  parameters: QueryParameter[];
  page_size?: number;
  mode?: "exact";
  partition_key?: PartitionKeyComponent[] | null;
  ru_budget?: number;
  result_mode?: "json_values" | "documents";
}
export interface NormalizedQuery extends QueryInput { language: "cosmos_sql"; page_size: number }
export interface DocumentEntry { document: JsonObject; identity: DocumentIdentity; etag: string }
export type QueryData = { kind: "json_values"; values: JsonValue[] } | { kind: "documents"; documents: DocumentEntry[] };
export interface ResultSnapshot {
  sets: { kind: "json_values" | "documents"; columns: JsonValue[]; rows: JsonValue[] }[];
  revision: string;
  limits: Limits;
  allow_truncation: false;
  warnings: string[];
}
export interface QueryCursorBinding { sdk_version: string; container_rid: string; endpoint_identity: string; partition_hash: string }
export interface QueryNativeState {
  version: 1;
  sdk_version: string;
  connection_id: string;
  auth_identity: string;
  auth_binding: string;
  endpoint_identity: string;
  database_rid: string;
  container_rid: string;
  partition_hash: string;
  query_hash: string;
  query: NormalizedQuery;
  original_options: JsonObject;
  continuation_token: string;
  revision: string;
}
export interface QueryCursor {
  binding: QueryCursorBinding;
  native_state: QueryNativeState;
  tail: JsonValue[];
  columns: JsonValue[];
  page_size: number;
  expires_at_ms: number;
}
export interface QueryDriverOutput {
  data: QueryData;
  snapshot: ResultSnapshot;
  cursor: QueryCursor | null;
  metrics: Metrics;
  limits: Limits;
  warnings: string[];
  confirmed_cancelled: false;
  session_closed: false;
}
export interface QueryHandlers {
  query_page: RpcHandler;
  execute_query: RpcHandler;
  execute(query: QueryInput, context: RpcRequestContext): Promise<QueryDriverOutput>;
  continue(query: QueryInput, state: QueryCursor, context: RpcRequestContext): Promise<QueryDriverOutput>;
}
