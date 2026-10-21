import test from "node:test";
import assert from "node:assert/strict";
import { createClientProvider, MAX_CACHED_CLIENTS } from "../../dist/connection/index.js";
import { createCosmosRuntime } from "../../dist/runtime/index.js";
import { fixtureAuth, fixtureSettings, rpcRequest } from "../support/fixture.mjs";
import { sdkTransportFixture } from "../support/sdk-transport.mjs";
import { invalidateRequest, deferred } from "../support/x0b-fixture.mjs";

function boundedProvider(t) {
  const created = [];
  const disposed = [];
  const clients = createClientProvider({ createClient: () => { const client = { dispose() { disposed.push(client); } }; created.push(client); return client; } });
  t.after(() => clients.dispose());
  const context = (connection_id) => ({ connection_id, auth: fixtureAuth, settings: fixtureSettings });
  return { clients, created, disposed, context };
}
async function fillClients(f) { for (let index = 0; index < MAX_CACHED_CLIENTS; index++) await f.clients.get(f.context(`client-${index}`)); }

test("4096개 client cache는 새 연결을 SDK 생성 전에 거부하고 기존 연결은 유지한다", async (t) => {
  // given
  const f = boundedProvider(t);
  await fillClients(f);
  async function scenario() {
    const overflow = await f.clients.get(f.context("overflow")).catch((error) => error);
    const existing = await f.clients.get(f.context("client-0"));
    return { overflow, existing };
  }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.overflow.code, "RESOURCE_LIMIT");
  assert.equal(actual.existing, f.created[0]);
  assert.equal(f.created.length, 4096);
  assert.deepEqual(f.disposed, []);
});

test("명시적으로 무효화한 client slot만 새 연결에 재사용한다", async (t) => {
  // given
  const f = boundedProvider(t);
  await fillClients(f);
  async function scenario() { await f.clients.invalidate("client-0"); return await f.clients.get(f.context("new")); }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual, f.created.at(-1));
  assert.equal(f.created.length, 4097);
  assert.deepEqual(f.disposed, [f.created[0]]);
});

test("실제 SDK의 계층형 PK metadata는 native에서만 구조를 확장한다", async (t) => {
  // given
  const f = sdkTransportFixture(t, { partitionPaths: ["/tenant", "/region"] });
  const runtime = createCosmosRuntime({ clients: f.clients });
  t.after(() => runtime.close());
  const native = rpcRequest("get_columns", { table: { database: "fixture", schema: null, table: "items" } }, { id: "native" });
  const legacy = rpcRequest("get_columns", {}, { id: "legacy" });
  delete legacy.params.input;
  legacy.params.table = "items";
  async function scenario() { return { legacy: await runtime.dispatch(legacy), native: await runtime.dispatch(native) }; }

  // when
  const actual = await scenario();

  // then
  assert.equal(Array.isArray(actual.legacy.result), true);
  assert.deepEqual(actual.native.result.data, { columns: actual.legacy.result, partition_key_paths: ["/tenant", "/region"], partition_key_kind: "MultiHash", partition_key_version: 2, system_key: false });
  assert.equal(JSON.stringify(actual).includes("container-rid"), false);
  assert.equal(JSON.stringify(actual).includes("db-rid"), false);
  assert.equal(f.calls.filter((call) => call.operation === "container").length, 2);
  assert.equal(f.clientOptions.length, 1);
});

test("실제 SDK의 전송된 쓰기는 invalidate 뒤 outcome unknown이며 재송신하지 않는다", { timeout: 5000 }, async (t) => {
  // given
  const ready = deferred();
  const f = sdkTransportFixture(t, { onRequest({ operation, request }) {
    if (operation !== "create") return undefined;
    ready.resolve();
    return new Promise((_, reject) => {
      const aborted = () => reject(Object.assign(new Error("x0b-write-secret-canary"), { name: "AbortError" }));
      if (request.abortSignal.aborted) aborted(); else request.abortSignal.addEventListener("abort", aborted, { once: true });
    });
  } });
  const runtime = createCosmosRuntime({ clients: f.clients });
  t.after(() => runtime.close());
  const request = rpcRequest("create_document", { database: "fixture", container: "items", document: { id: "new", tenant: "a" }, partition_key: [{ type: "string", value: "a" }] }, { id: "write", connection_id: "target" });
  async function scenario() {
    const pending = runtime.dispatch(request);
    await ready.promise;
    const invalidated = await runtime.dispatch(invalidateRequest("target"));
    return { invalidated, write: await pending };
  }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.invalidated.result.status, "succeeded");
  assert.equal(actual.write.result.status, "outcome_unknown");
  assert.equal(actual.write.result.error.code, "OUTCOME_UNKNOWN");
  assert.equal(actual.write.result.error.retryable, false);
  assert.equal(f.calls.filter((call) => call.operation === "create").length, 1);
  assert.equal(JSON.stringify(actual).includes("canary"), false);
});

for (const value of ["1", 0, 2, null, true]) {
  test(`초기화 service protocol ${JSON.stringify(value)} 값은 SDK 요청 없이 거부한다`, async (t) => {
    // given
    const f = sdkTransportFixture(t);
    const runtime = createCosmosRuntime({ clients: f.clients });
    t.after(() => runtime.close());

    // when
    const actual = await runtime.dispatch({ jsonrpc: "2.0", id: "initialize", method: "initialize", params: { service_protocol: value } });

    // then
    assert.equal(actual.error.data.code, "PROTOCOL_MISMATCH");
    assert.deepEqual(f.calls, []);
    assert.deepEqual(f.clientOptions, []);
  });
}
