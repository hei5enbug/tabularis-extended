import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ServiceRequest, ServiceResponse } from "@tabularis/service-contracts";
import { useServiceQuery } from "../../src/hooks/useServiceQuery";
import { snapshotResult } from "../../src/utils/serviceQuery";

const api = vi.hoisted(() => ({ call: vi.fn(), executeWrite: vi.fn(), capabilities: vi.fn() }));
vi.mock("../../src/hooks/usePluginService", () => ({ usePluginService: () => api }));
const input = { scopeId: "tab-a", connectionId: "conn-a", driverId: "postgresql", database: "fixture", text: "  SELECT 1 AS same, NULL AS same\n", pageSize: 5000 };
const response = (request: ServiceRequest, data?: ServiceResponse["data"]): ServiceResponse => ({ protocol_version: 1, request_id: request.request_id, connection_id: request.connection_id ?? null, status: "succeeded", job_id: null, result_id: "result-a", data: data ?? { kind: "tabular", columns: [{ name: "same", column_index: 0 }, { name: "same", column_index: 1 }], rows: [[1, null]], result_set_count: 1, result_set_index: 0, offset: 0, result_revision: "rev-a", affected_rows: 0 }, page: { has_more: false, next_token: null, resume_mode: "materialized" }, limits: { truncated: false, reasons: [] }, metrics: { elapsed_ms: 0, request_charge: null, retry_count: 0 }, warnings: [], error: null });
const execute = async (hook: ReturnType<typeof renderHook<ReturnType<typeof useServiceQuery>, unknown>>, request = input) => { let result; await act(async () => { result = await hook.result.current.execute(request); }); return result; };

beforeEach(() => {
  api.call.mockReset().mockImplementation(async (request: ServiceRequest) => request.operation === "connection.list" ? response(request, { connections: [{ connection_id: "conn-a", allow_writes: true, version: 2 }] }) : response(request));
  api.executeWrite.mockReset().mockImplementation(async (request: ServiceRequest) => response(request));
  api.capabilities.mockReset().mockResolvedValue({ service_protocol: 1, operations: ["query.execute"] });
});

describe("서비스 실행 hook", () => {
  it("연결과 SQL 원문을 고정하고 기본 읽기 전용 경로를 사용한다", async () => {
    // Given
    const hook = renderHook(useServiceQuery);
    // When
    const result = await execute(hook);
    // Then
    expect(api.call).toHaveBeenCalledWith(expect.objectContaining({ connection_id: "conn-a", session_id: null, input: { language: "sql", text: input.text, parameters: [], database: "fixture", page_size: 1000 } }), { signal: expect.any(AbortSignal) });
    expect(api.executeWrite).not.toHaveBeenCalled();
    expect(result).toMatchObject({ service: { connectionId: "conn-a", resultId: "result-a" } });
  });

  it("추가 결과 집합은 같은 result ID로 읽으며 SQL을 한 번만 실행한다", async () => {
    // Given
    api.call.mockImplementation(async (request: ServiceRequest) => { const value = response(request); value.data = { ...(value.data as object), result_set_count: 2, result_set_index: request.operation === "result.get" ? 1 : 0 }; return value; });
    const hook = renderHook(useServiceQuery);
    // When
    const result = await execute(hook);
    // Then
    expect(api.call.mock.calls.filter(([request]) => request.operation === "query.execute")).toHaveLength(1);
    expect(api.call).toHaveBeenCalledWith(expect.objectContaining({ operation: "result.get", input: { result_id: "result-a", result_set_index: 1, offset: 0, limit: 1000 } }), { signal: expect.any(AbortSignal) });
    expect(result).toMatchObject({ additional_results: [{ service: { resultId: "result-a", resultSetIndex: 1 } }] });
  });

  it("명시적 쓰기는 연결 정책 확인 후 executeWrite만 호출한다", async () => {
    // Given
    const hook = renderHook(useServiceQuery);
    const scenario = async () => { let result; await act(async () => { result = await hook.result.current.execute({ ...input, write: true }); }); return result; };
    // When
    const result = await scenario();
    // Then
    expect(api.call.mock.calls.filter(([request]) => request.operation === "query.execute")).toHaveLength(0);
    expect(api.executeWrite).toHaveBeenCalledTimes(1);
    expect(result).toMatchObject({ service: { resultId: "result-a" } });
  });

  it("읽기 전용 session에서 쓰기를 허용하지 않는다", async () => {
    // Given
    const hook = renderHook(useServiceQuery);
    const scenario = async () => { let error; await act(async () => { error = await hook.result.current.execute({ ...input, write: true, session: { sessionId: "real-session", mode: "read_only", connectionId: "conn-a" } }).catch((error: unknown) => error); }); return error; };
    // When
    const error = await scenario();
    // Then
    expect(error).toEqual(new Error("The session is read-only."));
    expect(api.call).not.toHaveBeenCalled();
    expect(api.executeWrite).not.toHaveBeenCalled();
  });

  it("다른 연결의 session을 재사용하지 않는다", async () => {
    // Given
    const hook = renderHook(useServiceQuery);
    const scenario = async () => { let error; await act(async () => { error = await hook.result.current.execute({ ...input, session: { sessionId: "real-session", mode: "write", connectionId: "conn-b" } }).catch((error: unknown) => error); }); return error; };
    // When
    const error = await scenario();
    // Then
    expect(error).toEqual(new Error("The session belongs to another connection."));
    expect(api.executeWrite).not.toHaveBeenCalled();
  });

  it("같은 tab의 새 실행은 이전 요청만 취소하고 늦은 결과를 거부한다", async () => {
    // Given
    let finish: ((value: ServiceResponse) => void) | undefined;
    let firstRequest: ServiceRequest;
    let firstSignal: AbortSignal | undefined;
    let ready: (() => void) | undefined;
    const started = new Promise<void>((resolve) => { ready = resolve; });
    api.call.mockImplementationOnce((request: ServiceRequest, options: { signal: AbortSignal }) => { firstRequest = request; firstSignal = options.signal; ready?.(); return new Promise<ServiceResponse>((resolve) => { finish = resolve; }); });
    const hook = renderHook(useServiceQuery);
    const scenario = async () => { let old: unknown, next: unknown; await act(async () => { const pending = hook.result.current.execute(input).catch((error: unknown) => error); await started; next = await hook.result.current.execute({ ...input, connectionId: "conn-b" }); finish?.(response(firstRequest)); old = await pending; }); return { old, next }; };
    // When
    const result = await scenario();
    // Then
    expect(firstSignal?.aborted).toBe(true);
    expect(result.old).toMatchObject({ name: "AbortError" });
    expect(result.next).toMatchObject({ service: { connectionId: "conn-b" } });
  });

  it("Stop은 지정한 cell 요청만 취소한다", async () => {
    // Given
    const signals = new Map<string, AbortSignal>();
    let ready: (() => void) | undefined;
    const started = new Promise<void>((resolve) => { ready = resolve; });
    api.call.mockImplementation((request: ServiceRequest, options: { signal: AbortSignal }) => { signals.set(request.connection_id!, options.signal); if (signals.size === 2) ready?.(); return new Promise<ServiceResponse>(() => {}); });
    const hook = renderHook(useServiceQuery);
    const scenario = async () => { void hook.result.current.execute(input); void hook.result.current.execute({ ...input, scopeId: "cell-b", connectionId: "conn-b" }); await started; return hook.result.current.cancel("tab-a"); };
    // When
    const cancelled = await scenario();
    // Then
    expect(cancelled).toBe(true);
    expect(signals.get("conn-a")?.aborted).toBe(true);
    expect(signals.get("conn-b")?.aborted).toBe(false);
  });

  it("미확정 쓰기 결과를 자동 재전송하지 않는다", async () => {
    // Given
    api.executeWrite.mockImplementation(async (request: ServiceRequest) => ({ ...response(request), status: "outcome_unknown", error: { code: "OUTCOME_UNKNOWN", message: "Unknown.", retryable: false, outcome: "unknown", details: null } }));
    const hook = renderHook(useServiceQuery);
    const scenario = async () => { let error; await act(async () => { error = await hook.result.current.execute({ ...input, write: true }).catch((error: unknown) => error); }); return error; };
    // When
    const error = await scenario();
    // Then
    expect(error).toEqual(new Error("OUTCOME_UNKNOWN: Unknown."));
    expect(api.executeWrite).toHaveBeenCalledTimes(1);
  });
});

