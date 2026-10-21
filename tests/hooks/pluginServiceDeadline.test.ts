import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import type { ServiceRequest, ServiceResponse } from "@tabularis/service-contracts";
import { usePluginService } from "../../src/hooks/usePluginService";

const request: ServiceRequest = { protocol_version: 1, operation: "query.execute", request_id: "deadline-original", connection_id: "conn-a", deadline_ms: 1, input: { language: "sql", text: "SELECT 1", parameters: [], page_size: 100 } };
const queued: ServiceResponse = { protocol_version: 1, request_id: request.request_id, connection_id: "conn-a", status: "queued", job_id: "job-a", result_id: null, data: { owner_instance_id: "owner-a" }, page: { has_more: false, next_token: null, resume_mode: "none" }, limits: { truncated: false, reasons: [] }, metrics: { elapsed_ms: 0, request_charge: null, retry_count: 0 }, warnings: [], error: null };
afterEach(() => { vi.useRealTimers(); vi.mocked(invoke).mockReset(); });

describe("서비스 outer deadline", () => {
  it("초기 invoke가 끝나지 않아도 원래 요청을 미확정 상태로 종료한다", async () => {
    // Given
    vi.useFakeTimers();
    vi.mocked(invoke).mockImplementation(() => new Promise(() => {}));
    const hook = renderHook(usePluginService);
    const scenario = async () => { const pending = hook.result.current.call(request); await vi.advanceTimersByTimeAsync(2001); return pending; };
    // When
    const result = await scenario();
    // Then
    expect(result).toMatchObject({ request_id: "deadline-original", connection_id: "conn-a", status: "outcome_unknown", job_id: null, error: { code: "OUTCOME_UNKNOWN", outcome: "unknown", retryable: false } });
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("job polling과 취소가 멈춰도 하나의 deadline 뒤 listener를 정리한다", async () => {
    // Given
    vi.useFakeTimers();
    const controller = new AbortController();
    const remove = vi.spyOn(controller.signal, "removeEventListener");
    vi.mocked(invoke).mockResolvedValueOnce(queued).mockImplementation(() => new Promise(() => {}));
    const hook = renderHook(usePluginService);
    const scenario = async () => { const pending = hook.result.current.call(request, { signal: controller.signal }); await vi.advanceTimersByTimeAsync(2001); controller.abort(); return pending; };
    // When
    const result = await scenario();
    // Then
    expect(result).toMatchObject({ request_id: request.request_id, status: "outcome_unknown", job_id: "job-a", error: { code: "OUTCOME_UNKNOWN", outcome: "unknown" } });
    expect(vi.mocked(invoke).mock.calls.filter(([, args]) => (args as { request: ServiceRequest }).request.operation === "job.cancel")).toHaveLength(1);
    expect(remove).toHaveBeenCalledWith("abort", expect.any(Function));
    expect(vi.getTimerCount()).toBe(0);
  });

  it("최대 120초를 넘는 요청도 같은 상한과 2초 grace로 종료한다", async () => {
    // Given
    vi.useFakeTimers();
    vi.mocked(invoke).mockImplementation(() => new Promise(() => {}));
    const hook = renderHook(usePluginService);
    const scenario = async () => { const pending = hook.result.current.call({ ...request, deadline_ms: 999999 }); await vi.advanceTimersByTimeAsync(122000); return pending; };
    // When
    const result = await scenario();
    // Then
    expect(result.status).toBe("outcome_unknown");
    expect(vi.getTimerCount()).toBe(0);
  });
});
