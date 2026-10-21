import test from "node:test";
import assert from "node:assert/strict";
import { querySdkFixture, queryPlan } from "../support/query-sdk.mjs";
import { abortable, rpcRequest } from "../support/fixture.mjs";
import { createCosmosRuntime, MetricsAccumulator } from "../../dist/runtime/index.js";
import { queryInput } from "../support/query-fixture.mjs";

test("실제 SDK의 scoped query는 문자열 SQL과 typed PK를 HTTP에서 보존한다", async (t) => {
  // given
  const text = "SELECT VALUE c.value FROM c WHERE c.name = @name";
  const parameters = [{ name: "@name", value: "a' OR true --" }];
  const f = querySdkFixture(t, { pages: { "": { values: [1, null] } } });

  // when
  const actual = await f.execute({ text, parameters, partition_key: [{ type: "boolean", value: false }] });

  // then
  assert.deepEqual(actual.data.values, [1, null]);
  assert.deepEqual(JSON.parse(f.calls.find((call) => call.operation === "query").request.body), { query: text, parameters });
  assert.equal(f.calls.find((call) => call.operation === "query").request.headers.get("x-ms-documentdb-partitionkey"), "[false]");
  assert.equal(actual.cursor, null);
});

test("실제 SDK의 unscoped native token을 새 iterator에 전달해 중복 없이 재개한다", async (t) => {
  // given
  const f = querySdkFixture(t, { pages: { "": { values: [1, 2], token: "backend-next" }, "backend-next": { values: [3, 4] } } });
  const first = await f.execute();

  // when
  const actual = await f.resume(first.cursor);

  // then
  assert.deepEqual([...first.data.values, ...actual.data.values], [1, 2, 3, 4]);
  assert.equal(actual.cursor, null);
  assert.equal(f.calls.filter((call) => call.operation === "query").length, 2);
  assert.equal(f.calls.find((call) => call.operation === "query" && call.request.headers.get("x-ms-continuation") === "backend-next").request.headers.get("x-ms-documentdb-partitionkey"), undefined);
});

test("실제 SDK의 초기 빈 backend 페이지 뒤 결과를 같은 iterator에서 얻는다", async (t) => {
  // given
  const f = querySdkFixture(t, { pages: { "": { values: [], token: "backend-next", ru: 2 }, "backend-next": { values: ["result"], ru: 3 } } });

  // when
  const actual = await f.execute();

  // then
  assert.deepEqual(actual.data.values, ["result"]);
  assert.equal(actual.cursor, null);
  assert.equal(f.calls.filter((call) => call.operation === "query").length, 2);
  assert.ok(actual.metrics.request_charge >= 7);
});

test("실제 SDK의 GROUP BY는 native token 없이 완성된 집계만 materialize한다", async (t) => {
  // given
  const plan = queryPlan({ hasSelectValue: false, groupByExpressions: ["c.status"], groupByAliasToAggregateType: { status: null, n: "Count" } });
  const f = querySdkFixture(t, { plan, pages: { "": { values: [{ groupByItems: ["a"], payload: { status: "a", n: { item2: 2 } } }], token: "backend-next" }, "backend-next": { values: [{ groupByItems: ["a"], payload: { status: "a", n: { item2: 3 } } }, { groupByItems: ["b"], payload: { status: "b", n: { item2: 1 } } }] } } });

  // when
  const actual = await f.execute({ text: "SELECT c.status, COUNT(1) AS n FROM c GROUP BY c.status", page_size: 1 });

  // then
  assert.equal(actual.cursor, null);
  assert.deepEqual(actual.snapshot.sets[0].rows, [{ status: "a", n: 5 }, { status: "b", n: 1 }]);
  assert.equal(actual.data.values.length, 1);
  assert.equal(f.calls.filter((call) => call.operation === "query").length, 2);
});

test("실제 SDK의 COUNT는 전체 backend 비용을 확인한 후 완성된 scalar를 반환한다", async (t) => {
  // given
  const plan = queryPlan({ aggregates: ["Count"] });
  const f = querySdkFixture(t, { plan, pages: { "": { values: [[{ item2: 2 }]], token: "backend-next", ru: 2 }, "backend-next": { values: [[{ item2: 3 }]], ru: 3 } } });

  // when
  const actual = await f.execute({ text: "SELECT VALUE COUNT(1) FROM c" });

  // then
  assert.equal(actual.cursor, null);
  assert.deepEqual(actual.data.values, [5]);
  assert.deepEqual(actual.snapshot.sets[0].rows, [5]);
  assert.ok(actual.metrics.request_charge >= 7);
});