it("서비스 미지원 실행은 active registry를 비워 legacy Stop 경로를 가리지 않는다", async () => {
  // Given
  api.capabilities.mockResolvedValue({ service_protocol: 1, operations: [] });
  const hook = renderHook(useServiceQuery);
  const scenario = async () => { const result = await execute(hook); return { result, cancelled: hook.result.current.cancel("tab-a") }; };
  // When
  const result = await scenario();
  // Then
  expect(result.result).toBeNull();
  expect(result.cancelled).toBe(false);
  expect(api.call).not.toHaveBeenCalled();
});

it.each(["page", "copyAll"] as const)("같은 연결 새 실행 뒤 이전 %s 실패는 AbortError이며 새 로딩 상태를 덮지 않는다", async (operation) => {
  // Given
  let rejectOld: ((error: Error) => void) | undefined;
  let resolveNew: ((value: ServiceResponse) => void) | undefined;
  let oldStarted: (() => void) | undefined;
  let newStarted: (() => void) | undefined;
  const reading = new Promise<void>((resolve) => { oldStarted = resolve; });
  const executing = new Promise<void>((resolve) => { newStarted = resolve; });
  const firstRequest: ServiceRequest = { protocol_version: 1, operation: "query.execute", request_id: "first", connection_id: "conn-a", session_id: null, map_id: null, expected_version: null, deadline_ms: 30000, input: {} };
  const firstEnvelope = response(firstRequest);
  firstEnvelope.page!.has_more = true;
  const first = snapshotResult(firstEnvelope, { connectionId: "conn-a", driverId: "postgresql", generation: 1, pageSize: 100 });
  api.call.mockImplementation((request: ServiceRequest) => {
    if (request.operation === "result.get") { oldStarted?.(); return new Promise<ServiceResponse>((_resolve, reject) => { rejectOld = reject; }); }
    newStarted?.(); return new Promise<ServiceResponse>((resolve) => { resolveNew = () => resolve(response(request)); });
  });
  const hook = renderHook(useServiceQuery);
  const scenario = async () => {
    const ui = { isLoading: true, error: "" };
    let old: unknown;
    await act(async () => {
      const pending = (operation === "page" ? hook.result.current.page("tab-a", first, 2) : hook.result.current.copyAll("tab-a", first)).catch((error: unknown) => {
        old = error;
        if (!(error instanceof DOMException && error.name === "AbortError")) { ui.error = String(error); ui.isLoading = false; }
      });
      await reading;
      const next = hook.result.current.execute(input);
      await executing;
      rejectOld?.(new Error("old snapshot failed"));
      await pending;
      resolveNew?.(firstEnvelope);
      await next;
    });
    return { old, ui };
  };
  // When
  const result = await scenario();
  // Then
  expect(result.old).toMatchObject({ name: "AbortError" });
  expect(result.ui).toEqual({ isLoading: true, error: "" });
  expect(api.call.mock.calls.filter(([request]) => request.operation === "query.execute")).toHaveLength(1);
});
