import { afterEach, describe, expect, it, vi } from "vitest";
import { assertHostCompat } from "../packages/plugin-api/src/host";
import { API_VERSION, MIN_HOST_VERSION } from "../packages/plugin-api/src/version";

describe("플러그인 SDK API 버전 호환", () => {
  afterEach(() => { vi.unstubAllGlobals(); });

  it.each(["0.2.0", "0.3.0"])("host API %s는 SDK 최소 API 버전을 만족한다", (version) => {
    // given
    vi.stubGlobal("__TABULARIS_API_VERSION__", version);
    // when
    const actual = (() => { try { assertHostCompat(); return null; } catch (error) { return error; } })();
    // then
    expect(actual).toBeNull();
    expect(API_VERSION).toBe("0.2.0");
    expect(MIN_HOST_VERSION).toBe("0.2.0");
  });

  it.each([undefined, "0.1.1"])("host API %s는 새 서비스 SDK의 최소 버전에 미달한다", (version) => {
    // given
    vi.stubGlobal("__TABULARIS_API_VERSION__", version);
    // when
    const actual = (() => { try { assertHostCompat(); return null; } catch (error) { return error; } })();
    // then
    expect(actual).toBeInstanceOf(Error);
    expect(actual).toMatchObject({ message: expect.stringContaining("required 0.2.0") });
  });
});
