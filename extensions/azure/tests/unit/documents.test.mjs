import test from "node:test";
import assert from "node:assert/strict";
import { validateResponse } from "@tabularis/service-contracts";
import { createFixture, fixtureIdentity } from "../support/fixture.mjs";

test("문서 읽기는 중첩 구조와 누락 필드 및 원본 ETag를 보존한다", async (t) => {
  // given
  const document = { id: "1", tenant: "a", nested: { value: null }, tags: ["x", { y: true }], _etag: "etag-1" };
  const f = await createFixture(t, { document });

  // when
  const actual = await f.read();

  // then
  assert.deepEqual(actual.data.document, document);
  assert.equal(Object.hasOwn(actual.data.document, "missing"), false);
  assert.deepEqual(actual.data.identity, fixtureIdentity);
  assert.equal(actual.data.etag, "etag-1");
  assert.equal(actual.metrics.request_charge, 5);
  assert.equal(validateResponse(actual).valid, true);
});

test("문서 생성은 원본 JSON을 펼치지 않고 시스템 필드를 제거한다", async (t) => {
  // given
  const document = { id: "1", tenant: "a", nested: { value: null }, tags: ["x", "y"], _etag: "readonly", _rid: "readonly", _ts: 4 };
  const f = await createFixture(t);

  // when
  const actual = await f.create({ document, partition_key: fixtureIdentity.partition_key });

  // then
  assert.equal(actual.status, "succeeded");
  assert.deepEqual(f.sdk.calls.find((call) => call.operation === "create").body, { id: "1", tenant: "a", nested: { value: null }, tags: ["x", "y"] });
  assert.equal(f.sdk.calls.find((call) => call.operation === "create").options.disableAutomaticIdGeneration, true);
  assert.equal(f.sdk.calls.filter((call) => call.operation === "create").length, 1);
});

for (const component of [{ type: "string", value: "a" }, { type: "number", value: 7.5 }, { type: "boolean", value: false }, { type: "null" }, { type: "undefined" }]) {
  test(`전체 partition key의 ${component.type} 타입은 강제 변환 없이 SDK에 전달된다`, async (t) => {
    // given
    const identity = { id: "1", partition_key: [component] };
    const document = { id: "1", ...(component.type === "undefined" ? {} : { tenant: component.type === "null" ? null : component.value }), _etag: "etag-1" };
    const f = await createFixture(t, { identity, document });
    const expected = component.type === "undefined" ? [{}] : [component.type === "null" ? null : component.value];

    // when
    const actual = await f.read();

    // then
    assert.equal(actual.status, "succeeded");
    assert.deepEqual(f.sdk.calls.find((call) => call.operation === "read").partitionKey, expected);
    assert.deepEqual(actual.data.identity, identity);
  });
}

test("계층형 partition key는 메타데이터 순서대로 모두 전달된다", async (t) => {
  // given
  const identity = { id: "1", partition_key: [{ type: "string", value: "a" }, { type: "number", value: 7 }, { type: "null" }] };
  const f = await createFixture(t, { identity, partitionPaths: ["/tenant", "/nested/region", "/optional"], document: { id: "1", tenant: "a", nested: { region: 7 }, optional: null, _etag: "etag-1" } });

  // when
  const actual = await f.replace({ identity, if_match: "etag-1", document: f.document });

  // then
  assert.equal(actual.status, "succeeded");
  assert.deepEqual(f.sdk.calls.find((call) => call.operation === "replace").partitionKey, ["a", 7, null]);
  assert.deepEqual(f.sdk.calls.find((call) => call.operation === "replace").options.accessCondition, { type: "IfMatch", condition: "etag-1" });
});

test("부분 partition key의 point read는 SDK 문서 요청 전에 거부한다", async (t) => {
  // given
  const f = await createFixture(t, { partitionPaths: ["/tenant", "/region"] });

  // when
  const actual = await f.read();

  // then
  assert.equal(actual.error.code, "INVALID_ARGUMENT");
  assert.equal(f.sdk.calls.some((call) => call.operation === "read"), false);
});

for (const [label, document] of [["명시적 id가 없는", { tenant: "a" }], ["안전한 정수 범위를 벗어난", { id: "1", tenant: "a", nested: { n: 9007199254740992 } }]]) {
  test(`${label} 문서 생성은 SDK 호출 없이 거부한다`, async (t) => {
    // given
    const f = await createFixture(t);

    // when
    const actual = await f.create({ document, partition_key: fixtureIdentity.partition_key });

    // then
    assert.equal(actual.error.code, "INVALID_ARGUMENT");
    assert.equal(actual.error.outcome, "not_started");
    assert.equal(f.sdk.calls.length, 0);
  });
}

test("읽기 전용 연결과 권한 플래그 누락은 문서 쓰기를 거부한다", async (t) => {
  // given
  const f = await createFixture(t);
  const input = { document: { id: "1", tenant: "a" }, partition_key: fixtureIdentity.partition_key };

  // when
  const actual = await f.create(input, { read_only: undefined });

  // then
  assert.equal(actual.error.code, "WRITE_NOT_ALLOWED");
  assert.equal(f.sdk.calls.length, 0);
});

