import type { AuthMode, JsonObject } from "@tabularis/service-contracts/types";
import { DriverError } from "../runtime/errors.js";

export interface ConnectionSettings { endpoint: string; database?: string; container?: string; auth_mode: AuthMode; auth_source?: "oauth" | "azure_cli"; tenant_id?: string; client_id?: string; credential_ref?: string }
const keys = new Set(["endpoint", "database", "container", "auth_mode", "auth_source", "tenant_id", "client_id", "credential_ref"]);
function record(value: unknown): Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {}; }
function text(value: unknown): string | undefined { return typeof value === "string" && value.length > 0 ? value : undefined; }
export function validateEndpoint(value: string): string {
  if (/^mongodb(?:\+srv)?:/i.test(value) || /\.mongo\.cosmos\.azure\.com/i.test(value)) throw new DriverError("UNSUPPORTED_OPERATION", "Cosmos MongoDB endpoints are unsupported.");
  let endpoint: URL;
  try { endpoint = new URL(value); } catch { throw new DriverError("INVALID_ARGUMENT", "A valid Cosmos HTTPS endpoint is required."); }
  if (endpoint.protocol !== "https:" || endpoint.username || endpoint.password || endpoint.search || endpoint.hash || (endpoint.port && endpoint.port !== "443") || endpoint.pathname !== "/" || !/^[a-z0-9][a-z0-9-]*\.documents\.azure\.com$/.test(endpoint.hostname)) {
    throw new DriverError("INVALID_ARGUMENT", "An Azure Public Cloud Cosmos NoSQL HTTPS endpoint is required.");
  }
  return endpoint.origin;
}
export function normalizeConnectionSettings(value: JsonObject): ConnectionSettings {
  for (const key of ["approved", "permissions", "caller", "driver_context", "input"]) if (Object.hasOwn(value, key)) throw new DriverError("INVALID_ARGUMENT", "Caller policy claims are not connection settings.");
  if (value.extra !== undefined && (typeof value.extra !== "object" || value.extra === null || Array.isArray(value.extra))) throw new DriverError("INVALID_ARGUMENT", "Cosmos connection extra settings must be an object.");
  const extra = record(value.extra);
  for (const key of Object.keys(extra)) if (!keys.has(key)) throw new DriverError("INVALID_ARGUMENT", "Unknown Cosmos connection setting.");
  for (const key of ["key", "account_key", "access_token", "client_secret", "refresh_token", "rejectUnauthorized", "disable_tls_validation"]) {
    if (key in value || key in extra) throw new DriverError("INVALID_ARGUMENT", "Credentials and TLS overrides are not connection settings.");
  }
  const endpoint = text(extra.endpoint ?? value.endpoint ?? value.host);
  const mode = extra.auth_mode ?? value.auth_mode;
  if (!endpoint || (mode !== "account_key" && mode !== "entra_user" && mode !== "entra_service_principal")) throw new DriverError("INVALID_ARGUMENT", "Endpoint and supported auth_mode are required.");
  const settings: ConnectionSettings = { endpoint: validateEndpoint(endpoint), auth_mode: mode };
  const source = extra.auth_source ?? value.auth_source;
  if (source !== undefined) {
    if ((source !== "oauth" && source !== "azure_cli") || (source === "azure_cli" && (mode !== "entra_user" || (extra.client_id ?? value.client_id) !== "04b07795-8ddb-461a-bbee-02f9e1bf7b46"))) throw new DriverError("INVALID_ARGUMENT", "The authentication source is unsupported.");
    settings.auth_source = source;
  }
  const selected = extra.database ?? value.database;
  const database = text(Array.isArray(selected) ? selected[0] : selected);
  if (database) settings.database = database;
  const container = text(extra.container);
  if (container) settings.container = container;
  for (const key of ["tenant_id", "client_id", "credential_ref"] as const) { const field = text(extra[key] ?? value[key]); if (field) settings[key] = field; }
  if (mode !== "account_key" && (!settings.tenant_id || !settings.client_id)) throw new DriverError("INVALID_ARGUMENT", "Tenant and client IDs are required for Entra authentication.");
  return settings;
}
