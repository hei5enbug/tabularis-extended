import { createClientProvider } from "../../dist/connection/index.js";
import { createQueryHandlers } from "../../dist/query/index.js";
import { createCosmosRuntime, MetricsAccumulator } from "../../dist/runtime/index.js";
import { fixtureAuth, fixtureSettings } from "./fixture.mjs";

export const queryInput = Object.freeze({ language: "cosmos_sql", database: "fixture", container: "items", text: "SELECT VALUE c.value FROM c", parameters: [], page_size: 2 });
export function queryFixture(test, options = {}) {
  const calls = [];
  const meta = { database_rid: "db-rid", container_rid: "container-rid", paths: options.paths ?? ["/tenant"], ...options.metadata };
  const metadataCharge = options.metadataCharge ?? 0.5;
  const pages = options.pages ?? [{ resources: [1, null], hasMore: false, ru: 1 }];
  const client = { dispose() { calls.push({ method: "dispose" }); }, database(database) { return {
    async read(requestOptions) { calls.push({ method: "database", database, options: requestOptions }); return { resource: { id: database, _rid: meta.database_rid }, requestCharge: metadataCharge }; },
    container(container) { return {
      async read(requestOptions) { calls.push({ method: "container", container, options: requestOptions }); return { resource: { id: container, _rid: meta.container_rid, partitionKey: { paths: meta.paths, kind: meta.paths.length > 1 ? "MultiHash" : "Hash", version: meta.paths.length > 1 ? 2 : 1 } }, requestCharge: metadataCharge }; },
      items: { query(spec, requestOptions) {
        calls.push({ method: "query", spec: structuredClone(spec), options: requestOptions });
        const supplied = requestOptions.continuationToken === undefined ? pages : options.resumePages?.[requestOptions.continuationToken];
        if (!supplied) throw new Error("CAPABILITY_UNAVAILABLE: no supplied resume response");
        let index = 0;
        let more = true;
        return { hasMoreResults() { return more; }, async fetchNext() {
          calls.push({ method: "fetchNext", options: requestOptions, index });
          const next = supplied[index++];
          if (!next) throw new Error("CAPABILITY_UNAVAILABLE: no supplied query response");
          if (next.action) await next.action(requestOptions, spec);
          if (next.error) throw next.error;
          more = next.hasMore;
          return { resources: structuredClone(next.resources), continuationToken: next.token, requestCharge: next.ru ?? 1, diagnostics: { clientSideRequestStatistics: { retryDiagnostics: { failedAttempts: next.retries ?? [] } } } };
        } };
      } },
      item(id, partitionKey) { return { async read(requestOptions) {
        calls.push({ method: "point", id, partitionKey, options: requestOptions });
        if (options.pointRead) return await options.pointRead({ id, partitionKey, options: requestOptions });
        const document = options.documents?.[id];
        if (!document) throw Object.assign(new Error("synthetic-point-canary"), { code: 404, requestCharge: 1 });
        return { resource: structuredClone(document), requestCharge: 1 };
      } }; },
    }; },
  }; } };
  const clients = createClientProvider({ createClient: () => client });
  test?.after(() => clients.dispose());
  const handlers = createQueryHandlers(clients);
  let sequence = 0;
  const context = (overrides = {}) => ({ connection_id: "query-connection", request_id: `query-${++sequence}`, deadline_ms: 1000, read_only: true, signal: new AbortController().signal, auth: fixtureAuth, connection: fixtureSettings, metrics: new MetricsAccumulator(), ...overrides });
  return { calls, meta, clients, handlers, context, runtime: createCosmosRuntime({ clients, handlers: { query_page: handlers.query_page, execute_query: handlers.execute_query } }),
    execute: (input = {}, overrides = {}) => handlers.execute({ ...queryInput, ...input }, context(overrides)),
    resume: (state, input = {}, overrides = {}) => handlers.continue({ ...queryInput, ...input }, state, context(overrides)),
    rpc: (input = {}, state, overrides = {}) => handlers.query_page({ query: { ...queryInput, ...input }, ...(state === undefined ? {} : { cursor_state: state }) }, context(overrides)),
  };
}
