import test from "node:test";
import assert from "node:assert/strict";
import { COSMOS_SCOPE } from "../../dist/connection/index.js";
import { fixtureSettings } from "../support/fixture.mjs";
import { sdkTransportFixture } from "../support/sdk-transport.mjs";
import { createCosmosRuntime } from "../../dist/runtime/index.js";
import { rpcRequest, fixtureIdentity } from "../support/fixture.mjs";

for (const operation of ["create", "replace", "delete"]) {
  test(`실제 SDK의 ${operation} 전송 오류는 쓰기를 재전송하지 않는다`, { timeout: 5000 }, async (t) => {
    // given
    const f = sdkTransportFixture(t, { onRequest({ operation: current }) { if (current === operation) throw Object.assign(new Error("sdk-network-secret-canary"), { code: "REQUEST_SEND_ERROR" }); } });

    // when
    const actual = await (operation === "delete" ? f.remove() : f[operation]());

    // then
    assert.equal(actual.status, "outcome_unknown");
    assert.equal(actual.error.code, "OUTCOME_UNKNOWN");
    assert.equal(actual.error.retryable, false);
    assert.equal(f.calls.filter((call) => call.operation === operation).length, 1);
    assert.equal(actual.metrics.request_charge, null);
    assert.equal(JSON.stringify(actual).includes("canary"), false);
  });
}

test("실제 SDK는 명시적 429만 제한된 정책으로 재시도한다", { timeout: 5000 }, async (t) => {
  // given
  const f = sdkTransportFixture(t, { onRequest({ operation, calls, request, respond }) {
    if (operation === "create" && calls.filter((call) => call.operation === "create").length === 1) return respond(request, 429, { code: "TooManyRequests", message: "sdk-error-secret-canary" }, { "x-ms-retry-after-ms": "1", "x-ms-request-charge": "1.25" });
  } });

  // when
  const actual = await f.create();

  // then
  assert.equal(actual.status, "succeeded");
  assert.equal(f.calls.filter((call) => call.operation === "create").length, 2);
  assert.equal(actual.metrics.retry_count, 1);
  assert.equal(actual.metrics.request_charge, 6.25);
  assert.equal(f.clientOptions[0].connectionPolicy.retryOptions.maxRetryAttemptCount, 3);
  assert.equal(f.clientOptions[0].connectionPolicy.retryOptions.maxWaitTimeInSeconds, 10);
});

test("실제 SDK의 IfMatch와 전체 partition key는 HTTP 요청에 보존된다", { timeout: 5000 }, async (t) => {
  // given
  const identity = { id: "1", partition_key: [{ type: "string", value: "a" }, { type: "number", value: 7 }, { type: "undefined" }] };
  const f = sdkTransportFixture(t, { partitionPaths: ["/tenant", "/region", "/optional"], document: { id: "1", tenant: "a", region: 7, _etag: "etag-1", nested: { value: null } } });

  // when
  const actual = await f.replace({ identity, if_match: "etag-1", document: f.document });

  // then
  assert.equal(actual.status, "succeeded");
  assert.equal(f.calls.find((call) => call.operation === "replace").request.headers.get("if-match"), "etag-1");
  assert.equal(f.calls.find((call) => call.operation === "replace").request.headers.get("x-ms-documentdb-partitionkey"), '["a",7,{}]');
  assert.deepEqual(JSON.parse(f.calls.find((call) => call.operation === "replace").request.body), { id: "1", tenant: "a", region: 7, nested: { value: null } });
});

test("실제 SDK의 Entra credential은 호스트가 제공한 Cosmos scope만 요청한다", { timeout: 5000 }, async (t) => {
  // given
  const auth = { kind: "entra_token", identity: "principal", tenant_id: "tenant", client_id: "client", principal_id: "oid", access_token: "synthetic-entra-token", expires_at_ms: Date.now() + 900000, scope: COSMOS_SCOPE };
  const f = sdkTransportFixture(t, { context: { auth, connection: { ...fixtureSettings, auth_mode: "entra_user", tenant_id: "tenant", client_id: "client" } } });

  // when
  const actual = await f.read();

  // then
  assert.equal(actual.status, "succeeded");
  assert.equal(f.clientOptions[0].aadScope, COSMOS_SCOPE);
  assert.equal(decodeURIComponent(f.calls.find((call) => call.operation === "read").request.headers.get("authorization")).includes("synthetic-entra-token"), true);
  assert.equal(f.calls.every((call) => call.request.agent.options.rejectUnauthorized !== false), true);
  assert.equal(f.calls.every((call) => call.request.allowInsecureConnection !== true), true);
});

