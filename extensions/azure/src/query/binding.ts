import { createHash } from "node:crypto";
import { PartitionKeyBuilder, type FeedOptions } from "@azure/cosmos";
import type { JsonObject, JsonValue } from "@tabularis/service-contracts/types";
import type { ContainerMetadata } from "../connection/metadata.js";
import { connectionContext } from "../connection/index.js";
import type { RpcRequestContext } from "../runtime/contracts.js";
import { DriverError } from "../runtime/errors.js";
import { jsonObject, onlyKeys, validateOperationInput } from "../runtime/validation.js";
import { COSMOS_SDK_VERSION, MAX_SPOOL_BYTES, MAX_SPOOL_ROWS, type NormalizedQuery, type QueryCursor, type QueryInput } from "./contracts.js";

export function jsonHash(value: unknown): string {
  function canonical(item: unknown): unknown {
    if (Array.isArray(item)) return item.map(canonical);
    if (jsonObject(item)) return Object.fromEntries(Object.keys(item).sort().map((key) => [key, canonical(item[key])]));
    return item;
  }
  return createHash("sha256").update(JSON.stringify(canonical(value))).digest("hex");
}
export function normalizeQuery(value: QueryInput): NormalizedQuery {
  if (!jsonObject(value)) throw new DriverError("INVALID_ARGUMENT", "A Cosmos query input is required.");
  const candidate = { language: "cosmos_sql", page_size: 100, ...value };
  const input = validateOperationInput("query.execute", candidate);
  if (input.language !== "cosmos_sql" || !input.database || !input.container) throw new DriverError("UNSUPPORTED_OPERATION", "Only Cosmos NoSQL queries are supported.");
  return structuredClone(input) as NormalizedQuery;
}
export function endpointIdentity(context: RpcRequestContext): string { return jsonHash(connectionContext(context).settings.endpoint); }
export function authBinding(context: RpcRequestContext): string {
  const { auth } = connectionContext(context);
  const identity = auth.kind === "account_key" ? { kind: auth.kind, identity: auth.identity } : { kind: auth.kind, identity: auth.identity, tenant: auth.tenant_id, client: auth.client_id, principal: auth.principal_id, scope: auth.scope };
  return jsonHash(context.native_generation === undefined ? identity : { ...identity, native_generation: context.native_generation });
}
export function originalOptions(query: NormalizedQuery, metadata: ContainerMetadata): JsonObject {
  const key = query.partition_key;
  if (key !== undefined && key !== null) {
    if (key.length < 1 || key.length > metadata.partition_key_paths.length || (key.length < metadata.partition_key_paths.length && (metadata.partition_key_kind !== "MultiHash" || metadata.partition_key_version !== 2))) throw new DriverError("INVALID_ARGUMENT", "The query partition key must target valid complete or hierarchical prefix components.");
  }
  return plannedOptions(query);
}
function plannedOptions(query: NormalizedQuery): JsonObject {
  const options: JsonObject = { enableQueryControl: true, maxDegreeOfParallelism: 2, bufferItems: false, maxItemCount: query.page_size };
  const key = query.partition_key;
  if (key !== undefined && key !== null) {
    const builder = new PartitionKeyBuilder();
    for (const component of key) {
      if (component.type === "undefined") builder.addNoneValue();
      else if (component.type === "null") builder.addNullValue();
      else builder.addValue(component.value);
    }
    options.partitionKey = builder.build() as JsonValue;
  } else options.forceQueryPlan = true;
  return options;
}
export function feedOptions(options: JsonObject, context: RpcRequestContext, token?: string): FeedOptions {
  return { ...options, abortSignal: context.signal, ...(token === undefined ? {} : { continuationToken: token }) } as FeedOptions;
}
export function validateCursor(value: QueryCursor, query: NormalizedQuery, context: RpcRequestContext): QueryCursor {
  const invalid = (): never => { throw new DriverError("INVALID_PAGE_TOKEN", "The query cursor does not match this request.", "not_applied"); };
  if (!jsonObject(value) || !jsonObject(value.native_state) || !jsonObject(value.binding)) return invalid();
  try {
    onlyKeys(value as unknown as JsonObject, ["binding", "native_state", "tail", "columns", "page_size", "expires_at_ms"]);
    onlyKeys(value.binding as unknown as JsonObject, ["sdk_version", "container_rid", "endpoint_identity", "partition_hash"]);
    onlyKeys(value.native_state as unknown as JsonObject, ["version", "sdk_version", "connection_id", "auth_identity", "auth_binding", "endpoint_identity", "database_rid", "container_rid", "partition_hash", "query_hash", "query", "original_options", "continuation_token", "revision"]);
    const state = value.native_state;
    if (!Number.isSafeInteger(value.expires_at_ms) || !Array.isArray(value.tail) || value.tail.length > MAX_SPOOL_ROWS || !Array.isArray(value.columns) || value.columns.length !== 0 || value.page_size !== query.page_size || Buffer.byteLength(JSON.stringify(value), "utf8") > MAX_SPOOL_BYTES) return invalid();
    if (value.expires_at_ms <= (context.now ?? Date.now)()) throw new DriverError("CURSOR_EXPIRED", "The query cursor has expired.", "not_applied");
    if (state.version !== 1 || state.sdk_version !== COSMOS_SDK_VERSION || value.binding.sdk_version !== COSMOS_SDK_VERSION || state.connection_id !== context.connection_id || state.auth_identity !== context.auth?.identity || state.auth_binding !== authBinding(context) || state.endpoint_identity !== endpointIdentity(context) || state.query_hash !== jsonHash(query) || jsonHash(state.query) !== jsonHash(query) || state.partition_hash !== jsonHash(query.partition_key ?? null) || state.partition_hash !== value.binding.partition_hash || state.endpoint_identity !== value.binding.endpoint_identity || state.container_rid !== value.binding.container_rid || typeof state.database_rid !== "string" || !state.database_rid || typeof state.container_rid !== "string" || !state.container_rid || !jsonObject(state.original_options) || jsonHash(state.original_options) !== jsonHash(plannedOptions(query)) || typeof state.continuation_token !== "string" || !state.continuation_token || state.continuation_token.length > 1024 * 1024 || typeof state.revision !== "string" || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(state.revision)) return invalid();
    return structuredClone(value);
  } catch (error) { if (error instanceof DriverError && error.code === "CURSOR_EXPIRED") throw error; return invalid(); }
}
