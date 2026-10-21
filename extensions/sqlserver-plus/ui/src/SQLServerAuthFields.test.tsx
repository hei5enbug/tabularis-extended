import { useCallback, useState } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SlotComponentProps } from "@tabularis/plugin-api";
import SQLServerAuthFields from "./index";
import { DRIVER_ID } from "./auth";
import {
  CLI_PATH, ENTRA_EXTRA, ENTRA_USERNAME, INTEGRATED_EXTRA, LEGACY_INTEGRATED_EXTRA,
  OBJECT_ID, SQL_EXTRA, SQL_PASSWORD, SQL_USERNAME, TENANT,
} from "./testFixtures";

afterEach(() => { cleanup(); });

interface HostOptions {
  extra?: Record<string, string>;
  username?: string;
  password?: string;
  passwordToggle?: boolean;
  driver?: string;
}

function renderHost({ extra = {}, username = "", password = "", passwordToggle = true, driver = DRIVER_ID }: HostOptions = {}) {
  const spies = { setExtraField: vi.fn(), setCredentialFieldsHidden: vi.fn(), setPasswordFieldHidden: vi.fn() };
  const current = { extra: { ...extra }, username, password };
  let resetFlags: () => void = () => {};

  function Host() {
    const [values, setValues] = useState<Record<string, string>>(() => ({ ...extra }));
    const [login, setLogin] = useState({ username, password });
    const [flags, setFlags] = useState({ credentials: false, password: false });
    resetFlags = () => setFlags({ credentials: false, password: false });
    Object.assign(current, { extra: values, ...login });

    const setExtraField = useCallback((key: string, value: string) => {
      spies.setExtraField(key, value);
      setValues((previous) => {
        const next = { ...previous };
        if (value === "") delete next[key];
        else next[key] = value;
        return next;
      });
    }, []);
    const setCredentialFieldsHidden = useCallback((hidden: boolean) => {
      spies.setCredentialFieldsHidden(hidden);
      setFlags((previous) => ({ ...previous, credentials: hidden }));
      if (hidden) setLogin({ username: "", password: "" });
    }, []);
    const setPasswordFieldHidden = useCallback((hidden: boolean) => {
      spies.setPasswordFieldHidden(hidden);
      setFlags((previous) => ({ ...previous, password: hidden }));
      if (hidden) setLogin((previous) => ({ ...previous, password: "" }));
    }, []);

    const context = {
      driver,
      extra: values,
      setExtraField,
      credentialFieldsHidden: flags.credentials,
      setCredentialFieldsHidden,
      ...(passwordToggle ? { passwordFieldHidden: flags.password, setPasswordFieldHidden } : {}),
    };
    return <>
      <SQLServerAuthFields context={context as SlotComponentProps["context"]} pluginId={DRIVER_ID} />
      {!flags.credentials && <label>사용자 이름<input
        value={login.username}
        onChange={(event) => setLogin((previous) => ({ ...previous, username: event.target.value }))}
      /></label>}
      {!flags.credentials && !flags.password && <label>암호<input
        type="password"
        value={login.password}
        onChange={(event) => setLogin((previous) => ({ ...previous, password: event.target.value }))}
      /></label>}
    </>;
  }

  render(<Host />);
  return { spies, current, reinitialise: () => act(() => resetFlags()) };
}

function chooseMode(mode: string) {
  fireEvent.change(screen.getByLabelText("인증 방식"), { target: { value: mode } });
}

function input(label: string) {
  return screen.getByLabelText(label) as HTMLInputElement;
}

function hiddenOnce(spy: ReturnType<typeof vi.fn>) {
  return spy.mock.calls.some(([hidden]) => hidden === true);
}

