import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { InfoTab } from "../../../src/components/settings/InfoTab";

const translations: Record<string, string> = {
  "settings.releaseChannel": "Release channel",
  "settings.releaseChannelDesc":
    "Stable ships tested releases. Nightly builds from the latest main branch — newer, but may be unstable.",
  "settings.channelStable": "Stable",
  "settings.channelNightly": "Nightly",
  "update.nightlyWarning":
    "Nightly builds are unstable pre-releases. Only assets built for the nightly channel are installed.",
  "update.managedByPackageManager": "Updates managed by {{source}}",
  "update.managedByPackageManagerDesc":
    "Use your package manager to update Tabularis.",
};

vi.mock("lucide-react", () => ({
  Github: () => null,
  CheckCircle2: () => null,
  Circle: () => null,
  Heart: () => null,
  Info: () => null,
  Code2: () => null,
  Library: () => null,
  Download: () => null,
  Loader2: () => null,
  ExternalLink: () => null,
  Activity: () => null,
  Sparkles: () => null,
  Share2: () => null,
  AlertTriangle: () => null,
  ArrowUpCircle: () => null,
  RefreshCw: () => null,
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: Record<string, unknown>) =>
      (translations[key] ?? key).replace(
        "{{source}}",
        String(values?.source ?? ""),
      ),
  }),
}));

vi.mock("../../../src/hooks/useTheme", () => ({
  useTheme: vi.fn(() => ({
    currentTheme: { id: "tabularis-dark", colors: {} },
  })),
}));

vi.mock("../../../src/hooks/useChangelog", () => ({
  useChangelog: vi.fn(() => ({
    entries: [],
    isLoading: false,
    error: null,
  })),
}));

// Not under test here; avoid pulling in their own icon/markdown dependencies.
vi.mock("../../../src/components/modals/WhatsNewModal", () => ({
  WhatsNewModal: () => null,
}));
vi.mock("../../../src/components/modals/OpenSourceLibrariesModal", () => ({
  OpenSourceLibrariesModal: () => null,
}));

describe("Extended 정보 화면", () => {
  beforeEach(() => { vi.clearAllMocks(); });

  it("앱 이름을 Tabularis Extended로 표시합니다.", () => {
    // given
    const component = <InfoTab />;
    // when
    render(component);
    // then
    expect(screen.getByRole("heading", { name: "Tabularis Extended" })).toBeInTheDocument();
  });

  it("수동 설치 안내를 표시하고 공식 코어 업데이트와 채널 선택을 제공하지 않습니다.", () => {
    // given
    const component = <InfoTab />;
    // when
    render(component);
    // then
    expect(screen.getByText("update.extendedManual")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "settings.checkNow" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "update.downloadAndInstall" })).not.toBeInTheDocument();
    expect(screen.queryByText("Release channel")).not.toBeInTheDocument();
    expect(screen.queryByText("settings.autoCheckUpdates")).not.toBeInTheDocument();
  });
});
