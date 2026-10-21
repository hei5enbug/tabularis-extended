import { useEffect, useId } from "react";
import type { SlotComponentProps } from "@tabularis/plugin-api";
import AuthPanel from "./AuthPanel";
import {
  AUTH_MODES, AZURE_CLI_SOURCE, DRIVER_ID, REQUIRED_HOST,
  authIssues, authMode, clearEntraChanges, modeChanges,
  type AuthMode, type FieldChange, type IssueKey,
} from "./auth";
import { styles } from "./styles";

/** Every member is optional because older hosts provide only part of the slot context. */
interface ExtraFieldsContext {
  driver?: string;
  extra?: Record<string, string>;
  setExtraField?: (key: string, value: string) => void;
  credentialFieldsHidden?: boolean;
  setCredentialFieldsHidden?: (hidden: boolean) => void;
  passwordFieldHidden?: boolean;
  setPasswordFieldHidden?: (hidden: boolean) => void;
}

const MODE_LABELS: Record<AuthMode, string> = {
  sql_password: "SQL Server 인증 (사용자 이름과 암호)",
  integrated: "Windows/Kerberos 통합 인증",
  entra_user: "Microsoft Entra 사용자 (Azure CLI)",
};

const MODE_NOTES: Record<AuthMode, string> = {
  sql_password: "사용자 이름과 암호 입력란으로 로그인합니다. Windows/Kerberos로 바꾸면 사용자 이름과 암호가, "
    + "Microsoft Entra로 바꾸면 암호가 지워집니다.",
  integrated: "이 컴퓨터에 로그인한 Windows/Kerberos 계정으로 연결하므로 사용자 이름과 암호 입력란을 숨깁니다. "
    + "Microsoft Entra로 바꾸면 Windows 통합 인증 설정이 지워집니다.",
  entra_user: "Azure CLI에 로그인한 Entra 사용자로 연결하므로 암호 입력란만 숨깁니다. "
    + "다른 방식으로 바꾸면 로그인 소스, 테넌트 ID, 개체 ID, Azure CLI 경로가 지워집니다.",
};

const FIXES: Partial<Record<IssueKey, { action: string; changes: (extra: Record<string, string>) => FieldChange[] }>> = {
  integrated_auth: { action: "Windows 통합 인증 설정 지우기", changes: () => [["integrated_auth", ""]] },
  entra_keys: { action: "Entra 설정 지우기", changes: clearEntraChanges },
  auth_source: { action: "Azure CLI로 설정", changes: () => [["auth_source", AZURE_CLI_SOURCE]] },
};

export default function SQLServerAuthFields({ context }: SlotComponentProps) {
  const host = context as ExtraFieldsContext;
  const id = useId();
  const active = host.driver === undefined || host.driver === DRIVER_ID;
  const extra = host.extra ?? {};
  const mode = authMode(extra);
  const hidePasswordField = typeof host.setPasswordFieldHidden === "function" ? host.setPasswordFieldHidden : null;
  const hideCredentialFields = host.setCredentialFieldsHidden;
  const entraSupported = hidePasswordField !== null;
  const credentialsHidden = mode === "integrated";
  const passwordHidden = mode === "entra_user" && entraSupported;

  useEffect(() => {
    if (active && host.credentialFieldsHidden !== credentialsHidden) hideCredentialFields?.(credentialsHidden);
  }, [active, credentialsHidden, host.credentialFieldsHidden, hideCredentialFields]);

  useEffect(() => {
    if (active && hidePasswordField && host.passwordFieldHidden !== passwordHidden) hidePasswordField(passwordHidden);
  }, [active, passwordHidden, host.passwordFieldHidden, hidePasswordField]);

  if (!active) return null;

  const write = (changes: readonly FieldChange[]) => {
    for (const [key, value] of changes) host.setExtraField?.(key, value);
  };
  const choose = (next: AuthMode) => {
    if (next === "entra_user" && !entraSupported) return;
    write(modeChanges(extra, next));
  };
  const editable = mode !== "entra_user" || entraSupported;
  const issues = authIssues(extra);
  const unsupported = issues.find((issue) => issue.key === "auth_mode");
  const fixable = editable ? issues.filter((issue) => FIXES[issue.key]) : [];

  return <section aria-label="SQL Server Plus 인증" style={styles.section}>
    <div style={styles.field}>
      <label htmlFor={`${id}-mode`} style={styles.label}>인증 방식</label>
      <select
        id={`${id}-mode`}
        value={mode ?? ""}
        aria-describedby={mode ? `${id}-note` : undefined}
        aria-invalid={mode === null ? true : undefined}
        onChange={(event) => choose(event.target.value as AuthMode)}
        style={mode === null ? { ...styles.control, ...styles.invalid } : styles.control}
      >
        {mode === null && <option value="" disabled>지원하지 않는 인증 방식</option>}
        {AUTH_MODES.map((option) => <option
          key={option}
          value={option}
          disabled={option === "entra_user" && !entraSupported && mode !== "entra_user"}
        >
          {MODE_LABELS[option]}
        </option>)}
      </select>
      {mode && <p id={`${id}-note`} style={styles.help}>{MODE_NOTES[mode]}</p>}
    </div>

    {unsupported && <p role="alert" style={styles.error}>{unsupported.message}</p>}

    {!entraSupported && (mode === "entra_user"
      ? <p role="alert" style={styles.notice}>
        이 Tabularis에서는 Microsoft Entra 사용자 연결을 편집할 수 없습니다. 암호 입력란만 숨기는 기능이 있는{" "}
        {REQUIRED_HOST} 이상이 필요합니다. 저장된 사용자 이름과 Entra 설정은 그대로 둡니다.
      </p>
      : <p style={styles.notice}>
        Microsoft Entra 사용자 인증에는 {REQUIRED_HOST} 이상이 필요합니다. SQL Server 인증과 Windows/Kerberos 통합 인증은
        지금 버전에서도 쓸 수 있습니다.
      </p>)}

    {fixable.map((issue) => {
      const fix = FIXES[issue.key]!;
      return <div key={issue.key} role="alert" style={styles.issue}>
        <p style={styles.issueText}>{issue.message}</p>
        <button type="button" style={styles.button} onClick={() => write(fix.changes(extra))}>{fix.action}</button>
      </div>;
    })}

    {mode === "entra_user" && entraSupported && <AuthPanel
      extra={extra}
      issues={issues}
      setExtraField={(key, value) => host.setExtraField?.(key, value)}
    />}
  </section>;
}
