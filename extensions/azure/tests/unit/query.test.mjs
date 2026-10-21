import test from "node:test";
import assert from "node:assert/strict";
import { queryFixture, queryInput } from "../support/query-fixture.mjs";
import { MAX_PAGE_BYTES, MAX_SPOOL_BYTES, MAX_SPOOL_ROWS } from "../../dist/query/index.js";

test.beforeEach((t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 1_700_000_000_000 });
});

test("초기 빈 페이지는 같은 iterator에서 실제 결과가 나올 때까지 읽는다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [], hasMore: true, ru: 1 }, { resources: [null, [1, { missing: null }]], hasMore: true, token: "native-next", ru: 2 }] });

  // when
  const actual = await f.execute({ text: "SELECT VALUE c.nested FROM c ORDER BY c.sort" });

  // then
  assert.deepEqual(actual.data, { kind: "json_values", values: [null, [1, { missing: null }]] });
  assert.equal(actual.metrics.request_charge, 4);
  assert.equal(actual.cursor.native_state.continuation_token, "native-next");
  assert.equal(f.calls.filter((call) => call.method === "query").length, 1);
  assert.equal(f.calls.filter((call) => call.method === "fetchNext").length, 2);
  assert.deepEqual(actual.snapshot.sets[0].rows, actual.data.values);
});

test("native token이 없는 결과는 iterator가 끝난 후 전체 snapshot을 반환한다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [{ status: "a", n: 3 }, { status: "b", n: 2 }], hasMore: true, ru: 2 }, { resources: [{ status: "c", n: 1 }], hasMore: false, ru: 3 }] });

  // when
  const actual = await f.execute({ text: "SELECT c.status, COUNT(1) AS n FROM c GROUP BY c.status" });

  // then
  assert.equal(actual.cursor, null);
  assert.equal(actual.snapshot.sets[0].rows.length, 3);
  assert.equal(actual.data.values.length, 2);
  assert.equal(actual.metrics.request_charge, 6);
  assert.equal(actual.snapshot.allow_truncation, false);
  assert.equal(actual.limits.truncated, false);
});

test("materialize 도중 오류가 나면 부분 집계를 성공으로 반환하지 않는다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [10], hasMore: true }, { error: Object.assign(new Error("query-body-secret-canary"), { code: 429, requestCharge: 2, headers: { Authorization: "secret-canary" } }) }] });

  // when
  const actual = await f.rpc({ text: "SELECT VALUE SUM(c.value) FROM c" });

  // then
  assert.equal(actual.status, "failed");
  assert.equal(actual.error.code, "RATE_LIMITED");
  assert.equal(actual.data, null);
  assert.equal(actual.metrics.request_charge, 4);
  assert.equal(JSON.stringify(actual).includes("canary"), false);
});

test("RU 예산을 넘은 집계는 다음 fetch를 시작하지 않는다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [10], hasMore: true, ru: 3 }, { resources: [20], hasMore: false }] });

  // when
  const actual = await f.rpc({ ru_budget: 3 });

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
  assert.equal(actual.metrics.request_charge, 4);
  assert.equal(f.calls.filter((call) => call.method === "fetchNext").length, 1);
});

test("기본 RU 예산 100을 넘으면 다음 fetch를 시작하지 않는다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [], hasMore: true, ru: 101 }] });

  // when
  const actual = await f.rpc();

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.metrics.request_charge, 102);
  assert.equal(f.calls.filter((call) => call.method === "fetchNext").length, 1);
});

test("native 페이지의 byte tail은 다음 SDK fetch보다 먼저 반환한다", async (t) => {
  // given
  const values = ["a", "b", "c"].map((id) => ({ id, text: id.repeat(3 * 1024 * 1024) }));
  const f = queryFixture(t, { pages: [{ resources: values, hasMore: true, token: "native-next" }], resumePages: { "native-next": [{ resources: ["last"], hasMore: false }] } });
  const first = await f.execute({ page_size: 100 });
  const before = f.calls.filter((call) => call.method === "fetchNext").length;

  // when
  const actual = await f.resume(first.cursor, { page_size: 100 });

  // then
  assert.deepEqual(actual.data.values.map((row) => row.id), ["c"]);
  assert.equal(f.calls.filter((call) => call.method === "fetchNext").length, before);
  assert.deepEqual(actual.snapshot.sets[0].rows, actual.data.values);
  assert.equal(actual.cursor.tail.length, 0);
  assert.ok(Buffer.byteLength(JSON.stringify(actual.data)) < MAX_PAGE_BYTES - 60 * 1024);
});

test("tail 다음의 native 결과까지 순서대로 재개해 중복과 누락을 만들지 않는다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [1, 2, 3], hasMore: true, token: "native-next" }], resumePages: { "native-next": [{ resources: [4, 5], hasMore: false }] } });
  const first = await f.execute();
  const second = await f.resume(first.cursor);

  // when
  const actual = await f.resume(second.cursor);

  // then
  assert.deepEqual([...first.data.values, ...second.data.values, ...actual.data.values], [1, 2, 3, 4, 5]);
  assert.equal(actual.cursor, null);
  assert.equal(actual.snapshot.revision, first.snapshot.revision);
});

