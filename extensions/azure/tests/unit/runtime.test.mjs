import test from "node:test";
import assert from "node:assert/strict";
import { Writable, Readable, PassThrough } from "node:stream";
import { LineFramer, JsonLineWriter, MAX_FRAME_BYTES } from "../../dist/runtime/framing.js";
import { runStdio } from "../../dist/runtime/index.js";
import { createFixture, abortable, rpcRequest, fixtureIdentity } from "../support/fixture.mjs";

function readRequest(options = {}) { return rpcRequest("read_document", { database: "fixture", container: "items", identity: fixtureIdentity }, options); }
function capture() { const chunks = []; return { chunks, output: new Writable({ write(chunk, _, callback) { chunks.push(chunk.toString("utf8")); setImmediate(callback); } }) }; }

test("UTF-8 문자가 chunk 사이에서 나뉘어도 한 줄의 JSON을 보존한다", () => {
  // given
  const framer = new LineFramer();
  const buffer = Buffer.from('{"한글":"문서"}\r\n');
  framer.push(buffer.subarray(0, 4));

  // when
  const actual = framer.push(buffer.subarray(4));

  // then
  assert.deepEqual(actual, [{ kind: "json", text: '{"한글":"문서"}' }]);
});

test("16 MiB를 초과한 프레임을 버린 뒤 다음 줄을 정상적으로 읽는다", () => {
  // given
  const framer = new LineFramer();
  framer.push(Buffer.alloc(MAX_FRAME_BYTES, 97));

  // when
  const actual = framer.push(Buffer.from('a\n{}\n'));

  // then
  assert.deepEqual(actual, [{ kind: "error", code: "FRAME_TOO_LARGE" }, { kind: "json", text: "{}" }]);
});

test("잘못된 UTF-8은 대체 문자로 바꾸지 않고 거부한다", () => {
  // given
  const framer = new LineFramer();

  // when
  const actual = framer.push(Buffer.from([0x22, 0xc3, 0x28, 0x22, 0x0a]));

  // then
  assert.deepEqual(actual, [{ kind: "error", code: "INVALID_UTF8" }]);
});

test("newline 없는 마지막 chunk는 JSON 프레임으로 실행하지 않는다", () => {
  // given
  const framer = new LineFramer();
  framer.push('{}');

  // when
  const actual = framer.finish();

  // then
  assert.deepEqual(actual, [{ kind: "error", code: "INCOMPLETE_FRAME" }]);
});

test("동시 응답 writer는 backpressure를 기다리며 한 줄씩 기록한다", async () => {
  // given
  const recorded = [];
  const output = new Writable({ highWaterMark: 1, write(chunk, _, callback) { recorded.push(chunk.toString("utf8")); setTimeout(callback, 5); } });
  const writer = new JsonLineWriter(output);
  const responses = Array.from({ length: 20 }, (_, id) => ({ jsonrpc: "2.0", id, result: `한글-${id}` }));

  // when
  const actual = await Promise.all(responses.map((response) => writer.write(response)));

  // then
  assert.equal(actual.length, 20);
  assert.deepEqual(recorded.map((chunk) => JSON.parse(chunk)), responses);
  assert.equal(recorded.every((chunk) => chunk.endsWith("\n") && chunk.split("\n").length === 2), true);
  assert.equal(writer.queueLength, 0);
  assert.equal(writer.queuedBytes, 0);
});

test("출력 상한 오류도 원래 RPC 식별자를 보존한다", async () => {
  // given
  const f = capture();
  const writer = new JsonLineWriter(f.output, 1024);

  // when
  await writer.write({ jsonrpc: "2.0", id: "oversized", result: "a".repeat(2048) });

  // then
  assert.equal(JSON.parse(f.chunks.join("")).id, "oversized");
  assert.equal(JSON.parse(f.chunks.join("")).error.data.code, "RESOURCE_LIMIT");
  assert.equal(JSON.parse(f.chunks.join("")).error.data.outcome, "unknown");
});

