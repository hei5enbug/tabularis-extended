import { StrictMode, useState } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { ServiceRequest, ServiceResponse, SlotComponentProps, UsePluginServiceReturn } from "@tabularis/plugin-api";
import SQLServerAuthFields from "./index";
import { CLIENT, CONNECTION_ID, EXTRA, JOB, TENANT, authData, deferred, fixture, response, savedConnection } from "./testFixtures";

const sdk = vi.hoisted(() => ({ service: null as UsePluginServiceReturn | null, connection: vi.fn(() => { throw new Error("Implicit connection forbidden"); }) }));
vi.mock("@tabularis/plugin-api", () => ({
  usePluginService: () => sdk.service,
  usePluginConnection: sdk.connection,
}));

let boundary: ReturnType<typeof fixture>;
beforeEach(() => { boundary = fixture(); sdk.service = boundary.service; sdk.connection.mockClear(); });
afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });

function mount(extra: Record<string, string> = EXTRA, connectionId: string | null = CONNECTION_ID, strict = false) {
  const setExtraField = vi.fn();
  const hidden = vi.fn();
  function Form({ id, initial }: { id: string | null; initial: Record<string, string> }) {
    const [values, setValues] = useState(initial);
    const context = {
      connectionId: id, extra: values, setCredentialFieldsHidden: hidden,
      setExtraField: (key: string, value: string) => { setExtraField(key, value); setValues((current) => ({ ...current, [key]: value })); },
    };
    return <SQLServerAuthFields context={context as SlotComponentProps["context"]} pluginId="sqlserver" />;
  }
  const element = (id: string | null, initial: Record<string, string>) => strict
    ? <StrictMode><Form id={id} initial={initial} /></StrictMode> : <Form id={id} initial={initial} />;
  const view = render(element(connectionId, extra));
  return { ...view, setExtraField, hidden, changeConnection: (id: string | null) => view.rerender(element(id, extra)) };
}

async function settle() { await act(async () => { for (let index = 0; index < 20; index += 1) await Promise.resolve(); }); }
async function click(name: string) { await act(async () => { fireEvent.click(screen.getByRole("button", { name })); }); await settle(); }
async function change(label: string, value: string) { await act(async () => { fireEvent.change(screen.getByLabelText(label), { target: { value } }); }); await settle(); }
async function tick(milliseconds = 1_000) { await act(async () => { await vi.advanceTimersByTimeAsync(milliseconds); }); await settle(); }
async function finish<T>(pending: ReturnType<typeof deferred<T>>, value: T) { await act(async () => pending.resolve(value)); await settle(); }
async function loginAndPoll() { await click("로그인"); await tick(); }
async function tickThenUnmount(view: { unmount: () => void }) { await tick(); const content = document.body.textContent; view.unmount(); await settle(); return content; }
function calls(operation: ServiceRequest["operation"]) { return boundary.call.mock.calls.filter(([request]) => request.operation === operation); }
function disabled(name: string) { return (screen.getByRole("button", { name }) as HTMLButtonElement).disabled; }

test.each([
  ["SQL 암호", "sql_password", false, ""], ["Windows 통합", "integrated", true, "true"],
  ["Entra 사용자", "entra_user", true, ""], ["Entra 서비스 주체", "entra_service_principal", true, ""],
])("%s 모드를 선택하면 저장 필드와 기존 자격 증명 표시를 함께 갱신한다", async (_title, mode, hide, legacy) => {
  // given
  const view = mount({});
  // when
  await change("인증 방식", mode as string);
  // then
  expect(view.setExtraField).toHaveBeenCalledWith("auth_mode", mode);
  expect(view.setExtraField).toHaveBeenCalledWith("integrated_auth", legacy);
  expect(view.hidden).toHaveBeenLastCalledWith(hide);
  expect(calls("auth.begin")).toHaveLength(0);
});

test.each([["기존 통합 인증", { integrated_auth: "true" }, "integrated", true], ["기존 암호 인증", {}, "sql_password", false]])(
  "%s 기본값에서는 Entra 요청을 보내지 않는다", async (_title, extra, mode, hidden) => {
    // given
    const input = extra as Record<string, string>;
    // when
    const view = mount(input);
    // then
    expect((screen.getByLabelText("인증 방식") as HTMLSelectElement).value).toBe(mode);
    expect(view.hidden).toHaveBeenLastCalledWith(hidden);
    expect(boundary.call).not.toHaveBeenCalled();
    expect(boundary.service.capabilities).not.toHaveBeenCalled();
  },
);

