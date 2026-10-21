import { runStdio, createCosmosRuntime } from "../../dist/runtime/index.js";
import { createFixture, abortable } from "./fixture.mjs";
import { sdkTransportFixture } from "./sdk-transport.mjs";

let fixture;
if (process.argv[2] === "sdk-logging") {
  fixture = sdkTransportFixture(undefined, { onRequest({ operation, request, respond }) {
    if (operation !== "create") return;
    const document = JSON.parse(request.body);
    if (document.id === "network") throw Object.assign(new Error("transport-logging-secret-canary"), { code: "REQUEST_SEND_ERROR" });
    return respond(request, document.id === "conflict" ? 409 : 429, { code: "SyntheticError", message: "response-logging-secret-canary", body: "document-logging-secret-canary" }, { "x-ms-request-charge": "1.5", "x-ms-retry-after-ms": "1" });
  } });
  fixture.runtime = createCosmosRuntime({ clients: fixture.clients });
} else {
  fixture = await createFixture(undefined, { hooks: {
    read({ id, options, response }) {
      if (id === "slow") { process.send?.({ type: "started", operation: "read" }); return abortable(options.abortSignal); }
      if (id === "redact") throw Object.assign(new Error("protocol-sdk-secret-canary"), { code: 403, body: "body-secret-canary", headers: { Authorization: "header-secret-canary" } });
      return response({ id: "1", tenant: "a", nested: { value: null }, _etag: "etag-1" }, 3);
    },
    create({ options }) { process.send?.({ type: "started", operation: "create" }); return abortable(options.abortSignal); },
  } });
}
try { await runStdio(process.stdin, process.stdout, fixture.runtime); }
catch { process.exitCode = 1; }
finally { process.send?.({ type: "closed", dispose_count: fixture.sdk?.calls.filter((call) => call.operation === "dispose").length ?? null }); if (process.connected) process.disconnect(); }