describe("기존 연결을 불러오면 저장된 값을 지키고 호스트 입력란만 인증 방식에 맞춘다", () => {
  it("Entra 연결을 불러오면 사용자 이름과 Entra 식별 정보를 그대로 두고 암호 입력란만 숨긴다", () => {
    // given
    const saved = { extra: ENTRA_EXTRA, username: ENTRA_USERNAME };
    // when
    const host = renderHost(saved);
    // then
    expect(host.spies.setExtraField).not.toHaveBeenCalled();
    expect(host.current.extra).toEqual(ENTRA_EXTRA);
    expect(host.current.username).toBe(ENTRA_USERNAME);
    expect(input("사용자 이름").value).toBe(ENTRA_USERNAME);
    expect(screen.queryByLabelText("암호")).toBeNull();
    expect(input("테넌트 ID").value).toBe(TENANT);
    expect(input("개체 ID").value).toBe(OBJECT_ID);
    expect(input("Azure CLI 경로 (선택)").value).toBe(CLI_PATH);
    expect(hiddenOnce(host.spies.setCredentialFieldsHidden)).toBe(false);
  });

  it("SQL Server 인증 연결을 불러오면 사용자 이름과 암호를 그대로 두고 두 입력란을 모두 보여 준다", () => {
    // given
    const saved = { extra: SQL_EXTRA, username: SQL_USERNAME, password: SQL_PASSWORD };
    // when
    const host = renderHost(saved);
    // then
    expect(host.spies.setExtraField).not.toHaveBeenCalled();
    expect(input("사용자 이름").value).toBe(SQL_USERNAME);
    expect(input("암호").value).toBe(SQL_PASSWORD);
    expect(hiddenOnce(host.spies.setCredentialFieldsHidden)).toBe(false);
    expect(host.spies.setPasswordFieldHidden).not.toHaveBeenCalled();
  });

  it("auth_mode가 없는 기존 통합 인증 연결을 불러오면 저장 값을 바꾸지 않고 사용자 이름과 암호 입력란을 숨긴다", () => {
    // given
    const saved = { extra: LEGACY_INTEGRATED_EXTRA };
    // when
    const host = renderHost(saved);
    // then
    expect((screen.getByLabelText("인증 방식") as HTMLSelectElement).value).toBe("integrated");
    expect(host.spies.setExtraField).not.toHaveBeenCalled();
    expect(host.current.extra).toEqual(LEGACY_INTEGRATED_EXTRA);
    expect(screen.queryByLabelText("사용자 이름")).toBeNull();
    expect(screen.queryByLabelText("암호")).toBeNull();
  });

  it.each([
    ["SQL Server 인증", SQL_EXTRA, false],
    ["Windows/Kerberos 통합 인증", INTEGRATED_EXTRA, false],
    ["Microsoft Entra 사용자", ENTRA_EXTRA, true],
  ])("%s 연결에서 암호 입력란만 숨기는 요청은 Entra일 때만 보낸다", (_title, extra, expected) => {
    // given
    const saved = { extra, username: "" };
    // when
    const host = renderHost(saved);
    // then
    expect(hiddenOnce(host.spies.setPasswordFieldHidden)).toBe(expected);
  });

  it("호스트가 입력란 상태를 다시 초기화해도 Entra 연결의 암호 입력란을 다시 숨긴다", () => {
    // given
    const host = renderHost({ extra: ENTRA_EXTRA, username: ENTRA_USERNAME });
    // when
    host.reinitialise();
    // then
    expect(screen.queryByLabelText("암호")).toBeNull();
    expect(input("사용자 이름").value).toBe(ENTRA_USERNAME);
    expect(host.spies.setPasswordFieldHidden).toHaveBeenLastCalledWith(true);
  });

  it("지원하지 않는 인증 방식이 저장된 연결은 값을 바꾸지 않고 다시 선택하라고 알린다", () => {
    // given
    const saved = { extra: { auth_mode: "device_code", tenant_id: TENANT }, username: ENTRA_USERNAME };
    // when
    const host = renderHost(saved);
    // then
    expect(screen.getByRole("alert").textContent).toContain("저장된 인증 방식은 지원하지 않습니다.");
    expect(host.spies.setExtraField).not.toHaveBeenCalled();
    expect(host.current.username).toBe(ENTRA_USERNAME);
  });
});

