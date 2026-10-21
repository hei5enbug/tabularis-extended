import * as api from "@tabularis/plugin-api";
import type { SlotComponentProps } from "@tabularis/plugin-api";
import "maplibre-gl/dist/maplibre-gl.css";
import "./styles.css";
import { CompatToolbar } from "./compat";
import { MapToolbar } from "./MapToolbar";
import { MapRenderer } from "./MapRenderer";

export default function SpatialPlugin(props: SlotComponentProps) {
  if (typeof api.usePluginService !== "function" || typeof api.usePluginAssets !== "function") {
    return <CompatToolbar {...props} />;
  }
  if (!props.context.connectionId) return <MapRenderer pluginId={props.pluginId} />;
  return ["postgres", "postgresql"].includes((props.context.driver ?? "").toLowerCase()) ? <MapToolbar {...props} /> : null;
}
