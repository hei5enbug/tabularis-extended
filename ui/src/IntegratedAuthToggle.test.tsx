import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { SlotComponentProps } from "@tabularis/plugin-api";
import IntegratedAuthToggle from "./IntegratedAuthToggle";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const mounted: { root: Root; container: HTMLDivElement }[] = [];
const savedEntra = {
  auth_mode: "entra_user",
  auth_source: "azure_cli",
  tenant_id: "expected-tenant",
  entra_object_id: "expected-object",
  azure_cli_path: "/opt/example/bin/az",
  unrelated_option: "preserved",
};

function editor(extra: Record<string, string>, supportsEntra = true) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  const state = {
    extra: { ...extra },
    username: "expected-user@example.invalid",
    password: "fixture-password",
    credentialFieldsHidden: false,
    passwordFieldHidden: false,
  };
  const setExtraField = vi.fn((key: string, value: string) => {
    state.extra = { ...state.extra, [key]: value };
    render();
  });
  const setCredentialFieldsHidden = vi.fn((hidden: boolean) => {
    state.credentialFieldsHidden = hidden;
    if (hidden) { state.username = ""; state.password = ""; }
  });
  const setPasswordFieldHidden = vi.fn((hidden: boolean) => {
    state.passwordFieldHidden = hidden;
    if (hidden) state.password = "";
  });
  function render() {
    const context = {
      extra: state.extra,
      setExtraField,
      credentialFieldsHidden: state.credentialFieldsHidden,
      setCredentialFieldsHidden,
      passwordFieldHidden: state.passwordFieldHidden,
      ...(supportsEntra ? { setPasswordFieldHidden } : {}),
    };
    root.render(<IntegratedAuthToggle context={context as SlotComponentProps["context"]} pluginId="sqlserver-plus" />);
  }
  return {
    container, state, setExtraField, setCredentialFieldsHidden, setPasswordFieldHidden,
    render: () => act(render),
    selectMode: (value: string) => act(() => {
      const select = container.querySelector("select")!;
      select.value = value;
      select.dispatchEvent(new Event("change", { bubbles: true }));
    }),
    input: (name: string) => Array.from(container.querySelectorAll("label")).find(label => label.textContent?.trim() === name)?.querySelector("input") ?? null,
    replaceExtra: (value: Record<string, string>) => act(() => { state.extra = { ...value }; render(); }),
  };
}

afterEach(() => {
  for (const { root, container } of mounted.splice(0)) {
    act(() => root.unmount());
    container.remove();
  }
});

