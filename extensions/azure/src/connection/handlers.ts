import type { QueryIterator, Resource, ContainerDefinition, DatabaseDefinition } from "@azure/cosmos";
import type { JsonObject, JsonValue } from "@tabularis/service-contracts/types";
import type { RpcHandler, RpcRequestContext } from "../runtime/contracts.js";
import { assertRequestActive, DriverError, mapSdkError } from "../runtime/errors.js";
import { jsonObject, onlyKeys } from "../runtime/validation.js";
import { connectionContext, type ClientProvider } from "./index.js";
import { getContainerMetadata } from "./metadata.js";
import { validateEndpoint } from "./settings.js";

export interface ConnectionHandlers { initialize: RpcHandler; ping: RpcHandler; shutdown: RpcHandler; test_connection: RpcHandler; get_databases: RpcHandler; get_tables: RpcHandler; get_columns: RpcHandler; get_foreign_keys: RpcHandler; get_indexes: RpcHandler; get_schemas: RpcHandler; get_connection_metadata: RpcHandler }
export const legacyConnectionMetadata = {
  capabilities: { schemas: false, views: false, materialized_views: false, routines: false, triggers: false, user_management: false, routine_management: false, alter_primary_key: false, alter_column: false, create_foreign_keys: false, manage_tables: false, explain: false, readonly: false },
  data_types: [{ name: "JSON", category: "json", requires_length: false, requires_precision: false, default_length: null }], type_mappings: { json: "JSON" },
} satisfies JsonObject;
function boundedName(value: unknown): string {
  if (typeof value !== "string" || !value || /[\\/#?\u0000-\u001f\u007f]/.test(value) || Buffer.byteLength(value, "utf8") > 255) throw new DriverError("INVALID_ARGUMENT", "A valid database or container name is required.");
  return value;
}
function databaseName(value: JsonObject, context: RpcRequestContext): string { return boundedName(value.database ?? context.connection?.database); }
async function collect<T>(iterator: QueryIterator<T>, context: RpcRequestContext): Promise<T[]> {
  const rows: T[] = [];
  let bytes = 0;
  let pages = 0;
  while (iterator.hasMoreResults()) {
    assertRequestActive(context);
    if (++pages > 1000) throw new DriverError("RESOURCE_LIMIT", "Metadata discovery exceeded its page limit.", "not_applied");
    const response = await iterator.fetchNext();
    context.metrics?.add(response);
    assertRequestActive(context);
    for (const row of response.resources) {
      bytes += Buffer.byteLength(JSON.stringify(row), "utf8");
      if (rows.length >= 1000 || bytes > 2 * 1024 * 1024) throw new DriverError("RESOURCE_LIMIT", "Metadata discovery exceeded its size limit.", "not_applied");
      rows.push(row);
    }
  }
  return rows;
}
function columns(): JsonValue[] {
  return [
    { name: "_document", data_type: "JSON", is_pk: false, is_nullable: false, is_auto_increment: false, is_generated: false },
    { name: "id", data_type: "string", is_pk: true, is_nullable: false, is_auto_increment: false, is_generated: true },
    { name: "_partition_key", data_type: "JSON", is_pk: true, is_nullable: false, is_auto_increment: false, is_generated: true },
    { name: "_etag", data_type: "string", is_pk: false, is_nullable: false, is_auto_increment: false, is_generated: true },
  ];
}
export function createConnectionHandlers(clients: ClientProvider, options: { query_page_v1?: boolean } = {}): ConnectionHandlers {
  function boundary(action: RpcHandler): RpcHandler {
    return async (value, context) => {
      try {
        if (context.session_handle !== undefined) throw new DriverError("UNSUPPORTED_OPERATION", "Cosmos SQL sessions are unsupported.");
        return await action(value, context);
      } catch (error) { if (!(error instanceof DriverError)) context.metrics?.add(error); throw mapSdkError(error, context); }
    };
  }
  const testConnection: RpcHandler = boundary(async (value, context) => {
    if (Object.hasOwn(value, "params")) onlyKeys(value, ["params", "driver_context"]);
    const client = await clients.get(connectionContext(context));
    if (context.connection?.database) {
      const response = await client.database(boundedName(context.connection.database)).read({ abortSignal: context.signal });
      context.metrics?.add(response);
      if (!response.resource?._rid) throw new DriverError("INVALID_ARGUMENT", "The database was not found.", "not_applied");
    } else await collect(client.databases.readAll({ maxItemCount: 100, abortSignal: context.signal }), context);
    assertRequestActive(context);
    return null;
  });
  return {
    initialize: async (value) => {
      onlyKeys(value, ["settings", "service_protocol"]);
      if (Object.hasOwn(value, "service_protocol") && value.service_protocol !== 1) throw new DriverError("PROTOCOL_MISMATCH", "The requested service protocol is unsupported.");
      if (value.settings !== undefined && !jsonObject(value.settings)) throw new DriverError("INVALID_ARGUMENT", "Plugin settings must be an object.");
      if (jsonObject(value.settings)) {
        onlyKeys(value.settings, ["endpoint", "database", "container", "auth_mode", "auth_source", "tenant_id", "client_id", "credential_ref"]);
        for (const [key, field] of Object.entries(value.settings)) {
          if (typeof field !== "string" || !field) throw new DriverError("INVALID_ARGUMENT", "Plugin connection defaults must be nonempty strings.");
          if (key === "auth_source" && !["oauth", "azure_cli"].includes(field)) throw new DriverError("INVALID_ARGUMENT", "The authentication source is unsupported.");
          if (key === "endpoint") validateEndpoint(field);
          if (key === "auth_mode" && !["account_key", "entra_user", "entra_service_principal"].includes(field)) throw new DriverError("INVALID_ARGUMENT", "The authentication mode is unsupported.");
        }
      }
      return { service_capabilities: { protocol_version: 1, documents_v1: true, query_page_v1: options.query_page_v1 === true, cancel_v1: true, sessions_v1: false, ...(Object.hasOwn(value, "service_protocol") ? { service_protocol: 1 } : {}) } };
    },
    ping: testConnection,
    shutdown: async (value) => { onlyKeys(value, []); return null; },
    test_connection: testConnection,
    get_databases: boundary(async (value, context) => {
      if (Object.hasOwn(value, "params")) onlyKeys(value, ["params", "driver_context"]);
      const client = await clients.get(connectionContext(context));
      const resources = await collect<DatabaseDefinition & Resource>(client.databases.readAll({ maxItemCount: 100, abortSignal: context.signal }), context);
      return resources.map((resource) => boundedName(resource.id));
    }),
    get_tables: boundary(async (value, context) => {
      if (Object.hasOwn(value, "params")) onlyKeys(value, ["params", "driver_context", "schema"]);
      if (value.schema !== undefined && value.schema !== null) throw new DriverError("UNSUPPORTED_OPERATION", "Cosmos schemas are unsupported.");
      const client = await clients.get(connectionContext(context));
      const database = databaseName(value, context);
      const response = await client.database(database).read({ abortSignal: context.signal });
      context.metrics?.add(response);
      if (!response.resource?._rid) throw new DriverError("INVALID_ARGUMENT", "The database was not found.", "not_applied");
      const resources = await collect<ContainerDefinition & Resource>(client.database(database).containers.readAll({ maxItemCount: 100, abortSignal: context.signal }), context);
      return resources.map((resource) => ({ name: boundedName(resource.id), comment: null }));
    }),
    get_columns: boundary(async (value, context) => {
      if (Object.hasOwn(value, "params")) onlyKeys(value, ["params", "driver_context", "schema", "table"]);
      const table = jsonObject(value.table) ? value.table : { database: context.connection?.database ?? null, schema: value.schema ?? null, table: value.table ?? null };
      if (table.schema !== null && table.schema !== undefined) throw new DriverError("UNSUPPORTED_OPERATION", "Cosmos schemas are unsupported.");
      const { metadata } = await getContainerMetadata(clients, context, databaseName(table, context), boundedName(table.table));
      return context.native_v1 ? { columns: columns(), partition_key_paths: metadata.partition_key_paths, partition_key_kind: metadata.partition_key_kind, partition_key_version: metadata.partition_key_version, system_key: metadata.system_key } : columns();
    }),
    get_connection_metadata: boundary(async (value, context) => {
      onlyKeys(value, ["params", "driver_context"]);
      if (value.driver_context !== undefined || context.native_v1) await clients.get(connectionContext(context));
      return { ...legacyConnectionMetadata, capabilities: { ...legacyConnectionMetadata.capabilities, readonly: context.read_only !== false } };
    }),
    get_foreign_keys: async (value) => { onlyKeys(value, ["params", "driver_context", "schema", "table"]); return []; },
    get_indexes: async (value) => { onlyKeys(value, ["params", "driver_context", "schema", "table"]); return []; },
    get_schemas: async (value) => { onlyKeys(value, ["params", "driver_context"]); return []; },
  };
}
