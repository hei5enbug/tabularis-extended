export const TENANT = "11111111-1111-1111-1111-111111111111";
export const OBJECT_ID = "22222222-2222-2222-2222-222222222222";
export const ENTRA_USERNAME = "entra.user@example.com";
export const SQL_USERNAME = "sql-login";
export const SQL_PASSWORD = "sql-password-placeholder";
export const CLI_PATH = "/usr/local/bin/az";

export const SQL_EXTRA: Record<string, string> = { auth_mode: "sql_password" };
export const LEGACY_INTEGRATED_EXTRA: Record<string, string> = { integrated_auth: "true" };
export const INTEGRATED_EXTRA: Record<string, string> = { auth_mode: "integrated", integrated_auth: "true" };
export const ENTRA_EXTRA: Record<string, string> = {
  auth_mode: "entra_user",
  auth_source: "azure_cli",
  tenant_id: TENANT,
  entra_object_id: OBJECT_ID,
  azure_cli_path: CLI_PATH,
};