test("같은 요청 id라도 연결별 취소 신호는 다른 읽기에 영향을 주지 않는다", async (t) => {
  // given
  let started;
  let startCount = 0;
  const ready = new Promise((resolve) => { started = resolve; });
  const f = await createFixture(t, { hooks: { read({ options, response }) { if (++startCount === 2) started(); return options.abortSignal.aborted ? abortable(options.abortSignal) : startCount === 1 ? abortable(options.abortSignal) : Promise.resolve(response({ id: "1", tenant: "a", _etag: "etag-1" }, 3)); } } });
  const pendingA = f.runtime.dispatch(readRequest({ id: "a", connection_id: "a", request_id: "same" }));
  const pendingB = f.runtime.dispatch(readRequest({ id: "b", connection_id: "b", request_id: "same" }));
  await ready;

  // when
  const actual = await f.runtime.dispatch({ jsonrpc: "2.0", id: "cancel", method: "cancel_request", params: { connection_id: "a", request_id: "same" } });

  // then
  assert.deepEqual(actual.result, { cancellation_requested: true });
  assert.equal((await pendingA).result.status, "cancelled");
  assert.equal((await pendingB).result.status, "succeeded");
});

test("전송된 쓰기의 취소 ACK와 결과 불명 상태를 구분한다", async (t) => {
  // given
  let started;
  const ready = new Promise((resolve) => { started = resolve; });
  const f = await createFixture(t, { hooks: { create({ options }) { started(); return abortable(options.abortSignal); } } });
  const pending = f.runtime.dispatch(rpcRequest("create_document", { database: "fixture", container: "items", document: { id: "1", tenant: "a" }, partition_key: fixtureIdentity.partition_key }, { request_id: "write" }));
  await ready;

  // when
  const actual = await f.runtime.dispatch({ jsonrpc: "2.0", id: "cancel", method: "cancel_request", params: { connection_id: "connection-a", request_id: "write" } });

  // then
  assert.deepEqual(actual.result, { cancellation_requested: true });
  assert.equal((await pending).result.status, "outcome_unknown");
  assert.equal((await pending).result.error.code, "OUTCOME_UNKNOWN");
  assert.equal(f.sdk.calls.filter((call) => call.operation === "create").length, 1);
});

test("요청 deadline은 해당 읽기만 중단하고 RU 미측정을 null로 표시한다", async (t) => {
  // given
  const f = await createFixture(t, { hooks: { read({ options }) { return abortable(options.abortSignal); } } });

  // when
  const actual = await f.runtime.dispatch(readRequest({ deadline_ms: 10 }));

  // then
  assert.equal(actual.result.error.code, "DEADLINE_EXCEEDED");
  assert.equal(actual.result.metrics.request_charge, null);
  assert.equal(actual.result.error.outcome, "not_applied");
});

test("진행 중인 같은 연결과 요청 id의 중복은 재실행하지 않는다", async (t) => {
  // given
  let started;
  const ready = new Promise((resolve) => { started = resolve; });
  const f = await createFixture(t, { hooks: { read({ options }) { started(); return abortable(options.abortSignal); } } });
  const request = readRequest({ deadline_ms: 30 });
  const pending = f.runtime.dispatch(request);
  await ready;

  // when
  const actual = await f.runtime.dispatch(request);

  // then
  assert.equal(actual.result.error.code, "INVALID_ARGUMENT");
  assert.equal((await pending).result.error.code, "DEADLINE_EXCEEDED");
  assert.equal(f.sdk.calls.filter((call) => call.operation === "read").length, 1);
});

