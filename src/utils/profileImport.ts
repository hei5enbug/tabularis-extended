import { invoke } from "@tauri-apps/api/core";

export interface ProfileCounts {
  connections: number;
  groups: number;
  savedQueries: number;
  notebooks: number;
  themes: number;
  pluginPackages: number;
}

export interface OfficialProfileImportStatus {
  sourceFound: boolean;
  destinationEmpty: boolean;
  canImport: boolean;
  sourceDir: string;
  destinationDir: string;
  counts: ProfileCounts;
}

export interface DriverRemapReport {
  /** Ids of the connections moved to a plus driver. */
  remapped: string[];
  /** Ids left on their legacy driver because their fields cannot be converted. */
  unconverted: string[];
  /** Plus packages found verified in the plugin directory. */
  verifiedPackages: string[];
}

export interface OfficialProfileImportReport {
  backupDir: string;
  counts: ProfileCounts;
  secretsStored: number;
  pluginPackagesCopied: string[];
  pluginPackagesSkipped: string[];
  skippedLinks: number;
  remap: DriverRemapReport | null;
}

/** Stable prefixes of the error strings the import command rejects with. */
export const PROFILE_IMPORT_ERROR_CODES = [
  "IMPORT_SOURCE_NOT_FOUND",
  "IMPORT_DESTINATION_NOT_EMPTY",
  "IMPORT_SOURCE_IS_DESTINATION",
  "IMPORT_BACKUP_FAILED",
  "IMPORT_BUILD_FAILED",
  "IMPORT_SECRET_FAILED",
  "IMPORT_PUBLISH_FAILED",
] as const;

export type ProfileImportErrorCode = (typeof PROFILE_IMPORT_ERROR_CODES)[number];

export function profileImportErrorCode(error: unknown): ProfileImportErrorCode | null {
  const text = typeof error === "string" ? error : error instanceof Error ? error.message : "";
  return PROFILE_IMPORT_ERROR_CODES.find((code) => text.startsWith(`${code}:`)) ?? null;
}

export function getOfficialProfileImportStatus(): Promise<OfficialProfileImportStatus> {
  return invoke<OfficialProfileImportStatus>("official_profile_import_status");
}

export function importOfficialProfile(): Promise<OfficialProfileImportReport> {
  return invoke<OfficialProfileImportReport>("import_official_profile");
}

export function remapConnectionsToPlusDrivers(): Promise<DriverRemapReport> {
  return invoke<DriverRemapReport>("remap_connections_to_plus_drivers");
}