describe("인증 방식을 직접 바꾸면 고른 방식과 맞지 않는 값만 지운다", () => {
  it("SQL Server 인증에서 Entra로 바꾸면 사용자 이름은 남기고 암호만 지운다", () => {
    // given
    const host = renderHost({ extra: SQL_EXTRA, username: SQL_USERNAME, password: SQL_PASSWORD });
    // when
    chooseMode("entra_user");
    // then
    expect(host.current.extra).toEqual({ auth_mode: "entra_user", auth_source: "azure_cli" });
    expect(host.current.username).toBe(SQL_USERNAME);
    expect(host.current.password).toBe("");
    expect(input("사용자 이름").value).toBe(SQL_USERNAME);
    expect(screen.queryByLabelText("암호")).toBeNull();
    expect(input("테넌트 ID").value).toBe("");
  });

  it("통합 인증에서 Entra로 바꾸면 통합 인증 설정을 지우고 사용자 이름 입력란을 다시 보여 준다", () => {
    // given
    const host = renderHost({ extra: INTEGRATED_EXTRA });
    // when
    chooseMode("entra_user");
    // then
    expect(host.current.extra).toEqual({ auth_mode: "entra_user", auth_source: "azure_cli" });
    expect(screen.getByLabelText("사용자 이름")).toBeTruthy();
    expect(screen.queryByLabelText("암호")).toBeNull();
  });

  it("Entra에서 SQL Server 인증으로 바꾸면 Entra 전용 키를 지우고 암호 입력란을 되돌린다", () => {
    // given
    const host = renderHost({ extra: ENTRA_EXTRA, username: ENTRA_USERNAME });
    // when
    chooseMode("sql_password");
    // then
    expect(host.current.extra).toEqual({ auth_mode: "sql_password" });
    expect(input("사용자 이름").value).toBe(ENTRA_USERNAME);
    expect(screen.getByLabelText("암호")).toBeTruthy();
    expect(host.spies.setPasswordFieldHidden).toHaveBeenLastCalledWith(false);
    expect(screen.queryByLabelText("테넌트 ID")).toBeNull();
  });

  it("Entra에서 Windows/Kerberos로 바꾸면 Entra 전용 키를 지우고 사용자 이름과 암호 입력란을 숨긴다", () => {
    // given
    const host = renderHost({ extra: ENTRA_EXTRA, username: ENTRA_USERNAME });
    // when
    chooseMode("integrated");
    // then
    expect(host.current.extra).toEqual({ auth_mode: "integrated", integrated_auth: "true" });
    expect(host.current.username).toBe("");
    expect(screen.queryByLabelText("사용자 이름")).toBeNull();
    expect(screen.queryByLabelText("암호")).toBeNull();
  });

  it.each([
    ["SQL Server 인증", SQL_EXTRA, "Microsoft Entra로 바꾸면 암호가 지워집니다."],
    ["Windows/Kerberos 통합 인증", INTEGRATED_EXTRA, "Microsoft Entra로 바꾸면 Windows 통합 인증 설정이 지워집니다."],
    ["Microsoft Entra 사용자", ENTRA_EXTRA, "다른 방식으로 바꾸면 로그인 소스, 테넌트 ID, 개체 ID, Azure CLI 경로가 지워집니다."],
  ])("%s을 선택한 상태에서는 다른 방식으로 바꿀 때 지워지는 값을 안내한다", (_title, extra, note) => {
    // given
    const saved = { extra };
    // when
    renderHost(saved);
    // then
    expect(screen.getByLabelText("인증 방식").getAttribute("aria-describedby")).toBeTruthy();
    expect(document.body.textContent).toContain(note);
  });
});

