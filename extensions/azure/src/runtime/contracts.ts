import type { JsonObject, JsonValue, ServiceError, ServiceResponse } from "@tabularis/service-contracts/types";
import type { ConnectionSettings, TransientAuthContext } from "../connection/index.js";
import type { MetricsAccumulator } from "./metrics.js";

export const rpcMethods = ["initialize", "ping", "shutdown", "test_connection", "get_databases", "get_tables", "get_columns", "get_foreign_keys", "get_indexes", "get_schemas", "get_connection_metadata", "execute_query", "query_page", "read_document", "create_document", "replace_document", "delete_document", "cancel", "cancel_request", "service_invalidate_auth"] as const;
export type RpcMethod = (typeof rpcMethods)[number];
export interface RpcRequest { jsonrpc: "2.0"; id?: string | number | null; method: string; params?: JsonObject }
export interface RpcRequestContext {
  connection_id: string;
  request_id: string;
  deadline_ms: number;
  signal: AbortSignal;
  auth?: TransientAuthContext;
  connection?: ConnectionSettings;
  read_only?: boolean;
  deadline_at_ms?: number;
  now?: () => number;
  metrics?: MetricsAccumulator;
  markWriteDispatched?: () => void;
  write_dispatched?: () => boolean;
  session_handle?: string;
  native_v1?: boolean;
  native_generation?: number;
}
export type RpcResult = JsonValue | ServiceResponse;
export type RpcHandler = (params: JsonObject, context: RpcRequestContext) => Promise<RpcResult>;
export interface RpcFailure { jsonrpc: "2.0"; id: string | number | null; error: { code: number; message: string; data?: ServiceError } }
export interface RpcSuccess { jsonrpc: "2.0"; id: string | number | null; result: RpcResult }
export type RpcResponse = RpcFailure | RpcSuccess;
export type ResolvedContext = Omit<RpcRequestContext, "signal" | "markWriteDispatched" | "write_dispatched" | "metrics" | "deadline_at_ms">;
