import { createRequire } from "node:module";
import { CosmosClient } from "@azure/cosmos";
import { createClientProvider } from "../../dist/connection/index.js";
import { createQueryHandlers } from "../../dist/query/index.js";
import { MetricsAccumulator } from "../../dist/runtime/index.js";
import { fixtureAuth, fixtureEndpoint, fixtureSettings } from "./fixture.mjs";
import { queryInput } from "./query-fixture.mjs";

const { createHttpHeaders } = createRequire(import.meta.resolve("@azure/cosmos"))("@azure/core-rest-pipeline");
export const databaseRid = "k6d9AA==";
export const containerRid = "k6d9AKfK7vM=";
export function queryPlan(queryInfo = {}) {
  return { partitionedQueryExecutionInfoVersion: 2, queryInfo: { distinctType: "None", top: null, offset: null, limit: null, orderBy: [], orderByExpressions: [], groupByExpressions: [], groupByAliasToAggregateType: {}, aggregates: [], hasSelectValue: true, ...queryInfo }, queryRanges: [{ min: "", max: "FF", isMinInclusive: true, isMaxInclusive: false }] };
}
export function querySdkFixture(test, options = {}) {
  const calls = [];
  const paths = options.paths ?? ["/tenant"];
  let sequence = 0;
  const respond = (request, status, body, headers = {}) => ({ request, status, bodyAsText: body === undefined ? "" : JSON.stringify(body), headers: createHttpHeaders({ "x-ms-request-charge": "1", "x-ms-activity-id": "00000000-0000-4000-8000-000000000002", ...headers }) });
  const httpClient = { async sendRequest(request) {
    const path = new URL(request.url).pathname;
    const operation = request.headers.get("x-ms-cosmos-is-query-plan-request") === "True" ? "plan" : path.endsWith("/pkranges") ? "ranges" : /\/docs(?:\/|$)/.test(path) ? request.method === "POST" ? "query" : "point" : /\/colls\//.test(path) ? "container" : /\/dbs\//.test(path) ? "database" : "account";
    const call = { operation, request, path };
    calls.push(call);
    if (options.onRequest) { const result = await options.onRequest({ ...call, calls, respond }); if (result !== undefined) return result; }
    if (operation === "database") return respond(request, 200, { id: "fixture", _rid: options.databaseRid ?? databaseRid });
    if (operation === "container") return respond(request, 200, { id: "items", _rid: options.containerRid ?? containerRid, partitionKey: { paths, kind: paths.length > 1 ? "MultiHash" : "Hash", version: paths.length > 1 ? 2 : 1 } });
    if (operation === "account") return respond(request, 200, { id: "fixture-account", writableLocations: [{ name: "West US", databaseAccountEndpoint: fixtureEndpoint }], readableLocations: [{ name: "West US", databaseAccountEndpoint: fixtureEndpoint }], enableMultipleWriteLocations: false });
    if (operation === "plan") return respond(request, 200, options.plan ?? queryPlan(), { "x-ms-request-charge": "0" });
    if (operation === "ranges") return respond(request, 200, { PartitionKeyRanges: options.ranges ?? [{ id: "0", minInclusive: "", maxExclusive: "FF" }], _rid: containerRid, _count: options.ranges?.length ?? 1 }, { "x-ms-request-charge": "0" });
    if (operation === "point") {
      const id = decodeURIComponent(path.split("/").at(-1));
      const document = options.documents?.[id];
      return respond(request, document ? 200 : 404, document ?? { code: "NotFound", message: "synthetic-query-point-canary" });
    }
    const token = request.headers.get("x-ms-continuation") ?? "";
    const page = options.pages?.[token];
    if (!page) throw new Error("CAPABILITY_UNAVAILABLE: no supplied HTTP query page");
    return respond(request, 200, { Documents: page.values, _rid: containerRid, _count: page.values.length }, { ...(page.token ? { "x-ms-continuation": page.token } : {}), "x-ms-request-charge": String(page.ru ?? 1) });
  } };
  const clients = createClientProvider({ httpClient, createClient: (settings) => new CosmosClient(settings) });
  test?.after(() => clients.dispose());
  const handlers = createQueryHandlers(clients);
  const context = (overrides = {}) => ({ connection_id: "sdk-query-connection", request_id: `sdk-query-${++sequence}`, deadline_ms: 3000, read_only: true, signal: new AbortController().signal, connection: fixtureSettings, auth: fixtureAuth, metrics: new MetricsAccumulator(), ...overrides });
  return { calls, clients, handlers, context,
    execute: (input = {}, overrides = {}) => handlers.execute({ ...queryInput, ...input }, context(overrides)),
    resume: (state, input = {}, overrides = {}) => handlers.continue({ ...queryInput, ...input }, state, context(overrides)),
    rpc: (input = {}, state, overrides = {}) => handlers.query_page({ query: { ...queryInput, ...input }, ...(state === undefined ? {} : { cursor_state: state }) }, context(overrides)),
  };
}
