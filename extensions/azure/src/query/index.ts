import { randomUUID } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import type { Container, FeedResponse } from "@azure/cosmos";
import type { JsonValue } from "@tabularis/service-contracts/types";
import type { ClientProvider } from "../connection/index.js";
import { connectionContext } from "../connection/index.js";
import { metadataFromResource, type ContainerMetadata } from "../connection/metadata.js";
import { extractPartitionComponents, fullPartitionKey, validateDocumentId } from "../documents/identity.js";
import type { RpcHandler, RpcRequestContext } from "../runtime/contracts.js";
import { assertRequestActive, DriverError, mapSdkError, safeError, serviceResponse } from "../runtime/errors.js";
import { MetricsAccumulator, withRequestMetrics } from "../runtime/metrics.js";
import { jsonObject, onlyKeys } from "../runtime/validation.js";
import { authBinding, endpointIdentity, feedOptions, jsonHash, normalizeQuery, originalOptions, validateCursor } from "./binding.js";
import { COSMOS_SDK_VERSION, CURSOR_TTL_MS, type NormalizedQuery, type QueryCursor, type QueryDriverOutput, type QueryHandlers, type QueryInput } from "./contracts.js";
import { boundResumeInput, boundRpcOutput, boundRows, output, publicPage } from "./page.js";
export * from "./contracts.js";

