import { useState, type ReactNode } from "react";
import { expect } from "vitest";
import { act, render, waitFor } from "@testing-library/react";
import { MapRenderer } from "../src/MapRenderer";
import { applyRequest, harness, mapFixture } from "./fixtures";
import type { MapState } from "../src/models";
import type { PluginMapApplyRequest } from "@tabularis/plugin-api";

export function HostFixture() {
  const [content, setContent] = useState<ReactNode>(null);
  harness.openModal.mockImplementation(options => setContent(options.content));
  harness.closeModal.mockImplementation(() => setContent(null));
  return <><MapRenderer pluginId="spatial-fixture" />{content ? <div role="dialog" aria-modal="true" aria-label="공간 지도" onKeyDown={event => { if (event.key === "Escape") harness.closeModal(); }}>{content}</div> : null}</>;
}
export async function loadFixtureCss(): Promise<void> {
  await waitFor(() => expect(document.head.querySelector("link[rel=stylesheet]")).not.toBeNull());
  await act(async () => { document.head.querySelector("link[rel=stylesheet]")!.dispatchEvent(new Event("load")); });
}
export async function openFixture(map: MapState = mapFixture) {
  const view = render(<HostFixture />);
  await waitFor(() => expect(harness.handler).not.toBeNull());
  let pending!: ReturnType<NonNullable<typeof harness.handler>>;
  await act(async () => { pending = harness.handler!(applyRequest("open", map)); });
  await loadFixtureCss();
  let ack!: Awaited<typeof pending>;
  await act(async () => { ack = await pending; });
  return { view, ack };
}
export async function applyThroughHost(request: PluginMapApplyRequest) {
  let pending!: ReturnType<NonNullable<typeof harness.handler>>;
  await act(async () => { pending = harness.handler!(request); });
  return pending;
}