test.each([["새 연결", null], ["빈 저장 ID", ""], ["잘못된 저장 ID", "bad\n"]])("%s은 저장 전 인증을 비활성화한다", async (_title, id) => {
  // given
  const connectionId = id;
  // when
  mount(EXTRA, connectionId);
  // then
  expect(screen.getByText("연결을 먼저 저장한 뒤 로그인해 주세요.")).toBeTruthy();
  expect(disabled("로그인")).toBe(true);
  expect(boundary.call).not.toHaveBeenCalled();
});

test.each([["테넌트", { ...EXTRA, tenant_id: "tenant" }], ["클라이언트", { ...EXTRA, client_id: "client" }]])("잘못된 %s UUID로 인증을 호출하지 않는다", (_title, extra) => {
  // given
  const input = extra;
  // when
  mount(input);
  // then
  expect(screen.getByText("테넌트 ID와 클라이언트 ID에 올바른 UUID를 입력해 주세요.")).toBeTruthy();
  expect(disabled("로그인")).toBe(true);
  expect(boundary.call).not.toHaveBeenCalled();
});

test("명시적인 저장 ID만 조회하고 선택된 다른 연결을 사용하지 않는다", async () => {
  // given
  boundary.state.connections = [savedConnection("selected-other"), savedConnection()];
  const view = mount();
  await settle();
  // when
  await click("로그인");
  // then
  expect(boundary.service.capabilities).toHaveBeenCalledWith(CONNECTION_ID);
  expect(boundary.call.mock.calls.every(([request]) => request.connection_id === CONNECTION_ID)).toBe(true);
  expect(calls("auth.begin")[0][0]).toMatchObject({ protocol_version: 1, connection_id: CONNECTION_ID, input: { auth_mode: "entra_user", persistence: "session" }, deadline_ms: 60_000 });
  expect(calls("auth.begin")[0][0].request_id).toMatch(/^[0-9a-f-]{36}$/);
  expect(sdk.connection).not.toHaveBeenCalled();
  expect(view.setExtraField).not.toHaveBeenCalled();
});

test.each([
  ["테넌트", { ...EXTRA, tenant_id: CLIENT }], ["클라이언트", { ...EXTRA, client_id: TENANT }],
  ["인증 방식", { ...EXTRA, auth_mode: "entra_service_principal" }],
])("저장 안 된 %s 설정에서는 로그인과 비밀 등록이 비활성화된다", async (_title, extra) => {
  // given
  mount(extra);
  // when
  await settle();
  // then
  expect(screen.getByText("현재 인증 설정을 연결에 저장한 뒤 로그인해 주세요.")).toBeTruthy();
  expect(disabled("로그인")).toBe(true);
  expect(calls("auth.begin")).toHaveLength(0);
  expect(boundary.importCredential).not.toHaveBeenCalled();
});

test("같은 ID의 다른 드라이버에는 인증을 시작하지 않는다", async () => {
  // given
  boundary.state.handle = (request) => request.operation === "connection.list"
    ? Promise.resolve(response(request.request_id, { connections: [{ ...savedConnection(), driver: "cosmos" }] })) : null;
  mount();
  // when
  await settle();
  // then
  expect(disabled("로그인")).toBe(true);
  expect(calls("auth.begin")).toHaveLength(0);
});

test.each(["auth.begin", "auth.status", "auth.cancel", "auth.logout", "connection.list"] as const)("호스트의 %s 기능이 없으면 인증을 시작하지 않는다", async (missing) => {
  // given
  boundary.state.caps.operations = boundary.state.caps.operations.filter((operation) => operation !== missing);
  mount();
  // when
  await settle();
  // then
  expect(screen.getByText("호스트에서 공통 인증 기능을 사용할 수 없습니다.")).toBeTruthy();
  expect(disabled("로그인")).toBe(true);
  expect(boundary.call).not.toHaveBeenCalled();
});

test("프로토콜이 맞지 않으면 인증을 시작하지 않는다", async () => {
  // given
  boundary.service.capabilities = vi.fn(async () => ({ ...boundary.state.caps, service_protocol: 2 })) as UsePluginServiceReturn["capabilities"];
  mount();
  // when
  await settle();
  // then
  expect(disabled("로그인")).toBe(true);
  expect(boundary.call).not.toHaveBeenCalled();
});

