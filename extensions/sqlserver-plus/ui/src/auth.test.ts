import { describe, expect, it } from "vitest";
import { authIssues, authMode, clearEntraChanges, isAbsolutePath, modeChanges } from "./auth";
import { CLI_PATH, ENTRA_EXTRA, INTEGRATED_EXTRA, LEGACY_INTEGRATED_EXTRA, OBJECT_ID, SQL_EXTRA, TENANT } from "./testFixtures";

describe("저장된 인증 값은 드라이버와 같은 규칙으로 해석된다", () => {
  it.each([
    ["auth_mode가 없는 기존 암호 연결", {}, "sql_password"],
    ["auth_mode 없이 integrated_auth만 켠 기존 연결", LEGACY_INTEGRATED_EXTRA, "integrated"],
    ["auth_mode 없이 integrated_auth를 Yes로 저장한 연결", { integrated_auth: "Yes" }, "integrated"],
    ["Entra 사용자 연결", ENTRA_EXTRA, "entra_user"],
    ["지원하지 않는 auth_mode를 저장한 연결", { auth_mode: "device_code" }, null],
  ])("%s은 알맞은 인증 방식으로 읽힌다", (_title, extra, expected) => {
    // given
    const stored: Record<string, string> = extra;
    // when
    const mode = authMode(stored);
    // then
    expect(mode).toBe(expected);
  });
});

describe("인증 방식을 고르면 다른 방식 전용 값만 지운다", () => {
  it("Entra에서 SQL Server 인증으로 바꾸면 Entra 전용 키를 모두 지운다", () => {
    // given
    const extra = ENTRA_EXTRA;
    // when
    const changes = modeChanges(extra, "sql_password");
    // then
    expect(changes).toEqual([
      ["auth_mode", "sql_password"],
      ["auth_source", ""],
      ["tenant_id", ""],
      ["entra_object_id", ""],
      ["azure_cli_path", ""],
    ]);
  });

  it("Entra에서 Windows/Kerberos로 바꾸면 Entra 전용 키를 지우고 통합 인증을 켠다", () => {
    // given
    const extra = ENTRA_EXTRA;
    // when
    const changes = modeChanges(extra, "integrated");
    // then
    expect(changes).toEqual([
      ["auth_mode", "integrated"],
      ["integrated_auth", "true"],
      ["auth_source", ""],
      ["tenant_id", ""],
      ["entra_object_id", ""],
      ["azure_cli_path", ""],
    ]);
  });

  it("통합 인증에서 Entra로 바꾸면 Azure CLI 소스를 기록하고 통합 인증 값만 지운다", () => {
    // given
    const extra = INTEGRATED_EXTRA;
    // when
    const changes = modeChanges(extra, "entra_user");
    // then
    expect(changes).toEqual([
      ["auth_mode", "entra_user"],
      ["auth_source", "azure_cli"],
      ["integrated_auth", ""],
    ]);
  });

  it("Entra로 바꿀 때 이미 입력한 식별 정보는 지우지 않는다", () => {
    // given
    const extra = { ...SQL_EXTRA, tenant_id: TENANT, entra_object_id: OBJECT_ID };
    // when
    const changes = modeChanges(extra, "entra_user");
    // then
    expect(changes.map(([key]) => key)).not.toContain("tenant_id");
    expect(changes.map(([key]) => key)).not.toContain("entra_object_id");
  });

  it("이미 같은 값으로 저장된 키는 다시 쓰지 않는다", () => {
    // given
    const extra = SQL_EXTRA;
    // when
    const changes = modeChanges(extra, "sql_password");
    // then
    expect(changes).toEqual([]);
  });

  it("Entra 설정 지우기는 저장된 Entra 전용 키만 지운다", () => {
    // given
    const extra = { ...SQL_EXTRA, tenant_id: TENANT, azure_cli_path: CLI_PATH };
    // when
    const changes = clearEntraChanges(extra);
    // then
    expect(changes).toEqual([["tenant_id", ""], ["azure_cli_path", ""]]);
  });
});

