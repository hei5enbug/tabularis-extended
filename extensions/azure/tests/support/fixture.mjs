export const fixtureDatabase = "fixture";
export const fixtureContainer = "items";
export const fixtureDocument = Object.freeze({ id: "1", tenant: "a", nested: { value: null }, tags: ["x"] });
export const fixtureIdentity = Object.freeze({ id: "1", partition_key: [{ type: "string", value: "a" }] });
export const fixturePartitionPaths = ["/tenant"];
export const fixtureHierarchicalPartitionPaths = ["/tenant", "/region"];

import { createClientProvider } from "../../dist/connection/index.js";
import { createDocumentHandlers } from "../../dist/documents/index.js";
import { createCosmosRuntime, MetricsAccumulator } from "../../dist/runtime/index.js";

export const fixtureEndpoint = "https://fixture-account.documents.azure.com";
export const fixtureAuth = Object.freeze({ kind: "account_key", account_key: Buffer.from("synthetic-sdk-test-key").toString("base64"), identity: "synthetic-principal" });
export const fixtureSettings = Object.freeze({ endpoint: fixtureEndpoint, database: fixtureDatabase, auth_mode: "account_key" });
export const fixtureConnectionParams = Object.freeze({ driver: "cosmos-nosql", database: fixtureDatabase, extra: { endpoint: fixtureEndpoint, database: fixtureDatabase, auth_mode: "account_key" } });
export function rpcRequest(method, input, { id = "rpc", connection_id = "connection-a", request_id = String(id), read_only = false, deadline_ms = 1000, auth = fixtureAuth, params = fixtureConnectionParams, context = {} } = {}) {
  return { jsonrpc: "2.0", id, method, params: { params, driver_context: { protocol_version: 1, connection_id, request_id, deadline_ms, read_only, auth, ...context }, input } };
}
export function abortable(signal) {
  return new Promise((_, reject) => {
    const aborted = () => reject(Object.assign(new Error("synthetic-abort-secret-canary"), { name: "AbortError" }));
    if (signal.aborted) aborted(); else signal.addEventListener("abort", aborted, { once: true });
  });
}
export async function createFixture(test, options = {}) {
  const calls = [];
  const paths = options.partitionPaths ?? fixturePartitionPaths;
  const document = structuredClone(options.document ?? { ...fixtureDocument, _etag: "etag-1", _rid: "item-rid", _self: "dbs/db-rid/colls/container-rid/docs/item-rid/", _attachments: "attachments/", _ts: 1 });
  function response(resource, requestCharge = 1) { return { resource, requestCharge, diagnostics: { clientSideRequestStatistics: { retryDiagnostics: { failedAttempts: [] } } } }; }
  async function invoke(operation, details, defaultResponse) {
    calls.push({ operation, ...details });
    const hook = options.hooks?.[operation];
    if (hook) return await hook({ ...details, calls, response });
    const error = options.errors?.[operation];
    if (error) throw error;
    return defaultResponse;
  }
  function feed(operation, resources) {
    let pending = true;
    return { hasMoreResults: () => pending, async fetchNext() { pending = false; return await invoke(operation, {}, { resources, requestCharge: 1 }); } };
  }
  const containerResource = { id: fixtureContainer, _rid: "container-rid", partitionKey: { paths, kind: paths.length > 1 ? "MultiHash" : "Hash", version: paths.length > 1 ? 2 : 1, systemKey: false }, ...options.containerMetadata };
  const client = {
    dispose() { calls.push({ operation: "dispose" }); },
    databases: { readAll: () => feed("databases", options.databases ?? [{ id: fixtureDatabase, _rid: "db-rid" }]) },
    database(database) {
      return {
        async read(requestOptions) { return await invoke("database", { database, options: requestOptions }, response({ id: database, _rid: "db-rid" })); },
        containers: { readAll: () => feed("containers", options.containers ?? [containerResource]) },
        container(container) {
          return {
            async read(requestOptions) { return await invoke("container", { database, container, options: requestOptions }, response(containerResource)); },
            items: { async create(body, requestOptions) { return await invoke("create", { database, container, body, options: requestOptions }, response(options.writeResource ?? document, 3)); } },
            item(id, partitionKey) {
              return {
                async read(requestOptions) { return await invoke("read", { database, container, id, partitionKey, options: requestOptions }, response(document, 3)); },
                async replace(body, requestOptions) { return await invoke("replace", { database, container, id, partitionKey, body, options: requestOptions }, response(options.writeResource ?? document, 3)); },
                async delete(requestOptions) { return await invoke("delete", { database, container, id, partitionKey, options: requestOptions }, response(undefined, 3)); },
              };
            },
          };
        },
      };
    },
  };
  const clients = createClientProvider({ createClient: () => client });
  test?.after(() => clients.dispose());
  const handlers = createDocumentHandlers(clients);
  let sequence = 0;
  const context = (overrides = {}) => ({ connection_id: "connection-a", request_id: `fixture-${++sequence}`, deadline_ms: 1000, read_only: false, signal: new AbortController().signal, auth: fixtureAuth, connection: fixtureSettings, metrics: new MetricsAccumulator(), ...overrides });
  const scope = { database: fixtureDatabase, container: fixtureContainer };
  return {
    sdk: { calls, client }, clients, handlers, context, document, identity: structuredClone(options.identity ?? fixtureIdentity),
    runtime: createCosmosRuntime({ clients, ...options.runtime }),
    read: (input = {}, overrides = {}) => handlers.read_document({ ...scope, identity: options.identity ?? fixtureIdentity, ...input }, context(overrides)),
    create: (input, overrides = {}) => handlers.create_document({ ...scope, ...input }, context(overrides)),
    replace: (input, overrides = {}) => handlers.replace_document({ ...scope, ...input }, context(overrides)),
    remove: (input, overrides = {}) => handlers.delete_document({ ...scope, ...input }, context(overrides)),
  };
}
