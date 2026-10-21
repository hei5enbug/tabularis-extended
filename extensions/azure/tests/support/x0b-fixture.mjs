import { createCosmosRuntime } from "../../dist/runtime/index.js";
import { DriverError } from "../../dist/runtime/errors.js";
import { fixtureConnectionParams, fixtureAuth, rpcRequest } from "./fixture.mjs";

export function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}
export function invalidateRequest(connection_id = "a", overrides = {}) {
  return { jsonrpc: "2.0", id: "invalidate", method: "service_invalidate_auth", params: {
    params: { driver: "cosmos-nosql", connection_id },
    driver_context: { protocol_version: 1, connection_id, request_id: "invalidate", deadline_ms: 1000, read_only: true },
    input: {}, ...overrides,
  } };
}
export function workRequest(connection_id = "a", id = "work", input = {}) {
  return rpcRequest("query_page", input, { params: fixtureConnectionParams, auth: fixtureAuth, connection_id, id });
}
export function runtimeFixture(test, options = {}) {
  const calls = [];
  const clients = { async get() { calls.push({ method: "get" }); throw new Error("SDK-get-secret-canary"); }, async invalidate(connection_id) { calls.push({ method: "invalidate", connection_id }); await options.invalidate?.(connection_id); }, async dispose() { calls.push({ method: "dispose" }); } };
  const handler = async (input, context) => {
    calls.push({ method: "handler", connection_id: context.connection_id, request_id: context.request_id, generation: context.native_generation });
    if (input.block) return await new Promise((_, reject) => {
      const aborted = () => reject(context.signal.reason ?? new DriverError("CANCELLED", "Cancelled."));
      if (context.signal.aborted) aborted(); else context.signal.addEventListener("abort", aborted, { once: true });
    });
    return { generation: context.native_generation ?? null };
  };
  const runtime = createCosmosRuntime({ clients, handlers: { query_page: handler, ...options.handlers }, ...options.runtime });
  test.after(() => runtime.close());
  return { calls, clients, runtime };
}
export async function fencedScenario(f) {
  const invalidation = f.runtime.dispatch(invalidateRequest());
  const target = f.runtime.dispatch(workRequest("a", "after"));
  const other = await f.runtime.dispatch(workRequest("b", "other"));
  const target_started_before_release = f.calls.some((call) => call.request_id === "after");
  f.release.resolve();
  return { other, target_started_before_release, invalidation: await invalidation, target: await target };
}
export async function capacityScenario(f, limit) {
  for (let index = 0; index < limit; index++) await f.runtime.dispatch(workRequest(`connection-${index}`));
  return await f.runtime.dispatch(workRequest("overflow"));
}
export async function controlCapacityScenario(f) {
  const pending = Array.from({ length: 7 }, (_, index) => f.runtime.dispatch(invalidateRequest(`control-${index}`)));
  const shared = f.runtime.dispatch(invalidateRequest("control-0"));
  const overflow = await f.runtime.dispatch(invalidateRequest("overflow"));
  f.release.resolve();
  return { overflow, completed: await Promise.all([...pending, shared]) };
}