describe("Entra 입력란은 Azure CLI 로그인 안내와 드라이버 규칙을 보여 준다", () => {
  it("Entra를 고르면 앱 밖의 az login 안내와 호스트, TLS 연결 조건을 보여 준다", () => {
    // given
    const saved = { extra: ENTRA_EXTRA, username: ENTRA_USERNAME };
    // when
    renderHost(saved);
    // then
    const text = document.body.textContent ?? "";
    expect(text).toContain("로그인과 MFA는 앱 밖의 터미널에서 az login --tenant <테넌트 ID>로");
    expect(text).toContain(".database.windows.net으로 끝나는 Azure SQL 서버");
    expect(text).toContain("SSL 모드는 verify-full이어야 합니다.");
    expect(text).toContain("Trust Server Certificate는 꺼져 있어야 합니다.");
  });

  it("테넌트 ID를 입력하면 extra의 tenant_id로 기록한다", () => {
    // given
    const host = renderHost({ extra: { auth_mode: "entra_user", auth_source: "azure_cli" } });
    // when
    fireEvent.change(input("테넌트 ID"), { target: { value: TENANT } });
    // then
    expect(host.spies.setExtraField).toHaveBeenCalledWith("tenant_id", TENANT);
    expect(host.current.extra.tenant_id).toBe(TENANT);
  });

  it("Azure CLI 경로를 비우면 extra에서 azure_cli_path를 지운다", () => {
    // given
    const host = renderHost({ extra: ENTRA_EXTRA });
    // when
    fireEvent.change(input("Azure CLI 경로 (선택)"), { target: { value: "" } });
    // then
    expect("azure_cli_path" in host.current.extra).toBe(false);
  });

  it("필수 값이 없거나 경로가 절대 경로가 아니면 해당 입력란 아래에 안내 문구를 보여 준다", () => {
    // given
    const saved = { extra: { auth_mode: "entra_user", auth_source: "azure_cli", azure_cli_path: "bin/az" } };
    // when
    renderHost(saved);
    // then
    expect(screen.getByText("테넌트 ID를 입력하세요.")).toBeTruthy();
    expect(screen.getByText("Entra 개체 ID를 입력하세요.")).toBeTruthy();
    expect(screen.getByText("Azure CLI 경로는 앞뒤 공백 없는 절대 경로여야 합니다. 비워 두면 PATH에서 az를 찾습니다.")).toBeTruthy();
    expect(input("테넌트 ID").getAttribute("aria-invalid")).toBe("true");
    expect(input("Azure CLI 경로 (선택)").getAttribute("aria-invalid")).toBe("true");
  });

  it("형식이 틀린 테넌트 ID를 입력하면 허용 문자를 안내한다", () => {
    // given
    renderHost({ extra: ENTRA_EXTRA });
    // when
    fireEvent.change(input("테넌트 ID"), { target: { value: "-tenant" } });
    // then
    expect(screen.getByText("테넌트 ID에는 영문자, 숫자, 하이픈(-), 마침표(.)만 쓸 수 있고 하이픈으로 시작할 수 없습니다.")).toBeTruthy();
  });

  it("올바른 Entra 설정에는 오류 안내를 보여 주지 않는다", () => {
    // given
    const saved = { extra: ENTRA_EXTRA, username: ENTRA_USERNAME };
    // when
    renderHost(saved);
    // then
    expect(screen.queryByRole("alert")).toBeNull();
    expect(input("테넌트 ID").getAttribute("aria-invalid")).toBeNull();
  });
});