for (const [label, input] of [["쿼리문", { text: "SELECT VALUE 2" }], ["파라미터", { parameters: [{ name: "@x", value: "changed" }] }], ["partition key", { partition_key: [{ type: "string", value: "other" }] }], ["페이지 크기", { page_size: 3 }], ["결과 모드", { result_mode: "documents" }]]) {
  test(`cursor의 ${label}가 바뀌면 network 전에 거부한다`, async (t) => {
    // given
    const f = queryFixture(t, { pages: [{ resources: [1], hasMore: true, token: "native-next" }] });
    const first = await f.execute();
    const before = f.calls.length;

    // when
    const actual = await f.rpc(input, first.cursor);

    // then
    assert.equal(actual.error.code, "INVALID_PAGE_TOKEN");
    assert.equal(f.calls.length, before);
  });
}

for (const [label, change, context] of [
  ["SDK 버전", (state) => { state.native_state.sdk_version = "other"; }, {}],
  ["고정 옵션", (state) => { state.native_state.original_options.maxDegreeOfParallelism = 10; }, {}],
  ["만료 시간", (state) => { state.expires_at_ms = 1; }, {}],
  ["연결", () => {}, { connection_id: "other" }],
  ["인증 identity", () => {}, { auth: { kind: "account_key", account_key: "synthetic", identity: "other" } }],
  ["endpoint", () => {}, { connection: { endpoint: "https://other.documents.azure.com", database: "fixture", auth_mode: "account_key" } }],
]) {
  test(`cursor의 ${label} 바인딩이 다르면 network 전에 거부한다`, async (t) => {
    // given
    const f = queryFixture(t, { pages: [{ resources: [1], hasMore: true, token: "native-next" }] });
    const first = await f.execute();
    const state = structuredClone(first.cursor);
    change(state);
    const before = f.calls.length;

    // when
    const actual = await f.rpc({}, state, context);

    // then
    assert.equal(actual.error.code, label === "만료 시간" ? "CURSOR_EXPIRED" : "INVALID_PAGE_TOKEN");
    assert.equal(f.calls.length, before);
  });
}

for (const field of ["database_rid", "container_rid"]) {
  test(`${field}가 바뀌면 metadata 확인 후 query fetch 전에 거부한다`, async (t) => {
    // given
    const f = queryFixture(t, { pages: [{ resources: [1], hasMore: true, token: "native-next" }] });
    const first = await f.execute();
    f.meta[field] = "changed-rid";
    const before = f.calls.filter((call) => call.method === "fetchNext").length;

    // when
    const actual = await f.rpc({}, first.cursor);

    // then
    assert.equal(actual.error.code, "INVALID_PAGE_TOKEN");
    assert.equal(f.calls.filter((call) => call.method === "fetchNext").length, before);
  });
}

test("materialized spool의 만 행 제한을 넘으면 실패한다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: Array.from({ length: MAX_SPOOL_ROWS }, (_, index) => index), hasMore: true }, { resources: [MAX_SPOOL_ROWS], hasMore: false }] });

  // when
  const actual = await f.rpc();

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
});

test("materialized spool의 32MiB 제한을 넘으면 실패한다", async (t) => {
  // given
  const rows = Array.from({ length: 6 }, () => "x".repeat(6 * 1024 * 1024));
  const f = queryFixture(t, { pages: rows.map((row, index) => ({ resources: [row], hasMore: index < rows.length - 1 })) });

  // when
  const actual = await f.rpc();

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
  assert.ok(MAX_SPOOL_BYTES < Buffer.byteLength(JSON.stringify(rows)));
});

test("한 JSON 값이 public envelope 상한보다 크면 빈 페이지 대신 오류를 반환한다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: ["x".repeat(MAX_PAGE_BYTES)], hasMore: false }] });

  // when
  const actual = await f.rpc();

  // then
  assert.equal(actual.error.code, "DOCUMENT_TOO_LARGE");
  assert.equal(actual.data, null);
});

test("쿼리문과 named parameter 값은 보간하거나 변경하지 않는다", async (t) => {
  // given
  const f = queryFixture(t);
  const text = "SELECT VALUE c.value FROM c WHERE c.name = @name /* unchanged */";
  const parameters = [{ name: "@name", value: "a' OR true --" }, { name: "@json", value: { nested: [null, true] } }];

  // when
  const actual = await f.execute({ text, parameters });

  // then
  assert.equal(actual.cursor, null);
  assert.deepEqual(f.calls.find((call) => call.method === "query").spec, { query: text, parameters });
});

for (const component of [{ type: "string", value: "a" }, { type: "number", value: 7 }, { type: "boolean", value: false }, { type: "null" }, { type: "undefined" }]) {
  test(`query targeting의 ${component.type} 타입을 SDK에 그대로 전달한다`, async (t) => {
    // given
    const f = queryFixture(t);

    // when
    const actual = await f.execute({ partition_key: [component] });

    // then
    assert.equal(actual.cursor, null);
    assert.deepEqual(f.calls.find((call) => call.method === "query").options.partitionKey, [component.type === "undefined" ? {} : component.type === "null" ? null : component.value]);
    assert.equal(Object.hasOwn(f.calls.find((call) => call.method === "query").options, "forceQueryPlan"), false);
  });
}

