import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { Connections } from "../../src/pages/Connections";
import { ToastContext } from "../../src/contexts/ToastContext";
import type { SavedConnection } from "../../src/contexts/DatabaseContext";

const mocks = vi.hoisted(() => ({
  connections: [] as SavedConnection[],
  drivers: [] as unknown[],
  loadConnections: vi.fn(),
  migrateConnection: vi.fn(),
  undoMigration: vi.fn(),
  navigate: vi.fn(),
  openConnectionInNewWindow: vi.fn(),
  createSqliteDatabase: vi.fn(),
  settings: { autoConnectLastConnection: false },
  connectionGroups: [] as unknown[],
  database: {} as Record<string, unknown>,
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: Record<string, unknown>) =>
      options && Object.keys(options).length > 0
        ? `${key} ${JSON.stringify(options)}`
        : key,
    i18n: { language: "en", changeLanguage: vi.fn() },
  }),
}));

vi.mock("lucide-react", () => vi.importActual("lucide-react"));

vi.mock("react-router-dom", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router-dom")>()),
  useLocation: () => ({ state: null }),
  useNavigate: () => mocks.navigate,
}));

// Every value is stable across renders, as the real context's are.
mocks.database = {
  connect: vi.fn(),
  disconnect: vi.fn(),
  isConnectionOpen: () => false,
  isConnectionOpenAnywhere: () => false,
  switchConnection: vi.fn(),
  connectionGroups: mocks.connectionGroups,
  createGroupPath: vi.fn(),
  updateGroup: vi.fn(),
  moveGroupToParent: vi.fn(),
  deleteGroup: vi.fn(),
  moveConnectionToGroup: vi.fn(),
  reorderConnectionsInGroup: vi.fn(),
  reorderGroups: vi.fn(),
  toggleGroupCollapsed: vi.fn(),
  loadConnections: mocks.loadConnections,
  connections: mocks.connections,
};

vi.mock("../../src/hooks/useDatabase", () => ({
  useDatabase: () => mocks.database,
}));

vi.mock("../../src/hooks/useDrivers", () => ({
  useDrivers: () => ({
    drivers: mocks.drivers,
    allDrivers: mocks.drivers,
    installedPlugins: [],
  }),
}));

vi.mock("../../src/hooks/usePluginRegistry", () => ({
  usePluginRegistry: () => ({ plugins: [], updates: [], loading: false, error: null, refresh: vi.fn() }),
}));

vi.mock("../../src/hooks/useSettings", () => ({
  useSettings: () => ({ settings: mocks.settings, isLoading: false }),
}));

vi.mock("../../src/hooks/useConnectionTags", () => ({
  useConnectionTags: () => ({
    tags: [],
    refresh: vi.fn().mockResolvedValue(undefined),
    createTag: vi.fn(),
    updateTag: vi.fn(),
    deleteTag: vi.fn(),
  }),
}));

vi.mock("../../src/hooks/useOpenConnectionInNewWindow", () => ({
  useOpenConnectionInNewWindow: () => mocks.openConnectionInNewWindow,
}));

vi.mock("../../src/hooks/useCreateSqliteDatabase", () => ({
  useCreateSqliteDatabase: () => ({
    createSqliteDatabase: mocks.createSqliteDatabase,
    isCreating: false,
  }),
}));

vi.mock("../../src/hooks/useBuiltinDriverMigration", () => ({
  useBuiltinPostgresMigration: () => ({
    builtinId: "postgres",
    pluginId: "postgresql",
    builtinConnections: [],
    needsMigration: false,
    pluginReady: false,
    registryOffline: false,
    banner: null,
    dismissBanner: vi.fn(),
    migrateConnection: mocks.migrateConnection,
    undoMigration: mocks.undoMigration,
  }),
}));

vi.mock("../../src/hooks/useConnectionCatalogue", () => ({
  useConnectionCatalogue: () => ({
    groups: [],
    facets: [],
    loading: false,
    registryOffline: false,
    registry: [],
    refresh: vi.fn(),
  }),
}));

vi.mock("../../src/components/modals/NewConnectionModal", () => ({
  NewConnectionModal: () => null,
}));

