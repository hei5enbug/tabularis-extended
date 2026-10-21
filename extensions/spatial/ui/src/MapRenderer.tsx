import { useEffect, useRef } from "react";
import { usePluginModal, usePluginService, usePluginToast, usePluginTranslation, type PluginMapApplyRequest } from "@tabularis/plugin-api";
import { MapModal } from "./MapModal";
import { failure, MapSession, parseApply } from "./session";
import { errorText } from "./service";

export function MapRenderer({ pluginId }: { pluginId: string }) {
  const service = usePluginService();
  const modal = usePluginModal();
  const toast = usePluginToast();
  const t = usePluginTranslation(pluginId);
  const current = useRef<MapSession | null>(null);
  const seen = useRef(new Map<string, { version: number; generation: number }>());
  const opening = useRef(0);
  const services = useRef({ modal, toast, t });
  services.current = { modal, toast, t };

  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | null = null;
    const handler = async (request: PluginMapApplyRequest) => {
      try {
        if (!active) return failure(request);
        const { action, map } = parseApply(request);
        const previous = seen.current.get(map.map_id);
        if (previous && (request.state_version < previous.version || request.generation < previous.generation)) return failure(request);
        const session = current.current;
        if (session?.getSnapshot().map.map_id === map.map_id && !session.isClosed() && !session.canApply(request, map)) return failure(request);
        seen.current.set(map.map_id, { version: request.state_version, generation: request.generation });
        if (action === "close") {
          if (session?.getSnapshot().map.map_id === map.map_id) {
            session.close();
            services.current.modal.closeModal();
            await session.whenDisposed;
            if (current.current === session) current.current = null;
          }
          return active ? { state_version: request.state_version, rendered_version: request.state_version, generation: request.generation, gui_applied: true } : failure(request);
        }
        if (action === "update" && (!session || session.isClosed() || session.getSnapshot().map.map_id !== map.map_id)) return failure(request);
        if (action === "open" && (!session || session.isClosed() || session.getSnapshot().map.map_id !== map.map_id)) {
          const transition = ++opening.current;
          if (session && !session.isClosed()) {
            session.close();
            services.current.modal.closeModal();
            await session.whenDisposed;
            if (!active || opening.current !== transition || seen.current.get(map.map_id)?.generation !== request.generation || seen.current.get(map.map_id)?.version !== request.state_version) return failure(request);
          }
          const next = new MapSession(map);
          current.current = next;
          const promise = next.apply(request, map);
          services.current.modal.openModal({ title: services.current.t("map.title", { defaultValue: "공간 지도" }), size: "xl", content: <MapModal key={map.map_id} session={next} pluginId={pluginId} /> });
          return promise;
        }
        return current.current!.apply(request, map);
      } catch (error) { if (active) void services.current.toast.showError(errorText(error)); return failure(request); }
    };
    void service.subscribeMap(handler).then(cleanup => { if (active) unsubscribe = cleanup; else cleanup(); }).catch(error => { if (active) void services.current.toast.showError(errorText(error)); });
    return () => {
      active = false;
      unsubscribe?.();
      if (current.current && !current.current.isClosed()) { current.current.close(); services.current.modal.closeModal(); }
      current.current = null;
    };
  }, [service, pluginId]);
  return null;
}
