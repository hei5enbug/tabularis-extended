import test from "node:test";
import assert from "node:assert/strict";
import { authBinding, jsonHash } from "../../dist/query/binding.js";
import { COSMOS_SCOPE } from "../../dist/connection/index.js";
import { fixtureAuth, fixtureSettings, rpcRequest } from "../support/fixture.mjs";
import { queryFixture, queryInput } from "../support/query-fixture.mjs";
import { invalidateRequest } from "../support/x0b-fixture.mjs";

test("직접 legacy context의 auth hash는 generation 없는 기존 값 그대로 유지한다", (t) => {
  // given
  const f = queryFixture(t);

  // when
  const actual = authBinding(f.context());

  // then
  assert.equal(actual, jsonHash({ kind: fixtureAuth.kind, identity: fixtureAuth.identity }));
});

test("동일 principal의 갱신 토큰도 logout 이전 native cursor를 재개하지 못한다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [1, 2], hasMore: true, token: "private-sdk-token" }], resumePages: { "private-sdk-token": [{ resources: [3], hasMore: false }] } });
  const auth = { kind: "entra_token", identity: "same-principal", tenant_id: "tenant", client_id: "client", principal_id: "oid", scope: COSMOS_SCOPE, access_token: "synthetic-token-secret-canary", expires_at_ms: Date.now() + 900000 };
  const params = { driver: "cosmos-nosql", extra: { ...fixtureSettings, auth_mode: "entra_user", tenant_id: "tenant", client_id: "client" } };
  const request = (id, input, token = auth) => rpcRequest("query_page", input, { id, connection_id: "cursor-connection", read_only: true, auth: token, params });
  const initial = await f.runtime.dispatch(request("first", { query: queryInput }));
  const cursor = initial.result.data.cursor;
  const network_before = f.calls.filter((call) => call.method !== "dispose").length;
  async function scenario() {
    const invalidated = await f.runtime.dispatch(invalidateRequest("cursor-connection"));
    const resumed = await f.runtime.dispatch(request("resume", { query: queryInput, cursor_state: cursor }, { ...auth, access_token: "refreshed-token-secret-canary", expires_at_ms: auth.expires_at_ms + 1000 }));
    const resume_network = f.calls.filter((call) => call.method !== "dispose").length - network_before;
    const fresh = await f.runtime.dispatch(request("fresh", { query: queryInput }));
    return { invalidated, resumed, fresh, resume_network };
  }

  // when
  const actual = await scenario();

  // then
  assert.equal(initial.result.status, "succeeded");
  assert.equal(actual.invalidated.result.status, "succeeded");
  assert.equal(actual.resumed.result.error.code, "INVALID_PAGE_TOKEN");
  assert.equal(actual.resume_network, 0);
  assert.equal(actual.fresh.result.status, "succeeded");
  assert.notEqual(actual.fresh.result.data.cursor.native_state.auth_binding, cursor.native_state.auth_binding);
  assert.equal(JSON.stringify(actual.resumed).includes("canary"), false);
});
