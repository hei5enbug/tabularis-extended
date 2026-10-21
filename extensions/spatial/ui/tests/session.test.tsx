import { describe, expect, it } from "vitest";
import { MapSession, parseApply } from "../src/session";
import { applyRequest, mapFixture } from "./fixtures";

function captureError(action: () => unknown): unknown { try { action(); return null; } catch (error) { return error; } }

describe("지도 version과 generation", () => {
  it("map_version이_다른_callback은_거부한다", () => {
    // given
    const request = { ...applyRequest("open"), state_version: 2 };
    // when
    const actual = captureError(() => parseApply(request));
    // then
    expect(actual).toMatchObject({ code: "INVALID_ARGUMENT" });
  });

  it("낮은_layer_generation의_늦은_callback은_적용하지_않는다", async () => {
    // given
    const current = structuredClone(mapFixture);
    current.layers[0].generation = 2;
    const session = new MapSession(current);
    const request = applyRequest("update", mapFixture, 3);
    // when
    const actual = await session.apply(request, mapFixture);
    // then
    expect(actual.gui_applied).toBe(false);
    expect(actual.rendered_version).toBeNull();
    expect(session.getSnapshot().map.layers[0].generation).toBe(2);
  });

  it("새_apply가_이전_pending_ACK를_false로_완료한다", async () => {
    // given
    const session = new MapSession(mapFixture);
    const first = session.apply(applyRequest("open"), mapFixture);
    const latest = { ...mapFixture, version: 2 };
    // when
    const second = session.apply(applyRequest("update", latest, 2), latest);
    // then
    expect((await first).gui_applied).toBe(false);
    expect(session.getSnapshot().map.version).toBe(2);
    expect(second).toBeInstanceOf(Promise);
  });

  it("unmount는_pending_ACK를_false_null로_완료한다", async () => {
    // given
    const session = new MapSession(mapFixture);
    const pending = session.apply(applyRequest("open"), mapFixture);
    session.markMounted();
    // when
    session.unmounted();
    // then
    expect(await pending).toMatchObject({ gui_applied: false, rendered_version: null });
    await expect(session.whenDisposed).resolves.toBeUndefined();
  });

  it("render됐지만_effect가_아직_시작되지_않은_close도_cleanup을_기다린다", async () => {
    // given
    const session = new MapSession(mapFixture);
    session.markPresented();
    let disposed = false;
    void session.whenDisposed.then(() => { disposed = true; });
    session.close();
    await Promise.resolve();
    const before = disposed;
    // when
    session.unmounted();
    // then
    await expect(session.whenDisposed).resolves.toBeUndefined();
    expect(before).toBe(false);
    expect(disposed).toBe(true);
  });
});
