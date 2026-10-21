export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export type JsonObject = { [key: string]: JsonValue };
export const operationNames = [
  "connection.list",
  "connection.create",
  "connection.update",
  "connection.delete",
  "connection.test",
  "catalog.databases",
  "catalog.schemas",
  "catalog.objects",
  "catalog.describe",
  "query.execute",
  "query.next",
  "result.get",
  "result.release",
  "row.insert",
  "row.update",
  "row.delete",
  "document.read",
  "document.create",
  "document.replace",
  "document.delete",
  "session.open",
  "session.commit",
  "session.rollback",
  "session.close",
  "spatial.columns",
  "spatial.query_result",
  "spatial.table_query",
  "spatial.feature",
  "spatial.export",
  "map.create",
  "map.update",
  "map.get",
  "map.list",
  "map.open",
  "map.close",
  "map.layer.add",
  "map.layer.remove",
  "map.layer.update",
  "map.viewport.set",
  "map.selection.set",
  "map.save",
  "job.get",
  "job.result",
  "job.cancel",
  "auth.begin",
  "auth.status",
  "auth.cancel",
  "auth.logout",
  "credential.import"
] as const;
export type Operation = (typeof operationNames)[number];
export const errorCodes = [
  "INVALID_ARGUMENT",
  "PROTOCOL_MISMATCH",
  "CONNECTION_NOT_FOUND",
  "PERMISSION_DENIED",
  "WRITE_NOT_ALLOWED",
  "CAPABILITY_UNAVAILABLE",
  "UNSUPPORTED_TYPE",
  "UNSUPPORTED_OPERATION",
  "AUTH_REQUIRED",
  "AUTH_EXPIRED",
  "INTERACTION_REQUIRED",
  "TLS_VALIDATION_FAILED",
  "SRID_REQUIRED",
  "INVALID_GEOMETRY",
  "FEATURE_TOO_LARGE",
  "VERSION_CONFLICT",
  "ETAG_CONFLICT",
  "DOCUMENT_ALREADY_EXISTS",
  "PARTITION_KEY_IMMUTABLE",
  "DOCUMENT_TOO_LARGE",
  "RESOURCE_LIMIT",
  "INVALID_PAGE_TOKEN",
  "CURSOR_EXPIRED",
  "RESULT_EXPIRED",
  "RATE_LIMITED",
  "DEADLINE_EXCEEDED",
  "CANCELLED",
  "GUI_UNAVAILABLE",
  "APPLY_TIMEOUT",
  "DRIVER_EXITED",
  "OUTCOME_UNKNOWN",
  "DOCUMENT_NOT_FOUND"
] as const;
export type ErrorCode = (typeof errorCodes)[number];
export type JobStatus = "queued" | "running" | "succeeded" | "failed" | "cancelled" | "interrupted" | "outcome_unknown";
export type Outcome = "not_started" | "not_applied" | "applied" | "unknown";
export type ResumeMode = "none" | "native" | "materialized";
export type AuthMode = "account_key" | "entra_user" | "entra_service_principal";
export type PartitionKeyComponent =
  | { type: "string"; value: string }
  | { type: "number"; value: number }
  | { type: "boolean"; value: boolean }
  | { type: "null" }
  | { type: "undefined" };