describe("드라이버가 거부하는 설정은 안내 문구로 알려 준다", () => {
  it("완전한 Entra 설정에는 안내할 문제가 없다", () => {
    // given
    const extra = ENTRA_EXTRA;
    // when
    const issues = authIssues(extra);
    // then
    expect(issues).toEqual([]);
  });

  it("Entra 설정에 테넌트 ID와 개체 ID가 없으면 둘 다 입력하라고 안내한다", () => {
    // given
    const extra = { auth_mode: "entra_user", auth_source: "azure_cli" };
    // when
    const issues = authIssues(extra);
    // then
    expect(issues).toEqual([
      { key: "tenant_id", message: "테넌트 ID를 입력하세요." },
      { key: "entra_object_id", message: "Entra 개체 ID를 입력하세요." },
    ]);
  });

  it("공백만 있는 테넌트 ID는 입력하지 않은 것으로 안내한다", () => {
    // given
    const extra = { ...ENTRA_EXTRA, tenant_id: "   " };
    // when
    const issues = authIssues(extra);
    // then
    expect(issues).toEqual([{ key: "tenant_id", message: "테넌트 ID를 입력하세요." }]);
  });

  it.each([
    ["하이픈으로 시작하는 테넌트 ID", "-tenant"],
    ["허용하지 않는 문자가 있는 테넌트 ID", "contoso/tenant"],
  ])("%s는 형식 오류로 안내한다", (_title, tenant) => {
    // given
    const extra = { ...ENTRA_EXTRA, tenant_id: tenant };
    // when
    const issues = authIssues(extra);
    // then
    expect(issues.map((issue) => issue.key)).toEqual(["tenant_id"]);
    expect(issues[0].message).toContain("하이픈으로 시작할 수 없습니다");
  });

  it.each([
    ["상대 경로", "bin/az"],
    ["앞에 공백이 있는 경로", " /usr/local/bin/az"],
    ["빈 경로", ""],
  ])("Azure CLI 경로가 %s이면 절대 경로를 요구한다", (_title, path) => {
    // given
    const extra = { ...ENTRA_EXTRA, azure_cli_path: path };
    // when
    const issues = authIssues(extra);
    // then
    expect(issues).toEqual([{
      key: "azure_cli_path",
      message: "Azure CLI 경로는 앞뒤 공백 없는 절대 경로여야 합니다. 비워 두면 PATH에서 az를 찾습니다.",
    }]);
  });

  it.each([
    ["macOS와 Linux 경로", "/opt/homebrew/bin/az", true],
    ["Windows 드라이브 경로", "C:\\Program Files\\Microsoft SDKs\\Azure\\CLI2\\wbin\\az.cmd", true],
    ["Windows 네트워크 경로", "\\\\server\\share\\az.cmd", true],
    ["드라이브 문자만 있는 경로", "C:az.cmd", false],
    ["상대 경로", "./az", false],
  ])("%s의 절대 경로 여부를 드라이버처럼 판단한다", (_title, path, expected) => {
    // given
    const value = path;
    // when
    const absolute = isAbsolutePath(value);
    // then
    expect(absolute).toBe(expected);
  });

  it("Entra 설정에 통합 인증이 켜져 있으면 함께 쓸 수 없다고 안내한다", () => {
    // given
    const extra = { ...ENTRA_EXTRA, integrated_auth: "true" };
    // when
    const issues = authIssues(extra);
    // then
    expect(issues).toEqual([{ key: "integrated_auth", message: "Microsoft Entra 인증은 Windows 통합 인증과 함께 쓸 수 없습니다." }]);
  });

  it("Entra 로그인 소스가 Azure CLI가 아니면 안내한다", () => {
    // given
    const extra = { ...ENTRA_EXTRA, auth_source: "browser" };
    // when
    const issues = authIssues(extra);
    // then
    expect(issues).toEqual([{ key: "auth_source", message: "Microsoft Entra 로그인 소스는 Azure CLI여야 합니다." }]);
  });

  it("SQL Server 인증에 Entra 전용 키가 남아 있으면 저장할 수 없다고 안내한다", () => {
    // given
    const extra = { ...SQL_EXTRA, tenant_id: TENANT };
    // when
    const issues = authIssues(extra);
    // then
    expect(issues.map((issue) => issue.key)).toEqual(["entra_keys"]);
  });

  it("지원하지 않는 인증 방식은 다시 선택하라고만 안내한다", () => {
    // given
    const extra = { auth_mode: "device_code", tenant_id: TENANT };
    // when
    const issues = authIssues(extra);
    // then
    expect(issues.map((issue) => issue.key)).toEqual(["auth_mode"]);
  });
});
