import { useEffect } from "react";
import type { SlotComponentProps } from "@tabularis/plugin-api";
import AuthPanel from "./AuthPanel";
import { settings, type AuthMode } from "./auth";

interface ExtraFieldsContext {
  connectionId?: string | null;
  extra?: Record<string, string>;
  setExtraField?: (key: string, value: string) => void;
  setCredentialFieldsHidden?: (hidden: boolean) => void;
}

export default function SQLServerAuthFields({ context }: SlotComponentProps) {
  const fields = context as ExtraFieldsContext;
  const form = settings(fields.extra ?? {});
  const hideCredentials = fields.setCredentialFieldsHidden;
  const hidden = form.mode !== "sql_password";
  useEffect(() => { hideCredentials?.(hidden); }, [hideCredentials, hidden]);
  const entra = form.mode === "entra_user" || form.mode === "entra_service_principal";
  const key = JSON.stringify([fields.connectionId, form.mode, form.tenant, form.client, form.source]);

  const changeMode = (mode: AuthMode) => {
    fields.setExtraField?.("auth_mode", mode);
    if (form.source) fields.setExtraField?.("auth_source", "oauth");
    fields.setExtraField?.("integrated_auth", mode === "integrated" ? "true" : "");
  };

  return <section aria-label="SQL Server 인증 설정" style={{ display: "grid", gap: 8 }}>
    <label>인증 방식
      <select aria-label="인증 방식" value={form.mode ?? "invalid"} onChange={(event) => changeMode(event.target.value as AuthMode)}>
        {form.mode === null && <option value="invalid" disabled>지원하지 않는 인증 방식</option>}
        <option value="sql_password">SQL Server 사용자 이름과 암호</option>
        <option value="integrated">Windows 통합 인증</option>
        <option value="entra_user">Microsoft Entra 사용자 로그인</option>
        <option value="entra_service_principal">Microsoft Entra 서비스 주체</option>
      </select>
    </label>
    {form.mode === "entra_user" && <label>로그인 소스<select aria-label="로그인 소스" value={form.source ?? "oauth"} onChange={event => { fields.setExtraField?.("auth_source", event.target.value); if (event.target.value === "azure_cli") fields.setExtraField?.("client_id", "04b07795-8ddb-461a-bbee-02f9e1bf7b46"); }}><option value="oauth">앱에서 로그인</option><option value="azure_cli">Azure CLI (az login)</option></select></label>}
    {form.source === "azure_cli" && <p>이 컴퓨터에서 az login을 먼저 실행하세요. 앱 로그아웃은 Azure CLI 세션을 종료하지 않습니다.</p>}
    {entra && <>
      <label>테넌트 ID<input aria-label="테넌트 ID" value={form.tenant} autoComplete="off"
        onChange={(event) => fields.setExtraField?.("tenant_id", event.target.value)} /></label>
      <label>클라이언트 ID<input aria-label="클라이언트 ID" value={form.client} readOnly={form.source === "azure_cli"} autoComplete="off"
        onChange={(event) => fields.setExtraField?.("client_id", event.target.value)} /></label>
      <AuthPanel key={key} connectionId={fields.connectionId} form={form} />
    </>}
    {form.mode === "sql_password" && <p>연결의 사용자 이름과 암호는 기존 입력란에서 설정해 주세요.</p>}
    {form.mode === "integrated" && <p>Windows 통합 인증은 데이터베이스 연결 시 사용됩니다.</p>}
    {form.mode === null && <p role="alert">지원하는 인증 방식을 선택해 주세요.</p>}
  </section>;
}
