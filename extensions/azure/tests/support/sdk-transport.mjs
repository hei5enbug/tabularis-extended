import { createRequire } from "node:module";
import { CosmosClient } from "@azure/cosmos";
import { createClientProvider } from "../../dist/connection/index.js";
import { createDocumentHandlers } from "../../dist/documents/index.js";
import { MetricsAccumulator } from "../../dist/runtime/index.js";
import { fixtureAuth, fixtureEndpoint, fixtureSettings, fixtureIdentity } from "./fixture.mjs";

const { createHttpHeaders } = createRequire(import.meta.resolve("@azure/cosmos"))("@azure/core-rest-pipeline");
export function sdkTransportFixture(test, options = {}) {
  const calls = [];
  const clientOptions = [];
  const paths = options.partitionPaths ?? ["/tenant"];
  const document = options.document ?? { id: "1", tenant: "a", _etag: "etag-1", _rid: "item-rid", nested: { missing: null } };
  function respond(request, status, body, headers = {}) { return { request, status, bodyAsText: body === undefined ? "" : JSON.stringify(body), headers: createHttpHeaders({ "x-ms-request-charge": "1", "x-ms-activity-id": "00000000-0000-4000-8000-000000000001", ...headers }) }; }
  const httpClient = { async sendRequest(request) {
      const path = new URL(request.url).pathname;
      const operation = /\/docs(?:\/|$)/.test(path) ? request.method === "POST" ? "create" : request.method === "PUT" ? "replace" : request.method === "DELETE" ? "delete" : "read" : /\/colls\//.test(path) ? "container" : /\/dbs\//.test(path) ? "database" : "account";
      const call = { operation, request, path };
      calls.push(call);
      if (options.onRequest) {
        const supplied = await options.onRequest({ ...call, calls, respond });
        if (supplied !== undefined) return supplied;
      }
      if (operation === "database") return respond(request, 200, { id: "fixture", _rid: "db-rid" });
      if (operation === "container") return respond(request, 200, { id: "items", _rid: "container-rid", partitionKey: { paths, kind: paths.length > 1 ? "MultiHash" : "Hash", version: paths.length > 1 ? 2 : 1 } });
      if (operation === "account") return respond(request, 200, { id: "fixture-account", writableLocations: [{ name: "West US", databaseAccountEndpoint: fixtureEndpoint }], readableLocations: [{ name: "West US", databaseAccountEndpoint: fixtureEndpoint }], enableMultipleWriteLocations: false });
      return respond(request, operation === "create" ? 201 : operation === "delete" ? 204 : 200, operation === "delete" ? undefined : document, { "x-ms-request-charge": "3" });
  } };
  const clients = createClientProvider({ httpClient, createClient: (settings) => { clientOptions.push(settings); return new CosmosClient(settings); } });
  test?.after(() => clients.dispose());
  const handlers = createDocumentHandlers(clients);
  const context = { connection_id: "sdk-connection", request_id: "sdk-request", deadline_ms: 1000, read_only: false, signal: new AbortController().signal, auth: fixtureAuth, connection: fixtureSettings, metrics: new MetricsAccumulator(), ...options.context };
  const scope = { database: "fixture", container: "items" };
  return { calls, clientOptions, clients, context, document,
    create: (input = { document: { id: "1", tenant: "a" }, partition_key: fixtureIdentity.partition_key }) => handlers.create_document({ ...scope, ...input }, context),
    read: (input = { identity: fixtureIdentity }) => handlers.read_document({ ...scope, ...input }, context),
    replace: (input = { identity: fixtureIdentity, if_match: "etag-1", document }) => handlers.replace_document({ ...scope, ...input }, context),
    remove: (input = { identity: fixtureIdentity, if_match: "etag-1" }) => handlers.delete_document({ ...scope, ...input }, context),
  };
}
