import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fixtureFile, requestSchema, operationNames, inputSchema, validateRequest, validateResponse, unavailableResponse } from "../dist/index.js";
const fixtures = JSON.parse(readFileSync(fixtureFile, "utf8"));
for (const fixture of fixtures) {
  test(fixture.name, () => {
    // given
    const validator = fixture.schema === "request" ? validateRequest : validateResponse;
    const value = structuredClone(fixture.value);

    // when
    const actual = validator(value);

    // then
    assert.equal(actual.valid, fixture.valid);
    assert.deepEqual(value, fixture.value);
  });
}
test("operation 목록과 모든 입력 스키마가 같은 계약을 사용한다", () => {
  // given
  const expected = requestSchema.properties.operation.enum;

  // when
  const actual = operationNames.map((operation) => [operation, inputSchema(operation)]);

  // then
  assert.deepEqual(actual.map(([name]) => name), expected);
  assert.ok(actual.every(([, schema]) => schema.additionalProperties === false));
});
test("연결되지 않은 핸들러는 명시적인 미지원 응답을 반환한다", () => {
  // given
  const request = fixtures.find((fixture) => fixture.valid && fixture.value.operation === "connection.list").value;

  // when
  const actual = unavailableResponse(request);

  // then
  assert.equal(actual.status, "failed");
  assert.equal(actual.error.code, "CAPABILITY_UNAVAILABLE");
  assert.equal(actual.error.outcome, "not_started");
  assert.equal(actual.data, null);
  assert.equal(validateResponse(actual).valid, true);
});