test("대기열이 가득 차도 제어 취소는 큐를 우회한다", async (t) => {
  // given
  let started;
  const ready = new Promise((resolve) => { started = resolve; });
  const f = await createFixture(t, { runtime: { globalConcurrency: 1, connectionConcurrency: 1, queueLimit: 1 }, hooks: { read({ options }) { started(); return abortable(options.abortSignal); } } });
  const running = f.runtime.dispatch(readRequest({ id: "running", deadline_ms: 100 }));
  await ready;
  const queued = f.runtime.dispatch(readRequest({ id: "queued" }));
  const rejected = await f.runtime.dispatch(readRequest({ id: "rejected" }));

  // when
  const actual = await f.runtime.dispatch({ jsonrpc: "2.0", id: "cancel", method: "cancel_request", params: { connection_id: "connection-a", request_id: "queued" } });

  // then
  assert.equal(actual.result.cancellation_requested, true);
  assert.equal((await queued).result.error.outcome, "not_started");
  assert.equal(rejected.result.error.code, "RESOURCE_LIMIT");
  assert.equal(f.sdk.calls.filter((call) => call.operation === "read").length, 1);
  assert.equal((await running).result.error.code, "DEADLINE_EXCEEDED");
});

test("내부 wire의 임의 permission 플래그는 권한으로 해석하지 않는다", async (t) => {
  // given
  const f = await createFixture(t);
  const request = readRequest({ context: { approved: true, permissions: ["write"] } });

  // when
  const actual = await f.runtime.dispatch(request);

  // then
  assert.equal(actual.error.data.code, "INVALID_ARGUMENT");
  assert.equal(f.sdk.calls.length, 0);
});

test("SQL session handle을 문서 작업으로 전달하지 않는다", async (t) => {
  // given
  const f = await createFixture(t);

  // when
  const actual = await f.runtime.dispatch(readRequest({ context: { session_handle: "unsupported-session" } }));

  // then
  assert.equal(actual.result.error.code, "UNSUPPORTED_OPERATION");
  assert.equal(f.sdk.calls.length, 0);
});

test("알 수 없는 RPC와 malformed JSON은 원문을 stdout에 노출하지 않는다", async () => {
  // given
  const f = capture();
  const input = Readable.from(['{"secret":"runtime-secret-canary"\n', '{"jsonrpc":"2.0","id":1,"method":"raw.arbitrary"}\n']);

  // when
  await runStdio(input, f.output);

  // then
  assert.equal(f.chunks.join("").includes("runtime-secret-canary"), false);
  assert.equal(JSON.parse(f.chunks[0]).error.code, -32700);
  assert.equal(JSON.parse(f.chunks[1]).error.data.code, "UNSUPPORTED_OPERATION");
});

test("기본 동시 실행 상한은 프로세스 8개와 연결별 4개를 지킨다", async (t) => {
  // given
  let started;
  let count = 0;
  const ready = new Promise((resolve) => { started = resolve; });
  const f = await createFixture(t, { hooks: { read({ options }) { if (++count === 8) started(); return abortable(options.abortSignal); } } });
  const pending = ["a", "b"].flatMap((connection_id) => Array.from({ length: 5 }, (_, n) => f.runtime.dispatch(readRequest({ connection_id, id: `${connection_id}-${n}` }))));
  await ready;

  // when
  const actual = await f.runtime.close();

  // then
  assert.equal(actual, undefined);
  assert.equal(f.sdk.calls.filter((call) => call.operation === "read").length, 8);
  assert.equal((await pending[4]).result.error.outcome, "not_started");
  assert.equal((await pending[9]).result.error.outcome, "not_started");
  assert.equal((await pending[0]).result.status, "cancelled");
  assert.equal((await pending[5]).result.status, "cancelled");
});

test("stdout 종료는 pending 읽기를 즉시 중단하고 내부 종료를 한 번 실행한다", async (t) => {
  // given
  let started;
  const ready = new Promise((resolve) => { started = resolve; });
  const f = await createFixture(t, { hooks: { read({ options }) { started(); return abortable(options.abortSignal); } } });
  const input = new PassThrough();
  const output = new Writable({ write(_, __, callback) { callback(); } });
  const completion = runStdio(input, output, f.runtime).catch((error) => error);
  input.write(JSON.stringify(readRequest({ deadline_ms: 60000 })) + "\n");
  await ready;

  // when
  output.destroy(new Error("output-secret-canary"));

  // then
  assert.equal((await completion).code, "DRIVER_EXITED");
  assert.equal((await completion).message.includes("canary"), false);
  assert.equal(f.sdk.calls.filter((call) => call.operation === "dispose").length, 1);
});
