import test from "node:test";
import assert from "node:assert/strict";
import { MAX_FRAME_BYTES } from "../../dist/runtime/index.js";
import { validateResponse } from "@tabularis/service-contracts";
import { rpcRequest, fixtureIdentity } from "../support/fixture.mjs";
import { frame, protocolScenario } from "../support/protocol.mjs";

const readInput = { database: "fixture", container: "items", identity: fixtureIdentity };
function slow(options) { return rpcRequest("read_document", { ...readInput, identity: { ...fixtureIdentity, id: "slow" } }, options); }

test("독립 stdio child는 분할 한글 프레임에 공용 문서 응답을 반환한다", async () => {
  // given
  const value = Buffer.from(frame(rpcRequest("read_document", readInput, { id: "한글" })));
  const start = value.indexOf(Buffer.from("한글")) + 1;

  // when
  const actual = await protocolScenario({ chunks: [value.subarray(0, start), value.subarray(start)] });

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.stderr, "");
  assert.equal(actual.responses.length, 1);
  assert.equal(actual.responses[0].id, "한글");
  assert.equal(actual.responses[0].result.status, "succeeded");
  assert.equal(validateResponse(actual.responses[0].result).valid, true);
});

test("독립 stdio child는 대형 프레임과 잘못된 UTF-8 뒤에도 제어 요청을 읽는다", async () => {
  // given
  const chunks = [Buffer.alloc(MAX_FRAME_BYTES + 1, 97), Buffer.from("\n"), Buffer.from([0xc3, 0x28, 0x0a]), frame({ jsonrpc: "2.0", id: "init", method: "initialize", params: { settings: {} } })];

  // when
  const actual = await protocolScenario({ chunks, expectedResponses: 3 });

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.responses.length, 3);
  assert.equal(actual.responses[0].error.code, -32600);
  assert.equal(actual.responses[1].error.code, -32700);
  assert.equal(actual.responses[2].result.service_capabilities.documents_v1, true);
  assert.equal(actual.stderr, "");
});

test("부모 stdin이 닫히면 전송된 쓰기를 중단하고 클라이언트를 한 번 정리한다", async () => {
  // given
  const requests = [rpcRequest("create_document", { database: "fixture", container: "items", document: { id: "1", tenant: "a" }, partition_key: fixtureIdentity.partition_key }, { id: "write", deadline_ms: 60000 })];

  // when
  const actual = await protocolScenario({ requests, eofAfterStarted: true });

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.responses.length, 1);
  assert.equal(actual.responses[0].result.status, "outcome_unknown");
  assert.equal(actual.responses[0].result.error.code, "OUTCOME_UNKNOWN");
  assert.equal(actual.messages.find((message) => message.type === "closed").dispose_count, 1);
  assert.equal(actual.stderr, "");
});

test("서로 다른 연결의 같은 request id에서 한 읽기만 취소한다", async () => {
  // given
  const requests = [slow({ id: "a", connection_id: "a", request_id: "same", deadline_ms: 500 }), slow({ id: "b", connection_id: "b", request_id: "same", deadline_ms: 500 }), rpcRequest("read_document", readInput, { id: "fast" })];
  const cancel = { jsonrpc: "2.0", id: "cancel", method: "cancel_request", params: { connection_id: "a", request_id: "same" } };

  // when
  const actual = await protocolScenario({ requests, cancel, startedCount: 2 });

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.responses.find((response) => response.id === "cancel").result.cancellation_requested, true);
  assert.equal(actual.responses.find((response) => response.id === "a").result.status, "cancelled");
  assert.equal(actual.responses.find((response) => response.id === "b").result.error.code, "DEADLINE_EXCEEDED");
  assert.equal(actual.responses.find((response) => response.id === "fast").result.status, "succeeded");
  assert.equal(actual.responses.findIndex((response) => response.id === "cancel") < actual.responses.findIndex((response) => response.id === "b"), true);
  assert.equal(actual.stderr, "");
});

test("stdio 쓰기의 전송 후 취소는 성공이나 확정 취소로 표시하지 않는다", async () => {
  // given
  const requests = [rpcRequest("create_document", { database: "fixture", container: "items", document: { id: "1", tenant: "a" }, partition_key: fixtureIdentity.partition_key }, { id: "write" })];
  const cancel = { jsonrpc: "2.0", id: "cancel", method: "cancel_request", params: { connection_id: "connection-a", request_id: "write" } };

  // when
  const actual = await protocolScenario({ requests, cancel });

  // then
  assert.equal(actual.code, 0);
  assert.deepEqual(actual.responses.find((response) => response.id === "cancel").result, { cancellation_requested: true });
  assert.equal(actual.responses.find((response) => response.id === "write").result.status, "outcome_unknown");
  assert.equal(actual.responses.find((response) => response.id === "write").result.error.code, "OUTCOME_UNKNOWN");
  assert.equal(actual.stderr, "");
});

test("실제 SDK 오류와 verbose 로그 설정에서도 응답 원문과 인증 정보를 노출하지 않는다", async () => {
  // given
  const requests = ["conflict", "rate", "network"].map((id) => rpcRequest("create_document", { database: "fixture", container: "items", document: { id, tenant: "a" }, partition_key: fixtureIdentity.partition_key }, { id }));

  // when
  const actual = await protocolScenario({ requests, mode: "sdk-logging", logLevel: "verbose" });

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.responses.length, 3);
  assert.equal(actual.responses.find((response) => response.id === "conflict").result.error.code, "DOCUMENT_ALREADY_EXISTS");
  assert.equal(actual.responses.find((response) => response.id === "rate").result.error.code, "RATE_LIMITED");
  assert.equal(actual.responses.find((response) => response.id === "network").result.error.code, "OUTCOME_UNKNOWN");
  assert.equal(actual.stdout.includes("canary"), false);
  assert.equal(actual.stderr, "");
});

test("stdio 알 수 없는 메서드와 SDK 실패는 고정 오류만 반환한다", async () => {
  // given
  const requests = [rpcRequest("read_document", { ...readInput, identity: { ...fixtureIdentity, id: "redact" } }), { jsonrpc: "2.0", id: "arbitrary", method: "sdk.raw.method", params: { token: "request-secret-canary" } }];

  // when
  const actual = await protocolScenario({ requests });

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.responses.find((response) => response.id === "rpc").result.error.code, "PERMISSION_DENIED");
  assert.equal(actual.responses.find((response) => response.id === "arbitrary").error.data.code, "UNSUPPORTED_OPERATION");
  assert.equal(actual.stdout.includes("canary"), false);
  assert.equal(actual.stderr, "");
});