for (const [label, change] of [["id", { id: "different" }], ["partition key", { tenant: "different" }]]) {
  test(`replace에서 ${label} 변경은 SDK 쓰기 전에 거부한다`, async (t) => {
    // given
    const f = await createFixture(t);
    const input = { identity: fixtureIdentity, if_match: "etag-1", document: { ...f.document, ...change } };

    // when
    const actual = await f.replace(input);

    // then
    assert.equal(actual.error.code, "PARTITION_KEY_IMMUTABLE");
    assert.equal(f.sdk.calls.some((call) => call.operation === "replace"), false);
  });
}

test("시스템 속성 변경은 원본 스냅샷과 비교하여 거부한다", async (t) => {
  // given
  const f = await createFixture(t);
  const input = { identity: fixtureIdentity, if_match: "etag-1", document: { ...f.document, _ts: 999 } };

  // when
  const actual = await f.replace(input);

  // then
  assert.equal(actual.error.code, "INVALID_ARGUMENT");
  assert.equal(f.sdk.calls.some((call) => call.operation === "replace"), false);
});

test("다른 partition의 같은 id는 선택한 전체 identity로 삭제한다", async (t) => {
  // given
  const identity = { id: "1", partition_key: [{ type: "string", value: "b" }] };
  const f = await createFixture(t, { identity, document: { id: "1", tenant: "b", _etag: "etag-b" } });

  // when
  const actual = await f.remove({ identity, if_match: "etag-b" });

  // then
  assert.equal(actual.status, "succeeded");
  assert.deepEqual(f.sdk.calls.find((call) => call.operation === "delete").partitionKey, ["b"]);
  assert.deepEqual(f.sdk.calls.find((call) => call.operation === "delete").options.accessCondition, { type: "IfMatch", condition: "etag-b" });
});

for (const [status, code, action] of [[409, "DOCUMENT_ALREADY_EXISTS", "create"], [412, "ETAG_CONFLICT", "replace"], [404, "DOCUMENT_NOT_FOUND", "read"]]) {
  test(`SDK ${status} 응답은 ${code}로 매핑하고 민감한 진단을 노출하지 않는다`, async (t) => {
    // given
    const sdkError = Object.assign(new Error("document-secret-canary"), { code: status, requestCharge: 2.5, headers: { Authorization: "token-canary" }, body: "body-canary", diagnostics: { secret: "diagnostics-canary", clientSideRequestStatistics: { retryDiagnostics: { failedAttempts: [1, 2] } } } });
    const f = await createFixture(t, { errors: { [action]: sdkError } });
    const input = action === "create" ? { document: { id: "1", tenant: "a" }, partition_key: fixtureIdentity.partition_key } : { identity: fixtureIdentity, if_match: "etag-1", document: f.document };

    // when
    const actual = await f[action](action === "read" ? {} : input);

    // then
    assert.equal(actual.error.code, code);
    assert.equal(actual.error.outcome, "not_applied");
    assert.equal(actual.error.retryable, false);
    assert.equal(actual.metrics.retry_count, 2);
    assert.equal(actual.metrics.request_charge, action === "replace" ? 7.5 : 4.5);
    assert.equal(JSON.stringify(actual).includes("canary"), false);
  });
}

test("데이터베이스의 404는 문서 부재로 잘못 표시하지 않는다", async (t) => {
  // given
  const f = await createFixture(t, { errors: { database: Object.assign(new Error("secret-canary"), { code: 404 }) } });

  // when
  const actual = await f.read();

  // then
  assert.equal(actual.error.code, "INVALID_ARGUMENT");
  assert.equal(actual.metrics.request_charge, null);
  assert.equal(JSON.stringify(actual).includes("canary"), false);
});

test("전송 이후 응답이 사라진 쓰기는 자동 재실행 없이 결과 불명으로 반환한다", async (t) => {
  // given
  const f = await createFixture(t, { errors: { create: Object.assign(new Error("network-secret-canary"), { code: "REQUEST_SEND_ERROR" }) } });

  // when
  const actual = await f.create({ document: { id: "1", tenant: "a" }, partition_key: fixtureIdentity.partition_key });

  // then
  assert.equal(actual.status, "outcome_unknown");
  assert.equal(actual.error.code, "OUTCOME_UNKNOWN");
  assert.equal(actual.error.retryable, false);
  assert.equal(actual.metrics.request_charge, null);
  assert.equal(f.sdk.calls.filter((call) => call.operation === "create").length, 1);
});

test("delete에서 IfMatch 누락은 SDK 호출 전에 거부한다", async (t) => {
  // given
  const f = await createFixture(t);

  // when
  const actual = await f.remove({ identity: fixtureIdentity });

  // then
  assert.equal(actual.error.code, "INVALID_ARGUMENT");
  assert.equal(f.sdk.calls.length, 0);
});
