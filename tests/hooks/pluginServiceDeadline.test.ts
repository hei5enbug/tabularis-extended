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

  it("query.execute가 아닌 요청은 120초 상한과 2초 grace로 종료한다", async () => {
    // Given
    vi.useFakeTimers();
    vi.mocked(invoke).mockImplementation(() => new Promise(() => {}));
    const hook = renderHook(usePluginService);
    const scenario = async () => { const pending = hook.result.current.call({ ...request, operation: "catalog.databases", input: {}, deadline_ms: 999999 }); await vi.advanceTimersByTimeAsync(122000); return pending; };
    // When
    const result = await scenario();
    // Then
    expect(result.status).toBe("outcome_unknown");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("query.execute는 한 시간 상한과 2초 grace를 쓰고 그 전에는 종료하지 않는다", async () => {
    // Given
    vi.useFakeTimers();
    vi.mocked(invoke).mockImplementation(() => new Promise(() => {}));
    const hook = renderHook(usePluginService);
    let settled = false;
    const pending = hook.result.current.call({ ...request, deadline_ms: 999999999 }).then((response) => { settled = true; return response; });
    // When
    await vi.advanceTimersByTimeAsync(3_601_999);
    const early = settled;
    await vi.advanceTimersByTimeAsync(1);
    const result = await pending;
    // Then
    expect(early).toBe(false);
    expect(result.status).toBe("outcome_unknown");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("승인을 기다리는 동안은 기한을 넘겨도 취소하지 않고 승인 뒤 남은 시간으로 끝낸다", async () => {
    // Given
    vi.useFakeTimers();
    let approved = false;
    const terminalResponse = { ...queued, status: "succeeded", job_id: "job-a", data: null } as ServiceResponse;
    vi.mocked(invoke).mockImplementation(async (_command, args) => {
      const operation = (args as { request: ServiceRequest }).request.operation;
      if (operation === "query.execute") return queued;
      if (operation === "job.get") return { ...queued, data: { job_id: "job-a", owner_instance_id: "owner-a", status: approved ? "succeeded" : "running", awaiting_approval: !approved } };
      if (operation === "job.result") return { ...queued, data: { status: "succeeded", response: terminalResponse } };
      return queued;
    });
    const hook = renderHook(usePluginService);
    const pending = hook.result.current.call(request);
    // When
    await vi.advanceTimersByTimeAsync(10_000);
    const operations = vi.mocked(invoke).mock.calls.map(([, args]) => (args as { request: ServiceRequest }).request.operation);
    approved = true;
    await vi.advanceTimersByTimeAsync(100);
    const result = await pending;
    // Then
    expect(operations).not.toContain("job.cancel");
    expect(operations.filter((operation) => operation === "job.get").length).toBeGreaterThan(10);
    expect(result.status).toBe("succeeded");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("승인 대기가 끝난 뒤에는 대기 시작 때 남았던 시간만 허용하고 넘으면 미확정으로 종료한다", async () => {
    // Given
    vi.useFakeTimers();
    let approved = false;
    vi.mocked(invoke).mockImplementation(async (_command, args) => {
      const operation = (args as { request: ServiceRequest }).request.operation;
      if (operation === "query.execute") return queued;
      if (operation === "job.get") return { ...queued, data: { job_id: "job-a", owner_instance_id: "owner-a", status: "running", awaiting_approval: !approved } };
      return queued;
    });
    const hook = renderHook(usePluginService);
    const pending = hook.result.current.call(request);
    // When
    await vi.advanceTimersByTimeAsync(10_000);
    approved = true;
    await vi.advanceTimersByTimeAsync(1_900);
    const operationsBefore = vi.mocked(invoke).mock.calls.length;
    await vi.advanceTimersByTimeAsync(200);
    const result = await pending;
    // Then
    expect(operationsBefore).toBeGreaterThan(0);
    expect(result).toMatchObject({ status: "outcome_unknown", job_id: "job-a", error: { code: "OUTCOME_UNKNOWN" } });
    expect(vi.mocked(invoke).mock.calls.filter(([, args]) => (args as { request: ServiceRequest }).request.operation === "job.cancel")).toHaveLength(1);
    expect(vi.getTimerCount()).toBe(0);
  });
});
