import type { ServiceRequest, ServiceResponse, UsePluginServiceReturn } from "@tabularis/plugin-api";

export type AuthMode = "sql_password" | "integrated" | "entra_user" | "entra_service_principal";
export type EntraMode = Extract<AuthMode, "entra_user" | "entra_service_principal">;
export type Persistence = "session" | "keychain";
export interface AuthSettings { mode: AuthMode | null; tenant: string; client: string; source?: "oauth" | "azure_cli" }
export interface AuthStatus {
  jobId: string;
  state: "pending" | "interaction_required" | "authenticated" | "cancelled" | "failed";
  challenge: { code: string; url: string | null } | null;
  errorCode: string | null;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const CONTROLS = /[\u0000-\u001f\u007f-\u009f]/;
export const POLL_INTERVAL_MS = 1_000;
export const POLL_LIMIT = 900;
export const POLL_DURATION_MS = 15 * 60_000;

export class AuthFault extends Error {
  constructor(readonly kind: "stale" | "failed" | "denied" | "unknown" | "malformed" | "capability") {
    super(kind);
  }
}

export function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function boundedId(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0 && value.length <= 128 && !CONTROLS.test(value);
}

export function validUuid(value: string): boolean { return UUID.test(value); }

export function settings(extra: Record<string, string>): AuthSettings {
  const raw = extra.auth_mode || (extra.integrated_auth === "true" ? "integrated" : "sql_password");
  const mode = ["sql_password", "integrated", "entra_user", "entra_service_principal"].includes(raw)
    ? raw as AuthMode : null;
  return { mode, tenant: extra.tenant_id ?? "", client: extra.client_id ?? "", ...(extra.auth_source ? { source: extra.auth_source as "oauth" | "azure_cli" } : {}) };
}

export function savedSettings(value: unknown): AuthSettings | null {
  if (!object(value)) return null;
  const extra = object(value.extra) ? value.extra : {};
  const fields: Record<string, string> = {};
  for (const key of ["auth_mode", "auth_source", "integrated_auth", "tenant_id", "client_id"]) {
    const field = Object.prototype.hasOwnProperty.call(extra, key) ? extra[key] : value[key];
    if (field !== undefined && typeof field !== "string") return null;
    fields[key] = typeof field === "string" ? field : "";
  }
  return settings(fields);
}

export function matchesSaved(data: unknown, connectionId: string, form: AuthSettings): boolean {
  if (!object(data) || !Array.isArray(data.connections)) return false;
  const matches = data.connections.filter((item: unknown) => object(item) && item.connection_id === connectionId);
  if (matches.length !== 1 || !object(matches[0]) || matches[0].driver !== "sqlserver") return false;
  const stored = savedSettings(matches[0].settings);
  return stored !== null && stored.mode === form.mode && stored.tenant === form.tenant && stored.client === form.client && (stored.source ?? "oauth") === (form.source ?? "oauth");
}

export function request<O extends ServiceRequest["operation"]>(
  operation: O, connectionId: string, input: ServiceRequest<O>["input"], deadline = 60_000,
): ServiceRequest<O> {
  return { protocol_version: 1, operation, connection_id: connectionId, request_id: crypto.randomUUID(), deadline_ms: deadline, input };
}

export function completed(response: unknown, connectionId: string, requestId?: string): ServiceResponse {
  if (!object(response) || response.protocol_version !== 1 || response.connection_id !== connectionId
    || !boundedId(response.request_id) || (requestId !== undefined && response.request_id !== requestId)) {
    throw new AuthFault("malformed");
  }
  if (response.status === "outcome_unknown" || (object(response.error) && response.error.outcome === "unknown")) {
    throw new AuthFault("unknown");
  }
  if (response.status !== "succeeded" || response.error !== null) {
    const code = object(response.error) ? response.error.code : null;
    throw new AuthFault(code === "PERMISSION_DENIED" || code === "WRITE_NOT_ALLOWED" ? "denied" : "failed");
  }
  if (!object(response.limits) || response.limits.truncated !== false) throw new AuthFault("malformed");
  return response as unknown as ServiceResponse;
}

export function parseStatus(data: unknown): AuthStatus {
  if (!object(data) || !boundedId(data.auth_job_id)
    || !["pending", "interaction_required", "authenticated", "cancelled", "failed"].includes(String(data.state))
    || typeof data.user_action_required !== "boolean"
    || !(data.error_code === null || (typeof data.error_code === "string" && /^[A-Z_]{1,64}$/.test(data.error_code)))) {
    throw new AuthFault("malformed");
  }
  if (data.state === "authenticated" && (data.error_code !== null || data.user_action_required !== false)) throw new AuthFault("malformed");
  let challenge: AuthStatus["challenge"] = null;
  if (data.challenge !== undefined) {
    if (!object(data.challenge) || typeof data.challenge.user_code !== "string"
      || data.challenge.user_code.length === 0 || data.challenge.user_code.length > 128
      || CONTROLS.test(data.challenge.user_code) || typeof data.challenge.verification_uri !== "string") {
      throw new AuthFault("malformed");
    }
    challenge = { code: data.challenge.user_code, url: data.challenge.verification_uri === "https://microsoft.com/devicelogin"
      ? "https://microsoft.com/devicelogin" : null };
  }
  return { jobId: data.auth_job_id, state: data.state as AuthStatus["state"], challenge, errorCode: data.error_code as string | null };
}

export function faultMessage(error: unknown): string {
  if (error instanceof AuthFault) {
    if (error.kind === "denied") return "이 연결의 인증을 관리할 권한이 없습니다.";
    if (error.kind === "unknown") return "처리 결과를 확인할 수 없습니다. 자동으로 다시 요청하지 않습니다.";
    if (error.kind === "malformed") return "인증 응답을 확인할 수 없습니다.";
    if (error.kind === "capability") return "호스트에서 공통 인증 기능을 사용할 수 없습니다.";
  }
  return "인증 요청을 완료하지 못했습니다.";
}

export function failedStatusMessage(code: string | null): string {
  if (code === "AUTH_EXPIRED") return "인증 시간이 만료되었습니다. 다시 로그인을 시작해 주세요.";
  if (code === "PERMISSION_DENIED") return "이 연결의 인증을 관리할 권한이 없습니다.";
  if (code === "KEYCHAIN_UNAVAILABLE") return "키체인을 사용할 수 없습니다. 세션 보관을 선택해 주세요.";
  return "로그인에 실패했습니다.";
}

export class AuthScope {
  private generation = 0;
  private active = true;
  private controllers = new Set<AbortController>();
  private job: string | null = null;
  private cancelledJobs = new Set<string>();