describe("드라이버와 맞지 않는 저장 값은 사용자가 고를 때만 지운다", () => {
  it.each([
    ["통합 인증이 켜진 Entra 연결", { ...ENTRA_EXTRA, integrated_auth: "true" }, "Windows 통합 인증 설정 지우기"],
    ["로그인 소스가 Azure CLI가 아닌 Entra 연결", { ...ENTRA_EXTRA, auth_source: "browser" }, "Azure CLI로 설정"],
    ["Entra 전용 키가 남은 SQL Server 인증 연결", { ...SQL_EXTRA, tenant_id: TENANT }, "Entra 설정 지우기"],
    ["통합 인증이 켜진 SQL Server 인증 연결", { ...SQL_EXTRA, integrated_auth: "true" }, "Windows 통합 인증 설정 지우기"],
  ])("%s을 불러오면 값을 바꾸지 않고 해결 버튼과 함께 알린다", (_title, extra, action) => {
    // given
    const saved = { extra };
    // when
    const host = renderHost(saved);
    // then
    expect(screen.getByRole("alert")).toBeTruthy();
    expect(screen.getByRole("button", { name: action })).toBeTruthy();
    expect(host.spies.setExtraField).not.toHaveBeenCalled();
    expect(host.current.extra).toEqual(extra);
  });

  it("Windows 통합 인증 설정 지우기를 누르면 Entra 식별 정보는 두고 그 값만 지운다", () => {
    // given
    const host = renderHost({ extra: { ...ENTRA_EXTRA, integrated_auth: "true" }, username: ENTRA_USERNAME });
    // when
    fireEvent.click(screen.getByRole("button", { name: "Windows 통합 인증 설정 지우기" }));
    // then
    expect(host.current.extra).toEqual(ENTRA_EXTRA);
    expect(host.current.username).toBe(ENTRA_USERNAME);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("Entra 설정 지우기를 누르면 SQL Server 인증 값은 두고 Entra 전용 키만 지운다", () => {
    // given
    const host = renderHost({ extra: { ...SQL_EXTRA, tenant_id: TENANT, azure_cli_path: CLI_PATH }, username: SQL_USERNAME });
    // when
    fireEvent.click(screen.getByRole("button", { name: "Entra 설정 지우기" }));
    // then
    expect(host.current.extra).toEqual(SQL_EXTRA);
    expect(host.current.username).toBe(SQL_USERNAME);
  });
});

describe("암호 입력란만 숨기는 기능이 없는 호스트에서는 Entra 대신 필요한 버전을 알린다", () => {
  it("Entra 연결을 불러오면 필요한 Tabularis 버전을 알리고 사용자 이름, 암호, Entra 설정을 지우지 않는다", () => {
    // given
    const saved = { extra: ENTRA_EXTRA, username: ENTRA_USERNAME, password: SQL_PASSWORD, passwordToggle: false };
    // when
    const host = renderHost(saved);
    // then
    expect(screen.getByRole("alert").textContent).toContain("Tabularis Extended 0.27.0+extended.1 이상이 필요합니다.");
    expect(host.current.extra).toEqual(ENTRA_EXTRA);
    expect(host.current.username).toBe(ENTRA_USERNAME);
    expect(host.current.password).toBe(SQL_PASSWORD);
    expect(hiddenOnce(host.spies.setCredentialFieldsHidden)).toBe(false);
    expect(host.spies.setExtraField).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("테넌트 ID")).toBeNull();
  });

  it("SQL Server 인증 연결에서는 Entra 선택지를 막고 필요한 버전을 알린다", () => {
    // given
    const saved = { extra: SQL_EXTRA, passwordToggle: false };
    // when
    renderHost(saved);
    // then
    const entra = screen.getByRole("option", { name: "Microsoft Entra 사용자 (Azure CLI)" }) as HTMLOptionElement;
    expect(entra.disabled).toBe(true);
    expect(document.body.textContent).toContain("Microsoft Entra 사용자 인증에는 Tabularis Extended 0.27.0+extended.1 이상이 필요합니다.");
  });

  it("SQL Server 인증에서 Windows/Kerberos로 바꾸는 동작은 그대로 쓸 수 있다", () => {
    // given
    const host = renderHost({ extra: SQL_EXTRA, username: SQL_USERNAME, password: SQL_PASSWORD, passwordToggle: false });
    // when
    chooseMode("integrated");
    // then
    expect(host.current.extra).toEqual(INTEGRATED_EXTRA);
    expect(screen.queryByLabelText("사용자 이름")).toBeNull();
    expect(screen.queryByLabelText("암호")).toBeNull();
  });

  it("Entra 선택지를 강제로 바꿔도 아무 값도 쓰지 않는다", () => {
    // given
    const host = renderHost({ extra: SQL_EXTRA, username: SQL_USERNAME, password: SQL_PASSWORD, passwordToggle: false });
    // when
    chooseMode("entra_user");
    // then
    expect(host.spies.setExtraField).not.toHaveBeenCalled();
    expect(host.current.password).toBe(SQL_PASSWORD);
  });
});

describe("다른 드라이버의 연결 창에서는 아무것도 하지 않는다", () => {
  it("sqlserver-plus가 아닌 드라이버에서는 입력란을 그리지 않고 호스트 입력란도 바꾸지 않는다", () => {
    // given
    const saved = { extra: LEGACY_INTEGRATED_EXTRA, username: SQL_USERNAME, driver: "sqlserver" };
    // when
    const host = renderHost(saved);
    // then
    expect(screen.queryByLabelText("인증 방식")).toBeNull();
    expect(host.spies.setCredentialFieldsHidden).not.toHaveBeenCalled();
    expect(host.spies.setPasswordFieldHidden).not.toHaveBeenCalled();
    expect(input("사용자 이름").value).toBe(SQL_USERNAME);
  });
});
