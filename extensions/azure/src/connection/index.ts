import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { CosmosClient, CosmosDbDiagnosticLevel, type CosmosClientOptions } from "@azure/cosmos";
import { DriverError, assertRequestActive } from "../runtime/errors.js";
import type { RpcRequestContext } from "../runtime/contracts.js";
import { admitRequestTransport } from "../runtime/metrics.js";
import { validateEndpoint, type ConnectionSettings } from "./settings.js";
export { normalizeConnectionSettings, validateEndpoint } from "./settings.js";
export type { ConnectionSettings } from "./settings.js";

export const COSMOS_SCOPE = "https://cosmos.azure.com/.default";
export const MAX_CACHED_CLIENTS = 4096;
type SdkLogger = { AzureLogger: { log: (...args: unknown[]) => void }; setLogLevel: (level?: string) => void };
const sdkRequire = createRequire(import.meta.resolve("@azure/cosmos"));
const loggerPackage = sdkRequire.resolve("@azure/logger/package.json");
const loggerExports = JSON.parse(readFileSync(loggerPackage, "utf8")) as { exports: { ".": { import: { default: string } } } };
const sdkLoggers: SdkLogger[] = [sdkRequire("@azure/logger") as SdkLogger, await import(new URL(loggerExports.exports["."].import.default, pathToFileURL(loggerPackage)).href) as SdkLogger];
for (const logger of sdkLoggers) { logger.AzureLogger.log = () => {}; logger.setLogLevel(); }
export type TransientAuthContext =
  | { kind: "account_key"; account_key: string; identity: string }
  | { kind: "entra_token"; access_token: string; expires_at_ms: number; tenant_id: string; client_id: string; principal_id: string; scope: string; identity: string };
