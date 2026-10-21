import { PartitionKeyBuilder, type PartitionKey } from "@azure/cosmos";
import type { JsonObject, JsonValue, PartitionKeyComponent } from "@tabularis/service-contracts/types";
import { DriverError } from "../runtime/errors.js";

export const systemFields = ["_rid", "_self", "_etag", "_attachments", "_ts"] as const;
function partitionPath(path: string): string[] {
  const parts: string[] = [];
  let offset = 0;
  while (offset < path.length) {
    if (path[offset] !== "/") throw new DriverError("UNSUPPORTED_TYPE", "The container partition-key path is invalid.", "not_applied");
    offset++;
    if (offset === path.length) break;
    const quote = path[offset];
    if (quote === '"' || quote === "'") {
      const start = ++offset;
      while (offset < path.length && (path[offset] !== quote || path[offset - 1] === "\\")) offset++;
      if (offset === path.length) throw new DriverError("UNSUPPORTED_TYPE", "The container partition-key path is invalid.", "not_applied");
      parts.push(path.slice(start, offset++));
    } else {
      const end = path.indexOf("/", offset);
      parts.push(path.slice(offset, end < 0 ? undefined : end).trim());
      offset = end < 0 ? path.length : end;
    }
  }
  if (!parts.length || parts.some((part) => !part)) throw new DriverError("UNSUPPORTED_TYPE", "The container partition-key path is invalid.", "not_applied");
  return parts;
}
export function extractPartitionComponents(document: JsonObject, paths: readonly string[]): PartitionKeyComponent[] {
  return paths.map((path) => {
    let value: JsonValue | undefined = document;
    for (const part of partitionPath(path)) {
      if (typeof value !== "object" || value === null || !Object.hasOwn(value, part)) { value = undefined; break; }
      value = (value as JsonObject)[part];
    }
    if (value === undefined) return { type: "undefined" };
    if (value === null) return { type: "null" };
    if (typeof value === "string") return { type: "string", value };
    if (typeof value === "number" && Number.isFinite(value)) return { type: "number", value };
    if (typeof value === "boolean") return { type: "boolean", value };
    throw new DriverError("INVALID_ARGUMENT", "Partition-key values must be scalar, null, or absent.");
  });
}
export function samePartitionKey(left: readonly PartitionKeyComponent[], right: readonly PartitionKeyComponent[]): boolean {
  return left.length === right.length && left.every((component, index) => {
    const other = right[index];
    return component.type === other?.type && (!("value" in component) || (other && "value" in other && component.value === other.value));
  });
}
export function fullPartitionKey(components: readonly PartitionKeyComponent[], paths: readonly string[]): PartitionKey {
  if (components.length !== paths.length) throw new DriverError("INVALID_ARGUMENT", "A complete partition key is required.");
  const builder = new PartitionKeyBuilder();
  for (const component of components) {
    if (component.type === "undefined") builder.addNoneValue();
    else if (component.type === "null") builder.addNullValue();
    else { if (component.type === "number" && !Number.isFinite(component.value)) throw new DriverError("INVALID_ARGUMENT", "Partition-key numbers must be finite."); builder.addValue(component.value); }
  }
  return builder.build();
}
export function validateDocumentId(id: unknown): asserts id is string {
  if (typeof id !== "string" || !id || /[\\/#?\u0000-\u001f\u007f]/.test(id) || Buffer.byteLength(id, "utf8") > 1023) throw new DriverError("INVALID_ARGUMENT", "An explicit valid document id is required.");
}
export function writableDocument(document: JsonObject): JsonObject {
  validateDocumentId(document.id);
  const body = Object.fromEntries(Object.entries(document).filter(([key]) => !(systemFields as readonly string[]).includes(key))) as JsonObject;
  let bytes: number;
  try { bytes = Buffer.byteLength(JSON.stringify(body), "utf8"); }
  catch { throw new DriverError("INVALID_ARGUMENT", "The document JSON structure cannot be serialized."); }
  if (bytes > 2 * 1024 * 1024) throw new DriverError("DOCUMENT_TOO_LARGE", "The document exceeds the service size limit.");
  const stack: JsonValue[] = [body];
  while (stack.length) {
    const value = stack.pop();
    if (typeof value === "number" && (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value)))) throw new DriverError("INVALID_ARGUMENT", "Unsafe integer values cannot be edited as JSON numbers.");
    if (typeof value === "object" && value !== null) for (const child of Object.values(value)) stack.push(child);
  }
  return body;
}
export function assertReadOnlyFields(document: JsonObject, snapshot: JsonObject): void {
  for (const key of systemFields) if (Object.hasOwn(document, key) && document[key] !== snapshot[key]) throw new DriverError("INVALID_ARGUMENT", "System properties are read-only.");
}
