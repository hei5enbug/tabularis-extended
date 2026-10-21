import test from "node:test";
import assert from "node:assert/strict";
import { queryFixture } from "../support/query-fixture.mjs";
import { abortable } from "../support/fixture.mjs";

const document = { id: "1", tenant: "a", nested: { value: null }, array: [1, null], _etag: "etag-1", _rid: "document-rid", _self: "synthetic-self", _attachments: "attachments/", _ts: 1 };

test("documents 모드는 point read와 같은 ETag와 전체 JSON일 때만 편집 가능한 원본을 반환한다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [document], hasMore: false }], documents: { "1": document } });

  // when
  const actual = await f.execute({ result_mode: "documents" });

  // then
  assert.deepEqual(actual.data, { kind: "documents", documents: [{ document, identity: { id: "1", partition_key: [{ type: "string", value: "a" }] }, etag: "etag-1" }] });
  assert.deepEqual(actual.snapshot.sets[0].rows, actual.data.documents);
  assert.equal(actual.snapshot.sets[0].kind, "documents");
  assert.equal(actual.metrics.request_charge, 3);
  assert.equal(f.calls.filter((call) => call.method === "point").length, 1);
});

test("id와 ETag와 partition key를 담은 projection도 전체 문서와 다르면 거부한다", async (t) => {
  // given
  const projection = { id: document.id, tenant: document.tenant, _etag: document._etag };
  const f = queryFixture(t, { pages: [{ resources: [projection], hasMore: false }], documents: { "1": document } });

  // when
  const actual = await f.rpc({ result_mode: "documents" });

  // then
  assert.equal(actual.error.code, "INVALID_ARGUMENT");
  assert.equal(actual.data, null);
  assert.equal(f.calls.filter((call) => call.method === "point").length, 1);
});

test("documents 검증 도중 ETag가 바뀌면 충돌로 실패한다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [document], hasMore: false }], documents: { "1": { ...document, _etag: "etag-2" } } });

  // when
  const actual = await f.rpc({ result_mode: "documents" });

  // then
  assert.equal(actual.error.code, "ETAG_CONFLICT");
  assert.equal(actual.error.outcome, "not_applied");
  assert.equal(actual.metrics.request_charge, 3);
});

for (const [label, value] of [["scalar", 1], ["array", [document]], ["id 누락", { tenant: "a", _etag: "etag-1" }], ["ETag 누락", { id: "1", tenant: "a" }]]) {
  test(`documents 후보의 ${label}는 point read 전에 거부한다`, async (t) => {
    // given
    const f = queryFixture(t, { pages: [{ resources: [value], hasMore: false }] });

    // when
    const actual = await f.rpc({ result_mode: "documents" });

    // then
    assert.equal(actual.error.code, "INVALID_ARGUMENT");
    assert.equal(f.calls.filter((call) => call.method === "point").length, 0);
  });
}

test("기본 json_values 모드는 문서처럼 보여도 point read나 문서 편집 판정을 하지 않는다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [document, 1, false, null, ["x"]], hasMore: false }] });

  // when
  const actual = await f.execute({ page_size: 100 });

  // then
  assert.deepEqual(actual.data, { kind: "json_values", values: [document, 1, false, null, ["x"]] });
  assert.equal(f.calls.filter((call) => call.method === "point").length, 0);
});

test("documents 검증은 두 point read까지만 동시에 실행하며 순서를 유지한다", async (t) => {
  // given
  const documents = Array.from({ length: 7 }, (_, index) => ({ ...document, id: String(index) }));
  let active = 0;
  let maximum = 0;
  const f = queryFixture(t, { pages: [{ resources: documents, hasMore: false }], async pointRead({ id }) { active++; maximum = Math.max(maximum, active); await new Promise((resolve) => setTimeout(resolve, 2)); active--; return { resource: documents[Number(id)], requestCharge: 1 }; } });

  // when
  const actual = await f.execute({ result_mode: "documents", page_size: 100 });

  // then
  assert.equal(maximum, 2);
  assert.deepEqual(actual.data.documents.map((entry) => entry.identity.id), documents.map((entry) => entry.id));
  assert.equal(actual.metrics.request_charge, 9);
});

test("documents point read의 RU도 query 예산에 포함한다", async (t) => {
  // given
  const documents = Array.from({ length: 6 }, (_, index) => ({ ...document, id: String(index) }));
  const f = queryFixture(t, { pages: [{ resources: documents, hasMore: false }], documents: Object.fromEntries(documents.map((value) => [value.id, value])) });

  // when
  const actual = await f.rpc({ result_mode: "documents", ru_budget: 3 });

  // then
  assert.equal(actual.error.code, "RESOURCE_LIMIT");
  assert.equal(actual.data, null);
  assert.ok(actual.metrics.request_charge >= 3);
  assert.ok(f.calls.filter((call) => call.method === "point").length <= 2);
});

test("누락과 null을 포함한 전체 hierarchical PK로 documents를 검증한다", async (t) => {
  // given
  const value = { ...document, tenant: null, region: false };
  const f = queryFixture(t, { paths: ["/tenant", "/region", "/missing"], pages: [{ resources: [value], hasMore: false }], documents: { "1": value } });

  // when
  const actual = await f.execute({ result_mode: "documents" });

  // then
  assert.deepEqual(actual.data.documents[0].identity.partition_key, [{ type: "null" }, { type: "boolean", value: false }, { type: "undefined" }]);
  assert.deepEqual(f.calls.find((call) => call.method === "point").partitionKey, [null, false, {}]);
});

test("query 취소는 동일 client의 다른 query를 중단하지 않는다", async (t) => {
  // given
  const controller = new AbortController();
  let ready;
  const started = new Promise((resolve) => { ready = resolve; });
  let otherReady;
  const otherStarted = new Promise((resolve) => { otherReady = resolve; });
  const f = queryFixture(t, { pages: [{ resources: [1], hasMore: false, async action(options, spec) {
    if (spec.query === "SELECT VALUE @cancel") { ready(); await abortable(options.abortSignal); }
    else { otherReady(); await new Promise((resolve) => setTimeout(resolve, 5)); }
  } }] });
  const pending = f.rpc({ text: "SELECT VALUE @cancel" }, undefined, { signal: controller.signal });
  await started;
  const other = f.rpc({ text: "SELECT VALUE @other" });
  await otherStarted;

  // when
  controller.abort();

  // then
  assert.equal((await pending).error.code, "CANCELLED");
  assert.equal((await pending).error.outcome, "not_applied");
  assert.equal((await other).status, "succeeded");
  assert.deepEqual((await other).data.data.values, [1]);
  assert.equal(f.calls.filter((call) => call.method === "dispose").length, 0);
});

test("직접 execute의 deadline은 SDK abort 신호로 대기 중 query를 종료한다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [], hasMore: true, action: (options) => abortable(options.abortSignal) }] });

  // when
  const actual = await f.rpc({}, undefined, { deadline_ms: 20 });

  // then
  assert.equal(actual.error.code, "DEADLINE_EXCEEDED");
  assert.equal(actual.data, null);
  assert.equal(f.calls.filter((call) => call.method === "fetchNext").length, 1);
});
