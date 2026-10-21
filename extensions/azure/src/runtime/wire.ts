import type { JsonObject } from "@tabularis/service-contracts/types";
import type { TransientAuthContext } from "../connection/index.js";
import { normalizeConnectionSettings } from "../connection/settings.js";
import type { ResolvedContext, RpcRequest } from "./contracts.js";
import { DriverError } from "./errors.js";
import { jsonObject, onlyKeys, validateOperationInput } from "./validation.js";
import { resolveLegacyContext } from "./compat.js";

const operationByMethod = { test_connection: "connection.test", get_databases: "catalog.databases", get_tables: "catalog.objects", get_columns: "catalog.describe", read_document: "document.read", create_document: "document.create", replace_document: "document.replace", delete_document: "document.delete" } as const;
const controlMethods = new Set(["initialize", "ping", "shutdown", "cancel_request"]);
export function identifier(value: unknown): value is string { return typeof value === "string" && value.length >= 1 && value.length <= 128; }
function requiredString(value: unknown): string {
  if (!identifier(value)) throw new DriverError("INVALID_ARGUMENT", "A valid internal connection and request identity is required.");
  return value;
}
export function isV1Request(request: RpcRequest): boolean { return jsonObject(request.params) && Object.hasOwn(request.params, "input"); }
function transientAuth(value: unknown): TransientAuthContext | undefined {
  if (value === undefined) return undefined;
  if (!jsonObject(value)) throw new DriverError("INVALID_ARGUMENT", "Invalid transient authentication context.");
  if (value.kind === "account_key") {
    onlyKeys(value, ["kind", "account_key", "identity"]);
    if (typeof value.account_key !== "string" || !value.account_key || !identifier(value.identity)) throw new DriverError("AUTH_REQUIRED", "A transient account key and identity are required.");
    return { kind: "account_key", account_key: value.account_key, identity: value.identity };
  }
  onlyKeys(value, ["kind", "access_token", "expires_at_ms", "tenant_id", "client_id", "principal_id", "scope", "identity"]);
  if (value.kind !== "entra_token" || typeof value.access_token !== "string" || !value.access_token || typeof value.expires_at_ms !== "number" || !Number.isSafeInteger(value.expires_at_ms) || !identifier(value.tenant_id) || !identifier(value.client_id) || !identifier(value.principal_id) || typeof value.scope !== "string" || !identifier(value.identity)) throw new DriverError("AUTH_REQUIRED", "A valid transient Entra context is required.");
  return { kind: "entra_token", access_token: value.access_token, expires_at_ms: value.expires_at_ms, tenant_id: value.tenant_id, client_id: value.client_id, principal_id: value.principal_id, scope: value.scope, identity: value.identity };
}
export function resolveWireContext(request: RpcRequest): ResolvedContext {
  const params = request.params ?? {};
  if (controlMethods.has(request.method) && request.method !== "ping") return { connection_id: "control", request_id: String(request.id ?? "notification"), deadline_ms: 30_000, read_only: true };
  const wire = params.driver_context;
  if (wire === undefined) return resolveLegacyContext(request);
  if (!jsonObject(wire)) throw new DriverError("AUTH_REQUIRED", "A host-provided driver context is required.");
  const invalidate = request.method === "service_invalidate_auth";
  onlyKeys(wire, invalidate ? ["protocol_version", "connection_id", "request_id", "deadline_ms", "read_only"] : ["protocol_version", "connection_id", "request_id", "deadline_ms", "read_only", "auth", "session_handle"]);
  if (wire.protocol_version !== 1) throw new DriverError("PROTOCOL_MISMATCH", "The driver context protocol version is unsupported.");
  const deadline = wire.deadline_ms ?? 30_000;
  if (typeof deadline !== "number" || !Number.isSafeInteger(deadline) || deadline < 1 || deadline > 120_000) throw new DriverError("INVALID_ARGUMENT", "The request deadline is invalid.");
  if (wire.read_only !== undefined && typeof wire.read_only !== "boolean") throw new DriverError("INVALID_ARGUMENT", "The internal read-only flag is invalid.");
  if (!jsonObject(params.params)) throw new DriverError("INVALID_ARGUMENT", "Host ConnectionParams are required.");
  if (isV1Request(request)) {
    onlyKeys(params, ["params", "driver_context", "input"]);
    if (!jsonObject(params.input)) throw new DriverError("INVALID_ARGUMENT", "A public operation input object is required.");
  }
  const context: ResolvedContext = { connection_id: requiredString(wire.connection_id), request_id: requiredString(wire.request_id), deadline_ms: deadline, read_only: wire.read_only ?? true, native_v1: isV1Request(request) };
  if (invalidate) {
    onlyKeys(params, ["params", "driver_context", "input"]);
    if (!jsonObject(params.input)) throw new DriverError("INVALID_ARGUMENT", "An empty invalidate input object is required.");
    onlyKeys(params.input, []);
    if (params.params.connection_id !== undefined && params.params.connection_id !== null && params.params.connection_id !== context.connection_id) throw new DriverError("INVALID_ARGUMENT", "The connection identity does not match this request.");
    return context;
  }
  context.connection = normalizeConnectionSettings(params.params);
  const auth = transientAuth(wire.auth);
  if (auth) context.auth = auth;
  if (wire.session_handle !== undefined) context.session_handle = requiredString(wire.session_handle);
  return context;
}
export function handlerInput(request: RpcRequest): JsonObject {
  const params = request.params ?? {};
  if (!isV1Request(request)) {
    if (["read_document", "create_document", "replace_document", "delete_document"].includes(request.method)) throw new DriverError("INVALID_ARGUMENT", "Document RPC requires the v1 internal envelope.");
    return params;
  }
  const operation = operationByMethod[request.method as keyof typeof operationByMethod];
  if (operation) validateOperationInput(operation, params.input);
  return params.input as JsonObject;
}
