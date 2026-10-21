import * as api from "@tabularis/plugin-api";
import type { SlotComponentProps } from "@tabularis/plugin-api";
import "maplibre-gl/dist/maplibre-gl.css";
import "./styles.css";
import { CompatToolbar } from "./compat";
import { MapToolbar } from "./MapToolbar";
import { MapRenderer } from "./MapRenderer";

const SERVICE_DRIVER = "postgresql-plus";
const BASIC_DRIVERS = ["postgres", "postgresql"];

export default function SpatialPlugin(props: SlotComponentProps) {
  if (typeof api.usePluginService !== "function" || typeof api.usePluginAssets !== "function") {
    return <CompatToolbar {...props} />;
  }
  if (!props.context.connectionId) return <MapRenderer pluginId={props.pluginId} />;
  const driver = (props.context.driver ?? "").toLowerCase();
  if (driver === SERVICE_DRIVER) return <MapToolbar {...props} />;
  return BASIC_DRIVERS.includes(driver) ? <CompatToolbar {...props} /> : null;
}
