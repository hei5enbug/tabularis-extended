import { afterEach, describe, expect, it, vi } from "vitest";
import { waitFor } from "@testing-library/react";
import { CSS_ASSET, loadAssets, WORKER_ASSET } from "../src/assets";

afterEach(() => document.head.querySelectorAll("link[data-fixture]").forEach(link => link.remove()));
function fixtureAssets() {
  const css = { url: new URL("./fixtures/style.css", import.meta.url).href, dispose: vi.fn() };
  const worker = { url: new URL("./fixtures/maplibre-worker.js", import.meta.url).href, dispose: vi.fn() };
  return { css, worker, assets: { resolve: vi.fn(async (path: string) => path === CSS_ASSET ? css : worker) } };
}
async function cssLink(): Promise<HTMLLinkElement> {
  await waitFor(() => expect(document.head.querySelector("link[rel=stylesheet]")).not.toBeNull());
  const link = document.head.querySelector<HTMLLinkElement>("link[rel=stylesheet]")!;
  link.dataset.fixture = "true";
  return link;
}

describe("SDK local asset 수명", () => {
  it("고정_CSS와_worker_경로를_resolve하고_CSS_load를_기다린다", async () => {
    // given
    const fixture = fixtureAssets();
    const pending = loadAssets(fixture.assets, new AbortController().signal);
    const link = await cssLink();
    // when
    link.dispatchEvent(new Event("load"));
    // then
    expect((await pending).workerUrl).toBe(fixture.worker.url);
    expect(fixture.assets.resolve.mock.calls.map(call => call[0])).toEqual([CSS_ASSET,WORKER_ASSET]);
    expect(link.onload).toBeNull();
    expect(link.onerror).toBeNull();
  });

  it("asset_dispose는_link와_URL을_한번만_해제한다", async () => {
    // given
    const fixture = fixtureAssets();
    const pending = loadAssets(fixture.assets, new AbortController().signal);
    const link = await cssLink();
    link.dispatchEvent(new Event("load"));
    const loaded = await pending;
    const disposeTwice = () => { loaded.dispose(); loaded.dispose(); };
    // when
    disposeTwice();
    // then
    expect(document.head.contains(link)).toBe(false);
    expect(fixture.worker.dispose).toHaveBeenCalledTimes(1);
    expect(fixture.css.dispose).toHaveBeenCalledTimes(1);
  });

  it("worker_누락은_성공_ACK_대신_CSS도_해제한다", async () => {
    // given
    const fixture = fixtureAssets();
    fixture.assets.resolve.mockImplementation(async path => { if (path === WORKER_ASSET) throw new Error("missing"); return fixture.css; });
    // when
    const actual = loadAssets(fixture.assets, new AbortController().signal);
    // then
    await expect(actual).rejects.toMatchObject({ code: "CAPABILITY_UNAVAILABLE" });
    expect(fixture.css.dispose).toHaveBeenCalledTimes(1);
  });

  it("CSS_로드_취소는_이벤트와_asset을_정리한다", async () => {
    // given
    const fixture = fixtureAssets();
    const controller = new AbortController();
    const pending = loadAssets(fixture.assets, controller.signal).catch(error => error);
    const link = await cssLink();
    // when
    controller.abort();
    // then
    expect(await pending).toMatchObject({ name: "AbortError" });
    expect(document.head.contains(link)).toBe(false);
    expect(link.onload).toBeNull();
    expect(fixture.css.dispose).toHaveBeenCalledTimes(1);
    expect(fixture.worker.dispose).toHaveBeenCalledTimes(1);
  });
});
