import { createHash, randomUUID } from "node:crypto";
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import type { TransientAuthContext } from "../connection/index.js";
import { COSMOS_SCOPE, normalizeConnectionSettings } from "../connection/index.js";
import type { ResolvedContext, RpcRequest } from "./contracts.js";
import { DriverError } from "./errors.js";
import { jsonObject } from "./validation.js";

const legacyMethods = new Set(["ping", "test_connection", "get_databases", "get_tables", "get_columns", "get_connection_metadata", "get_foreign_keys", "get_indexes", "get_schemas", "execute_query"]);
const CLI_CLIENT_ID = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";
const COSMOS_AUDIENCES = new Set(["https://cosmos.azure.com", "https://cosmos.azure.com/", "a232010e-820c-4083-83bb-3ace5fc29d0b"]);
const hash = (value: string) => createHash("sha256").update(value).digest("hex");

export function resolveLegacyContext(request: RpcRequest): ResolvedContext {
  const value = request.params ?? {};
  if (Object.hasOwn(value, "input") || !legacyMethods.has(request.method)) throw new DriverError("AUTH_REQUIRED", "This operation requires the shared-service host.");
  if (!jsonObject(value.params)) throw new DriverError("INVALID_ARGUMENT", "ConnectionParams are required.");
  const params = value.params;
  if (params.driver !== "cosmos-nosql" || params.connection_uri) throw new DriverError("INVALID_ARGUMENT", "A Cosmos NoSQL connection is required.");
  const connection = normalizeConnectionSettings(params);
  if (connection.auth_mode !== "account_key" && !(connection.auth_mode === "entra_user" && connection.auth_source === "azure_cli")) throw new DriverError("CAPABILITY_UNAVAILABLE", "Tabularis 0.26 compatibility supports account keys or Azure CLI user authentication.");
  const password = typeof params.password === "string" ? params.password : "";
  const identity = hash(JSON.stringify([connection, params.username ?? null, hash(password)]));
  const savedId = typeof params.connection_id === "string" && params.connection_id.length <= 128 && params.connection_id.length > 0 ? params.connection_id : `legacy-${identity}`;
  const context: ResolvedContext = { connection_id: savedId, request_id: request.id === undefined ? randomUUID() : `rpc:${typeof request.id}:${request.id}`, deadline_ms: 30_000, read_only: true, native_v1: false, connection };
  if (connection.auth_mode === "account_key" && password) context.auth = { kind: "account_key", account_key: password, identity };
  if (!context.auth && connection.auth_mode === "account_key" && !["get_connection_metadata", "get_foreign_keys", "get_indexes", "get_schemas"].includes(request.method)) throw new DriverError("AUTH_REQUIRED", "Enter the Cosmos account key in the connection Password field.");
  return context;
}

export function azureCliCommand(platform: string, searchPath: string, present = existsSync): { executable: string; prefix: string[] } {
  const paths = platform === "win32" ? path.win32 : path.posix;
  const separator = platform === "win32" ? ";" : ":";
  for (const directory of searchPath.split(separator).filter(value => paths.isAbsolute(value))) {
    const executable = paths.join(directory, platform === "win32" ? "az.exe" : "az");
    if (present(executable)) return { executable, prefix: [] };
    if (platform === "win32" && present(paths.join(directory, "az.cmd"))) {
      const python = paths.resolve(directory, "..", "python.exe");
      if (present(python)) return { executable: python, prefix: ["-IBm", "azure.cli"] };
    }
  }
  throw new DriverError("AUTH_REQUIRED", "Make the official Azure CLI available on the plugin process PATH before using Azure CLI authentication.");
}

export function runAzureCli(args: string[]): Promise<string> {
  let command: ReturnType<typeof azureCliCommand>;
  try { command = azureCliCommand(process.platform, process.env.PATH ?? ""); }
  catch (error) { return Promise.reject(error); }
  return new Promise((resolve, reject) => {
    execFile(command.executable, [...command.prefix, ...args], { timeout: 20_000, maxBuffer: 128 * 1024, windowsHide: true, encoding: "utf8", shell: false }, (error, stdout) => {
      if (error) reject(new DriverError("AUTH_REQUIRED", "Azure CLI authentication failed. Install Azure CLI, run az login, and select the intended tenant."));
      else resolve(stdout);
    });
  });
}

export async function resolveCliAuth(context: ResolvedContext, run = runAzureCli, now = Date.now): Promise<TransientAuthContext> {
  const settings = context.connection!;
  if (settings.auth_mode !== "entra_user" || settings.auth_source !== "azure_cli" || settings.client_id !== CLI_CLIENT_ID || !settings.tenant_id || !/^[0-9a-f-]{36}$/i.test(settings.tenant_id)) throw new DriverError("AUTH_REQUIRED", "Azure CLI user authentication requires the intended tenant ID.");
  let token: Record<string, unknown>;
  let claims: Record<string, unknown>;
  try {
    const response: unknown = JSON.parse(await run(["account", "get-access-token", "--scope", COSMOS_SCOPE, "--tenant", settings.tenant_id, "--output", "json", "--only-show-errors"]));
    if (!jsonObject(response) || typeof response.accessToken !== "string") throw new Error();
    token = response;
    const parts = response.accessToken.split(".");
    if (parts.length !== 3) throw new Error();
    const decoded: unknown = JSON.parse(Buffer.from(parts[1]!, "base64url").toString("utf8"));
    if (!jsonObject(decoded)) throw new Error();
    claims = decoded;
  } catch (error) {
    if (error instanceof DriverError) throw error;
    throw new DriverError("AUTH_REQUIRED", "Azure CLI returned an invalid Cosmos authentication response.");
  }
  const expires = typeof claims.exp === "number" ? claims.exp * 1000 : NaN;
  const tenant = claims.tid;
  const client = claims.azp ?? claims.appid;
  const principal = claims.oid;
  if (!COSMOS_AUDIENCES.has(String(claims.aud)) || tenant !== settings.tenant_id || client !== CLI_CLIENT_ID || typeof principal !== "string" || !principal || !Number.isSafeInteger(expires) || expires <= now() + 330_000) throw new DriverError("AUTH_REQUIRED", "The Azure CLI token identity, audience, or expiry does not match this connection. Run az login again.");
  return { kind: "entra_token", access_token: token.accessToken as string, expires_at_ms: expires, tenant_id: tenant, client_id: CLI_CLIENT_ID, principal_id: principal, scope: COSMOS_SCOPE, identity: hash(JSON.stringify([tenant, client, principal, COSMOS_SCOPE])) };
}