test("실제 SDK가 보고한 TLS 인증 오류는 인증서 검증 실패로 반환한다", { timeout: 5000 }, async (t) => {
  // given
  const f = sdkTransportFixture(t, { onRequest() { throw Object.assign(new Error("sdk-certificate-secret-canary"), { code: "ERR_TLS_CERT_ALTNAME_INVALID" }); } });

  // when
  const actual = await f.read();

  // then
  assert.equal(actual.error.code, "TLS_VALIDATION_FAILED");
  assert.equal(actual.error.outcome, "not_applied");
  assert.equal(JSON.stringify(actual).includes("canary"), false);
});

test("실제 SDK의 제한을 소진한 429는 실패 응답 RU도 한 번씩 합산한다", { timeout: 5000 }, async (t) => {
  // given
  const f = sdkTransportFixture(t, { onRequest({ operation, request, respond }) {
    if (operation === "create") return respond(request, 429, { code: "TooManyRequests", message: "retry-secret-canary" }, { "x-ms-retry-after-ms": "1", "x-ms-request-charge": "1.25" });
  } });

  // when
  const actual = await f.create();

  // then
  assert.equal(actual.error.code, "RATE_LIMITED");
  assert.equal(actual.error.outcome, "not_applied");
  assert.equal(actual.metrics.retry_count, 3);
  assert.equal(actual.metrics.request_charge, 7);
  assert.equal(f.calls.filter((call) => call.operation === "create").length, 4);
  assert.equal(f.context.metrics.knownCharge, 7);
  assert.equal(JSON.stringify(actual).includes("canary"), false);
});

test("실제 SDK의 병렬 RPC는 재시도 비용을 요청별로 분리한다", { timeout: 5000 }, async (t) => {
  // given
  const attempts = new Map();
  const f = sdkTransportFixture(t, { onRequest({ operation, request, respond }) {
    if (operation !== "create") return;
    const document = JSON.parse(request.body);
    const count = (attempts.get(document.id) ?? 0) + 1;
    attempts.set(document.id, count);
    if (document.id === "a" && count === 1) return respond(request, 429, { code: "TooManyRequests", message: "parallel-secret-canary" }, { "x-ms-retry-after-ms": "1", "x-ms-request-charge": "1.5" });
    return respond(request, 201, { ...document, _etag: "etag-1" }, { "x-ms-request-charge": document.id === "a" ? "4" : "7" });
  } });
  const runtime = createCosmosRuntime({ clients: f.clients });
  const requests = ["a", "b"].map((id) => rpcRequest("create_document", { database: "fixture", container: "items", document: { id, tenant: "a" }, partition_key: fixtureIdentity.partition_key }, { id, connection_id: id }));

  // when
  const actual = await Promise.all(requests.map((request) => runtime.dispatch(request)));

  // then
  assert.equal(actual[0].result.metrics.request_charge, 7.5);
  assert.equal(actual[0].result.metrics.retry_count, 1);
  assert.equal(actual[1].result.metrics.request_charge, 9);
  assert.equal(actual[1].result.metrics.retry_count, 0);
  assert.deepEqual([...attempts.entries()].sort(), [["a", 2], ["b", 1]]);
});

test("실제 HTTP 응답의 RU 누락은 알려진 부분 비용을 유지하며 총비용은 null로 표시한다", { timeout: 5000 }, async (t) => {
  // given
  const f = sdkTransportFixture(t, { onRequest({ operation, request, respond }) {
    if (operation === "read") { const result = respond(request, 200, { id: "1", tenant: "a", _etag: "etag-1" }); result.headers.delete("x-ms-request-charge"); return result; }
  } });

  // when
  const actual = await f.read();

  // then
  assert.equal(actual.status, "succeeded");
  assert.equal(actual.metrics.request_charge, null);
  assert.equal(f.context.metrics.knownCharge, 2);
  assert.equal(f.context.metrics.chargeIsComplete, false);
});

test("실제 SDK에 전달된 쓰기 abort 신호는 재전송 없이 결과 불명으로 끝난다", { timeout: 5000 }, async (t) => {
  // given
  const controller = new AbortController();
  let started;
  const ready = new Promise((resolve) => { started = resolve; });
  const f = sdkTransportFixture(t, { context: { signal: controller.signal }, onRequest({ operation, request }) {
    if (operation !== "create") return;
    started();
    return new Promise((_, reject) => request.abortSignal.addEventListener("abort", () => reject(Object.assign(new Error("abort-secret-canary"), { name: "AbortError" })), { once: true }));
  } });
  const completion = f.create();
  await ready;

  // when
  controller.abort();

  // then
  assert.equal((await completion).status, "outcome_unknown");
  assert.equal((await completion).error.code, "OUTCOME_UNKNOWN");
  assert.equal(f.calls.filter((call) => call.operation === "create").length, 1);
});