export interface DocumentIdentity { id: string; partition_key: PartitionKeyComponent[] }
export interface TableRef { database: string | null; schema: string | null; table: string }
export interface RowIdentity { columns: string[]; values: JsonValue[] }
export interface QueryParameter { name: string; value: JsonValue }
export interface Viewport { west: number; south: number; east: number; north: number; world?: boolean }
export interface Basemap { style_url: string; attribution: string }
export interface DisplayOptions { source_srid?: number | null; longitude_mode?: "preserve" | "shortest"; skip_invalid?: boolean }
export type LayerSource = DisplayOptions & (
  | { kind: "query_result"; result_id: string; result_set_index: number; column_index: number }
  | { kind: "table"; table: TableRef; column: string; viewport: Viewport; srid_filter?: number | null }
);
export interface LayerStyle {
  point?: { color?: string; opacity?: number; radius?: number };
  line?: { color?: string; opacity?: number; width?: number };
  polygon?: { color?: string; opacity?: number };
}
export interface FeatureRef { layer_id: string; feature_id: string }
export type InputChannel = { kind: "protected_stdin" } | { kind: "file_descriptor"; descriptor: number } | { kind: "gui_buffer"; channel_id: string };
export interface OperationInputs {
  "connection.list": Record<string, never>;
  "connection.create": { "name": string; "driver": string; "settings": JsonObject; "credential_ref": string | null };
  "connection.update": { "patch": JsonObject };
  "connection.delete": Record<string, never>;
  "connection.test": Record<string, never>;
  "catalog.databases": Record<string, never>;
  "catalog.schemas": { "database": string };
  "catalog.objects": { "database": string; "schema": string | null };
  "catalog.describe": { "table": TableRef };
  "query.execute": { "text": string; "parameters": (QueryParameter)[]; "page_size": number; "mode"?: "exact"; "database"?: string; "container"?: string; "partition_key"?: PartitionKeyComponent[] | null; "ru_budget"?: number } & ({ "language": "sql"; "result_mode"?: never } | { "language": "cosmos_sql"; "result_mode"?: "json_values" | "documents" });
  "query.next": { "next_token": string };
  "result.get": { "result_id": string; "result_set_index": number; "offset": number; "limit": number };
  "result.release": { "result_id": string };
  "row.insert": { "table": TableRef; "values": JsonObject };
  "row.update": { "table": TableRef; "identity": RowIdentity; "changes": JsonObject };
  "row.delete": { "table": TableRef; "identity": RowIdentity };
  "document.read": { "database": string; "container": string; "identity": DocumentIdentity };
  "document.create": { "database": string; "container": string; "document": JsonObject; "partition_key": PartitionKeyComponent[] };
  "document.replace": { "database": string; "container": string; "identity": DocumentIdentity; "if_match": string; "document": JsonObject };
  "document.delete": { "database": string; "container": string; "identity": DocumentIdentity; "if_match": string };
  "session.open": { "database": string; "mode": "read_only" | "write" };
  "session.commit": Record<string, never>;
  "session.rollback": Record<string, never>;
  "session.close": Record<string, never>;
  "spatial.columns": { "table": TableRef };
  "spatial.query_result": { "result_id": string; "result_set_index": number; "column_index": number; "source_srid"?: number | null; "longitude_mode"?: "preserve" | "shortest"; "skip_invalid"?: boolean; "page_size"?: number; "next_token"?: string | null };
  "spatial.table_query": { "table": TableRef; "column": string; "viewport": Viewport; "page_size": number; "next_token"?: string | null; "source_srid"?: number | null; "longitude_mode"?: "preserve" | "shortest"; "skip_invalid"?: boolean; "srid_filter"?: number | null };
  "spatial.feature": { "result_id"?: string; "result_set_index"?: number; "feature_id"?: string; "table"?: TableRef; "identity"?: RowIdentity };
  "spatial.export": { "source": LayerSource; "format": "geojson"; "filename": string; "export_limit_bytes"?: number };
  "map.create": { "name": string; "viewport"?: Viewport; "basemap"?: Basemap | null };
  "map.update": ({ "name": string; "basemap"?: Basemap | null } | { "name"?: string; "basemap": Basemap | null }) & { "await_gui"?: boolean; "gui_instance_id"?: string };
  "map.get": Record<string, never>;
  "map.list": Record<string, never>;
  "map.open": { "gui_instance_id": string };
  "map.close": { "gui_instance_id": string };
  "map.layer.add": { "source": LayerSource; "style": LayerStyle; "await_gui"?: boolean; "gui_instance_id"?: string };
  "map.layer.remove": { "layer_id": string; "await_gui"?: boolean; "gui_instance_id"?: string };
  "map.layer.update": { "layer_id": string; "visible"?: boolean; "style"?: LayerStyle; "await_gui"?: boolean; "gui_instance_id"?: string };
  "map.viewport.set": { "viewport": Viewport; "await_gui"?: boolean; "gui_instance_id"?: string };
  "map.selection.set": { "feature_refs": (FeatureRef)[]; "await_gui"?: boolean; "gui_instance_id"?: string };
  "map.save": Record<string, never>;
  "job.get": { "job_id": string; "owner_instance_id": string };
  "job.result": { "job_id": string; "owner_instance_id": string };
  "job.cancel": { "job_id": string; "owner_instance_id": string };
  "auth.begin": { "auth_mode": "account_key" | "entra_user" | "entra_service_principal"; "persistence"?: "keychain" | "session" };
  "auth.status": { "auth_job_id": string };
  "auth.cancel": { "auth_job_id": string };
  "auth.logout": Record<string, never>;
  "credential.import": { "slot": "db_password" | "cosmos_account_key" | "entra_client_secret" | "entra_refresh_token"; "input_channel": InputChannel; "persistence": "keychain" | "session" };
}

export interface ServiceRequest<O extends Operation = Operation> {
  protocol_version: 1;
  operation: O;
  request_id: string;
  connection_id?: string | null;
  session_id?: string | null;
  map_id?: string | null;
  expected_version?: number | null;
  deadline_ms?: number;
  input: OperationInputs[O];
}
export interface ServiceError { code: ErrorCode; message: string; retryable: boolean; outcome: Outcome; details: JsonValue }
export interface Page { next_token: string | null; has_more: boolean; resume_mode: ResumeMode }
export interface Limits { truncated: boolean; reasons: string[] }
export interface Metrics { elapsed_ms: number; request_charge: number | null; retry_count: number }
export interface ServiceResponse {
  protocol_version: 1;
  request_id: string;
  connection_id: string | null;
  status: JobStatus;
  job_id: string | null;
  result_id: string | null;
  data: JsonValue;
  page: Page;
  limits: Limits;
  metrics: Metrics;
  warnings: string[];
  error: ServiceError | null;
}
export interface ServiceCapabilities { service_protocol?: 1; spatial_v1?: boolean; documents_v1?: boolean; query_page_v1?: boolean; cancel_v1?: boolean }
