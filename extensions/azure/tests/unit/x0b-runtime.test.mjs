import test from "node:test";
import assert from "node:assert/strict";
import { MAX_NATIVE_CONNECTIONS, NATIVE_CONNECTION_RETENTION_MS } from "../../dist/runtime/index.js";
import { deferred, runtimeFixture, invalidateRequest, workRequest, fencedScenario, capacityScenario, controlCapacityScenario } from "../support/x0b-fixture.mjs";

test("삭제된 연결의 최소 params로 SDK 조회 없이 인증을 무효화한다", async (t) => {
  // given
  const f = runtimeFixture(t);
  const request = invalidateRequest("deleted", { params: { driver: "cosmos-nosql", connection_id: "deleted", extra: { endpoint: "malformed-secret-canary" } } });

  // when
  const actual = await f.runtime.dispatch(request);

  // then
  assert.equal(actual.result.status, "succeeded");
  assert.deepEqual(actual.result.data, { invalidated: true });
  assert.deepEqual(f.calls, [{ method: "invalidate", connection_id: "deleted" }]);
  assert.equal(JSON.stringify(actual).includes("canary"), false);
});

for (const [label, override] of [
  ["auth", { driver_context: { ...invalidateRequest().params.driver_context, auth: null } }],
  ["session", { driver_context: { ...invalidateRequest().params.driver_context, session_handle: "session" } }],
  ["공개 input 필드", { input: { approved: true } }],
  ["다른 params 연결", { params: { driver: "cosmos-nosql", connection_id: "other" } }],
  ["누락 input", { input: undefined }],
  ["배열 params", { params: [] }],
  ["알 수 없는 envelope 필드", { permissions: ["write"] }],
  ["내부 generation 주장", { driver_context: { ...invalidateRequest().params.driver_context, native_generation: 9 } }],
]) {
  test(`무효화 요청에 ${label}을 포함하면 상태 변경 전에 거부한다`, async (t) => {
    // given
    const f = runtimeFixture(t);
    const request = invalidateRequest("a", override);

    // when
    const actual = await f.runtime.dispatch(request);

    // then
    assert.equal(actual.error.data.code, "INVALID_ARGUMENT");
    assert.deepEqual(f.calls, []);
  });
}

test("가득 찬 큐에서도 무효화는 대상 연결만 취소한다", async (t) => {
  // given
  const f = runtimeFixture(t, { runtime: { globalConcurrency: 2, connectionConcurrency: 1, queueLimit: 2 } });
  const requests = [workRequest("a", "a-running", { block: true }), workRequest("b", "b-running", { block: true }), workRequest("a", "a-queued", { block: true }), workRequest("b", "b-queued", { block: true })];
  const pending = requests.map((request) => f.runtime.dispatch(request));
  async function scenario() {
    const invalidated = await f.runtime.dispatch(invalidateRequest());
    const unrelated_active = f.calls.filter((call) => call.method === "handler" && call.connection_id === "b").length;
    await f.runtime.dispatch({ jsonrpc: "2.0", id: "cancel-b", method: "cancel_request", params: { connection_id: "b", request_id: "b-running" } });
    await f.runtime.close();
    return { invalidated, unrelated_active, results: await Promise.all(pending) };
  }

  // when
  const actual = await scenario();

  // then
  assert.deepEqual(actual.invalidated.result.data, { invalidated: true });
  assert.equal(actual.unrelated_active, 1);
  assert.equal(actual.results[0].result.status, "cancelled");
  assert.equal(actual.results[2].result.error.outcome, "not_started");
  assert.equal(f.calls.some((call) => call.request_id === "a-queued"), false);
  assert.deepEqual(f.calls.filter((call) => call.method === "invalidate"), [{ method: "invalidate", connection_id: "a" }]);
});

test("연결별 무효화 fence는 다른 연결의 dispatch를 막지 않는다", async (t) => {
  // given
  const release = deferred();
  const f = { ...runtimeFixture(t, { invalidate: () => release.promise }), release };

  // when
  const actual = await fencedScenario(f);

  // then
  assert.equal(actual.target_started_before_release, false);
  assert.equal(actual.other.result.status, "succeeded");
  assert.equal(actual.target.result.status, "succeeded");
  assert.deepEqual(actual.invalidation.result.data, { invalidated: true });
});