test("연결 조회의 권한 오류는 원문 없이 안내하고 인증을 비활성화한다", async () => {
  // given
  boundary.state.handle = (request) => Promise.resolve({ ...response(request.request_id, null), status: "failed", error: { code: "PERMISSION_DENIED", message: "SECRET_CANARY", retryable: false, outcome: "not_applied", details: null } });
  mount();
  // when
  await settle();
  // then
  expect(screen.getByText("이 연결의 인증을 관리할 권한이 없습니다.")).toBeTruthy();
  expect(disabled("로그인")).toBe(true);
  expect(document.body.textContent).not.toContain("SECRET_CANARY");
});

test("키체인 보관을 명시적으로 선택하여 로그인을 시작한다", async () => {
  // given
  mount(); await settle(); await change("인증 정보 보관", "keychain");
  // when
  await click("로그인");
  // then
  expect(calls("auth.begin")[0][0].input).toEqual({ auth_mode: "entra_user", persistence: "keychain" });
});

test.each(["session", "keychain"] as const)("서비스 주체 비밀을 %s 경로로 보내기 전에 DOM에서 지운다", async (persistence) => {
  // given
  const extra = { ...EXTRA, auth_mode: "entra_service_principal" };
  boundary.state.connections = [savedConnection(CONNECTION_ID, extra)];
  const view = mount(extra); await settle(); await change("인증 정보 보관", persistence);
  const storage = vi.spyOn(Storage.prototype, "setItem");
  const secret = screen.getByLabelText("클라이언트 비밀") as HTMLInputElement;
  const observed: string[] = [];
  boundary.importCredential.mockImplementation(async (id) => { observed.push(secret.value); return response("credential", { credential_ref: "opaque" }, id); });
  fireEvent.change(secret, { target: { value: "SECRET_CANARY" } });
  // when
  await click("비밀 등록");
  // then
  expect(observed).toEqual([""]);
  expect(boundary.importCredential).toHaveBeenCalledWith(CONNECTION_ID, "entra_client_secret", "SECRET_CANARY", { persistence, signal: expect.any(AbortSignal) });
  expect(secret.value).toBe("");
  expect(view.setExtraField).not.toHaveBeenCalled();
  expect(storage).not.toHaveBeenCalled();
  expect(document.body.textContent).not.toContain("SECRET_CANARY");
  expect(disabled("로그인")).toBe(false);
});

test("서비스 주체는 보호된 비밀 등록을 완료한 뒤에만 로그인할 수 있다", async () => {
  // given
  const extra = { ...EXTRA, auth_mode: "entra_service_principal" };
  boundary.state.connections = [savedConnection(CONNECTION_ID, extra)];
  mount(extra); await settle();
  // when
  await click("로그인");
  // then
  expect(disabled("로그인")).toBe(true);
  expect(calls("auth.begin")).toHaveLength(0);
});

test("서비스 주체 비밀 등록 후 같은 저장 연결의 로그인을 시작한다", async () => {
  // given
  const extra = { ...EXTRA, auth_mode: "entra_service_principal" };
  boundary.state.connections = [savedConnection(CONNECTION_ID, extra)];
  mount(extra); await settle(); fireEvent.change(screen.getByLabelText("클라이언트 비밀"), { target: { value: "SECRET_CANARY" } });
  await click("비밀 등록");
  // when
  await click("로그인");
  // then
  expect(boundary.importCredential).toHaveBeenCalledTimes(1);
  expect(calls("auth.begin")[0][0]).toMatchObject({ connection_id: CONNECTION_ID, input: { auth_mode: "entra_service_principal", persistence: "session" } });
  expect(JSON.stringify(calls("auth.begin")[0][0])).not.toContain("SECRET_CANARY");
});

test("보관 방식을 바꾸면 서비스 주체의 비밀을 다시 등록해야 한다", async () => {
  // given
  const extra = { ...EXTRA, auth_mode: "entra_service_principal" };
  boundary.state.connections = [savedConnection(CONNECTION_ID, extra)];
  mount(extra); await settle(); fireEvent.change(screen.getByLabelText("클라이언트 비밀"), { target: { value: "SECRET_CANARY" } });
  await click("비밀 등록");
  // when
  await change("인증 정보 보관", "keychain");
  // then
  expect(disabled("로그인")).toBe(true);
  expect(calls("auth.begin")).toHaveLength(0);
});

