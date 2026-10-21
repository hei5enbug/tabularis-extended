import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  CheckCircle2,
  Download,
  KeyRound,
  Loader2,
  RefreshCw,
  X,
} from "lucide-react";
import { Modal } from "../ui/Modal";
import { toErrorMessage } from "../../utils/errors";
import { markProfileImportAwaitingRestart } from "../../utils/profileImportSession";
import {
  importOfficialProfile,
  profileImportErrorCode,
  type OfficialProfileImportReport,
  type OfficialProfileImportStatus,
  type ProfileCounts,
} from "../../utils/profileImport";

interface OfficialProfileImportModalProps {
  isOpen: boolean;
  onClose: () => void;
  status: OfficialProfileImportStatus | null;
  /**
   * Called once the import succeeded, before the result is shown. From then
   * on the dialog only offers a restart: the session's settings predate the
   * imported profile.
   */
  onImported?: (report: OfficialProfileImportReport) => void;
}

type Phase =
  | { kind: "intro" }
  | { kind: "importing" }
  | {
      kind: "done";
      report: OfficialProfileImportReport;
      connectionNames: Record<string, string>;
    }
  | { kind: "failed"; message: string };

const COUNT_KEYS: Array<keyof ProfileCounts> = [
  "connections",
  "groups",
  "savedQueries",
  "notebooks",
  "themes",
  "pluginPackages",
];

const secondaryButton =
  "px-4 py-2 text-secondary hover:text-primary transition-colors text-sm disabled:opacity-50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus";
const primaryButton =
  "flex items-center gap-2 px-4 py-2 bg-accent-primary hover:bg-accent-primary/90 disabled:opacity-50 text-inverse rounded-lg text-sm font-medium transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus";

async function loadConnectionNames(): Promise<Record<string, string>> {
  try {
    const connections =
      await invoke<Array<{ id: string; name: string }>>("get_connections");
    return Object.fromEntries(
      connections.map((connection) => [connection.id, connection.name]),
    );
  } catch {
    return {};
  }
}

export function OfficialProfileImportModal({
  isOpen,
  onClose,
  status,
  onImported,
}: OfficialProfileImportModalProps) {
  if (!isOpen) return null;
  return (
    <OfficialProfileImportDialog
      onClose={onClose}
      status={status}
      onImported={onImported}
    />
  );
}