test("무효화 fence 대기는 남은 deadline 안에서 종료한다", async (t) => {
  // given
  const release = deferred();
  const f = runtimeFixture(t, { invalidate: () => release.promise });
  const invalidation = f.runtime.dispatch(invalidateRequest());
  const request = workRequest("a", "deadline");
  request.params.driver_context.deadline_ms = 5;
  async function scenario() { const result = await f.runtime.dispatch(request); release.resolve(); await invalidation; return result; }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.result.error.code, "DEADLINE_EXCEEDED");
  assert.equal(actual.result.error.outcome, "not_started");
  assert.equal(f.calls.some((call) => call.request_id === "deadline"), false);
});

test("동일 연결의 중복 무효화는 한 control을 공유하고 아홉 번째 연결은 거부한다", async (t) => {
  // given
  const release = deferred();
  const f = { ...runtimeFixture(t, { invalidate: () => release.promise }), release };

  // when
  const actual = await controlCapacityScenario(f);

  // then
  assert.equal(actual.overflow.result.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.completed.every((response) => response.result.status === "succeeded"), true);
  assert.equal(f.calls.filter((call) => call.method === "invalidate").length, 7);
  assert.equal(f.calls.some((call) => call.connection_id === "overflow"), false);
});

test("동일 연결 control flood도 여덟 RPC만 대기하며 cancel은 즉시 실행한다", async (t) => {
  // given
  const release = deferred();
  const f = runtimeFixture(t, { invalidate: () => release.promise });
  const pending_read = f.runtime.dispatch(workRequest("other", "read", { block: true }));
  async function scenario() {
    const controls = Array.from({ length: 8 }, (_, index) => f.runtime.dispatch({ ...invalidateRequest(), id: `control-${index}` }));
    const overflow = await f.runtime.dispatch(invalidateRequest());
    const cancelled = await f.runtime.dispatch({ jsonrpc: "2.0", id: "cancel", method: "cancel_request", params: { connection_id: "other", request_id: "read" } });
    release.resolve();
    return { overflow, cancelled, controls: await Promise.all(controls), read: await pending_read };
  }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.overflow.result.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.controls.length, 8);
  assert.equal(actual.controls.every((response) => response.result.status === "succeeded"), true);
  assert.equal(f.calls.filter((call) => call.method === "invalidate").length, 1);
  assert.deepEqual(actual.cancelled.result, { cancellation_requested: true });
  assert.equal(actual.read.result.status, "cancelled");
});

test("무효화 control deadline 이후 lease는 회수하고 진행 중인 fence는 공유한다", async (t) => {
  // given
  const release = deferred();
  const f = runtimeFixture(t, { invalidate: () => release.promise });
  async function scenario() {
    const expired = await Promise.all(Array.from({ length: 8 }, () => {
      const request = invalidateRequest();
      request.params.driver_context.deadline_ms = 5;
      return f.runtime.dispatch(request);
    }));
    const next = f.runtime.dispatch(invalidateRequest());
    release.resolve();
    return { expired, next: await next };
  }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.expired.every((response) => response.result.error.code === "DEADLINE_EXCEEDED"), true);
  assert.equal(actual.next.result.status, "succeeded");
  assert.equal(f.calls.filter((call) => call.method === "invalidate").length, 1);
});

test("반복 control timeout 뒤에도 fence 하나만 유지하고 새 대기 lease를 회수한다", async (t) => {
  // given
  const release = deferred();
  const f = runtimeFixture(t, { invalidate: () => release.promise });
  async function scenario() {
    const expired = [];
    for (let batch = 0; batch < 10; batch++) expired.push(...await Promise.all(Array.from({ length: 8 }, () => {
      const request = invalidateRequest();
      request.params.driver_context.deadline_ms = 1;
      return f.runtime.dispatch(request);
    })));
    const next = f.runtime.dispatch(invalidateRequest());
    release.resolve();
    return { expired, next: await next };
  }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.expired.length, 80);
  assert.equal(actual.expired.every((response) => response.result.error.code === "DEADLINE_EXCEEDED"), true);
  assert.equal(actual.next.result.status, "succeeded");
  assert.equal(f.calls.filter((call) => call.method === "invalidate").length, 1);
});

test("값 없는 invalidate rejection도 성공 ACK로 바꾸지 않는다", async (t) => {
  // given
  const f = runtimeFixture(t, { invalidate: () => Promise.reject(undefined) });

  // when
  const actual = await f.runtime.dispatch(invalidateRequest());

  // then
  assert.equal(actual.result.status, "failed");
  assert.equal(actual.result.error.code, "DRIVER_EXITED");
  assert.equal(actual.result.data, null);
});

