import { readFileSync } from "node:fs";
import { Ajv, type ErrorObject } from "ajv";
import { operationNames, type Operation, type ServiceRequest, type ServiceResponse } from "./types.js";
export * from "./types.js";

export const PROTOCOL_VERSION = 1 as const;
export const DEFAULT_DEADLINE_MS = 30_000;
export const MAX_DEADLINE_MS = 120_000;
export const schemaDirectory = new URL("../schema/v1/", import.meta.url);
export const fixtureFile = new URL("../fixtures/v1/validation.json", import.meta.url);
export const requestSchema = JSON.parse(readFileSync(new URL("request.json", schemaDirectory), "utf8")) as Record<string, unknown>;
export const responseSchema = JSON.parse(readFileSync(new URL("response.json", schemaDirectory), "utf8")) as Record<string, unknown>;
const ajv = new Ajv({ allErrors: true, strict: false, coerceTypes: false, useDefaults: false, removeAdditional: false });
const requestValidator = ajv.compile<ServiceRequest>(requestSchema);
const responseValidator = ajv.compile<ServiceResponse>(responseSchema);
export type ValidationResult<T> = { valid: true; value: T } | { valid: false; errors: readonly ErrorObject[] };

export function validateRequest(value: unknown): ValidationResult<ServiceRequest> {
  return requestValidator(value) ? { valid: true, value } : { valid: false, errors: structuredClone(requestValidator.errors ?? []) };
}

export function validateResponse(value: unknown): ValidationResult<ServiceResponse> {
  return responseValidator(value) ? { valid: true, value } : { valid: false, errors: structuredClone(responseValidator.errors ?? []) };
}

export function isOperation(value: string): value is Operation {
  return (operationNames as readonly string[]).includes(value);
}

export function inputSchema(operation: Operation): Record<string, unknown> {
  const definitions = requestSchema.definitions as Record<string, Record<string, unknown>>;
  return { $schema: requestSchema.$schema, definitions, ...definitions[`input.${operation}`] };
}

export function unavailableResponse(request: ServiceRequest): ServiceResponse {
  return {
    protocol_version: PROTOCOL_VERSION, request_id: request.request_id, connection_id: request.connection_id ?? null,
    status: "failed", job_id: null, result_id: null, data: null,
    page: { next_token: null, has_more: false, resume_mode: "none" }, limits: { truncated: false, reasons: [] },
    metrics: { elapsed_ms: 0, request_charge: null, retry_count: 0 }, warnings: [],
    error: { code: "CAPABILITY_UNAVAILABLE", message: "The requested service handler is unavailable.", retryable: false, outcome: "not_started", details: null },
  };
}