describe("SQL Server 인증 방식 선택기", () => {
  test("기본 SQL 인증에서는 사용자 이름과 비밀번호 입력란을 유지합니다.", () => {
    // given
    const view = editor({});
    // when
    view.render();
    // then
    expect(view.container.querySelector("select")?.value).toBe("sql");
    expect(Array.from(view.container.querySelectorAll("option")).map(option => option.textContent)).toEqual(["SQL password", "Windows / Kerberos", "Entra user (Azure CLI)"]);
    expect(view.setCredentialFieldsHidden).toHaveBeenCalledWith(false);
    expect(view.setPasswordFieldHidden).toHaveBeenCalledWith(false);
    expect(view.state.username).toBe("expected-user@example.invalid");
    expect(view.state.password).toBe("fixture-password");
    expect(view.setExtraField).not.toHaveBeenCalled();
  });

  test("저장된 Windows 인증을 열면 기존 API로 두 자격 증명 입력란을 숨깁니다.", () => {
    // given
    const view = editor({ integrated_auth: "true", unrelated_option: "preserved" });
    // when
    view.render();
    // then
    expect(view.container.querySelector("select")?.value).toBe("integrated");
    expect(view.setCredentialFieldsHidden).toHaveBeenCalledWith(true);
    expect(view.state.username).toBe("");
    expect(view.state.password).toBe("");
    expect(view.setExtraField).not.toHaveBeenCalled();
    expect(view.state.extra).toEqual({ integrated_auth: "true", unrelated_option: "preserved" });
  });

  test("저장된 Entra 설정을 열면 식별자와 사용자 이름을 보존하고 비밀번호만 숨깁니다.", () => {
    // given
    const view = editor(savedEntra);
    // when
    view.render();
    // then
    expect(view.container.querySelector("select")?.value).toBe("entra_user");
    expect(view.input("Tenant ID")?.value).toBe("expected-tenant");
    expect(view.input("Object ID")?.value).toBe("expected-object");
    expect(view.input("Azure CLI executable path (optional)")?.value).toBe("/opt/example/bin/az");
    expect(view.input("Tenant ID")?.required).toBe(true);
    expect(view.input("Object ID")?.required).toBe(true);
    expect(view.input("Azure CLI executable path (optional)")?.required).toBe(false);
    expect(view.container.querySelectorAll("input")).toHaveLength(3);
    expect(view.setCredentialFieldsHidden).toHaveBeenCalledWith(false);
    expect(view.setCredentialFieldsHidden).not.toHaveBeenCalledWith(true);
    expect(view.setPasswordFieldHidden).toHaveBeenCalledWith(true);
    expect(view.state.username).toBe("expected-user@example.invalid");
    expect(view.state.password).toBe("");
    expect(view.state.extra).toEqual(savedEntra);
    expect(view.setExtraField).not.toHaveBeenCalled();
  });

  test("연결 설정이 늦게 로드되어도 저장된 Entra 식별자를 덮어쓰지 않습니다.", () => {
    // given
    const view = editor({});
    view.render();
    // when
    view.replaceExtra(savedEntra);
    // then
    expect(view.container.querySelector("select")?.value).toBe("entra_user");
    expect(view.input("Tenant ID")?.value).toBe("expected-tenant");
    expect(view.state.extra).toEqual(savedEntra);
    expect(view.state.username).toBe("expected-user@example.invalid");
    expect(view.setExtraField).not.toHaveBeenCalled();
  });

  test("SQL 설정을 열기만 하면 남아 있는 Entra 필드를 지우지 않습니다.", () => {
    // given
    const extra = { ...savedEntra, auth_mode: "" };
    const view = editor(extra);
    // when
    view.render();
    // then
    expect(view.container.querySelector("select")?.value).toBe("sql");
    expect(view.state.extra).toEqual(extra);
    expect(view.setExtraField).not.toHaveBeenCalled();
  });

  test("Entra를 선택하면 사용자 이름을 보존하고 비밀번호와 Windows 인증을 해제합니다.", () => {
    // given
    const view = editor({ integrated_auth: "false", unrelated_option: "preserved" });
    view.render();
    // when
    view.selectMode("entra_user");
    // then
    expect(view.container.querySelector("select")?.value).toBe("entra_user");
    expect(view.state.extra).toEqual({ integrated_auth: "", auth_mode: "entra_user", auth_source: "azure_cli", unrelated_option: "preserved" });
    expect(view.state.username).toBe("expected-user@example.invalid");
    expect(view.state.password).toBe("");
    expect(view.state.passwordFieldHidden).toBe(true);
    expect(view.state.credentialFieldsHidden).toBe(false);
    expect(view.setCredentialFieldsHidden).not.toHaveBeenCalledWith(true);
  });

  test("Windows에서 Entra로 전환하면 사용자 이름 입력란을 다시 표시합니다.", () => {
    // given
    const view = editor({ integrated_auth: "true" });
    view.render();
    // when
    view.selectMode("entra_user");
    // then
    expect(view.state.extra.integrated_auth).toBe("");
    expect(view.state.extra.auth_mode).toBe("entra_user");
    expect(view.state.extra.auth_source).toBe("azure_cli");
    expect(view.state.credentialFieldsHidden).toBe(false);
    expect(view.state.passwordFieldHidden).toBe(true);
  });

  test.each([
    { mode: "sql", label: "SQL", integrated: "", username: "expected-user@example.invalid", hidden: false },
    { mode: "integrated", label: "Windows", integrated: "true", username: "", hidden: true },
  ])("Entra에서 $label 인증으로 전환하면 Entra 전용 필드만 정리합니다.", ({ mode, integrated, username, hidden }) => {
    // given
    const view = editor(savedEntra);
    view.render();
    // when
    view.selectMode(mode);
    // then
    expect(view.container.querySelector("select")?.value).toBe(mode);
    expect(view.state.extra).toEqual({ auth_mode: "", auth_source: "", tenant_id: "", entra_object_id: "", azure_cli_path: "", integrated_auth: integrated, unrelated_option: "preserved" });
    expect(view.state.username).toBe(username);
    expect(view.state.credentialFieldsHidden).toBe(hidden);
    expect(view.state.passwordFieldHidden).toBe(false);
    expect(view.input("Tenant ID")).toBeNull();
  });

  test.each([
    { label: "Tenant ID", key: "tenant_id", value: "changed-tenant" },
    { label: "Object ID", key: "entra_object_id", value: "changed-object" },
    { label: "Azure CLI executable path (optional)", key: "azure_cli_path", value: "C:\\Azure CLI\\az.cmd" },
  ])("$label 입력은 해당 추가 필드만 갱신합니다.", ({ label, key, value }) => {
    // given
    const view = editor(savedEntra);
    view.render();
    const input = view.input(label)!;
    // when
    act(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    // then
    expect(view.setExtraField).toHaveBeenCalledExactlyOnceWith(key, value);
    expect(view.state.extra).toEqual({ ...savedEntra, [key]: value });
    expect(view.state.username).toBe("expected-user@example.invalid");
  });

  test("Entra 연결의 사용자와 로그인 및 TLS 요구사항을 안내합니다.", () => {
    // given
    const view = editor(savedEntra);
    // when
    view.render();
    // then
    expect(view.container.textContent).toContain("Expected username");
    expect(view.container.textContent).toContain("Username field");
    expect(view.container.textContent).toContain("Azure CLI must already be signed in");
    expect(view.container.textContent).toContain("complete MFA");
    expect(view.container.textContent).toContain("absolute executable path");
    expect(view.container.textContent).toContain(".database.windows.net");
    expect(view.container.textContent).toContain("verify-full");
    expect(view.container.textContent).toContain("Trust server certificate turned off");
    expect(view.setExtraField).not.toHaveBeenCalled();
  });

  test("구형 호스트에서는 Entra 선택을 비활성화하고 호환 호스트를 안내합니다.", () => {
    // given
    const view = editor({}, false);
    // when
    view.render();
    // then
    expect(view.container.querySelector<HTMLOptionElement>('option[value="entra_user"]')?.disabled).toBe(true);
    expect(view.container.querySelector('[role="alert"]')?.textContent).toContain("compatible Tabularis Extended host");
    expect(view.state.username).toBe("expected-user@example.invalid");
    expect(view.state.password).toBe("fixture-password");
    expect(view.setExtraField).not.toHaveBeenCalled();
  });

  test("구형 호스트의 Entra 전환 이벤트는 자격 증명을 변경하지 않습니다.", () => {
    // given
    const view = editor({}, false);
    view.render();
    view.setCredentialFieldsHidden.mockClear();
    // when
    view.selectMode("entra_user");
    // then
    expect(view.state.extra).toEqual({});
    expect(view.state.username).toBe("expected-user@example.invalid");
    expect(view.state.password).toBe("fixture-password");
    expect(view.setExtraField).not.toHaveBeenCalled();
    expect(view.setCredentialFieldsHidden).not.toHaveBeenCalled();
    expect(view.setPasswordFieldHidden).not.toHaveBeenCalled();
  });

  test("구형 호스트에서 저장된 Entra 설정을 열어도 자격 증명과 식별자를 지우지 않습니다.", () => {
    // given
    const view = editor(savedEntra, false);
    // when
    view.render();
    // then
    expect(view.container.querySelector("select")?.value).toBe("entra_user");
    expect(view.input("Tenant ID")?.value).toBe("expected-tenant");
    expect(view.state.extra).toEqual(savedEntra);
    expect(view.state.username).toBe("expected-user@example.invalid");
    expect(view.state.password).toBe("fixture-password");
    expect(view.setExtraField).not.toHaveBeenCalled();
    expect(view.setCredentialFieldsHidden).not.toHaveBeenCalled();
    expect(view.setPasswordFieldHidden).not.toHaveBeenCalled();
    expect(view.container.querySelector('[role="alert"]')?.textContent).toContain("compatible Tabularis Extended host");
  });

  test("구형 호스트에서도 Windows와 SQL 인증 사이를 전환할 수 있습니다.", () => {
    // given
    const view = editor({ integrated_auth: "true" }, false);
    view.render();
    // when
    view.selectMode("sql");
    // then
    expect(view.container.querySelector("select")?.value).toBe("sql");
    expect(view.state.extra.integrated_auth).toBe("");
    expect(view.state.credentialFieldsHidden).toBe(false);
    expect(view.setPasswordFieldHidden).not.toHaveBeenCalled();
  });
});