function checkBudget(context: RpcRequestContext, budget: number, before = false): void {
  assertRequestActive(context);
  const charge = context.metrics!.knownCharge;
  if (charge > budget || (before && charge >= budget)) throw new DriverError("RESOURCE_LIMIT", "The query request-charge budget was exhausted.", "not_applied");
}
async function measured<T>(action: () => Promise<T>, context: RpcRequestContext, budget: number, resource?: "database" | "container" | "document"): Promise<T> {
  checkBudget(context, budget, true);
  let response;
  try { response = await action(); }
  catch (error) {
    if (!(error instanceof DriverError)) context.metrics!.add(error);
    throw mapSdkError(error, context, { ...(resource === "document" ? { document: true } : resource ? { resource } : {}) });
  }
  context.metrics!.add(response);
  checkBudget(context, budget);
  return response;
}
async function metadata(clients: ClientProvider, query: NormalizedQuery, context: RpcRequestContext, budget: number): Promise<{ container: Container; metadata: ContainerMetadata }> {
  checkBudget(context, budget, true);
  const client = await clients.get(connectionContext(context));
  const database = client.database(query.database);
  const db = await measured(() => database.read({ abortSignal: context.signal }), context, budget, "database");
  if (!db.resource?._rid) throw new DriverError("INVALID_ARGUMENT", "The database was not found.", "not_applied");
  const container = database.container(query.container);
  const result = await measured(() => container.read({ abortSignal: context.signal }), context, budget, "container");
  if (!result.resource) throw new DriverError("INVALID_ARGUMENT", "The container was not found.", "not_applied");
  return { container, metadata: metadataFromResource(query.database, query.container, db.resource._rid, result.resource) };
}
async function documentRows(values: JsonValue[], container: Container, paths: string[], context: RpcRequestContext, budget: number): Promise<JsonValue[]> {
  const rows: JsonValue[] = new Array(values.length);
  let next = 0;
  let failure: unknown;
  const controller = new AbortController();
  const tracked = { ...context, signal: AbortSignal.any([context.signal, controller.signal]) };
  async function worker(): Promise<void> {
    while (failure === undefined && next < values.length) {
      const index = next++;
      const candidate = values[index];
      try {
        if (!jsonObject(candidate) || typeof candidate._etag !== "string" || !candidate._etag) throw new DriverError("INVALID_ARGUMENT", "Documents mode requires complete document identities and ETags.", "not_applied");
        validateDocumentId(candidate.id);
        const components = extractPartitionComponents(candidate, paths);
        const identity = { id: candidate.id, partition_key: components };
        const result = await measured(() => container.item(candidate.id as string, fullPartitionKey(components, paths)).read({ abortSignal: tracked.signal }), tracked, budget, "document");
        if (!jsonObject(result.resource)) throw new DriverError("DOCUMENT_NOT_FOUND", "The document was not found.", "not_applied");
        if (result.resource._etag !== candidate._etag) throw new DriverError("ETAG_CONFLICT", "The query document ETag has changed.", "not_applied");
        if (!isDeepStrictEqual(candidate, result.resource)) throw new DriverError("INVALID_ARGUMENT", "Documents mode requires the complete unchanged document, including system properties.", "not_applied");
        rows[index] = { document: candidate, identity, etag: candidate._etag };
      } catch (error) {
        if (failure === undefined) {
          failure = error instanceof DriverError && error.code === "DOCUMENT_NOT_FOUND" ? new DriverError("INVALID_ARGUMENT", "The query candidate cannot be verified as a complete document.", "not_applied") : error;
          controller.abort(failure);
        }
      }
    }
  }
  await Promise.all([worker(), worker()]);
  if (failure !== undefined) throw failure;
  return rows;
}
export function createQueryHandlers(clients: ClientProvider): QueryHandlers {
  async function execute(query: QueryInput, context: RpcRequestContext, previous?: QueryCursor): Promise<QueryDriverOutput> {
    const now = context.now ?? Date.now;
    if (!Number.isSafeInteger(context.deadline_ms) || context.deadline_ms < 1 || context.deadline_ms > 120_000) throw new DriverError("INVALID_ARGUMENT", "The query deadline is invalid.");
    const deadline = Math.min(context.deadline_at_ms ?? Infinity, now() + context.deadline_ms);
    const controller = new AbortController();
    const tracked: RpcRequestContext = { ...context, now, deadline_at_ms: deadline, signal: AbortSignal.any([context.signal, controller.signal]), metrics: context.metrics ?? new MetricsAccumulator(now(), now) };
    const timer = setTimeout(() => controller.abort(new DriverError("DEADLINE_EXCEEDED", "The query deadline was exceeded.", "not_applied")), Math.max(0, deadline - now()));
    let budget = 100;
    try {
      const result = await withRequestMetrics(tracked, async () => {
        assertRequestActive(tracked);
        if (tracked.session_handle !== undefined) throw new DriverError("UNSUPPORTED_OPERATION", "Cosmos SQL sessions are unsupported.");
        const input = normalizeQuery(query);
        if (previous !== undefined) boundResumeInput(input, previous, tracked);
        const state = previous === undefined ? undefined : validateCursor(previous, input, tracked);
        budget = input.ru_budget ?? 100;
        const target = await metadata(clients, input, tracked, budget);
        const options = originalOptions(input, target.metadata);
        if (state && (state.native_state.database_rid !== target.metadata.database_rid || state.native_state.container_rid !== target.metadata.container_rid || !isDeepStrictEqual(state.native_state.original_options, options))) throw new DriverError("INVALID_PAGE_TOKEN", "The query cursor container or options have changed.", "not_applied");
        const mode = input.result_mode ?? "json_values";
        const revision = state?.native_state.revision ?? randomUUID();
        const cursor = (tail: JsonValue[], token: string): QueryCursor => {
          const next: QueryCursor = {
          binding: { sdk_version: COSMOS_SDK_VERSION, container_rid: target.metadata.container_rid, endpoint_identity: endpointIdentity(tracked), partition_hash: jsonHash(input.partition_key ?? null) },
          native_state: { version: 1, sdk_version: COSMOS_SDK_VERSION, connection_id: tracked.connection_id, auth_identity: tracked.auth!.identity, auth_binding: authBinding(tracked), endpoint_identity: endpointIdentity(tracked), database_rid: target.metadata.database_rid, container_rid: target.metadata.container_rid, partition_hash: jsonHash(input.partition_key ?? null), query_hash: jsonHash(input), query: input, original_options: options, continuation_token: token, revision },
          tail, columns: [], page_size: input.page_size, expires_at_ms: state?.expires_at_ms ?? now() + CURSOR_TTL_MS,
          };
          boundResumeInput(input, next, tracked);
          return next;
        };
        if (state?.tail.length) {
          const page = publicPage(state.tail, mode, input.page_size, tracked);
          return output(page.rows, page.rows, mode, revision, tracked, cursor(page.tail, state.native_state.continuation_token));
        }
        const iterator = target.container.items.query<JsonValue>({ query: input.text, parameters: input.parameters }, feedOptions(options, tracked, state?.native_state.continuation_token));
        const completed: JsonValue[] = [];
        let materializing = false;
        while (iterator.hasMoreResults()) {
          const response: FeedResponse<JsonValue> = await measured(() => iterator.fetchNext(), tracked, budget);
          if (!Array.isArray(response.resources)) throw new DriverError("DRIVER_EXITED", "The query returned an invalid page.", "not_applied");
          boundRows(response.resources);
          const rows = mode === "documents" ? await documentRows(response.resources, target.container, target.metadata.partition_key_paths, tracked, budget) : response.resources;
          boundRows(rows);
          const more = iterator.hasMoreResults();
          if (!rows.length && more) continue;
          const token = response.continuationToken;
          if (!materializing && rows.length && more && typeof token === "string" && token.length) {
            const page = publicPage(rows, mode, input.page_size, tracked);
            const next = cursor(page.tail, token);
            boundRows([next as unknown as JsonValue]);
            return output(page.rows, page.rows, mode, revision, tracked, next);
          }
          if (rows.length && more) materializing = true;
          completed.push(...rows);
          boundRows(completed);
        }
        checkBudget(tracked, budget);
        const page = publicPage(completed, mode, input.page_size, tracked);
        return output(page.rows, completed, mode, revision, tracked, null);
      });
      tracked.metrics!.sealTransport();
      await tracked.metrics!.drainTransport(tracked.signal);
      checkBudget(tracked, budget);
      result.metrics = tracked.metrics!.finalize();
      return result;
    } catch (error) {
      tracked.metrics!.sealTransport();
      try { await tracked.metrics!.drainTransport(tracked.signal); } catch {}
      tracked.metrics!.finalize();
      throw mapSdkError(error, tracked);
    }
    finally { clearTimeout(timer); }
  }
  const queryPage: RpcHandler = async (params, context) => {
    const tracked = { ...context, metrics: context.metrics ?? new MetricsAccumulator() };
    try {
      onlyKeys(params, ["query", "cursor_state"]);
      if (!jsonObject(params.query)) throw new DriverError("INVALID_ARGUMENT", "An internal query operation input is required.");
      const result = await execute(params.query as unknown as QueryInput, tracked, params.cursor_state as unknown as QueryCursor | undefined);
      boundRpcOutput(result, tracked);
      assertRequestActive(tracked);
      return serviceResponse(tracked, result as unknown as JsonValue);
    } catch (error) { return serviceResponse(tracked, null, safeError(mapSdkError(error, tracked))); }
  };
  const legacy: RpcHandler = async (params, context) => {
    onlyKeys(params, ["params", "driver_context", "query", "parameters", "database", "container", "limit", "page", "schema", "partition_key", "ru_budget"]);
    if ((params.page ?? 1) !== 1) throw new DriverError("UNSUPPORTED_OPERATION", "Additional Cosmos pages require the query_page cursor API.");
    if (params.schema !== undefined && params.schema !== null) throw new DriverError("UNSUPPORTED_OPERATION", "Cosmos schemas are unsupported.");
    const database = params.database ?? context.connection?.database;
    const container = params.container ?? context.connection?.container;
    if (typeof params.query !== "string" || typeof database !== "string" || typeof container !== "string") throw new DriverError("INVALID_ARGUMENT", "Set the database and default container in the connection, then enter a Cosmos SQL query such as SELECT * FROM c.");
    const query = { language: "cosmos_sql", database, container, text: params.query, parameters: params.parameters ?? [], page_size: params.limit ?? 100, ...(params.partition_key === undefined ? {} : { partition_key: params.partition_key }), ...(params.ru_budget === undefined ? {} : { ru_budget: params.ru_budget }) } as unknown as QueryInput;
    const result = await execute(query, context);
    const values = result.data.kind === "json_values" ? result.data.values : [];
    const hasMore = result.cursor !== null || result.snapshot.sets[0]!.rows.length > values.length;
    const compat = params.driver_context === undefined;
    return { columns: ["_document"], rows: values.map((value) => [value]), affected_rows: 0, truncated: compat && hasMore, pagination: { page: 1, page_size: query.page_size!, total_rows: result.cursor === null ? result.snapshot.sets[0]!.rows.length : null, has_more: !compat && hasMore } };
  };
  return { query_page: queryPage, execute_query: legacy, execute: (query, context) => execute(query, context), continue: (query, state, context) => execute(query, context, state) };
}
