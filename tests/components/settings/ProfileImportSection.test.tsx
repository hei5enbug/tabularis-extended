import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ProfileImportSection } from "../../../src/components/settings/ProfileImportSection";

vi.unmock("lucide-react");
vi.mock("react-i18next", () => { const t = (key: string) => key; return { useTranslation: () => ({ t }) }; });
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const preview = { sourcePath: "/original", targetPath: "/extended", sourceExists: true, targetIsEmpty: false, canImport: true, refusalReason: null, connectionCount: 8, pluginPackageCount: 3 };
const result = { targetPath: "/extended", backupPath: "/backup/original", destinationBackupPath: "/backup/destination", connectionCount: 8, missingCredentials: [] };

beforeEach(() => {
  vi.mocked(invoke).mockReset().mockImplementation(async command => command === "preview_original_profile_import" ? preview : result);
});

describe("원본 프로필 가져오기", () => {
  it("미리보기만 읽고 원본과 대상 경로를 표시하며 가져오기는 자동 실행하지 않습니다.", async () => {
    // given
    const component = <ProfileImportSection />;
    // when
    render(component);
    // then
    await waitFor(() => expect(screen.getByText("/original")).toBeInTheDocument());
    expect(screen.getByText("/extended")).toBeInTheDocument();
    expect(screen.getByText("profileImport.credentials")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledExactlyOnceWith("preview_original_profile_import");
  });

  it("대상이 비어 있지 않아도 canImport가 참이면 명시적인 가져오기를 허용합니다.", async () => {
    // given
    render(<ProfileImportSection />);
    await waitFor(() => expect(screen.getByRole("button", { name: "profileImport.import" })).toBeEnabled());
    // when
    fireEvent.click(screen.getByRole("button", { name: "profileImport.import" }));
    // then
    await waitFor(() => expect(screen.getByText("profileImport.restartRequired")).toBeInTheDocument());
    expect(invoke).toHaveBeenCalledWith("import_original_profile");
    expect(screen.getByText("/backup/original")).toBeInTheDocument();
    expect(screen.getByText("/backup/destination")).toBeInTheDocument();
    expect(screen.queryByText("profileImport.missingCredentials")).not.toBeInTheDocument();
  });

  it("canImport가 거짓이면 빈 대상이어도 가져오기를 차단합니다.", async () => {
    // given
    vi.mocked(invoke).mockResolvedValue({ ...preview, targetIsEmpty: true, canImport: false, refusalReason: "source unavailable" });
    // when
    render(<ProfileImportSection />);
    // then
    await waitFor(() => expect(screen.getByText("profileImport.refused")).toBeInTheDocument());
    expect(screen.getByRole("button", { name: "profileImport.import" })).toBeDisabled();
    expect(invoke).not.toHaveBeenCalledWith("import_original_profile");
  });

  it("누락된 자격 증명만 다시 입력하도록 표시합니다.", async () => {
    // given
    vi.mocked(invoke).mockImplementation(async command => command === "preview_original_profile_import" ? preview : { ...result, missingCredentials: [{ connectionId: "missing-connection", slot: "password" }] });
    render(<ProfileImportSection />);
    await waitFor(() => expect(screen.getByRole("button", { name: "profileImport.import" })).toBeEnabled());
    // when
    fireEvent.click(screen.getByRole("button", { name: "profileImport.import" }));
    // then
    await waitFor(() => expect(screen.getByText("profileImport.missingCredentials")).toBeInTheDocument());
    expect(screen.getByText("missing-connection · password")).toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
    expect(invoke).not.toHaveBeenCalledWith("hide_connection", expect.anything());
  });

  it.each(["sourceUnavailable", "destinationNotEmpty", "sameSourceAndDestination", "failed", "recoveryRequired"])("%s 오류를 표시하고 가져오기를 자동 재시도하지 않습니다.", async kind => {
    // given
    vi.mocked(invoke).mockImplementation(async command => {
      if (command === "preview_original_profile_import") return preview;
      throw { kind, path: "/fixture", reason: "fixture", operation: "import", message: "fixture" };
    });
    render(<ProfileImportSection />);
    await waitFor(() => expect(screen.getByRole("button", { name: "profileImport.import" })).toBeEnabled());
    // when
    fireEvent.click(screen.getByRole("button", { name: "profileImport.import" }));
    // then
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(`profileImport.${kind}`));
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "import_original_profile")).toHaveLength(1);
  });

  it("미리보기 조회가 실패하면 오류를 표시하고 가져오기를 차단합니다.", async () => {
    // given
    vi.mocked(invoke).mockRejectedValue({ kind: "sourceUnavailable", path: "/original" });
    // when
    render(<ProfileImportSection />);
    // then
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("profileImport.sourceUnavailable"));
    expect(screen.getByRole("button", { name: "profileImport.import" })).toBeDisabled();
  });
});
