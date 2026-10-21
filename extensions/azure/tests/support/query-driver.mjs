import { createCosmosRuntime, runStdio } from "../../dist/runtime/index.js";
import { querySdkFixture } from "./query-sdk.mjs";
import { abortable } from "./fixture.mjs";

const fixture = querySdkFixture(undefined, {
  pages: { "": { values: [1, 2], token: "backend-next" }, "backend-next": { values: [3, 4] } },
  async onRequest({ operation, request, respond }) {
    if (operation !== "query") return;
    const spec = JSON.parse(request.body);
    if (spec.query === "SELECT VALUE @slow") { process.send?.({ type: "started" }); return abortable(request.abortSignal); }
    if (spec.query === "SELECT VALUE @forbidden") return respond(request, 403, { code: "Forbidden", message: "query-sdk-body-secret-canary", documents: ["query-document-secret-canary"] });
    if (spec.query === "SELECT VALUE @throttle") return respond(request, 429, { code: "TooManyRequests", message: "query-sdk-throttle-secret-canary" }, { "x-ms-request-charge": "1.25", "x-ms-retry-after-ms": "1" });
    if (spec.query === "SELECT VALUE @network") throw Object.assign(new Error("query-sdk-network-secret-canary"), { code: "REQUEST_SEND_ERROR" });
  },
});
const runtime = createCosmosRuntime({ clients: fixture.clients, handlers: { query_page: fixture.handlers.query_page, execute_query: fixture.handlers.execute_query } });
try { await runStdio(process.stdin, process.stdout, runtime); }
catch { process.exitCode = 1; }
finally { if (process.connected) process.disconnect(); }