test("hierarchical prefix key만 query targeting에서 허용한다", async (t) => {
  // given
  const f = queryFixture(t, { paths: ["/tenant", "/region", "/optional"] });

  // when
  const actual = await f.execute({ partition_key: [{ type: "string", value: "a" }, { type: "number", value: 7 }] });

  // then
  assert.equal(actual.cursor, null);
  assert.deepEqual(f.calls.find((call) => call.method === "query").options.partitionKey, ["a", 7]);
});

test("unscoped query는 고정된 query control 옵션과 forceQueryPlan을 사용한다", async (t) => {
  // given
  const f = queryFixture(t);

  // when
  const actual = await f.execute();

  // then
  assert.equal(actual.cursor, null);
  assert.equal(f.calls.find((call) => call.method === "query").options.enableQueryControl, true);
  assert.equal(f.calls.find((call) => call.method === "query").options.maxDegreeOfParallelism, 2);
  assert.equal(f.calls.find((call) => call.method === "query").options.bufferItems, false);
  assert.equal(f.calls.find((call) => call.method === "query").options.forceQueryPlan, true);
});

test("구형 query handler는 원본 JSON 한 페이지와 기존 QueryResult shape를 반환한다", async (t) => {
  // given
  const f = queryFixture(t);

  // when
  const actual = await f.handlers.execute_query({ query: queryInput.text, container: "items", limit: 2, page: 1 }, f.context());

  // then
  assert.deepEqual(actual, { columns: ["_document"], rows: [[1], [null]], affected_rows: 0, truncated: false, pagination: { page: 1, page_size: 2, total_rows: 2, has_more: false } });
});

test("구형 query handler의 두 번째 페이지는 실행 전에 cursor API 사용을 요구한다", async (t) => {
  // given
  const f = queryFixture(t);
  const action = () => f.handlers.execute_query({ query: queryInput.text, container: "items", page: 2 }, f.context());

  // when
  const actual = await action().catch((error) => error);

  // then
  assert.equal(actual.code, "UNSUPPORTED_OPERATION");
  assert.equal(f.calls.length, 0);
});

test("native cursor 재개 입력이 16MiB 여유 상한을 넘으면 첫 결과 전에 거부한다", async (t) => {
  // given
  const rows = Array.from({ length: 7 }, () => "x".repeat(3 * 1024 * 1024));
  const f = queryFixture(t, { pages: [{ resources: rows, hasMore: true, token: "native-next" }] });

  // when
  const actual = await f.rpc({ page_size: 1 });

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
  assert.ok(actual.metrics.request_charge > 0);
});

test("data와 snapshot 중복까지 포함한 native RPC 응답이 16MiB를 넘으면 실패한다", async (t) => {
  // given
  const rows = Array.from({ length: 4 }, () => "x".repeat(3 * 1024 * 1024));
  const f = queryFixture(t, { pages: [{ resources: rows, hasMore: true, token: "native-next" }] });

  // when
  const actual = await f.rpc({ page_size: 100 });

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
  assert.ok(actual.metrics.request_charge > 0);
});

test("완성된 materialized snapshot도 단일 RPC 전달 상한을 넘으면 부분 성공 없이 실패한다", async (t) => {
  // given
  const rows = Array.from({ length: 3 }, () => "x".repeat(6 * 1024 * 1024));
  const f = queryFixture(t, { pages: rows.map((value, index) => ({ resources: [value], hasMore: index < rows.length - 1 })) });

  // when
  const actual = await f.rpc();

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
  assert.equal(f.calls.filter((call) => call.method === "fetchNext").length, 3);
  assert.ok(actual.metrics.request_charge > 0);
});

test("직접 execute는 완성된 32MiB 이하 snapshot을 유지하며 RPC와 저장 상한을 구별한다", async (t) => {
  // given
  const rows = Array.from({ length: 3 }, () => "x".repeat(6 * 1024 * 1024));
  const f = queryFixture(t, { pages: rows.map((value, index) => ({ resources: [value], hasMore: index < rows.length - 1 })) });

  // when
  const actual = await f.execute();

  // then
  assert.equal(actual.cursor, null);
  assert.equal(actual.snapshot.sets[0].rows.length, 3);
  assert.ok(Buffer.byteLength(JSON.stringify(actual.snapshot)) < MAX_SPOOL_BYTES);
  assert.equal(actual.limits.truncated, false);
});

test("긴 원본 query가 cursor 재개 frame에 두 번 들어가면 사용불가능한 token 대신 실패한다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [1], hasMore: true, token: "native-next" }] });
  const text = "SELECT VALUE 1 /*" + "x".repeat(9 * 1024 * 1024) + "*/";

  // when
  const actual = await f.rpc({ text });

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
  assert.equal(f.calls.filter((call) => call.method === "fetchNext").length, 1);
});