test("실제 SDK의 ORDER BY token은 초기 빈 페이지 후 새 iterator에서 정렬 순서를 유지한다", async (t) => {
  // given
  const text = "SELECT * FROM c ORDER BY c.sort";
  const plan = queryPlan({ hasSelectValue: false, orderBy: ["Ascending"], orderByExpressions: ["c.sort"], rewrittenQuery: text });
  const wrap = (sort) => ({ _rid: `k6d9AKfK7vM${sort === 1 ? "BAAAAAAAAAA" : "CAAAAAAAAAA"}==`, orderByItems: [{ item: sort }], payload: { id: String(sort), sort } });
  const f = querySdkFixture(t, { plan, pages: { "": { values: [], token: "backend-data" }, "backend-data": { values: [wrap(1)], token: "backend-next" }, "backend-next": { values: [wrap(2)] } } });
  const first = await f.execute({ text, page_size: 1 });

  // when
  const actual = await f.resume(first.cursor, { text, page_size: 1 });

  // then
  assert.deepEqual([...first.data.values, ...actual.data.values], [{ id: "1", sort: 1 }, { id: "2", sort: 2 }]);
  assert.equal(actual.cursor, null);
  assert.ok(f.calls.filter((call) => call.operation === "query").length >= 3);
});

test("실제 SDK의 non-streaming ORDER BY는 native cursor 없이 완성된 정렬 결과를 반환한다", async (t) => {
  // given
  const plan = queryPlan({ hasSelectValue: false, hasNonStreamingOrderBy: true, orderBy: ["Ascending"], orderByExpressions: ["c.sort"], top: 3 });
  const wrap = (sort) => ({ orderByItems: [{ item: sort }], payload: { sort } });
  const f = querySdkFixture(t, { plan, pages: { "": { values: [wrap(3)], token: "backend-next" }, "backend-next": { values: [wrap(1), wrap(2)] } } });

  // when
  const actual = await f.execute({ text: "SELECT TOP 3 * FROM c ORDER BY c.sort", page_size: 1 });

  // then
  assert.equal(actual.cursor, null);
  assert.deepEqual(actual.snapshot.sets[0].rows, [{ sort: 1 }, { sort: 2 }, { sort: 3 }]);
  assert.deepEqual(actual.data.values, [{ sort: 1 }]);
});

test("실제 SDK의 scoped hierarchical prefix key는 query plan 강제 없이 typed HTTP key를 보존한다", async (t) => {
  // given
  const f = querySdkFixture(t, { paths: ["/tenant", "/region", "/optional"], pages: { "": { values: [1] } } });

  // when
  const actual = await f.execute({ partition_key: [{ type: "string", value: "a" }, { type: "null" }] });

  // then
  assert.deepEqual(actual.data.values, [1]);
  assert.equal(f.calls.find((call) => call.operation === "query").request.headers.get("x-ms-documentdb-partitionkey"), '["a",null]');
  assert.equal(f.calls.filter((call) => call.operation === "ranges").length, 0);
});

test("실제 SDK의 GROUP BY 예산 초과는 부분 집계를 반환하지 않는다", async (t) => {
  // given
  const plan = queryPlan({ hasSelectValue: false, groupByExpressions: ["c.status"], groupByAliasToAggregateType: { status: null, n: "Count" } });
  const f = querySdkFixture(t, { plan, pages: { "": { values: [{ groupByItems: ["a"], payload: { status: "a", n: { item2: 2 } } }], token: "backend-next", ru: 5 }, "backend-next": { values: [], ru: 5 } } });

  // when
  const actual = await f.rpc({ text: "SELECT c.status, COUNT(1) AS n FROM c GROUP BY c.status", ru_budget: 4 });

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
  assert.ok(actual.metrics.request_charge > 4);
  assert.equal(f.calls.filter((call) => call.operation === "query").length, 1);
});