function OfficialProfileImportDialog({
  onClose,
  status,
  onImported,
}: Omit<OfficialProfileImportModalProps, "isOpen">) {
  const { t } = useTranslation();
  const titleId = useId();
  const [phase, setPhase] = useState<Phase>({ kind: "intro" });
  const [restarting, setRestarting] = useState(false);
  const [restartError, setRestartError] = useState<string | null>(null);
  const busy = phase.kind === "importing" || restarting;
  const dismissible = phase.kind === "intro" || phase.kind === "failed";
  const close = () => {
    if (dismissible) onClose();
  };

  const restart = async () => {
    setRestarting(true);
    setRestartError(null);
    try {
      await invoke("relaunch_app");
    } catch (error) {
      setRestartError(toErrorMessage(error));
    } finally {
      setRestarting(false);
    }
  };

  const runImport = async () => {
    setPhase({ kind: "importing" });
    try {
      const report = await importOfficialProfile();
      markProfileImportAwaitingRestart();
      onImported?.(report);
      const connectionNames =
        report.remap && report.remap.unconverted.length > 0
          ? await loadConnectionNames()
          : {};
      setPhase({ kind: "done", report, connectionNames });
    } catch (error) {
      const code = profileImportErrorCode(error);
      setPhase({
        kind: "failed",
        message: code
          ? t(`profileImport.errors.${code}`)
          : t("profileImport.errors.unknown", { error: toErrorMessage(error) }),
      });
    }
  };

  return (
    <Modal isOpen onClose={close}>
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-busy={busy || undefined}
        className="bg-elevated border border-strong rounded-xl shadow-2xl w-[600px] max-w-[92vw] max-h-[90vh] overflow-hidden flex flex-col"
      >
        <div className="flex items-center justify-between p-4 border-b border-default bg-base">
          <div className="flex items-center gap-3">
            <div className="p-2 bg-accent-primary/15 rounded-lg">
              <Download size={20} className="text-accent" />
            </div>
            <div>
              <h2 id={titleId} className="text-lg font-semibold text-primary">
                {t("profileImport.title")}
              </h2>
              <p className="text-xs text-secondary">
                {t("profileImport.subtitle")}
              </p>
            </div>
          </div>
          {dismissible && (
            <button
              type="button"
              onClick={close}
              aria-label={t("common.close")}
              className="text-secondary hover:text-primary transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus"
            >
              <X size={20} />
            </button>
          )}
        </div>

        <div className="p-6 space-y-5 overflow-y-auto">
          {phase.kind === "intro" && <IntroContent status={status} />}
          {phase.kind === "importing" && (
            <div role="status" className="text-center py-8 text-muted">
              <Loader2 size={24} className="animate-spin mx-auto mb-3" />
              <p className="text-sm text-primary">{t("profileImport.importing")}</p>
              <p className="text-xs mt-1">{t("profileImport.importingKeychain")}</p>
            </div>
          )}
          {phase.kind === "done" && (
            <ResultContent
              report={phase.report}
              connectionNames={phase.connectionNames}
            />
          )}
          {phase.kind === "done" && restartError && (
            <div
              role="alert"
              className="flex gap-3 rounded-lg border border-accent-error/40 bg-accent-error/10 p-4"
            >
              <AlertTriangle size={18} className="mt-0.5 shrink-0 text-accent-error" />
              <div className="min-w-0 space-y-1">
                <p className="text-sm text-secondary leading-relaxed">
                  {t("profileImport.result.restartFailed")}
                </p>
                <p className="font-mono text-xs text-primary break-words">{restartError}</p>
              </div>
            </div>
          )}
          {phase.kind === "failed" && (
            <div
              role="alert"
              className="flex gap-3 rounded-lg border border-accent-error/40 bg-accent-error/10 p-4"
            >
              <AlertTriangle size={18} className="mt-0.5 shrink-0 text-accent-error" />
              <div className="min-w-0 space-y-1">
                <p className="text-sm font-semibold text-primary">
                  {t("profileImport.failedTitle")}
                </p>
                <p className="text-sm text-secondary leading-relaxed break-words">
                  {phase.message}
                </p>
              </div>
            </div>
          )}
        </div>

        <div className="p-4 border-t border-default bg-base/50 flex justify-end gap-3">
          {phase.kind === "intro" && (
            <>
              <button type="button" onClick={close} className={secondaryButton}>
                {t("profileImport.notNow")}
              </button>
              <button
                type="button"
                onClick={() => void runImport()}
                disabled={!status?.canImport}
                className={primaryButton}
              >
                <Download size={16} />
                {t("profileImport.import")}
              </button>
            </>
          )}
          {phase.kind === "importing" && (
            <button type="button" disabled className={primaryButton}>
              <Loader2 size={16} className="animate-spin" />
              {t("profileImport.import")}
            </button>
          )}
          {phase.kind === "done" && (
            <button
              type="button"
              onClick={() => void restart()}
              disabled={restarting}
              className={primaryButton}
            >
              {restarting ? (
                <Loader2 size={16} className="animate-spin" />
              ) : (
                <RefreshCw size={16} />
              )}
              {restartError
                ? t("profileImport.result.retryRestart")
                : t("profileImport.result.restart")}
            </button>
          )}
          {phase.kind === "failed" && (
            <button type="button" onClick={close} className={secondaryButton}>
              {t("common.close")}
            </button>
          )}
        </div>
      </div>
    </Modal>
  );
}

