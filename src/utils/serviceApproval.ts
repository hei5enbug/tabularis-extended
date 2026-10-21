import type { PendingApproval } from "../types/ai";

type ApprovalFields = Pick<
  PendingApproval,
  "query" | "queryKind" | "connectionId" | "editable" | "service"
>;

/** The request a service approval covers, as shown by the backend approval gate. */
export interface ServiceApprovalRequest {
  operation: string;
  input: unknown;
  /** `null` for a connection that does not exist yet. */
  connectionId: string | null;
  credentialRetarget: boolean;
}

export interface ApprovalChangeRow {
  field: string;
  value: string;
}

const CONNECTION_OPERATIONS = new Set(["connection.create", "connection.update"]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/**
 * True for an approval bound to one exact request: the approver may only
 * approve or deny it, never edit what it runs.
 */
export function isLockedApproval(
  approval: Pick<ApprovalFields, "editable" | "service" | "queryKind">,
): boolean {
  return (
    approval.editable === false ||
    approval.service !== undefined ||
    approval.queryKind.startsWith("service:")
  );
}

export function serviceOperation(
  approval: Pick<ApprovalFields, "service" | "queryKind">,
): string {
  return approval.service?.operation ?? approval.queryKind.replace(/^service:/, "");
}

/** Reads the request summary of a service approval; `null` when it is not one. */
export function parseServiceApprovalRequest(
  approval: Pick<ApprovalFields, "query" | "service" | "queryKind" | "connectionId">,
): ServiceApprovalRequest | null {
  let shown: unknown;
  try {
    shown = JSON.parse(approval.query);
  } catch {
    return null;
  }
  if (!isRecord(shown)) return null;
  const connectionId =
    typeof shown.connection_id === "string"
      ? shown.connection_id
      : shown.connection_id === null
        ? null
        : approval.connectionId || null;
  return {
    operation:
      typeof shown.operation === "string" ? shown.operation : serviceOperation(approval),
    input: shown.input ?? null,
    connectionId,
    credentialRetarget:
      approval.service?.credentialRetarget === true || shown.credential_retarget === true,
  };
}

function displayValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === undefined) return "";
  return JSON.stringify(value);
}

function flatten(value: unknown, path: string, rows: ApprovalChangeRow[]): void {
  if (isRecord(value) && Object.keys(value).length > 0) {
    for (const [key, item] of Object.entries(value)) {
      flatten(item, path ? `${path}.${key}` : key, rows);
    }
    return;
  }
  rows.push({ field: path, value: displayValue(value) });
}

/**
 * Every field a connection create or update sets, flattened to dotted paths.
 * `null` for any other operation.
 */
export function connectionChangeRows(
  request: ServiceApprovalRequest,
): ApprovalChangeRow[] | null {
  if (!CONNECTION_OPERATIONS.has(request.operation) || !isRecord(request.input)) {
    return null;
  }
  const rows: ApprovalChangeRow[] = [];
  if (request.operation === "connection.update") {
    const patch = request.input.patch;
    if (isRecord(patch)) {
      for (const [key, item] of Object.entries(patch)) flatten(item, key, rows);
    }
    return rows;
  }
  for (const [key, item] of Object.entries(request.input)) {
    if (key === "credential_ref" && item === null) continue;
    flatten(item, key, rows);
  }
  return rows;
}

export function formatApprovalJson(value: unknown): string {
  return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

/** The approval text indented for reading, or as stored when it is not JSON. */
export function formatApprovalQuery(query: string): string {
  try {
    return JSON.stringify(JSON.parse(query), null, 2);
  } catch {
    return query;
  }
}