test("서비스 주체의 비밀 전달 기능이 없으면 등록과 로그인 모두 비활성화된다", async () => {
  // given
  const extra = { ...EXTRA, auth_mode: "entra_service_principal" };
  boundary.state.connections = [savedConnection(CONNECTION_ID, extra)];
  boundary.state.caps.operations = boundary.state.caps.operations.filter((operation) => operation !== "credential.import");
  mount(extra);
  // when
  await settle();
  // then
  expect(disabled("비밀 등록")).toBe(true);
  expect(disabled("로그인")).toBe(true);
  expect(boundary.importCredential).not.toHaveBeenCalled();
});

test("비밀 등록 실패는 비밀 원문 없이 표시하고 로그인하지 않는다", async () => {
  // given
  const extra = { ...EXTRA, auth_mode: "entra_service_principal" };
  boundary.state.connections = [savedConnection(CONNECTION_ID, extra)];
  boundary.importCredential.mockRejectedValue(new Error("SECRET_CANARY"));
  mount(extra); await settle(); fireEvent.change(screen.getByLabelText("클라이언트 비밀"), { target: { value: "SECRET_CANARY" } });
  // when
  await click("비밀 등록");
  // then
  expect(screen.getByRole("alert").textContent).toBe("인증 요청을 완료하지 못했습니다.");
  expect((screen.getByLabelText("클라이언트 비밀") as HTMLInputElement).value).toBe("");
  expect(document.body.textContent).not.toContain("SECRET_CANARY");
  expect(disabled("로그인")).toBe(true);
  expect(calls("auth.begin")).toHaveLength(0);
});

test("공식 MFA 안내 주소와 제한된 로그인 코드만 표시한다", async () => {
  // given
  boundary.state.begin = { ...authData("interaction_required"), challenge: { verification_uri: "https://microsoft.com/devicelogin", user_code: "ABCD-EFGH" } };
  mount(); await settle();
  // when
  await click("로그인");
  // then
  expect(screen.getByRole("link").getAttribute("href")).toBe("https://microsoft.com/devicelogin");
  expect(screen.getByText("ABCD-EFGH")).toBeTruthy();
  expect(calls("auth.begin")).toHaveLength(1);
});

test("잘못된 MFA 주소는 링크를 만들지 않는다", async () => {
  // given
  boundary.state.begin = { ...authData("interaction_required"), challenge: { verification_uri: "https://microsoft.com:443/devicelogin", user_code: "CODE" } };
  mount(); await settle();
  // when
  await click("로그인");
  // then
  expect(screen.queryByRole("link")).toBeNull();
  expect(screen.getByText("안전한 로그인 주소를 확인할 수 없습니다. 로그인을 취소해 주세요.")).toBeTruthy();
});

test("1초마다 상태를 확인하고 인증 완료 시 폴링을 멈춘다", async () => {
  // given
  vi.useFakeTimers(); mount(); await settle();
  // when
  await loginAndPoll();
  // then
  expect(calls("auth.status")).toHaveLength(1);
  expect(calls("auth.status")[0][0].input).toEqual({ auth_job_id: JOB });
  expect(screen.getByText("로그인했습니다.")).toBeTruthy();
});

test("상태 확인이 실패하면 자동 폴링과 로그인 재전송을 멈춘다", async () => {
  // given
  vi.useFakeTimers();
  boundary.state.handle = (request) => request.operation === "auth.status" ? Promise.reject(new Error("SECRET_CANARY")) : null;
  mount(); await settle(); await click("로그인");
  // when
  await tick(10_000);
  // then
  expect(calls("auth.status")).toHaveLength(1);
  expect(calls("auth.begin")).toHaveLength(1);
  expect(screen.getByRole("alert").textContent).toBe("인증 요청을 완료하지 못했습니다.");
  expect(document.body.textContent).not.toContain("SECRET_CANARY");
});

