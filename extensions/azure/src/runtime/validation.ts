import { Ajv } from "ajv";
import { inputSchema } from "@tabularis/service-contracts";
import type { JsonObject, Operation, OperationInputs } from "@tabularis/service-contracts/types";
import { DriverError } from "./errors.js";

const ajv = new Ajv({ strict: false, allErrors: true, coerceTypes: false, useDefaults: false, removeAdditional: false });
const validators = new Map<Operation, ReturnType<typeof ajv.compile>>();
export function jsonObject(value: unknown): value is JsonObject { return typeof value === "object" && value !== null && !Array.isArray(value); }
export function onlyKeys(value: JsonObject, keys: readonly string[]): void {
  if (Object.keys(value).some((key) => !keys.includes(key))) throw new DriverError("INVALID_ARGUMENT", "Unexpected RPC input property.");
}
export function validateOperationInput<K extends Operation>(operation: K, value: unknown): OperationInputs[K] {
  let validator = validators.get(operation);
  if (!validator) { validator = ajv.compile(inputSchema(operation)); validators.set(operation, validator); }
  if (!validator(value)) throw new DriverError("INVALID_ARGUMENT", "The operation input does not match the service contract.");
  return value as OperationInputs[K];
}