test("실패한 무효화도 fence를 정리하고 고정 오류만 노출한다", async (t) => {
  // given
  const f = runtimeFixture(t, { invalidate: async () => { throw new Error("invalidate-secret-canary"); } });
  async function scenario() {
    const invalidation = await f.runtime.dispatch(invalidateRequest());
    const next = await f.runtime.dispatch(workRequest());
    return { invalidation, next };
  }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.invalidation.result.error.code, "DRIVER_EXITED");
  assert.equal(actual.next.result.status, "succeeded");
  assert.equal(JSON.stringify(actual).includes("canary"), false);
});

test("generation registry는 4096개 이후 새 연결을 dispatch 전에 거부한다", async (t) => {
  // given
  const f = runtimeFixture(t);

  // when
  const actual = await capacityScenario(f, MAX_NATIVE_CONNECTIONS);

  // then
  assert.equal(actual.result.error.code, "RESOURCE_LIMIT");
  assert.equal(f.calls.filter((call) => call.method === "handler").length, 4096);
});

test("20분 지난 비활성 generation만 새 연결을 위해 정리한다", async (t) => {
  // given
  let clock = 1000;
  const f = runtimeFixture(t, { runtime: { now: () => clock } });
  await capacityScenario(f, MAX_NATIVE_CONNECTIONS);
  clock += NATIVE_CONNECTION_RETENTION_MS;

  // when
  const actual = await f.runtime.dispatch(workRequest("new-after-retention"));

  // then
  assert.equal(actual.result.status, "succeeded");
  assert.equal(actual.result.data.generation, 0);
});

test("활성 generation은 보존기간 이후에도 정리하지 않는다", async (t) => {
  // given
  let clock = 1000;
  const f = runtimeFixture(t, { runtime: { now: () => clock, globalConcurrency: 2 } });
  const pending = f.runtime.dispatch(workRequest("held", "held", { block: true }));
  clock += NATIVE_CONNECTION_RETENTION_MS;
  async function scenario() {
    await capacityScenario(f, MAX_NATIVE_CONNECTIONS - 1);
    const invalidated = await f.runtime.dispatch(invalidateRequest("held"));
    const next = await f.runtime.dispatch(workRequest("held", "next"));
    return { invalidated, next, cancelled: await pending };
  }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.invalidated.result.status, "succeeded");
  assert.equal(actual.next.result.data.generation, 1);
  assert.equal(actual.cancelled.result.status, "cancelled");
});

test("무효화 중인 generation은 pending 요청 없이도 보존기간 이후 유지한다", async (t) => {
  // given
  let clock = 1000;
  const release = deferred();
  const f = runtimeFixture(t, { invalidate: () => release.promise, runtime: { now: () => clock } });
  await f.runtime.dispatch(workRequest("held", "first"));
  const invalidation = f.runtime.dispatch(invalidateRequest("held"));
  clock += NATIVE_CONNECTION_RETENTION_MS;
  async function scenario() {
    const overflow = await capacityScenario(f, MAX_NATIVE_CONNECTIONS - 1);
    const next = f.runtime.dispatch(workRequest("held", "after-invalidation"));
    release.resolve();
    const invalidated = await invalidation;
    return { overflow, invalidated, next: await next };
  }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.overflow.result.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.next.result.data.generation, 1);
  assert.equal(actual.invalidated.result.error.code, "DEADLINE_EXCEEDED");
});

test("설정 정규화 실패는 generation registry를 소모하지 않는다", async (t) => {
  // given
  const f = runtimeFixture(t);
  async function scenario() {
    for (let index = 0; index < MAX_NATIVE_CONNECTIONS; index++) {
      const request = workRequest(`invalid-${index}`);
      request.params.params = { driver: "cosmos-nosql" };
      await f.runtime.dispatch(request);
    }
    return await f.runtime.dispatch(workRequest("valid"));
  }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.result.status, "succeeded");
  assert.equal(f.calls.filter((call) => call.method === "handler").length, 1);
});

test("명시 종료와 EOF 종료를 반복해도 provider dispose는 한 번만 호출한다", async (t) => {
  // given
  const f = runtimeFixture(t);
  async function scenario() { const response = await f.runtime.dispatch({ jsonrpc: "2.0", id: "shutdown", method: "shutdown", params: {} }); await Promise.all([f.runtime.close(), f.runtime.close()]); return response; }

  // when
  const actual = await scenario();

  // then
  assert.equal(actual.result, null);
  assert.equal(f.calls.filter((call) => call.method === "dispose").length, 1);
});