test("15분의 로그인 대기 상한을 넘으면 폴링을 멈추고 작업 취소를 요청한다", async () => {
  // given
  vi.useFakeTimers(); boundary.state.status = authData("pending");
  mount(); await settle(); await click("로그인");
  vi.setSystemTime(Date.now() + 15 * 60_000);
  // when
  await tick(10_000);
  // then
  expect(calls("auth.status")).toHaveLength(0);
  expect(calls("auth.cancel")).toHaveLength(1);
  expect(screen.getByRole("alert").textContent).toBe("로그인 대기 시간이 끝났습니다. 취소 후 다시 시작해 주세요.");
  expect(calls("auth.begin")).toHaveLength(1);
});

test("서로 다른 작업 ID의 폴링 응답을 거부하고 원래 작업만 취소한다", async () => {
  // given
  vi.useFakeTimers(); boundary.state.status = authData("authenticated", "other-job");
  const view = mount(); await settle(); await click("로그인");
  // when
  const content = await tickThenUnmount(view);
  // then
  expect(screen.queryByText("로그인했습니다.")).toBeNull();
  expect(content).toContain("인증 응답을 확인할 수 없습니다.");
  expect(calls("auth.cancel")[0][0].input).toEqual({ auth_job_id: JOB });
  expect(calls("auth.cancel").some(([request]) => "auth_job_id" in request.input && request.input.auth_job_id === "other-job")).toBe(false);

});

test("성공 envelope의 실패 인증 상태를 로그인 성공으로 표시하지 않는다", async () => {
  // given
  boundary.state.begin = { ...authData("failed"), error_code: "AUTH_EXPIRED" };
  mount(); await settle();
  // when
  await click("로그인");
  // then
  expect(screen.getByRole("alert").textContent).toBe("인증 시간이 만료되었습니다. 다시 로그인을 시작해 주세요.");
  expect(screen.queryByText("로그인했습니다.")).toBeNull();
  expect(calls("auth.status")).toHaveLength(0);
});

test("인증 시작 권한 거부를 짧은 안내로 표시하고 다시 전송하지 않는다", async () => {
  // given
  boundary.state.handle = (request) => request.operation === "auth.begin" ? Promise.resolve({ ...response(request.request_id, null), status: "failed",
    error: { code: "PERMISSION_DENIED", message: "SECRET_CANARY", outcome: "not_applied", retryable: false, details: null } }) : null;
  mount(); await settle();
  // when
  await click("로그인");
  // then
  expect(screen.getByRole("alert").textContent).toBe("이 연결의 인증을 관리할 권한이 없습니다.");
  expect(calls("auth.begin")).toHaveLength(1);
  expect(disabled("로그인")).toBe(true);
  expect(disabled("로그아웃")).toBe(true);
  expect(document.body.textContent).not.toContain("SECRET_CANARY");
});

test.each([
  ["알 수 없는 상태", { ...authData("pending"), state: "success" }],
  ["없는 작업 ID", { state: "authenticated", user_action_required: false, error_code: null }],
  ["잘못된 사용자 동작", { ...authData("authenticated"), user_action_required: "false" }],
  ["실패 코드가 있는 완료", { ...authData("authenticated"), error_code: "AUTH_REQUIRED" }],
  ["사용자 동작이 남은 완료", { ...authData("authenticated"), user_action_required: true }],
  ["제어 문자 코드", { ...authData("interaction_required"), challenge: { verification_uri: "https://microsoft.com/devicelogin", user_code: "CODE\n" } }],
])("%s 응답을 로그인 성공으로 표시하지 않는다", async (_title, data) => {
  // given
  boundary.state.begin = data; mount(); await settle();
  // when
  await click("로그인");
  // then
  expect(screen.getByRole("alert").textContent).toBe("인증 응답을 확인할 수 없습니다.");
  expect(screen.queryByText("로그인했습니다.")).toBeNull();
});

test("진행 중인 인증 작업을 명시적 ID로 취소한다", async () => {
  // given
  mount(); await settle(); await click("로그인");
  // when
  await click("로그인 취소");
  // then
  expect(calls("auth.cancel")).toHaveLength(1);
  expect(calls("auth.cancel")[0][0]).toMatchObject({ connection_id: CONNECTION_ID, input: { auth_job_id: JOB } });
  expect(screen.getByText("로그인을 취소했습니다.")).toBeTruthy();
});