export interface ConnectionContext { connection_id: string; settings: ConnectionSettings; auth: TransientAuthContext; deadline_ms?: number }
export interface ClientProvider { get(context: ConnectionContext): Promise<CosmosClient>; invalidate(connection_id: string): Promise<void>; dispose?(): Promise<void> }
export type DriverClientOptions = CosmosClientOptions & { aadScope?: string };
export interface ClientProviderOptions { createClient?: (options: DriverClientOptions) => CosmosClient; httpClient?: CosmosClientOptions["httpClient"]; now?: () => number }
export class CapabilityUnavailableError extends DriverError {
  constructor() { super("CAPABILITY_UNAVAILABLE", "The Cosmos data-plane handler is unavailable."); }
}
export function connectionContext(context: RpcRequestContext): ConnectionContext {
  assertRequestActive(context);
  if (!context.connection || !context.auth) throw new DriverError("AUTH_REQUIRED", "A host-provided authentication context is required.");
  return { connection_id: context.connection_id, settings: context.connection, auth: context.auth, deadline_ms: context.deadline_ms };
}
export function validateAuth(context: ConnectionContext, now: number): void {
  const auth = context.auth;
  if (typeof auth.identity !== "string" || !auth.identity) throw new DriverError("AUTH_REQUIRED", "An authentication identity is required.");
  if (!["account_key", "entra_user", "entra_service_principal"].includes(context.settings.auth_mode)) throw new DriverError("INVALID_ARGUMENT", "A supported Cosmos authentication mode is required.");
  if (context.settings.auth_mode === "account_key") {
    if (auth.kind !== "account_key" || !auth.account_key) throw new DriverError("AUTH_REQUIRED", "A transient account key is required.");
  } else {
    if (auth.kind !== "entra_token" || !auth.access_token || !auth.principal_id || !auth.tenant_id || !auth.client_id || auth.tenant_id !== context.settings.tenant_id || auth.client_id !== context.settings.client_id || auth.scope !== COSMOS_SCOPE) {
      throw new DriverError("AUTH_REQUIRED", "The transient Entra context does not match this connection.");
    }
    if (!Number.isSafeInteger(auth.expires_at_ms) || auth.expires_at_ms <= now + (context.deadline_ms ?? 30_000) + 300_000) throw new DriverError("AUTH_EXPIRED", "The host must refresh the Entra token before this request.");
  }
}
export function createClientProvider(options: ClientProviderOptions = {}): ClientProvider {
  const now = options.now ?? Date.now;
  const factory = options.createClient ?? ((settings) => new CosmosClient(settings));
  const baseTransport = options.httpClient ?? (sdkRequire("@azure/core-rest-pipeline") as { createDefaultHttpClient: () => NonNullable<CosmosClientOptions["httpClient"]> }).createDefaultHttpClient();
  const transport: NonNullable<CosmosClientOptions["httpClient"]> = { async sendRequest(request) {
    const lease = admitRequestTransport();
    if (!lease.allowed) throw new DriverError("CANCELLED", "The request transport is closed.", "not_applied");
    let response;
    try { response = await baseTransport.sendRequest(request); return response; }
    finally { lease.complete(response?.headers.get("x-ms-request-charge")); }
  } };
  const entries = new Map<string, { fingerprint: string; client: CosmosClient; state: { auth: TransientAuthContext; active: boolean; settings: ConnectionSettings } }>();
  return {
    async get(context) {
      validateEndpoint(context.settings.endpoint);
      validateAuth(context, now());
      const auth = { ...context.auth };
      const authIdentity = auth.kind === "account_key" ? createHash("sha256").update(auth.account_key).digest("hex") : [auth.tenant_id, auth.client_id, auth.principal_id, auth.scope];
      const fingerprint = JSON.stringify([context.settings, auth.kind, auth.identity, authIdentity]);
      let entry = entries.get(context.connection_id);
      if (!entry && entries.size >= MAX_CACHED_CLIENTS) throw new DriverError("RESOURCE_LIMIT", "The driver client cache is full.");
      if (entry && entry.fingerprint !== fingerprint) { entry.state.active = false; entry.client.dispose(); entries.delete(context.connection_id); entry = undefined; }
      if (entry) {
        if (auth.kind !== "entra_token" || entry.state.auth.kind !== "entra_token" || auth.expires_at_ms >= entry.state.auth.expires_at_ms) entry.state.auth = auth;
        return entry.client;
      }
      const state = { auth, active: true, settings: { ...context.settings } };
      const clientOptions: DriverClientOptions = {
        endpoint: context.settings.endpoint, diagnosticLevel: CosmosDbDiagnosticLevel.info, httpClient: transport,
        connectionPolicy: { enableEndpointDiscovery: false, enableBackgroundEndpointRefreshing: false, enablePartitionLevelFailover: false, enablePartitionLevelCircuitBreaker: false, useMultipleWriteLocations: false, retryOptions: { maxRetryAttemptCount: 3, maxWaitTimeInSeconds: 10 } },
      };
      if (auth.kind === "account_key") clientOptions.key = auth.account_key;
      else {
        clientOptions.aadScope = COSMOS_SCOPE;
        clientOptions.aadCredentials = { async getToken(scopes) {
          const requested = typeof scopes === "string" ? [scopes] : scopes;
          if (requested.length !== 1 || requested[0] !== COSMOS_SCOPE) throw new DriverError("AUTH_REQUIRED", "The requested token scope is unsupported.");
          if (!state.active) throw new DriverError("AUTH_REQUIRED", "The authentication context has been invalidated.");
          const current = state.auth;
          validateAuth({ connection_id: context.connection_id, settings: state.settings, auth: current, deadline_ms: 0 }, now());
          if (current.kind !== "entra_token") throw new DriverError("AUTH_REQUIRED", "An Entra token is required.");
          return { token: current.access_token, expiresOnTimestamp: current.expires_at_ms };
        } };
      }
      const client = factory(clientOptions);
      entries.set(context.connection_id, { fingerprint, client, state });
      return client;
    },
    async invalidate(connection_id) { const entry = entries.get(connection_id); entries.delete(connection_id); if (entry) { entry.state.active = false; entry.client.dispose(); } },
    async dispose() { const cached = [...entries.values()]; entries.clear(); for (const entry of cached) { entry.state.active = false; entry.client.dispose(); } },
  };
}
