import type { Readable, Writable } from "node:stream";
import type { JsonValue, ServiceResponse } from "@tabularis/service-contracts/types";
import { createClientProvider, type ClientProvider } from "../connection/index.js";
import { createConnectionHandlers } from "../connection/handlers.js";
import { createDocumentHandlers } from "../documents/index.js";
import { rpcMethods, type ResolvedContext, type RpcFailure, type RpcHandler, type RpcRequest, type RpcRequestContext, type RpcResponse, type RpcResult } from "./contracts.js";
import { assertRequestActive, DriverError, mapSdkError, safeError, serviceResponse } from "./errors.js";
import { JsonLineWriter, LineFramer, MAX_FRAME_BYTES } from "./framing.js";
import { MetricsAccumulator, withRequestMetrics } from "./metrics.js";
import { jsonObject, onlyKeys } from "./validation.js";
import { handlerInput, identifier, isV1Request, resolveWireContext } from "./wire.js";
import { resolveCliAuth } from "./compat.js";

export interface RuntimeOptions {
  handlers?: Partial<Record<(typeof rpcMethods)[number], RpcHandler>>;
  resolveContext?: (request: RpcRequest) => ResolvedContext;
  onShutdown?: () => Promise<void>;
  onInvalidate?: (connection_id: string) => Promise<void>;
  now?: () => number;
  globalConcurrency?: number;
  connectionConcurrency?: number;
  queueLimit?: number;
}
export interface RpcRuntime { dispatch(request: RpcRequest): Promise<RpcResponse | undefined>; idle(): Promise<void>; close(): Promise<void> }
interface ConnectionEpoch { generation: number; last_used: number; pending: number }
type InvalidationResult = { ok: true } | { ok: false; error: unknown };
interface Invalidation { promise: Promise<void>; waiters: Set<(result: InvalidationResult) => void> }
interface Pending { request: RpcRequest; context: RpcRequestContext; controller: AbortController; resolve: (response: RpcResponse | undefined) => void; timer: NodeJS.Timeout; started: boolean; key: string; bytes: number; epoch?: ConnectionEpoch }
export const MAX_NATIVE_CONNECTIONS = 4096;
export const NATIVE_CONNECTION_RETENTION_MS = 20 * 60_000;
export const MAX_INVALIDATION_CONTROLS = 8;
const writes = new Set(["create_document", "replace_document", "delete_document"]);
function knownMethod(value: string): boolean { return (rpcMethods as readonly string[]).includes(value); }
function rpcError(request: RpcRequest, error: DriverError, code = -32000): RpcFailure {
  return { jsonrpc: "2.0", id: request.id ?? null, error: { code, message: error.message, data: safeError(error) } };
}
function isServiceResponse(value: RpcResult): value is ServiceResponse { return jsonObject(value) && value.protocol_version === 1 && typeof value.status === "string" && Object.hasOwn(value, "metrics") && Object.hasOwn(value, "error"); }
export function dispatchRpc(request: RpcRequest): RpcFailure {
  const known = knownMethod(request.method);
  return rpcError(request, new DriverError(known ? "CAPABILITY_UNAVAILABLE" : "UNSUPPORTED_OPERATION", known ? "The requested driver handler is unavailable." : "The requested RPC method is unsupported."), known ? -32000 : -32601);
}
export function createRpcRuntime(options: RuntimeOptions = {}): RpcRuntime {
  const handlers = options.handlers ?? {};
  for (const method of Object.keys(handlers)) if (!knownMethod(method)) throw new DriverError("UNSUPPORTED_OPERATION", "Unsupported RPC handler registration.");
  const now = options.now ?? Date.now;
  const globalLimit = options.globalConcurrency ?? 8;
  const connectionLimit = options.connectionConcurrency ?? 4;
  const queueLimit = options.queueLimit ?? 64;
  if (![globalLimit, connectionLimit].every((limit) => Number.isSafeInteger(limit) && limit > 0) || !Number.isSafeInteger(queueLimit) || queueLimit < 0) throw new DriverError("INVALID_ARGUMENT", "Invalid runtime concurrency limits.");
  const registry = new Map<string, Pending>();
  const queue: Pending[] = [];
  const perConnection = new Map<string, number>();
  const epochs = new Map<string, ConnectionEpoch>();
  const invalidations = new Map<string, Invalidation>();
  const idleWaiters: (() => void)[] = [];
  let active = 0;
  let activeControls = 0;
  let pendingBytes = 0;
  let closing = false;
  let shutdown: Promise<void> | undefined;
  const contextResolver = options.resolveContext ?? resolveWireContext;
  function notifyIdle(): void {
    if (!registry.size && !invalidations.size) for (const resolve of idleWaiters.splice(0)) resolve();
  }
  function prepareEpoch(context: RpcRequestContext): ConnectionEpoch | undefined {
    if (!context.connection) return undefined;
    let epoch = epochs.get(context.connection_id);
    if (!epoch) {
      for (const [connection_id, candidate] of epochs) if (!candidate.pending && !invalidations.has(connection_id) && now() - candidate.last_used >= NATIVE_CONNECTION_RETENTION_MS) epochs.delete(connection_id);
      if (epochs.size >= MAX_NATIVE_CONNECTIONS) throw new DriverError("RESOURCE_LIMIT", "The driver connection generation registry is full.");
      epoch = { generation: 0, last_used: now(), pending: 0 };
      epochs.set(context.connection_id, epoch);
    }
    epoch.last_used = now();
    if (context.native_v1) context.native_generation = epoch.generation;
    return epoch;
  }
  function response(request: RpcRequest, context: RpcRequestContext, result: RpcResult): RpcResponse | undefined {
    if (!Object.hasOwn(request, "id")) return undefined;
    return { jsonrpc: "2.0", id: request.id ?? null, result: isV1Request(request) && !isServiceResponse(result) ? serviceResponse(context, result as JsonValue) : result };
  }
  function failure(request: RpcRequest, context: RpcRequestContext, error: unknown): RpcResponse | undefined {
    if (!Object.hasOwn(request, "id")) return undefined;
    const normalized = mapSdkError(error, context, { write: writes.has(request.method), document: writes.has(request.method) || request.method === "read_document" });
    return isV1Request(request) ? response(request, context, serviceResponse(context, null, safeError(normalized))) : rpcError(request, normalized);
  }
  function finish(pending: Pending, result: RpcResponse | undefined): void {
    clearTimeout(pending.timer);
    registry.delete(pending.key);
    pendingBytes -= pending.bytes;
    if (pending.epoch) { pending.epoch.pending--; pending.epoch.last_used = now(); }
    pending.resolve(result);
    notifyIdle();
  }
  function abort(pending: Pending, reason: DriverError): void {
    pending.controller.abort(reason);
    if (!pending.started) {
      const index = queue.indexOf(pending);
      if (index >= 0) queue.splice(index, 1);
      finish(pending, failure(pending.request, pending.context, new DriverError(reason.code, reason.message, "not_started")));
    }
  }
  async function execute(pending: Pending): Promise<void> {
    let result: RpcResponse | undefined;
    try {
      if (pending.context.session_handle !== undefined) throw new DriverError("UNSUPPORTED_OPERATION", "Cosmos SQL sessions are unsupported.");
      const handler = handlers[pending.request.method as keyof typeof handlers];
      if (!handler) throw new DriverError("CAPABILITY_UNAVAILABLE", "The requested driver handler is unavailable.");
      if (pending.request.params?.driver_context === undefined && pending.context.connection?.auth_source === "azure_cli" && !pending.context.auth && !["get_connection_metadata", "get_foreign_keys", "get_indexes", "get_schemas"].includes(pending.request.method)) {
        pending.context.auth = await resolveCliAuth(pending.context);
        assertRequestActive(pending.context);
      }
      result = response(pending.request, pending.context, await withRequestMetrics(pending.context, () => handler(handlerInput(pending.request), pending.context)));
    } catch (error) { result = failure(pending.request, pending.context, error); }
    finally {
      active--;
      const count = (perConnection.get(pending.context.connection_id) ?? 1) - 1;
      if (count) perConnection.set(pending.context.connection_id, count); else perConnection.delete(pending.context.connection_id);
      finish(pending, result);
      pump();
    }
  }
  function pump(): void {
    while (active < globalLimit) {
      const index = queue.findIndex((pending) => !invalidations.has(pending.context.connection_id) && (perConnection.get(pending.context.connection_id) ?? 0) < connectionLimit);
      if (index < 0) break;
      const pending = queue.splice(index, 1)[0]!;
      if (pending.context.native_v1 && pending.epoch) pending.context.native_generation = pending.epoch.generation;
      pending.started = true;
      active++;
      perConnection.set(pending.context.connection_id, (perConnection.get(pending.context.connection_id) ?? 0) + 1);
      void execute(pending);
    }
  }
  const runtime: RpcRuntime = {
    async idle() { if (registry.size || invalidations.size) await new Promise<void>((resolve) => idleWaiters.push(resolve)); },
    async close() {
      if (!shutdown) {
        closing = true;
        for (const pending of [...registry.values()]) abort(pending, new DriverError("CANCELLED", "The driver connection has closed.", "not_applied"));
        shutdown = runtime.idle().then(() => options.onShutdown?.());
      }
      await shutdown;
    },
    async dispatch(request) {
      if (request.method === "cancel" && !Object.hasOwn(request, "id")) {
        const value = request.params;
        if (jsonObject(value) && Object.keys(value).length === 1 && (typeof value.id === "string" || typeof value.id === "number")) {
          for (const pending of registry.values()) if (pending.request.id === value.id) abort(pending, new DriverError("CANCELLED", "The host abandoned this request.", "not_applied"));
        }
        return undefined;
      }
      if (!knownMethod(request.method)) return Object.hasOwn(request, "id") ? dispatchRpc(request) : undefined;
      if (request.method === "service_invalidate_auth" && !options.onInvalidate) return Object.hasOwn(request, "id") ? dispatchRpc(request) : undefined;
      if (!handlers[request.method as keyof typeof handlers] && request.method !== "cancel_request" && !(request.method === "service_invalidate_auth" && options.onInvalidate)) return Object.hasOwn(request, "id") ? dispatchRpc(request) : undefined;
      try {
        if (request.method === "cancel_request") {
          const value = request.params ?? {};
          onlyKeys(value, ["connection_id", "request_id"]);
          if (!identifier(value.connection_id) || !identifier(value.request_id)) throw new DriverError("INVALID_ARGUMENT", "Cancellation requires connection_id and request_id.");
          const pending = registry.get(JSON.stringify([value.connection_id, value.request_id]));
          if (pending) abort(pending, new DriverError("CANCELLED", "The request was cancelled.", "not_applied"));
          return Object.hasOwn(request, "id") ? { jsonrpc: "2.0", id: request.id ?? null, result: { cancellation_requested: pending !== undefined } } : undefined;
        }
        const resolved = request.method === "service_invalidate_auth" ? resolveWireContext(request) : contextResolver(request);
        const controller = new AbortController();
        const startedAt = now();
        let writeDispatched = false;
        const context: RpcRequestContext = { ...resolved, native_v1: isV1Request(request), signal: controller.signal, now, deadline_at_ms: startedAt + resolved.deadline_ms, metrics: new MetricsAccumulator(startedAt, now), markWriteDispatched: () => { writeDispatched = true; }, write_dispatched: () => writeDispatched };
        delete context.native_generation;
        if (request.method === "shutdown") {
          const result = await handlers.shutdown!(handlerInput(request), context);
          await runtime.close();
          return response(request, context, result);
        }
        if (closing) return failure(request, context, new DriverError("DRIVER_EXITED", "The driver is shutting down."));
        if (request.method === "initialize") {
          try { return response(request, context, await handlers.initialize!(handlerInput(request), context)); }
          catch (error) { return failure(request, context, error); }
        }
        if (request.method === "service_invalidate_auth") {
          if (activeControls >= MAX_INVALIDATION_CONTROLS) return failure(request, context, new DriverError("RESOURCE_LIMIT", "The driver invalidation control limit was exceeded."));
          let invalidation = invalidations.get(context.connection_id);
          if (!invalidation) {
            if (invalidations.size >= MAX_INVALIDATION_CONTROLS) return failure(request, context, new DriverError("RESOURCE_LIMIT", "The driver invalidation control limit was exceeded."));
            const epoch = epochs.get(context.connection_id);
            if (epoch && !Number.isSafeInteger(epoch.generation + 1)) return failure(request, context, new DriverError("RESOURCE_LIMIT", "The connection generation limit was exceeded."));
            if (epoch) { epoch.generation++; epoch.last_used = now(); }
            for (const pending of [...registry.values()]) if (pending.context.connection_id === context.connection_id) abort(pending, new DriverError("CANCELLED", "The connection authentication was invalidated.", "not_applied"));
            invalidation = { promise: Promise.resolve().then(() => options.onInvalidate!(context.connection_id)), waiters: new Set() };
            invalidations.set(context.connection_id, invalidation);
            const current = invalidation;
            const finished = (result: InvalidationResult): void => {
              invalidations.delete(context.connection_id);
              if (epoch) epoch.last_used = now();
              for (const complete of current.waiters) complete(result);
              current.waiters.clear();
              pump(); notifyIdle();
            };
            void current.promise.then(() => finished({ ok: true }), (error: unknown) => finished({ ok: false, error }));
          }
          activeControls++;
          let timer: NodeJS.Timeout | undefined;
          try {
            const current = invalidation;
            await new Promise<void>((resolve, reject) => {
              const complete = (result: InvalidationResult): void => { current.waiters.delete(complete); if (result.ok) resolve(); else reject(result.error); };
              current.waiters.add(complete);
              timer = setTimeout(() => complete({ ok: false, error: new DriverError("DEADLINE_EXCEEDED", "The request deadline was exceeded.", "not_applied") }), context.deadline_ms);
            });
            assertRequestActive(context);
            return response(request, context, { invalidated: true });
          } catch (error) { return failure(request, context, error); }
          finally { activeControls--; if (timer) clearTimeout(timer); }
        }
        const key = JSON.stringify([context.connection_id, context.request_id]);
        if (registry.has(key)) return failure(request, context, new DriverError("INVALID_ARGUMENT", "This connection and request identity is already in progress."));
        const bytes = Buffer.byteLength(JSON.stringify(request), "utf8");
        if (pendingBytes + bytes > 64 * 1024 * 1024) return failure(request, context, new DriverError("RESOURCE_LIMIT", "The driver pending request byte limit was exceeded."));
        if ((invalidations.has(context.connection_id) || active >= globalLimit || (perConnection.get(context.connection_id) ?? 0) >= connectionLimit) && queue.length >= queueLimit) return failure(request, context, new DriverError("RESOURCE_LIMIT", "The driver request queue is full."));
        let epoch: ConnectionEpoch | undefined;
        try { epoch = prepareEpoch(context); } catch (error) { return failure(request, context, error); }
        return await new Promise<RpcResponse | undefined>((resolve) => {
          const timer = setTimeout(() => abort(pending, new DriverError("DEADLINE_EXCEEDED", "The request deadline was exceeded.", "not_applied")), context.deadline_ms);
          const pending: Pending = { request, context, controller, resolve, timer, started: false, key, bytes, ...(epoch ? { epoch } : {}) };
          if (epoch) epoch.pending++;
          pendingBytes += bytes; registry.set(key, pending); queue.push(pending); pump();
        });
      } catch (error) {
        const normalized = error instanceof DriverError ? error : new DriverError("DRIVER_EXITED", "The driver operation failed.");
        return Object.hasOwn(request, "id") ? rpcError(request, normalized) : undefined;
      }
    },
  };
  return runtime;
}
export function createCosmosRuntime(options: Omit<RuntimeOptions, "handlers" | "onShutdown" | "onInvalidate"> & { clients?: ClientProvider; handlers?: RuntimeOptions["handlers"] } = {}): RpcRuntime {
  const clients = options.clients ?? createClientProvider();
  const queryReady = options.handlers?.query_page !== undefined && options.handlers.execute_query !== undefined;
  const handlers = { ...createConnectionHandlers(clients, { query_page_v1: queryReady }), ...createDocumentHandlers(clients), ...options.handlers };
  return createRpcRuntime({ ...options, handlers, onInvalidate: (connection_id) => clients.invalidate(connection_id), onShutdown: async () => { await clients.dispose?.(); } });
}
function parseRequest(value: unknown): RpcRequest | undefined {
  if (!jsonObject(value) || value.jsonrpc !== "2.0" || typeof value.method !== "string" || Object.keys(value).some((key) => !["jsonrpc", "id", "method", "params"].includes(key)) || (value.params !== undefined && !jsonObject(value.params))) return undefined;
  if (Object.hasOwn(value, "id") && value.id !== null && typeof value.id !== "string" && (typeof value.id !== "number" || !Number.isSafeInteger(value.id))) return undefined;
  if (typeof value.id === "string" && value.id.length > 128) return undefined;
  return value as unknown as RpcRequest;
}
export async function runStdio(input: Readable, output: Writable, runtime: RpcRuntime = createRpcRuntime()): Promise<void> {
  const framer = new LineFramer();
  const writer = new JsonLineWriter(output);
  const pending = new Set<Promise<void>>();
  let outputFailed = false;
  const outputClosed = (): void => { outputFailed = true; input.destroy(); void runtime.close().catch(() => {}); };
  output.on("error", outputClosed);
  output.on("close", outputClosed);
  const emit = (value: unknown): void => {
    const action = writer.write(value).catch(outputClosed);
    pending.add(action); void action.finally(() => pending.delete(action));
  };
  const handle = (frame: ReturnType<LineFramer["push"]>[number]): void => {
    if (frame.kind === "error") { emit({ jsonrpc: "2.0", id: null, error: { code: frame.code === "FRAME_TOO_LARGE" ? -32600 : -32700, message: frame.code === "FRAME_TOO_LARGE" ? "The request exceeds the frame limit." : "Invalid UTF-8 newline JSON frame." } }); return; }
    let value: unknown;
    try { value = JSON.parse(frame.text); }
    catch { emit({ jsonrpc: "2.0", id: null, error: { code: -32700, message: "Invalid JSON." } }); return; }
    const request = parseRequest(value);
    if (!request) { emit({ jsonrpc: "2.0", id: null, error: { code: -32600, message: "Invalid JSON-RPC request." } }); return; }
    const action = runtime.dispatch(request).then((response) => { if (response) emit(response); }).catch(() => { emit({ jsonrpc: "2.0", id: request.id ?? null, error: { code: -32603, message: "The driver operation failed." } }); });
    pending.add(action); void action.finally(() => pending.delete(action));
  };
  try {
    for await (const chunk of input) {
      if (outputFailed) break;
      for (const frame of framer.frames(chunk as Uint8Array)) {
        handle(frame);
        if (writer.queueLength >= 32 || writer.queuedBytes >= MAX_FRAME_BYTES) await writer.flush();
      }
    }
  }
  catch { throw new DriverError("DRIVER_EXITED", "The RPC input stream has closed."); }
  finally {
    try { for (const frame of framer.finish()) handle(frame); await runtime.close(); while (pending.size) await Promise.allSettled([...pending]); await writer.flush(); }
    catch { throw new DriverError("DRIVER_EXITED", "The RPC transport has closed."); }
    finally { output.off("error", outputClosed); output.off("close", outputClosed); }
  }
}