test("실제 SDK의 소진된 429 비용과 재시도 횟수를 실패 응답에 유지한다", async (t) => {
  // given
  const f = querySdkFixture(t, { onRequest({ operation, request, respond }) { if (operation === "query") return respond(request, 429, { code: "TooManyRequests", message: "query-secret-canary" }, { "x-ms-retry-after-ms": "1", "x-ms-request-charge": "1.25" }); } });

  // when
  const actual = await f.rpc();

  // then
  assert.equal(actual.error.code, "RATE_LIMITED");
  assert.equal(actual.metrics.retry_count, 3);
  assert.equal(f.calls.filter((call) => call.operation === "query").length, 4);
  assert.ok(actual.metrics.request_charge >= 7);
  assert.equal(JSON.stringify(actual).includes("canary"), false);
});

test("실제 SDK 병렬 query는 429 비용을 request별로 분리한다", async (t) => {
  // given
  const attempts = new Map();
  const f = querySdkFixture(t, { onRequest({ operation, request, respond }) {
    if (operation !== "query") return;
    const spec = JSON.parse(request.body);
    const count = (attempts.get(spec.query) ?? 0) + 1;
    attempts.set(spec.query, count);
    if (spec.query === "SELECT VALUE @a" && count === 1) return respond(request, 429, { code: "TooManyRequests", message: "parallel-query-canary" }, { "x-ms-retry-after-ms": "1", "x-ms-request-charge": "2" });
    return respond(request, 200, { Documents: [spec.query], _count: 1 }, { "x-ms-request-charge": spec.query === "SELECT VALUE @a" ? "3" : "5" });
  } });
  const runtime = createCosmosRuntime({ clients: f.clients, handlers: { query_page: f.handlers.query_page, execute_query: f.handlers.execute_query } });
  const requests = ["a", "b"].map((id) => rpcRequest("query_page", { query: { ...queryInput, text: `SELECT VALUE @${id}` } }, { id, connection_id: id, deadline_ms: 3000 }));

  // when
  const actual = await Promise.all(requests.map((request) => runtime.dispatch(request)));

  // then
  assert.equal(actual[0].result.status, "succeeded");
  assert.equal(actual[1].result.status, "succeeded");
  assert.equal(actual[0].result.metrics.retry_count, 1);
  assert.equal(actual[1].result.metrics.retry_count, 0);
  assert.equal(actual[0].result.metrics.request_charge, actual[1].result.metrics.request_charge);
  assert.deepEqual([...attempts.entries()].sort(), [["SELECT VALUE @a", 2], ["SELECT VALUE @b", 1]]);
});

test("실제 SDK의 측정할 수 없는 query RU는 알려진 비용을 유지하고 총비용을 null로 표시한다", async (t) => {
  // given
  const f = querySdkFixture(t, { onRequest({ operation, request, respond }) { if (operation === "query") { const result = respond(request, 200, { Documents: [1], _count: 1 }); result.headers.delete("x-ms-request-charge"); return result; } } });

  // when
  const actual = await f.rpc();

  // then
  assert.equal(actual.status, "succeeded");
  assert.equal(actual.metrics.request_charge, null);
  assert.ok(actual.data.metrics.request_charge === null);
});

test("실제 SDK의 query abort는 재요청 없이 취소로 종료한다", async (t) => {
  // given
  const controller = new AbortController();
  let ready;
  const started = new Promise((resolve) => { ready = resolve; });
  const f = querySdkFixture(t, { onRequest({ operation, request }) { if (operation === "query") { ready(); return abortable(request.abortSignal); } } });
  const pending = f.rpc({}, undefined, { signal: controller.signal });
  await started;

  // when
  controller.abort();

  // then
  assert.equal((await pending).error.code, "CANCELLED");
  assert.equal((await pending).error.outcome, "not_applied");
  assert.equal(f.calls.filter((call) => call.operation === "query").length, 1);
});

