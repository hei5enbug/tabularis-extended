import type { ErrorCode, JsonValue, Outcome, ServiceError, ServiceResponse } from "@tabularis/service-contracts/types";
import type { RpcRequestContext } from "./contracts.js";
import { MetricsAccumulator } from "./metrics.js";

export class DriverError extends Error {
  constructor(readonly code: ErrorCode, message: string, readonly outcome: Outcome = "not_started", readonly retryable = false) { super(message); }
}
export function safeError(error: unknown): ServiceError {
  if (error instanceof DriverError) return { code: error.code, message: error.message, outcome: error.outcome, retryable: error.retryable, details: null };
  return { code: "DRIVER_EXITED", message: "The driver operation failed.", outcome: "unknown", retryable: false, details: null };
}
export function assertRequestActive(context: RpcRequestContext): void {
  if (context.signal.aborted) {
    const reason: unknown = context.signal.reason;
    throw reason instanceof DriverError ? reason : new DriverError("CANCELLED", "The request was cancelled.", "not_applied");
  }
  if (context.deadline_at_ms !== undefined && (context.now ?? Date.now)() >= context.deadline_at_ms) {
    throw new DriverError("DEADLINE_EXCEEDED", "The request deadline was exceeded.", "not_applied");
  }
}
export function serviceResponse(context: RpcRequestContext, data: JsonValue, error: ServiceError | null = null, metrics = context.metrics ?? new MetricsAccumulator()): ServiceResponse {
  return {
    protocol_version: 1, request_id: context.request_id, connection_id: context.connection_id,
    status: error?.outcome === "unknown" ? "outcome_unknown" : error?.code === "CANCELLED" ? "cancelled" : error ? "failed" : "succeeded",
    job_id: null, result_id: null, data: error ? null : data,
    page: { next_token: null, has_more: false, resume_mode: "none" }, limits: { truncated: false, reasons: [] },
    metrics: metrics.snapshot(), warnings: [], error,
  };
}
const tlsErrors = new Set(["CERT_HAS_EXPIRED", "ERR_TLS_CERT_ALTNAME_INVALID", "DEPTH_ZERO_SELF_SIGNED_CERT", "UNABLE_TO_VERIFY_LEAF_SIGNATURE", "SELF_SIGNED_CERT_IN_CHAIN", "UNABLE_TO_GET_ISSUER_CERT_LOCALLY"]);
export function mapSdkError(error: unknown, context: RpcRequestContext, options: { write?: boolean; document?: boolean; resource?: "database" | "container" } = {}): DriverError {
  if (error instanceof DriverError) {
    if (options.write && context.write_dispatched?.() && (error.code === "CANCELLED" || error.code === "DEADLINE_EXCEEDED")) return new DriverError("OUTCOME_UNKNOWN", "The write outcome could not be confirmed.", "unknown");
    return error;
  }
  const record = typeof error === "object" && error !== null ? error as Record<string, unknown> : {};
  const raw = record.code ?? record.statusCode;
  const status = typeof raw === "number" ? raw : typeof raw === "string" && /^\d{3}$/.test(raw) ? Number(raw) : undefined;
  if (typeof raw === "string" && tlsErrors.has(raw)) return new DriverError("TLS_VALIDATION_FAILED", "TLS certificate or hostname validation failed.", "not_applied");
  if (status === 409) return new DriverError("DOCUMENT_ALREADY_EXISTS", "The document identity already exists.", "not_applied");
  if (status === 412) return new DriverError("ETAG_CONFLICT", "The document ETag has changed.", "not_applied");
  if (status === 429) return new DriverError("RATE_LIMITED", "The service rate limit was exceeded.", "not_applied", true);
  if (status === 401) return new DriverError("AUTH_REQUIRED", "The service requires authentication.", "not_applied");
  if (status === 403) return new DriverError("PERMISSION_DENIED", "The service denied access.", "not_applied");
  if (status === 404 && options.document) return new DriverError("DOCUMENT_NOT_FOUND", "The document was not found.", "not_applied");
  if (status === 404 && options.resource) return new DriverError("INVALID_ARGUMENT", options.resource === "database" ? "The database was not found." : "The container was not found.", "not_applied");
  if (status === 413) return new DriverError("DOCUMENT_TOO_LARGE", "The document exceeds the service size limit.", "not_applied");
  if (status === 400 || status === 404) return new DriverError("INVALID_ARGUMENT", "The requested resource or input is invalid.", "not_applied");
  if (options.write && context.write_dispatched?.()) return new DriverError("OUTCOME_UNKNOWN", "The write outcome could not be confirmed.", "unknown");
  if (context.signal.aborted) {
    const reason: unknown = context.signal.reason;
    return reason instanceof DriverError ? reason : new DriverError("CANCELLED", "The request was cancelled.", "not_applied");
  }
  return new DriverError("DRIVER_EXITED", "The service request could not be completed.", "not_applied");
}
