import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AiApprovalModal } from "../../../src/components/modals/AiApprovalModal";
import type { PendingApproval } from "../../../src/types/ai";

vi.mock("lucide-react", () => vi.importActual("lucide-react"));

vi.mock("../../../src/components/ui/LazyMonaco", () => ({
  MonacoEditor: ({
    value,
    onChange,
    options,
  }: {
    value?: string;
    onChange?: (value: string | undefined) => void;
    options?: { readOnly?: boolean };
  }) => (
    <textarea
      aria-label="query-editor"
      value={value ?? ""}
      readOnly={options?.readOnly === true}
      onChange={(event) => onChange?.(event.target.value)}
    />
  ),
}));

vi.mock("../../../src/hooks/useEditorTheme", () => ({
  useEditorTheme: () => ({ id: "test-theme" }),
}));

vi.mock("../../../src/themes/themeUtils", () => ({
  loadMonacoTheme: vi.fn(),
}));

vi.mock("../../../src/themes/themeRuntime", () => ({
  getMonacoThemeId: (id: string) => id,
}));

vi.mock("../../../src/components/explain/VisualExplainView", () => ({
  VisualExplainView: () => null,
}));

function approval(overrides: Partial<PendingApproval> = {}): PendingApproval {
  return {
    id: "approval-1",
    createdAt: "2026-10-21T00:00:00Z",
    sessionId: "session-1",
    connectionId: "fixture",
    connectionName: "Fixture DB",
    query: "UPDATE accounts SET active = 1",
    queryKind: "write",
    clientHint: null,
    explainPlan: null,
    explainError: null,
    ...overrides,
  };
}

function serviceApproval(
  operation: string,
  shown: Record<string, unknown>,
  credentialRetarget = false,
): PendingApproval {
  return approval({
    query: JSON.stringify({ operation, ...shown }),
    queryKind: `service:${operation}` as PendingApproval["queryKind"],
    clientHint: "service",
    editable: false,
    service: { operation, requestHash: "a".repeat(64), credentialRetarget },
  });
}

function renderModal(pending: PendingApproval) {
  const onApprove = vi.fn();
  const onDeny = vi.fn();
  render(
    <AiApprovalModal
      approval={pending}
      onApprove={onApprove}
      onDeny={onDeny}
      onClose={vi.fn()}
    />,
  );
  return { onApprove, onDeny };
}

describe("AiApprovalModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("수정할 수 없는 서비스 승인은 쿼리 편집 없이 작업과 요청 요약만 보여준다", async () => {
    // given
    const pending = serviceApproval("row.update", {
      input: { target: { table: "accounts" }, fields: ["active"], change_count: 1 },
    });
    const { onApprove } = renderModal(pending);

    // when
    fireEvent.click(screen.getByText("aiApproval.approve"));

    // then
    expect(screen.queryByText("aiApproval.editQuery")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("query-editor")).not.toBeInTheDocument();
    expect(screen.getByText("row.update")).toBeInTheDocument();
    expect(screen.getByText("aiApproval.service.locked")).toBeInTheDocument();
    expect(
      screen.getByLabelText("aiApproval.service.request"),
    ).toHaveTextContent('"table": "accounts"');
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    await waitFor(() => expect(onApprove).toHaveBeenCalledWith(undefined));
  });

  it("저장된 자격 증명을 새 대상으로 보내는 연결 수정 승인은 경고와 전체 변경 내용을 보여준다", () => {
    // given
    const pending = serviceApproval(
      "connection.update",
      {
        input: {
          patch: { settings: { host: "elsewhere.example", database: "fixture" } },
        },
        connection_id: "fixture",
        credential_retarget: true,
      },
      true,
    );

    // when
    renderModal(pending);

    // then
    expect(screen.getByRole("alert")).toHaveTextContent(
      "aiApproval.service.retargetTitle",
    );
    const table = screen.getByRole("table");
    expect(within(table).getByText("settings.host")).toBeInTheDocument();
    expect(within(table).getByText("elsewhere.example")).toBeInTheDocument();
    expect(within(table).getByText("settings.database")).toBeInTheDocument();
    expect(screen.getByText("Fixture DB (fixture)")).toBeInTheDocument();
  });

  it("새 연결 생성 승인은 새 연결 대상과 모든 설정을 보여주고 재지정 경고는 띄우지 않는다", () => {
    // given
    const pending = serviceApproval("connection.create", {
      input: {
        name: "Created",
        driver: "postgresql-plus",
        settings: { host: "localhost", port: 5432 },
        credential_ref: null,
      },
      connection_id: null,
      credential_retarget: false,
    });

    // when
    renderModal({ ...pending, connectionId: "", connectionName: "Created" });

    // then
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByText("aiApproval.service.newConnection")).toBeInTheDocument();
    const table = screen.getByRole("table");
    expect(within(table).getByText("Created")).toBeInTheDocument();
    expect(within(table).getByText("settings.port")).toBeInTheDocument();
    expect(within(table).getByText("5432")).toBeInTheDocument();
  });

  it("기존의 편집 가능한 승인은 이전처럼 쿼리를 고쳐서 승인할 수 있다", async () => {
    // given
    const { onApprove } = renderModal(approval());
    fireEvent.click(screen.getByText("aiApproval.editQuery"));
    fireEvent.change(screen.getByLabelText("query-editor"), {
      target: { value: "UPDATE accounts SET active = 0" },
    });

    // when
    fireEvent.click(screen.getByText("aiApproval.approve"));

    // then
    expect(screen.getByLabelText("query-editor")).not.toHaveAttribute("readonly");
    expect(screen.getByText("aiApproval.preflightPlan")).toBeInTheDocument();
    expect(screen.queryByText("aiApproval.service.locked")).not.toBeInTheDocument();
    await waitFor(() =>
      expect(onApprove).toHaveBeenCalledWith("UPDATE accounts SET active = 0"),
    );
  });
});
