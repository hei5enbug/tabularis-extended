export const DRIVER_ID = "sqlserver-plus";
export const REQUIRED_HOST = "Tabularis Extended 0.27.0+extended.1";
export const AZURE_CLI_SOURCE = "azure_cli";

export const AUTH_MODES = ["sql_password", "integrated", "entra_user"] as const;
export type AuthMode = (typeof AUTH_MODES)[number];

export const ENTRA_KEYS = ["auth_source", "tenant_id", "entra_object_id", "azure_cli_path"] as const;

export type Extra = Readonly<Record<string, string>>;
export type FieldChange = readonly [key: string, value: string];

export type IssueKey =
  | "auth_mode"
  | "integrated_auth"
  | "entra_keys"
  | "auth_source"
  | "tenant_id"
  | "entra_object_id"
  | "azure_cli_path";

export interface Issue {
  key: IssueKey;
  message: string;
}

const TRUE_VALUES = new Set(["true", "yes", "on", "1"]);
const TENANT = /^(?!-)[A-Za-z0-9.-]+$/;
const WINDOWS_DRIVE_PATH = /^[A-Za-z]:[\\/]/;
const WINDOWS_UNC_PATH = /^\\\\[^\\]/;

function has(extra: Extra, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(extra, key);
}

export function isEnabled(value: string | undefined): boolean {
  return value !== undefined && TRUE_VALUES.has(value.trim().toLowerCase());
}

/** Reads the mode the driver will use; `null` means the driver rejects the stored value. */
export function authMode(extra: Extra): AuthMode | null {
  if (!has(extra, "auth_mode")) return isEnabled(extra.integrated_auth) ? "integrated" : "sql_password";
  const mode = extra.auth_mode;
  return (AUTH_MODES as readonly string[]).includes(mode) ? (mode as AuthMode) : null;
}

/** Accepts the absolute forms the driver accepts on macOS, Linux, and Windows. */
export function isAbsolutePath(value: string): boolean {
  return value.startsWith("/") || WINDOWS_DRIVE_PATH.test(value) || WINDOWS_UNC_PATH.test(value);
}

/**
 * Field writes for an explicit mode choice. Leaving Entra drops every Entra-only key;
 * choosing Entra drops the integrated flag but keeps any identity already entered.
 * An empty value asks the host to remove the key.
 */
export function modeChanges(extra: Extra, next: AuthMode): FieldChange[] {
  const target: Record<string, string> = next === "entra_user"
    ? { auth_mode: next, auth_source: AZURE_CLI_SOURCE, integrated_auth: "" }
    : {
      auth_mode: next,
      integrated_auth: next === "integrated" ? "true" : "",
      ...Object.fromEntries(ENTRA_KEYS.map((key) => [key, ""])),
    };
  return Object.entries(target).filter(([key, value]) => (extra[key] ?? "") !== value);
}

export function clearEntraChanges(extra: Extra): FieldChange[] {
  return ENTRA_KEYS.filter((key) => has(extra, key)).map((key) => [key, ""] as const);
}

/** Mirrors the driver's connection checks for the values this form owns. */
export function authIssues(extra: Extra): Issue[] {
  const mode = authMode(extra);
  if (mode === null) {
    return [{
      key: "auth_mode",
      message: "저장된 인증 방식은 지원하지 않습니다. SQL Server, Windows/Kerberos, Microsoft Entra 중에서 다시 선택하세요.",
    }];
  }

  const issues: Issue[] = [];
  if (mode !== "integrated" && isEnabled(extra.integrated_auth)) {
    issues.push({
      key: "integrated_auth",
      message: mode === "entra_user"
        ? "Microsoft Entra 인증은 Windows 통합 인증과 함께 쓸 수 없습니다."
        : "SQL Server 인증은 Windows 통합 인증 설정과 함께 쓸 수 없습니다.",
    });
  }

  if (mode !== "entra_user") {
    if (ENTRA_KEYS.some((key) => has(extra, key))) {
      issues.push({
        key: "entra_keys",
        message: "이 인증 방식에는 Entra 전용 설정(로그인 소스, 테넌트 ID, 개체 ID, Azure CLI 경로)을 저장할 수 없습니다.",
      });
    }
    return issues;
  }

  if ((extra.auth_source ?? "").trim() !== AZURE_CLI_SOURCE) {
    issues.push({ key: "auth_source", message: "Microsoft Entra 로그인 소스는 Azure CLI여야 합니다." });
  }
  const tenant = (extra.tenant_id ?? "").trim();
  if (tenant === "") {
    issues.push({ key: "tenant_id", message: "테넌트 ID를 입력하세요." });
  } else if (!TENANT.test(tenant)) {
    issues.push({
      key: "tenant_id",
      message: "테넌트 ID에는 영문자, 숫자, 하이픈(-), 마침표(.)만 쓸 수 있고 하이픈으로 시작할 수 없습니다.",
    });
  }
  if ((extra.entra_object_id ?? "").trim() === "") {
    issues.push({ key: "entra_object_id", message: "Entra 개체 ID를 입력하세요." });
  }
  const path = extra.azure_cli_path;
  if (path !== undefined && (path === "" || path.trim() !== path || !isAbsolutePath(path))) {
    issues.push({
      key: "azure_cli_path",
      message: "Azure CLI 경로는 앞뒤 공백 없는 절대 경로여야 합니다. 비워 두면 PATH에서 az를 찾습니다.",
    });
  }
  return issues;
}
