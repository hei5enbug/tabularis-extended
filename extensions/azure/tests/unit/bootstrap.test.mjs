import test from "node:test";
import assert from "node:assert/strict";
import { dispatchRpc } from "../../dist/runtime/index.js";
import { fixtureFile, validateRequest, validateResponse } from "@tabularis/service-contracts";
import { readFileSync } from "node:fs";

for (const fixture of JSON.parse(readFileSync(fixtureFile, "utf8"))) {
  test(fixture.name, () => {
    // given
    const validator = fixture.schema === "request" ? validateRequest : validateResponse;

    // when
    const actual = validator(fixture.value);

    // then
    assert.equal(actual.valid, fixture.valid);
  });
}
test("구현되지 않은 고정 RPC는 기능 부재를 명시한다", () => {
  // given
  const request = { jsonrpc: "2.0", id: 1, method: "create_document", params: {} };

  // when
  const actual = dispatchRpc(request);

  // then
  assert.equal(actual.error.data.code, "CAPABILITY_UNAVAILABLE");
  assert.equal(actual.error.data.outcome, "not_started");
  assert.equal(actual.id, 1);
});
test("알 수 없는 RPC는 외부 드라이버로 전달하지 않고 거부한다", () => {
  // given
  const request = { jsonrpc: "2.0", id: "unknown", method: "arbitrary.raw" };

  // when
  const actual = dispatchRpc(request);

  // then
  assert.equal(actual.error.code, -32601);
  assert.equal(actual.error.data.code, "UNSUPPORTED_OPERATION");
});