test("취소가 이미 완료된 인증을 반환하면 취소 성공으로 바꾸지 않는다", async () => {
  // given
  boundary.state.cancel = authData("authenticated"); mount(); await settle(); await click("로그인");
  // when
  await click("로그인 취소");
  // then
  expect(screen.getByText("로그인했습니다.")).toBeTruthy();
  expect(screen.queryByText("로그인을 취소했습니다.")).toBeNull();
});

test("로그아웃 완료 응답 전에는 성공을 표시하지 않는다", async () => {
  // given
  boundary.state.begin = authData("authenticated");
  const pending = deferred<ServiceResponse>();
  boundary.state.handle = (request) => request.operation === "auth.logout" ? pending.promise : null;
  mount(); await settle(); await click("로그인");
  // when
  await click("로그아웃");
  // then
  expect(screen.queryByText("로그아웃했습니다.")).toBeNull();
  expect(screen.queryByText("로그인했습니다.")).toBeNull();
  expect(calls("auth.logout")).toHaveLength(1);
});

test("명시적인 로그아웃 확인 후에만 성공을 표시한다", async () => {
  // given
  mount(); await settle();
  // when
  await click("로그아웃");
  // then
  expect(screen.getByText("로그아웃했습니다.")).toBeTruthy();
  expect(calls("auth.logout")[0][0].input).toEqual({});
});

test.each([false, "true", null])("잘못된 로그아웃 확인 %s을 성공으로 표시하지 않는다", async (loggedOut) => {
  // given
  boundary.state.logout = { logged_out: loggedOut }; mount(); await settle();
  // when
  await click("로그아웃");
  // then
  expect(screen.queryByText("로그아웃했습니다.")).toBeNull();
  expect(screen.getByRole("alert").textContent).toBe("인증 응답을 확인할 수 없습니다.");
});

test.each(["outcome_unknown", "failed"] as const)("%s 로그인 응답을 자동 재전송하거나 성공으로 표시하지 않는다", async (state) => {
  // given
  boundary.state.handle = (request) => request.operation === "auth.begin" ? Promise.resolve({ ...response(request.request_id, authData("authenticated")), status: state,
    error: { code: "OUTCOME_UNKNOWN", message: "SECRET_CANARY", outcome: state === "failed" ? "applied" : "unknown", retryable: false, details: null } }) : null;
  mount(); await settle();
  // when
  await click("로그인");
  // then
  expect(calls("auth.begin")).toHaveLength(1);
  expect(screen.queryByText("로그인했습니다.")).toBeNull();
  expect(document.body.textContent).not.toContain("SECRET_CANARY");
  expect(screen.getByRole("alert")).toBeTruthy();
});

test("연결 변경 뒤 도착한 시작 응답은 이전 연결의 작업을 취소한다", async () => {
  // given
  const pending = deferred<ServiceResponse>();
  boundary.state.handle = (request) => request.operation === "auth.begin" ? pending.promise : null;
  const view = mount(); await settle(); await click("로그인");
  const outgoing = calls("auth.begin")[0][0];
  await act(async () => view.changeConnection("new-connection")); await settle();
  // when
  await finish(pending, response(outgoing.request_id, authData("authenticated")));
  // then
  expect(calls("auth.begin")[0][1]?.signal?.aborted).toBe(true);
  expect(calls("auth.cancel")[0][0]).toMatchObject({ connection_id: CONNECTION_ID, input: { auth_job_id: JOB } });
  expect(screen.queryByText("로그인했습니다.")).toBeNull();
});

test("인증 설정 변경은 진행 중 작업을 취소하고 다시 저장하도록 안내한다", async () => {
  // given
  mount(); await settle(); await click("로그인");
  // when
  await change("테넌트 ID", CLIENT);
  // then
  expect(calls("auth.cancel")[0][0].connection_id).toBe(CONNECTION_ID);
  expect(screen.getByText("현재 인증 설정을 연결에 저장한 뒤 로그인해 주세요.")).toBeTruthy();
  expect(disabled("로그인")).toBe(true);
});

test("언마운트 후 늦게 도착한 작업 ID도 취소한다", async () => {
  // given
  const pending = deferred<ServiceResponse>();
  boundary.state.handle = (request) => request.operation === "auth.begin" ? pending.promise : null;
  const view = mount(); await settle(); await click("로그인");
  const outgoing = calls("auth.begin")[0][0]; view.unmount();
  // when
  await finish(pending, response(outgoing.request_id, authData("pending")));
  // then
  expect(calls("auth.cancel")).toHaveLength(1);
  expect(calls("auth.cancel")[0][0].connection_id).toBe(CONNECTION_ID);
  expect(calls("auth.begin")[0][1]?.signal?.aborted).toBe(true);
});

