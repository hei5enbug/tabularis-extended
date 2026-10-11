import { useEffect, useId } from "react";
import type { SlotComponentProps } from "@tabularis/plugin-api";

interface ExtraFieldsContext {
  extra?: Record<string, string>;
  setExtraField?: (key: string, value: string) => void;
  credentialFieldsHidden?: boolean;
  setCredentialFieldsHidden?: (hidden: boolean) => void;
  passwordFieldHidden?: boolean;
  setPasswordFieldHidden?: (hidden: boolean) => void;
}

type AuthMode = "sql" | "integrated" | "entra_user";

const entraFields = ["auth_mode", "auth_source", "tenant_id", "entra_object_id", "azure_cli_path"];

export default function IntegratedAuthToggle({ context }: SlotComponentProps) {
  const c = context as ExtraFieldsContext;
  const id = useId();
  const mode: AuthMode = c.extra?.auth_mode === "entra_user" ? "entra_user" : c.extra?.integrated_auth === "true" ? "integrated" : "sql";
  const setCredentialFieldsHidden = c.setCredentialFieldsHidden;
  const setPasswordFieldHidden = c.setPasswordFieldHidden;
  const supportsEntra = typeof setPasswordFieldHidden === "function";

  useEffect(() => {
    if (mode === "entra_user") {
      if (typeof setPasswordFieldHidden !== "function") return;
      setCredentialFieldsHidden?.(false);
      setPasswordFieldHidden(true);
    } else {
      setPasswordFieldHidden?.(false);
      setCredentialFieldsHidden?.(mode === "integrated");
    }
  }, [mode, setCredentialFieldsHidden, setPasswordFieldHidden]);

  const changeMode = (nextMode: AuthMode) => {
    if (!c.setExtraField || nextMode === mode || (nextMode === "entra_user" && !supportsEntra)) return;
    if (nextMode === "entra_user") {
      c.setExtraField("integrated_auth", "");
      c.setExtraField("auth_mode", "entra_user");
      c.setExtraField("auth_source", "azure_cli");
      setCredentialFieldsHidden?.(false);
      setPasswordFieldHidden?.(true);
    } else {
      for (const field of entraFields) c.setExtraField(field, "");
      c.setExtraField("integrated_auth", nextMode === "integrated" ? "true" : "");
      setPasswordFieldHidden?.(false);
      setCredentialFieldsHidden?.(nextMode === "integrated");
    }
  };

  return (
    <div style={{ display: "grid", gap: 12 }}>
      <label style={{ display: "grid", gap: 4 }}>
        Authentication
        <select value={mode} disabled={!c.setExtraField} onChange={event => changeMode(event.target.value as AuthMode)}>
          <option value="sql">SQL password</option>
          <option value="integrated">Windows / Kerberos</option>
          <option value="entra_user" disabled={!supportsEntra}>Entra user (Azure CLI)</option>
        </select>
      </label>
      {!supportsEntra ? (
        <p role="alert" style={{ margin: 0 }}>
          Entra authentication requires a compatible Tabularis Extended host that can hide only the password field. Update the host to use Entra user (Azure CLI).
        </p>
      ) : null}
      {mode === "entra_user" ? (
        <div style={{ display: "grid", gap: 12 }}>
          <p id={`${id}-identity-help`} style={{ margin: 0 }}>
            Expected username: enter the expected Entra user principal in the connection's Username field. Azure CLI must already be signed in as this user in the specified tenant. Sign in and complete MFA in Azure CLI before connecting.
          </p>
          <label style={{ display: "grid", gap: 4 }}>
            Tenant ID
            <input
              type="text"
              required
              value={c.extra?.tenant_id ?? ""}
              disabled={!c.setExtraField}
              aria-describedby={`${id}-identity-help`}
              onChange={event => c.setExtraField?.("tenant_id", event.target.value)}
            />
          </label>
          <label style={{ display: "grid", gap: 4 }}>
            Object ID
            <input
              type="text"
              required
              value={c.extra?.entra_object_id ?? ""}
              disabled={!c.setExtraField}
              aria-describedby={`${id}-identity-help`}
              onChange={event => c.setExtraField?.("entra_object_id", event.target.value)}
            />
          </label>
          <label style={{ display: "grid", gap: 4 }}>
            Azure CLI executable path (optional)
            <input
              type="text"
              value={c.extra?.azure_cli_path ?? ""}
              disabled={!c.setExtraField}
              aria-describedby={`${id}-cli-help`}
              onChange={event => c.setExtraField?.("azure_cli_path", event.target.value)}
            />
          </label>
          <p id={`${id}-cli-help`} style={{ margin: 0 }}>
            Use an absolute executable path, or leave blank to find az on PATH.
          </p>
          <p style={{ margin: 0 }}>
            Entra requires a public Azure SQL hostname ending in .database.windows.net, TLS mode verify-full, and Trust server certificate turned off. Set these in the connection settings before connecting.
          </p>
        </div>
      ) : null}
    </div>
  );
}
