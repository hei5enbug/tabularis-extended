import type { ServiceRequest, ServiceResponse, UsePluginServiceReturn } from "@tabularis/plugin-api";
import { SpatialUiError, object, parseMap, type MapState } from "./models";

export type Operation = ServiceRequest["operation"];
export type Scope = Pick<ServiceRequest, "connection_id" | "map_id" | "expected_version">;
export async function call<O extends Operation>(service: UsePluginServiceReturn, operation: O, input: ServiceRequest<O>["input"], scope: Scope = {}, signal?: AbortSignal): Promise<ServiceResponse> {
  const response = await service.call({ protocol_version: 1, operation, request_id: crypto.randomUUID(), deadline_ms: 30000, ...scope, input }, { signal });
  if (signal?.aborted) throw new DOMException("Cancelled", "AbortError");
  if (response.error) throw new SpatialUiError(response.error.code, response.error.message);
  if (response.status !== "succeeded") throw new SpatialUiError("OUTCOME_UNKNOWN", "서비스 작업 결과를 확인할 수 없습니다.");
  return response;
}
export function mapResponse(response: ServiceResponse): MapState { return parseMap(object(response.data).map); }
export function errorText(error: unknown): string { return error instanceof SpatialUiError ? `${error.code}: ${error.message}` : error instanceof Error ? error.message : "작업을 완료하지 못했습니다."; }
export function isAborted(error: unknown): boolean { return error instanceof DOMException && error.name === "AbortError"; }