test("실제 SDK documents 모드는 전체 point JSON과 typed PK를 확인한다", async (t) => {
  // given
  const document = { id: "1", tenant: "a", region: 7, nested: { value: null }, _etag: "etag-1", _rid: "item-rid", _self: "synthetic", _attachments: "attachments/", _ts: 1 };
  const f = querySdkFixture(t, { paths: ["/tenant", "/region", "/missing"], pages: { "": { values: [document] } }, documents: { "1": document } });

  // when
  const actual = await f.execute({ result_mode: "documents" });

  // then
  assert.deepEqual(actual.data.documents[0].document, document);
  assert.equal(f.calls.find((call) => call.operation === "point").request.headers.get("x-ms-documentdb-partitionkey"), '["a",7,{}]');
  assert.equal(actual.data.documents[0].etag, "etag-1");
});

test("실제 SDK의 byte tail을 먼저 소비한 뒤 다음 native fetch를 실행한다", async (t) => {
  // given
  const values = ["a", "b", "c"].map((id) => ({ id, text: id.repeat(3 * 1024 * 1024) }));
  const f = querySdkFixture(t, { pages: { "": { values, token: "backend-next" }, "backend-next": { values: ["last"] } } });
  const first = await f.execute({ page_size: 100 });
  const before = f.calls.filter((call) => call.operation === "query").length;

  // when
  const actual = await f.resume(first.cursor, { page_size: 100 });

  // then
  assert.deepEqual(actual.data.values.map((value) => value.id), ["c"]);
  assert.equal(f.calls.filter((call) => call.operation === "query").length, before);
  assert.equal(actual.cursor.tail.length, 0);
  assert.equal(actual.cursor.native_state.continuation_token, first.cursor.native_state.continuation_token);
});

for (const [label, override, mutate] of [
  ["쿼리문", { text: "SELECT VALUE 2" }, () => {}],
  ["파라미터", { parameters: [{ name: "@x", value: "changed" }] }, () => {}],
  ["SDK 버전", {}, (state) => { state.binding.sdk_version = "4.9.0"; }],
]) {
  test(`실제 SDK cursor의 ${label} 변경은 HTTP 요청 전에 거부한다`, async (t) => {
    // given
    const f = querySdkFixture(t, { pages: { "": { values: [1], token: "backend-next" }, "backend-next": { values: [2] } } });
    const first = await f.execute();
    const state = structuredClone(first.cursor);
    mutate(state);
    const before = f.calls.length;

    // when
    const actual = await f.rpc(override, state);

    // then
    assert.equal(actual.error.code, "INVALID_PAGE_TOKEN");
    assert.equal(f.calls.length, before);
  });
}

for (const field of ["databaseRid", "containerRid"]) {
  test(`실제 SDK metadata의 ${field} 변경은 query HTTP 전에 거부한다`, async (t) => {
    // given
    const config = { pages: { "": { values: [1], token: "backend-next" }, "backend-next": { values: [2] } } };
    const f = querySdkFixture(t, config);
    const first = await f.execute();
    config[field] = "changed-rid";
    const before = f.calls.filter((call) => call.operation === "query").length;

    // when
    const actual = await f.rpc({}, first.cursor);

    // then
    assert.equal(actual.error.code, "INVALID_PAGE_TOKEN");
    assert.equal(f.calls.filter((call) => call.operation === "query").length, before);
  });
}

test("실제 SDK native 응답의 snapshot과 data 중복이 16MiB를 넘으면 RU를 보존하고 실패한다", async (t) => {
  // given
  const values = Array.from({ length: 4 }, () => "x".repeat(3 * 1024 * 1024));
  const f = querySdkFixture(t, { pages: { "": { values, token: "backend-next" } } });

  // when
  const actual = await f.rpc({ page_size: 100 });

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
  assert.ok(actual.metrics.request_charge > 0);
});

test("실제 SDK documents projection은 같은 ETag라도 point JSON과 다르면 실패한다", async (t) => {
  // given
  const candidate = { id: "1", tenant: "a", _etag: "etag-1" };
  const f = querySdkFixture(t, { pages: { "": { values: [candidate] } }, documents: { "1": { ...candidate, nested: { value: null }, _rid: "doc-rid" } } });

  // when
  const actual = await f.rpc({ result_mode: "documents" });

  // then
  assert.equal(actual.error.code, "INVALID_ARGUMENT");
  assert.equal(actual.data, null);
  assert.equal(f.calls.filter((call) => call.operation === "point").length, 1);
});

