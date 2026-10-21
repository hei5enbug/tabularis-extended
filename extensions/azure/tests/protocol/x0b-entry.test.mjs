import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

function runEntry(input) {
  return new Promise((resolve, reject) => {
    const driver = fileURLToPath(new URL("../../dist/index.js", import.meta.url));
    const guard = new URL("../support/x0b-network-guard.mjs", import.meta.url).href;
    const child = spawn(process.execPath, ["--import", guard, driver], { stdio: ["pipe", "pipe", "pipe", "ipc"], env: { PATH: process.env.PATH, TMPDIR: process.env.TMPDIR, AZURE_LOG_LEVEL: "verbose" } });
    const messages = [];
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => { child.kill(); reject(new Error("Synthetic production child timed out.")); }, 5000);
    child.on("message", (message) => messages.push(message));
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", reject);
    child.on("close", (code) => { clearTimeout(timer); resolve({ code, stdout, stderr, messages, responses: stdout.trim() ? stdout.trim().split("\n").map((line) => JSON.parse(line)) : [] }); });
    child.stdin.end(input);
  });
}
function frame(id, params, method = "initialize") { return JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n"; }

test("production child는 SDK 네트워크 없이 실제 capability를 협상하고 명시 종료한다", async () => {
  // given
  const input = frame("legacy", { settings: {} }) + frame("native", { settings: {}, service_protocol: 1 }) + frame("wrong", { service_protocol: "1" }) + frame("shutdown", {}, "shutdown");

  // when
  const actual = await runEntry(input);

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.stderr, "");
  assert.equal(actual.responses.length, 4);
  assert.deepEqual(actual.responses.find((response) => response.id === "legacy").result.service_capabilities, { protocol_version: 1, documents_v1: true, query_page_v1: true, cancel_v1: true, sessions_v1: false });
  assert.deepEqual(actual.responses.find((response) => response.id === "native").result.service_capabilities, { protocol_version: 1, documents_v1: true, query_page_v1: true, cancel_v1: true, sessions_v1: false, service_protocol: 1 });
  assert.equal(actual.responses.find((response) => response.id === "wrong").error.data.code, "PROTOCOL_MISMATCH");
  assert.equal(actual.responses.find((response) => response.id === "shutdown").result, null);
  assert.deepEqual(actual.messages, [{ type: "network_attempts", count: 0 }]);
});

test("production child는 손상된 입력과 삭제 연결 logout 뒤 EOF에서 원문을 노출하지 않는다", async () => {
  // given
  const invalidate = { jsonrpc: "2.0", id: "logout", method: "service_invalidate_auth", params: { params: { driver: "cosmos-nosql" }, driver_context: { protocol_version: 1, connection_id: "deleted", request_id: "logout", deadline_ms: 1000, read_only: true }, input: {} } };
  const input = '{"malformed":"production-secret-canary"\n' + frame("init", { service_protocol: 1 }) + JSON.stringify(invalidate) + "\n";

  // when
  const actual = await runEntry(input);

  // then
  assert.equal(actual.code, 0);
  assert.equal(actual.stderr, "");
  assert.equal(actual.responses.length, 3);
  assert.equal(actual.responses[0].error.code, -32700);
  assert.equal(actual.responses.find((response) => response.id === "logout").result.status, "succeeded");
  assert.deepEqual(actual.responses.find((response) => response.id === "logout").result.data, { invalidated: true });
  assert.equal(actual.stdout.includes("canary"), false);
  assert.deepEqual(actual.messages, [{ type: "network_attempts", count: 0 }]);
});
