import { useState } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import { invoke } from "@tauri-apps/api/core";
import {
  Github,
  CheckCircle2,
  Circle,
  Heart,
  Info,
  Code2,
  Library,
  Download,
  ExternalLink,
  Activity,
  Sparkles,
  Share2,
} from "lucide-react";
import clsx from "clsx";
import { useTheme } from "../../hooks/useTheme";
import { useChangelog } from "../../hooks/useChangelog";
import { APP_VERSION } from "../../version";
import { ROADMAP } from "../../utils/settings";
import {
  SettingSection,
} from "./SettingControls";
import { WhatsNewModal } from "../modals/WhatsNewModal";
import { OpenSourceLibrariesModal } from "../modals/OpenSourceLibrariesModal";
import { SocialLinks } from "../SocialLinks";

export function InfoTab() {
  const { t } = useTranslation();
  const { currentTheme } = useTheme();
  const {
    entries: changelogEntries,
    isLoading: isChangelogLoading,
  } = useChangelog();
  const [isWhatsNewOpen, setIsWhatsNewOpen] = useState(false);
  const [isOpenSourceLibrariesOpen, setIsOpenSourceLibrariesOpen] =
    useState(false);

  return (
    <div>
      {/* Hero */}
      <div className="bg-gradient-to-br from-accent-primary/10 to-elevated border border-accent-primary/20 rounded-2xl p-8 text-center relative overflow-hidden mb-8">
        <div className="absolute top-0 right-0 p-4 opacity-10">
          <Code2 size={120} />
        </div>

        <div className="p-2">
          <img
            src="/logo.png"
            alt="Tabularis Extended"
            className="w-16 h-16 rounded-2xl mx-auto mb-4 shadow-lg shadow-accent-primary/30"
            style={{
              backgroundColor: !currentTheme?.id?.includes("-light")
                ? currentTheme?.colors?.surface?.secondary || "#334155"
                : currentTheme?.colors?.bg?.elevated || "#f8fafc",
            }}
          />
        </div>

        <h1 className="text-3xl font-bold text-primary mb-2">Tabularis Extended</h1>
        <p className="text-secondary max-w-lg mx-auto mb-6">
          A lightweight, developer-focused database manager built with Tauri,
          Rust, and React. Born from a &quot;vibe coding&quot; experiment to
          create a modern, native tool in record time.
        </p>

        <div className="flex justify-center gap-4 flex-wrap">
          <button
            onClick={() => openUrl("https://github.com/TabularisDB/tabularis")}
            className="flex items-center gap-2 bg-surface-secondary hover:bg-surface-tertiary text-primary px-4 py-2 rounded-lg font-medium transition-colors border border-strong"
          >
            <Github size={18} />
            {t("settings.starOnGithub")}
          </button>
          <div className="flex items-center gap-2 bg-accent/10 text-accent px-4 py-2 rounded-lg border border-accent/30">
            <span className="text-xs font-bold uppercase tracking-wider">
              {t("settings.version")}
            </span>
            <span className="font-mono font-bold">
              {APP_VERSION} (Beta)
            </span>
          </div>
          <button
            onClick={() => setIsWhatsNewOpen(true)}
            className="flex items-center gap-2 bg-accent-secondary/10 hover:bg-accent-secondary/15 text-accent-secondary px-4 py-2 rounded-lg font-medium transition-colors border border-accent-secondary/30"
          >
            <Sparkles size={18} />
            {t("whatsNew.title")}
          </button>
          <button
            onClick={() => setIsOpenSourceLibrariesOpen(true)}
            className="flex items-center gap-2 bg-accent-primary/20 hover:bg-accent-primary/30 text-accent px-4 py-2 rounded-lg font-medium transition-colors border border-accent-primary/30"
          >
            <Library size={18} />
            {t("settings.openSourceLibraries")}
          </button>
        </div>
      </div>

      <SettingSection title={t("settings.updates")} icon={<Download size={14} className="text-muted" />}>
        <p className="py-3 text-sm text-secondary">{t("update.extendedManual")}</p>
        <p className="pb-3 font-mono text-sm text-primary">{APP_VERSION}</p>
      </SettingSection>

      {/* Roadmap */}
      <SettingSection
        title={t("settings.projectStatus")}
        icon={<Info size={14} className="text-muted" />}
        description={t("settings.roadmapDesc")}
      >
        <div className="bg-elevated border border-default rounded-xl overflow-hidden mt-3">
          <div className="divide-y divide-default">
            {ROADMAP.map((item, i) => {
              const content = (
                <>
                  {item.done ? (
                    <CheckCircle2
                      size={18}
                      className="text-accent-success shrink-0"
                    />
                  ) : (
                    <Circle
                      size={18}
                      className="text-surface-tertiary shrink-0"
                    />
                  )}
                  <span
                    className={clsx(
                      "flex-1 text-left",
                      item.done ? "text-primary" : "text-muted",
                    )}
                  >
                    {item.label}
                  </span>
                  {item.url && (
                    <ExternalLink
                      size={14}
                      className="text-surface-tertiary opacity-0 group-hover:opacity-100 transition-opacity"
                    />
                  )}
                </>
              );

              if (item.url) {
                return (
                  <button
                    key={i}
                    onClick={() => openUrl(item.url!)}
                    className="w-full p-4 flex items-center gap-3 hover:bg-surface-secondary/30 transition-colors group cursor-pointer"
                  >
                    {content}
                  </button>
                );
              }

              return (
                <div
                  key={i}
                  className="p-4 flex items-center gap-3 hover:bg-surface-secondary/30 transition-colors group"
                >
                  {content}
                </div>
              );
            })}
          </div>
        </div>
      </SettingSection>

      {/* Task Manager */}
      <SettingSection
        title={t("taskManager.header.title")}
        icon={<Activity size={14} className="text-muted" />}
        description={t("taskManager.header.description")}
        action={
          <button
            onClick={() => invoke("open_task_manager_window")}
            className="flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium bg-accent-primary/15 border border-accent-primary/25 text-accent hover:bg-accent-primary/25 transition-colors"
          >
            <Activity size={14} />
            {t("taskManager.header.open")}
          </button>
        }
      >
        <div />
      </SettingSection>

      {/* Follow us */}
      <SettingSection
        title={t("settings.followUs")}
        icon={<Share2 size={14} className="text-muted" />}
        description={t("settings.followUsDesc")}
      >
        <div className="pt-3">
          <SocialLinks showLabels />
        </div>
      </SettingSection>

      {/* Support */}
      <SettingSection
        title={t("settings.support")}
        icon={<Heart size={14} className="text-muted" />}
        description={t("settings.supportDesc")}
      >
        <div className="pt-3 flex flex-col items-center text-center">
          <button
            onClick={() => openUrl("https://github.com/TabularisDB/tabularis")}
            className="text-accent hover:text-accent/90 font-medium text-sm hover:underline"
          >
            github.com/TabularisDB/tabularis
          </button>
        </div>
      </SettingSection>

      <WhatsNewModal
        isOpen={isWhatsNewOpen}
        onClose={() => setIsWhatsNewOpen(false)}
        entries={changelogEntries}
        isLoading={isChangelogLoading}
      />

      <OpenSourceLibrariesModal
        isOpen={isOpenSourceLibrariesOpen}
        onClose={() => setIsOpenSourceLibrariesOpen(false)}
      />
    </div>
  );
}
