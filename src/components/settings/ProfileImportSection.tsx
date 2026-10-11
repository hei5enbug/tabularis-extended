import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { FolderInput, Loader2 } from "lucide-react";
import { SettingSection } from "./SettingControls";

interface ProfileImportPreview {
  sourcePath: string;
  targetPath: string;
  sourceExists: boolean;
  targetIsEmpty: boolean;
  canImport: boolean;
  refusalReason: string | null;
  connectionCount: number;
  pluginPackageCount: number;
}

interface ProfileImportResult {
  targetPath: string;
  backupPath: string;
  destinationBackupPath?: string;
  pluginBackupPath?: string;
  connectionCount: number;
  missingCredentials: { connectionId: string; slot: string }[];
}

const buttonClass = "px-3 py-2 rounded-lg border border-strong text-sm text-primary disabled:opacity-50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus";

export function ProfileImportSection() {
  const { t } = useTranslation();
  const [preview, setPreview] = useState<ProfileImportPreview | null>(null);
  const [loading, setLoading] = useState(true);
  const [importing, setImporting] = useState(false);
  const [result, setResult] = useState<ProfileImportResult | null>(null);
  const [error, setError] = useState("");

  const errorMessage = useCallback((failure: unknown) => {
    if (typeof failure === "object" && failure !== null && "kind" in failure) {
      const details = failure as { kind: string; path?: string; reason?: string; message?: string; operation?: string };
      switch (details.kind) {
        case "sourceUnavailable": return t("profileImport.sourceUnavailable", { path: details.path });
        case "destinationNotEmpty": return t("profileImport.destinationNotEmpty", { path: details.path, reason: details.reason });
        case "sameSourceAndDestination": return t("profileImport.sameSourceAndDestination", { path: details.path });
        case "recoveryRequired": return t("profileImport.recoveryRequired", { operation: details.operation, message: details.message });
        case "failed": return t("profileImport.failed", { message: details.message });
      }
    }
    return t("profileImport.failed", { message: failure instanceof Error ? failure.message : String(failure) });
  }, [t]);

  useEffect(() => {
    let alive = true;
    void invoke<ProfileImportPreview>("preview_original_profile_import").then(value => {
      if (alive) setPreview(value);
    }).catch(failure => {
      if (alive) setError(errorMessage(failure));
    }).finally(() => {
      if (alive) setLoading(false);
    });
    return () => { alive = false; };
  }, [errorMessage]);

  const refresh = async () => {
    setLoading(true);
    setError("");
    try {
      setPreview(await invoke<ProfileImportPreview>("preview_original_profile_import"));
    } catch (failure) {
      setPreview(null);
      setError(errorMessage(failure));
    } finally {
      setLoading(false);
    }
  };

  const importProfile = async () => {
    if (!preview?.canImport || loading || importing || result) return;
    setImporting(true);
    setError("");
    try {
      setResult(await invoke<ProfileImportResult>("import_original_profile"));
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setImporting(false);
    }
  };

  return <SettingSection title={t("profileImport.title")} icon={<FolderInput size={14} />}>
    <div id="original-profile-import" className="space-y-4 py-3">
      <p className="text-sm text-secondary">{t("profileImport.description")}</p>
      <p className="text-sm text-secondary">{t("profileImport.credentials")}</p>
      {loading && <p role="status" className="text-sm text-muted">{t("profileImport.loading")}</p>}
      {preview && <>
        <dl className="grid gap-2 text-sm">
          <div><dt className="text-muted">{t("profileImport.source")}</dt><dd className="font-mono break-all text-primary">{preview.sourcePath}</dd></div>
          <div><dt className="text-muted">{t("profileImport.target")}</dt><dd className="font-mono break-all text-primary">{preview.targetPath}</dd></div>
        </dl>
        <p className="text-sm text-secondary">{t("profileImport.counts", { connections: preview.connectionCount, plugins: preview.pluginPackageCount })}</p>
        {!preview.canImport && !result && <p role="status" className="text-sm text-accent-warning">{t("profileImport.refused", { reason: preview.refusalReason ?? t("profileImport.unavailable") })}</p>}
      </>}
      {error && <p role="alert" className="text-sm text-accent-error whitespace-pre-wrap">{error}</p>}
      {result ? <div className="space-y-3 rounded-lg border border-default bg-base p-4">
        <p role="status" className="text-sm text-accent-success">{t("profileImport.complete", { count: result.connectionCount })}</p>
        <p className="text-sm text-secondary">{t("profileImport.restartRequired")}</p>
        <dl className="text-xs text-secondary space-y-2">
          <div><dt>{t("profileImport.target")}</dt><dd className="font-mono break-all">{result.targetPath}</dd></div>
          <div><dt>{t("profileImport.backup")}</dt><dd className="font-mono break-all">{result.backupPath}</dd></div>
          {result.destinationBackupPath && <div><dt>{t("profileImport.destinationBackup")}</dt><dd className="font-mono break-all">{result.destinationBackupPath}</dd></div>}
          {result.pluginBackupPath && <div><dt>{t("profileImport.pluginBackup")}</dt><dd className="font-mono break-all">{result.pluginBackupPath}</dd></div>}
        </dl>
        {result.missingCredentials.length > 0 && <div role="alert" className="text-sm text-accent-warning">
          <p>{t("profileImport.missingCredentials")}</p>
          <ul className="list-disc pl-5">{result.missingCredentials.map(item => <li key={`${item.connectionId}:${item.slot}`}>{item.connectionId} · {item.slot}</li>)}</ul>
        </div>}
      </div> : <div className="flex gap-2">
        <button type="button" className={buttonClass} disabled={loading || importing} onClick={() => void refresh()}>{t("profileImport.refresh")}</button>
        <button type="button" className={`${buttonClass} bg-accent-primary text-inverse flex items-center gap-2`} disabled={!preview?.canImport || loading || importing} onClick={() => void importProfile()}>
          {importing && <Loader2 size={14} className="animate-spin" />}{t("profileImport.import")}
        </button>
      </div>}
    </div>
  </SettingSection>;
}