test("실제 SDK documents ETag 변경은 명확한 충돌로 실패한다", async (t) => {
  // given
  const candidate = { id: "1", tenant: "a", _etag: "etag-1" };
  const f = querySdkFixture(t, { pages: { "": { values: [candidate] } }, documents: { "1": { ...candidate, _etag: "etag-2" } } });

  // when
  const actual = await f.rpc({ result_mode: "documents" });

  // then
  assert.equal(actual.error.code, "ETAG_CONFLICT");
  assert.equal(actual.data, null);
});

test("실제 SDK의 container404는 document404로 오판하지 않는다", async (t) => {
  // given
  const f = querySdkFixture(t, { onRequest({ operation, request, respond }) { if (operation === "container") return respond(request, 404, { code: "NotFound", message: "container-secret-canary" }); } });

  // when
  const actual = await f.rpc();

  // then
  assert.equal(actual.error.code, "INVALID_ARGUMENT");
  assert.equal(actual.error.message, "The container was not found.");
  assert.equal(f.calls.filter((call) => call.operation === "query").length, 0);
  assert.equal(JSON.stringify(actual).includes("canary"), false);
});

test("실제 SDK의 scoped query에서 늦게 끝나는 query plan RU도 성공 전에 예산을 검증한다", async (t) => {
  // given
  const f = querySdkFixture(t, { pages: { "": { values: [1] } }, async onRequest({ operation, request, respond }) {
    if (operation === "plan") { await new Promise((resolve) => setTimeout(resolve, 30)); return respond(request, 200, queryPlan(), { "x-ms-request-charge": "25" }); }
  } });

  // when
  const actual = await f.rpc({ partition_key: [{ type: "string", value: "a" }], ru_budget: 10 });

  // then
  assert.equal(actual.error?.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
  assert.ok(actual.metrics.request_charge >= 27);
});

test("실제 SDK의 query plan이 429 대기 중이면 결과 확정 후 재송신하지 않는다", async (t) => {
  // given
  const metrics = new MetricsAccumulator();
  const f = querySdkFixture(t, { pages: { "": { values: [1] } }, onRequest({ operation, request, respond }) {
    if (operation === "plan") return respond(request, 429, { code: "TooManyRequests", message: "synthetic-plan-canary" }, { "x-ms-retry-after-ms": "40", "x-ms-request-charge": "1.25" });
  } });
  const finishAndObserve = async () => {
    const response = await f.rpc({ partition_key: [{ type: "string", value: "a" }] }, undefined, { metrics });
    const atReply = metrics.snapshot();
    const sendsAtReply = f.calls.length;
    await new Promise((resolve) => setTimeout(resolve, 100));
    return { response, atReply, afterRetry: metrics.snapshot(), sendsAtReply, sendsAfterRetry: f.calls.length };
  };

  // when
  const actual = await finishAndObserve();

  // then
  assert.equal(actual.response.status, "succeeded");
  assert.equal(actual.response.metrics.request_charge, 4.25);
  assert.equal(actual.response.metrics.retry_count, 0);
  assert.equal(f.calls.filter((call) => call.operation === "plan").length, 1);
  assert.equal(actual.sendsAfterRetry, actual.sendsAtReply);
  assert.deepEqual(actual.afterRetry, actual.atReply);
});

test("실제 SDK의 늦은 query plan 송신 실패는 총 RU를 null로 남긴다", async (t) => {
  // given
  const f = querySdkFixture(t, { pages: { "": { values: [1] } }, async onRequest({ operation }) {
    if (operation === "plan") { await new Promise((resolve) => setTimeout(resolve, 20)); throw new Error("synthetic-plan-network-canary"); }
  } });

  // when
  const actual = await f.rpc({ partition_key: [{ type: "string", value: "a" }] });

  // then
  assert.equal(actual.status, "succeeded");
  assert.equal(actual.metrics.request_charge, null);
  assert.equal(f.calls.filter((call) => call.operation === "plan").length, 1);
  assert.equal(JSON.stringify(actual).includes("canary"), false);
});

test("실제 SDK의 deadline 뒤에 끝나는 query plan은 확정한 실패 비용을 바꾸지 않는다", async (t) => {
  // given
  const metrics = new MetricsAccumulator();
  const f = querySdkFixture(t, { pages: { "": { values: [1] } }, async onRequest({ operation, request, respond }) {
    if (operation === "plan") { await new Promise((resolve) => setTimeout(resolve, 120)); return respond(request, 200, queryPlan(), { "x-ms-request-charge": "25" }); }
  } });
  const finishAndObserve = async () => {
    const response = await f.rpc({ partition_key: [{ type: "string", value: "a" }] }, undefined, { metrics, deadline_ms: 40 });
    const atReply = metrics.snapshot();
    const knownAtReply = metrics.knownCharge;
    const sendsAtReply = f.calls.length;
    await new Promise((resolve) => setTimeout(resolve, 150));
    return { response, atReply, knownAtReply, afterPlan: metrics.snapshot(), knownAfterPlan: metrics.knownCharge, sendsAtReply, sendsAfterPlan: f.calls.length };
  };

  // when
  const actual = await finishAndObserve();

  // then
  assert.equal(actual.response.error.code, "DEADLINE_EXCEEDED");
  assert.equal(actual.response.metrics.request_charge, null);
  assert.equal(actual.response.data, null);
  assert.deepEqual(actual.afterPlan, actual.atReply);
  assert.equal(actual.knownAfterPlan, actual.knownAtReply);
  assert.equal(actual.sendsAfterPlan, actual.sendsAtReply);
  assert.equal(f.calls.filter((call) => call.operation === "query").length, 1);
});

test("실제 SDK의 query plan 대기를 취소하면 진행 중 비용을 미확정으로 남긴다", async (t) => {
  // given
  const controller = new AbortController();
  let queried;
  const queryReady = new Promise((resolve) => { queried = resolve; });
  const f = querySdkFixture(t, { pages: { "": { values: [1] } }, onRequest({ operation, request }) {
    if (operation === "plan") return abortable(request.abortSignal);
    if (operation === "query") queried();
  } });
  const pending = f.rpc({ partition_key: [{ type: "string", value: "a" }] }, undefined, { signal: controller.signal });
  await queryReady;
  await new Promise((resolve) => setImmediate(resolve));

  // when
  controller.abort();

  // then
  assert.equal((await pending).error.code, "CANCELLED");
  assert.equal((await pending).metrics.request_charge, null);
  assert.equal(f.calls.filter((call) => call.operation === "query").length, 1);
  assert.equal(f.calls.filter((call) => call.operation === "plan").length, 1);
});

test("실제 SDK에서 한 연결의 송신을 닫아도 다른 연결의 필수 fetch는 계속한다", async (t) => {
  // given
  const f = querySdkFixture(t, { pages: { "": { values: [1] } }, async onRequest({ operation, request, respond }) {
    if (operation !== "plan") return;
    if (JSON.parse(request.body).query === queryInput.text) return respond(request, 429, { code: "TooManyRequests" }, { "x-ms-retry-after-ms": "40", "x-ms-request-charge": "1.25" });
    await new Promise((resolve) => setTimeout(resolve, 30));
    return respond(request, 200, queryPlan(), { "x-ms-request-charge": "25" });
  } });
  const finishAndObserve = async () => {
    const responses = await Promise.all([
      f.rpc({ partition_key: [{ type: "string", value: "a" }], ru_budget: 10 }, undefined, { connection_id: "sealed-connection" }),
      f.rpc({ text: "SELECT VALUE c.other FROM c", ru_budget: 100 }, undefined, { connection_id: "active-connection" }),
    ]);
    const sendsAtReply = f.calls.length;
    await new Promise((resolve) => setTimeout(resolve, 100));
    return { responses, sendsAtReply, sendsAfterRetry: f.calls.length };
  };

  // when
  const actual = await finishAndObserve();

  // then
  assert.equal(actual.responses[0].status, "succeeded");
  assert.equal(actual.responses[0].metrics.request_charge, 4.25);
  assert.equal(actual.responses[1].status, "succeeded");
  assert.ok(actual.responses[1].metrics.request_charge >= 28);
  assert.equal(actual.responses[0].metrics.retry_count, 0);
  assert.equal(actual.sendsAfterRetry, actual.sendsAtReply);
  assert.equal(f.calls.filter((call) => call.operation === "query").length, 2);
});