vi.mock("../../src/components/modals/ConfirmModal", () => ({
  ConfirmModal: ({ isOpen, onConfirm }: { isOpen: boolean; onConfirm: () => void }) =>
    isOpen ? (
      <button type="button" onClick={onConfirm}>
        confirm-modal
      </button>
    ) : null,
}));

vi.mock("../../src/components/modals/ExportConnectionsModal", () => ({
  ExportConnectionsModal: () => null,
}));

vi.mock("../../src/components/modals/ImportFromAppModal", () => ({
  ImportFromAppModal: () => null,
}));

const sqliteDriver = {
  id: "sqlite",
  name: "SQLite",
  version: "1.0.0",
  is_builtin: true,
  capabilities: { file_based: true, folder_based: false },
};

const postgresDrivers = [
  {
    id: "postgres",
    name: "PostgreSQL (built-in)",
    version: "1.0.0",
    is_builtin: true,
    capabilities: { file_based: false, folder_based: false },
    deprecated: { replacement_id: "postgresql" },
  },
  {
    id: "postgresql",
    name: "PostgreSQL",
    version: "1.0.0",
    is_builtin: false,
    capabilities: { file_based: false, folder_based: false },
  },
];

function setConnection(connection: SavedConnection) {
  mocks.connections.splice(0, mocks.connections.length, connection);
}

function renderPage() {
  const showToast = vi.fn();
  render(
    <ToastContext.Provider value={{ showToast }}>
      <Connections />
    </ToastContext.Provider>,
  );
  return { showToast };
}

describe("Connections save issues", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.spyOn(console, "error").mockImplementation(() => {});
    mocks.loadConnections.mockResolvedValue(undefined);
    mocks.drivers.splice(0, mocks.drivers.length, sqliteDriver);
    setConnection({
      id: "conn-prod",
      name: "Prod orders",
      params: { driver: "sqlite", database: "/tmp/orders.db" },
    } as SavedConnection);
  });

  it("삭제 중 이전 자격 증명을 복원하지 못하면 영향받은 연결 이름과 함께 안내한다", async () => {
    // given
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "delete_connection") {
        throw "CONNECTION_RECOVERY_REQUIRED:conn-prod";
      }
      return undefined;
    });
    renderPage();
    fireEvent.click(screen.getByTitle("connections.delete"));

    // when
    fireEvent.click(screen.getByText("confirm-modal"));

    // then
    const banner = await screen.findByText(/connections\.saveIssue\.recovery/);
    expect(banner).toHaveTextContent('"names":"Prod orders"');
    expect(banner).not.toHaveTextContent("CONNECTION_RECOVERY_REQUIRED");
  });

  it("복제 중 이전 자격 증명을 복원하지 못하면 일반 실패 대신 같은 안내를 보여준다", async () => {
    // given
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "duplicate_connection") {
        throw new Error("CONNECTION_RECOVERY_REQUIRED:conn-prod");
      }
      return undefined;
    });
    renderPage();

    // when
    fireEvent.click(screen.getByTitle("connections.clone"));

    // then
    const banner = await screen.findByText(/connections\.saveIssue\.recovery/);
    expect(banner).toHaveTextContent('"names":"Prod orders"');
    expect(screen.queryByText("connections.failDuplicate")).not.toBeInTheDocument();
  });

  it("드라이버 전환이 다른 곳의 변경과 충돌하면 충돌 안내를 알림으로 보여준다", async () => {
    // given
    mocks.drivers.splice(0, mocks.drivers.length, ...postgresDrivers);
    setConnection({
      id: "conn-pg",
      name: "Legacy PG",
      params: { driver: "postgres", database: "app" },
    } as SavedConnection);
    mocks.migrateConnection.mockResolvedValue({
      connectionId: "conn-pg",
      connectionName: "Legacy PG",
      status: "failed",
      error: "CONNECTION_CONFLICT",
      pluginId: "postgresql",
    });
    const { showToast } = renderPage();
    fireEvent.click(screen.getByTitle("migration.switchToPlugin"));

    // when
    fireEvent.click(screen.getByText("confirm-modal"));

    // then
    await waitFor(() =>
      expect(showToast).toHaveBeenCalledWith(
        "connections.saveIssue.conflict",
        expect.objectContaining({ kind: "error" }),
      ),
    );
  });
});
