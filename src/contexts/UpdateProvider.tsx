import { useMemo, type ReactNode } from "react";
import { UpdateContext } from "./UpdateContext";

export const UpdateProvider = ({ children }: { children: ReactNode }) => {
  const value = useMemo(() => ({
    availableUpdate: null,
    updateInfo: null,
    isChecking: false,
    isDownloading: false,
    downloadProgress: 0,
    checkForUpdates: async () => {},
    downloadAndInstall: async () => {},
    dismissUpdate: () => {},
    error: null,
    isUpToDate: false,
    installationSource: "extended-manual",
  }), []);
  return <UpdateContext.Provider value={value}>{children}</UpdateContext.Provider>;
};