test("시작 요청을 취소한 뒤 도착한 작업 ID를 다시 취소한다", async () => {
  // given
  const pending = deferred<ServiceResponse>();
  boundary.state.handle = (request) => request.operation === "auth.begin" ? pending.promise : null;
  mount(); await settle(); await click("로그인");
  const outgoing = calls("auth.begin")[0][0]; await click("로그인 취소");
  // when
  await finish(pending, response(outgoing.request_id, authData("authenticated")));
  // then
  expect(calls("auth.cancel")).toHaveLength(1);
  expect(calls("auth.begin")[0][1]?.signal?.aborted).toBe(true);
  expect(screen.queryByText("로그인했습니다.")).toBeNull();
});

test("이전 연결 요청의 늦은 오류는 새 패널에 표시하지 않는다", async () => {
  // given
  const pending = deferred<ServiceResponse>();
  boundary.state.handle = (request) => request.operation === "auth.begin" ? pending.promise : null;
  const view = mount(); await settle(); await click("로그인");
  await act(async () => view.changeConnection("new-connection")); await settle();
  // when
  await act(async () => pending.reject(new Error("SECRET_CANARY")));
  // then
  expect(screen.queryByRole("alert")).toBeNull();
  expect(document.body.textContent).not.toContain("SECRET_CANARY");
});

test("연결 변경은 보호된 비밀 등록을 중단하고 늦은 성공을 버린다", async () => {
  // given
  const extra = { ...EXTRA, auth_mode: "entra_service_principal" };
  boundary.state.connections = [savedConnection(CONNECTION_ID, extra)];
  const pending = deferred<ServiceResponse>(); boundary.importCredential.mockImplementation(() => pending.promise);
  const view = mount(extra); await settle(); fireEvent.change(screen.getByLabelText("클라이언트 비밀"), { target: { value: "SECRET_CANARY" } });
  await click("비밀 등록"); await act(async () => view.changeConnection("new-connection")); await settle();
  // when
  await finish(pending, response("credential", { credential_ref: "opaque" }));
  // then
  expect(boundary.importCredential.mock.calls[0][3].signal?.aborted).toBe(true);
  expect(screen.queryByText("클라이언트 비밀을 보호된 경로로 전달했습니다.")).toBeNull();
  expect(disabled("로그인")).toBe(true);
  expect(document.body.textContent).not.toContain("SECRET_CANARY");
});

test("늦은 capability 응답은 바뀐 연결에서 인증을 활성화하지 않는다", async () => {
  // given
  const pending = deferred<Awaited<ReturnType<UsePluginServiceReturn["capabilities"]>>>();
  boundary.service.capabilities = vi.fn((id) => id === CONNECTION_ID ? pending.promise : Promise.resolve(boundary.state.caps));
  const view = mount(); await act(async () => view.changeConnection("new-connection")); await settle();
  // when
  await finish(pending, boundary.state.caps);
  // then
  expect(calls("connection.list").every(([request]) => request.connection_id === "new-connection")).toBe(true);
  expect(disabled("로그인")).toBe(true);
});

test("엄격 모드의 effect 재시작 뒤에도 현재 패널을 활성화한다", async () => {
  // given
  mount(EXTRA, CONNECTION_ID, true);
  // when
  await settle();
  // then
  expect(disabled("로그인")).toBe(false);
  expect(screen.getByText("저장된 연결의 인증 설정을 확인했습니다.")).toBeTruthy();
});


test("Azure CLI를 선택하면 공식 클라이언트를 지정하고 직접 변경을 막는다", async () => {
  // given
  const view = mount();
  // when
  await change("로그인 소스", "azure_cli");
  // then
  expect(view.setExtraField).toHaveBeenCalledWith("auth_source", "azure_cli");
  expect(view.setExtraField).toHaveBeenCalledWith("client_id", "04b07795-8ddb-461a-bbee-02f9e1bf7b46");
  expect((screen.getByLabelText("클라이언트 ID") as HTMLInputElement).readOnly).toBe(true);
  expect(screen.queryByRole("textbox", { name: "클라이언트 비밀" })).toBeNull();
});