  constructor(readonly service: UsePluginServiceReturn, readonly connectionId: string) {}

  activate(): void { this.active = true; }
  isCurrent(generation: number): boolean { return this.active && generation === this.generation; }
  current(): number { return this.generation; }
  setJob(job: string | null): void { this.job = job; }

  private observeJob(response: unknown, generation: number, requestId: string): void {
    if (!object(response) || response.protocol_version !== 1 || response.connection_id !== this.connectionId
      || response.request_id !== requestId || !object(response.data) || !boundedId(response.data.auth_job_id)) return;
    const job = response.data.auth_job_id;
    if (!this.isCurrent(generation)) this.cancelBestEffort(job);
    else this.job = job;
  }

  cancelBestEffort(job: string): void {
    if (this.cancelledJobs.has(job)) return;
    this.cancelledJobs.add(job);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 10_000);
    void Promise.resolve().then(() => this.service.call(request("auth.cancel", this.connectionId, { auth_job_id: job }, 10_000), { signal: controller.signal }))
      .catch(() => undefined).finally(() => clearTimeout(timer));
  }

  stop(): string | null {
    this.generation += 1;
    for (const controller of this.controllers) controller.abort();
    this.controllers.clear();
    const job = this.job;
    this.job = null;
    return job;
  }

  dispose(): void {
    this.active = false;
    const job = this.stop();
    if (job !== null) this.cancelBestEffort(job);
  }

  async run<T>(action: (signal: AbortSignal) => Promise<T>, observe?: (value: T, generation: number) => void): Promise<T> {
    const generation = this.generation;
    if (!this.active) throw new AuthFault("stale");
    const controller = new AbortController();
    this.controllers.add(controller);
    try {
      const value = await action(controller.signal);
      observe?.(value, generation);
      if (!this.isCurrent(generation) || controller.signal.aborted) throw new AuthFault("stale");
      return value;
    } catch (error) {
      if (!this.isCurrent(generation) || controller.signal.aborted) throw new AuthFault("stale");
      throw error instanceof AuthFault ? error : new AuthFault("failed");
    } finally {
      this.controllers.delete(controller);
    }
  }

  async call<O extends ServiceRequest["operation"]>(operation: O, input: ServiceRequest<O>["input"], trackJob = false): Promise<ServiceResponse> {
    const outgoing = request(operation, this.connectionId, input);
    const response = await this.run((signal) => this.service.call(outgoing, { signal }),
      trackJob ? (value, generation) => this.observeJob(value, generation, outgoing.request_id) : undefined);
    return completed(response, this.connectionId, outgoing.request_id);
  }
}
