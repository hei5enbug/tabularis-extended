import test from "node:test";
import assert from "node:assert/strict";
import { queryProtocol } from "../support/query-protocol.mjs";
import { rpcRequest } from "../support/fixture.mjs";
import { queryInput } from "../support/query-fixture.mjs";
import { validateResponse } from "@tabularis/service-contracts";

test("실제 SDK의 private cursor는 새 stdio 프로세스에서도 같은 query를 이어 간다", async () => {
  // given
  const first = await queryProtocol({ requests: [rpcRequest("query_page", { query: queryInput }, { id: "first", connection_id: "connection-a", deadline_ms: 3000 })] });
  const state = first.responses[0].result.data.cursor;
  const next = rpcRequest("query_page", { query: queryInput, cursor_state: state }, { id: "next", connection_id: "connection-a", deadline_ms: 3000 });

  // when
  const actual = await queryProtocol({ requests: [next] });

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.stderr, "");
  assert.deepEqual([...first.responses[0].result.data.data.values, ...actual.responses[0].result.data.data.values], [1, 2, 3, 4]);
  assert.equal(actual.responses[0].result.data.cursor, null);
  assert.equal(validateResponse(actual.responses[0].result).valid, true);
  assert.equal(JSON.stringify(actual.responses[0].result.data.data).includes("continuation"), false);
});

test("stdio의 query cancel은 같은 request id를 쓰는 다른 연결과 격리된다", async () => {
  // given
  const target = rpcRequest("query_page", { query: { ...queryInput, text: "SELECT VALUE @slow" } }, { id: "slow", request_id: "same", connection_id: "a", deadline_ms: 3000 });
  const other = rpcRequest("query_page", { query: queryInput }, { id: "other", request_id: "same", connection_id: "b", deadline_ms: 3000 });
  const cancel = { jsonrpc: "2.0", id: "cancel", method: "cancel_request", params: { connection_id: "a", request_id: "same" } };

  // when
  const actual = await queryProtocol({ requests: [target, other], cancel });

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.stderr, "");
  assert.equal(actual.responses.find((response) => response.id === "slow").result.error.code, "CANCELLED");
  assert.equal(actual.responses.find((response) => response.id === "slow").result.error.outcome, "not_applied");
  assert.equal(actual.responses.find((response) => response.id === "other").result.status, "succeeded");
  assert.equal(actual.responses.find((response) => response.id === "cancel").result.cancellation_requested, true);
});

test("실제 SDK query 오류는 verbose logger에서도 body와 token을 stdout이나 stderr에 노출하지 않는다", async () => {
  // given
  const requests = ["forbidden", "throttle", "network"].map((kind) => rpcRequest("query_page", { query: { ...queryInput, text: `SELECT VALUE @${kind}` } }, { id: kind, connection_id: kind, deadline_ms: 3000 }));

  // when
  const actual = await queryProtocol({ requests, logLevel: "verbose" });

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.stderr, "");
  assert.equal(actual.stdout.includes("canary"), false);
  assert.equal(actual.responses.find((response) => response.id === "forbidden").result.error.code, "PERMISSION_DENIED");
  assert.equal(actual.responses.find((response) => response.id === "throttle").result.error.code, "RATE_LIMITED");
  assert.equal(actual.responses.find((response) => response.id === "throttle").result.metrics.retry_count, 3);
  assert.equal(actual.responses.every((response) => validateResponse(response.result).valid), true);
});