function IntroContent({ status }: { status: OfficialProfileImportStatus | null }) {
  const { t } = useTranslation();
  return (
    <>
      <div className="bg-surface-secondary/50 p-4 rounded-lg border border-strong space-y-3">
        <p className="text-sm text-secondary leading-relaxed">
          {t("profileImport.description")}
        </p>
        <ul className="grid grid-cols-2 gap-x-4 gap-y-1 text-sm text-primary">
          {COUNT_KEYS.map((key) => (
            <li key={key} className="flex items-center justify-between gap-2">
              <span>{t(`profileImport.items.${key}`)}</span>
              {status && (
                <span className="font-mono text-xs text-muted">
                  {status.counts[key]}
                </span>
              )}
            </li>
          ))}
          <li>{t("profileImport.items.settings")}</li>
        </ul>
      </div>
      <p className="text-sm text-secondary leading-relaxed">
        {t("profileImport.untouched")}
      </p>
      <div className="flex gap-3 rounded-lg border border-accent-warning/40 bg-accent-warning/10 p-3">
        <KeyRound size={16} className="mt-0.5 shrink-0 text-accent-warning" />
        <p className="text-sm text-secondary leading-relaxed">
          {t("profileImport.keychainNotice")}
        </p>
      </div>
      <p className="text-xs text-muted">{t("profileImport.laterHint")}</p>
    </>
  );
}

function ResultContent({
  report,
  connectionNames,
}: {
  report: OfficialProfileImportReport;
  connectionNames: Record<string, string>;
}) {
  const { t } = useTranslation();
  const remap = report.remap;
  const unconvertedNames = (remap?.unconverted ?? []).map(
    (id) => connectionNames[id] ?? id,
  );
  return (
    <>
      <div
        role="status"
        className="flex items-center gap-2 text-accent-success bg-accent-success/10 px-3 py-2 rounded-lg text-sm font-medium border border-accent-success/25"
      >
        <CheckCircle2 size={16} />
        <span>{t("profileImport.result.title")}</span>
      </div>
      <dl className="grid grid-cols-2 gap-x-6 gap-y-1.5 text-sm">
        {COUNT_KEYS.filter((key) => key !== "pluginPackages").map((key) => (
          <div key={key} className="flex items-center justify-between gap-2">
            <dt className="text-secondary">{t(`profileImport.items.${key}`)}</dt>
            <dd className="font-mono text-primary">{report.counts[key]}</dd>
          </div>
        ))}
        <div className="flex items-center justify-between gap-2">
          <dt className="text-secondary">{t("profileImport.result.pluginPackagesCopied")}</dt>
          <dd className="font-mono text-primary">{report.pluginPackagesCopied.length}</dd>
        </div>
        <div className="flex items-center justify-between gap-2">
          <dt className="text-secondary">{t("profileImport.result.secretsStored")}</dt>
          <dd className="font-mono text-primary">{report.secretsStored}</dd>
        </div>
        {remap && (
          <div className="flex items-center justify-between gap-2">
            <dt className="text-secondary">{t("profileImport.result.remapped")}</dt>
            <dd className="font-mono text-primary">{remap.remapped.length}</dd>
          </div>
        )}
      </dl>
      {report.pluginPackagesSkipped.length > 0 && (
        <ResultNote
          message={t("profileImport.result.pluginPackagesSkipped", {
            count: report.pluginPackagesSkipped.length,
          })}
          items={report.pluginPackagesSkipped}
        />
      )}
      {unconvertedNames.length > 0 && (
        <ResultNote
          message={t("profileImport.result.unconverted", {
            count: unconvertedNames.length,
          })}
          items={unconvertedNames}
        />
      )}
      {!remap && <ResultNote message={t("profileImport.result.remapUnavailable")} />}
      {report.skippedLinks > 0 && (
        <p className="text-xs text-muted">
          {t("profileImport.result.skippedLinks", { count: report.skippedLinks })}
        </p>
      )}
      <p className="text-xs text-muted break-all">
        {t("profileImport.result.backup", { path: report.backupDir })}
      </p>
      <p className="text-sm text-secondary leading-relaxed">
        {t("profileImport.result.restartHint")}
      </p>
    </>
  );
}

function ResultNote({ message, items }: { message: string; items?: string[] }) {
  return (
    <div className="flex gap-2 rounded-lg border border-accent-warning/40 bg-accent-warning/10 p-3 text-sm text-secondary leading-relaxed">
      <AlertTriangle size={16} className="mt-0.5 shrink-0 text-accent-warning" />
      <div className="min-w-0 space-y-1 break-words">
        <p>{message}</p>
        {items && items.length > 0 && (
          <ul className="list-disc pl-4 font-mono text-xs text-primary">
            {items.map((item, index) => (
              <li key={`${item}-${index}`}>{item}</li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
