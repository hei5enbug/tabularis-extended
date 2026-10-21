import type { ServiceCapabilities, ServiceRequest, ServiceResponse, UsePluginServiceReturn } from "@tabularis/plugin-api";
import { vi } from "vitest";
import type { AuthStatus } from "./auth";

export const CONNECTION_ID = "saved-sqlserver-connection";
export const TENANT = "11111111-1111-1111-1111-111111111111";
export const CLIENT = "22222222-2222-2222-2222-222222222222";
export const JOB = "a-33333333-3333-3333-3333-333333333333";
export const EXTRA = { auth_mode: "entra_user", tenant_id: TENANT, client_id: CLIENT };

export function savedConnection(connectionId = CONNECTION_ID, extra = EXTRA) {
  return { connection_id: connectionId, driver: "sqlserver", name: "합성 연결", version: 1, allow_writes: false, settings: { extra } };
}

export function authData(state: AuthStatus["state"], job = JOB) {
  return { auth_job_id: job, state, user_action_required: state === "interaction_required", error_code: null };
}

export function response(requestId: string, data: ServiceResponse["data"], connectionId = CONNECTION_ID): ServiceResponse {
  return {
    protocol_version: 1, request_id: requestId, connection_id: connectionId, status: "succeeded", job_id: null, result_id: null,
    data, page: { next_token: null, has_more: false, resume_mode: "none" }, limits: { truncated: false, reasons: [] },
    metrics: { elapsed_ms: 1, request_charge: null, retry_count: 0 }, warnings: [], error: null,
  };
}

export function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((accept, deny) => { resolve = accept; reject = deny; });
  return { promise, resolve, reject };
}

export function fixture() {
  const state = {
    connections: [savedConnection()],
    begin: authData("pending") as ServiceResponse["data"],
    status: authData("authenticated") as ServiceResponse["data"],
    cancel: authData("cancelled") as ServiceResponse["data"],
    logout: { logged_out: true } as ServiceResponse["data"],
    caps: {
      service_protocol: 1, operations: ["connection.list", "auth.begin", "auth.status", "auth.cancel", "auth.logout", "credential.import"],
      spatial_v1: false, documents_v1: false, cancel_v1: true,
    } as ServiceCapabilities,
    handle: null as null | ((request: ServiceRequest, options?: { signal?: AbortSignal }) => Promise<ServiceResponse> | null),
  };
  const call = vi.fn<UsePluginServiceReturn["call"]>(async (request, options) => {
    const custom = state.handle?.(request, options);
    if (custom) return custom;
    const data = request.operation === "connection.list" ? { connections: state.connections }
      : request.operation === "auth.begin" ? state.begin : request.operation === "auth.status" ? state.status
      : request.operation === "auth.cancel" ? state.cancel : state.logout;
    return response(request.request_id, data, request.connection_id ?? CONNECTION_ID);
  });
  const importCredential = vi.fn<UsePluginServiceReturn["importCredential"]>(async (connectionId) => response("credential-import", { credential_ref: "opaque-ref" }, connectionId));
  const service: UsePluginServiceReturn = {
    call, importCredential,
    capabilities: vi.fn<UsePluginServiceReturn["capabilities"]>(async () => state.caps),
    executeWrite: vi.fn<UsePluginServiceReturn["executeWrite"]>(async () => response("unused", null)),
    saveArtifact: vi.fn<UsePluginServiceReturn["saveArtifact"]>(async () => false),
    subscribeMap: vi.fn<UsePluginServiceReturn["subscribeMap"]>(async () => () => undefined),
  };
  return { state, service, call, importCredential };
}
