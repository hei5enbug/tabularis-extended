import { beforeEach, describe, expect, it, vi } from "vitest";
import { LayerDataStore } from "../src/data";
import { LAYER_LIMIT, WORLD, type FeaturePage } from "../src/models";
import { harness, mapFixture, pageFixture, resetHarness, response } from "./fixtures";

beforeEach(resetHarness);

async function pagedScenario(makePage: (index: number) => FeaturePage, attempts: number) {
  let index = 0;
  vi.mocked(harness.service.call).mockImplementation(async request => response(request, makePage(index++), `detail-cache-${index}`));
  const store = new LayerDataStore(harness.service);
  store.mark(mapFixture.layers);
  let result = await store.load(mapFixture.layers[0], new AbortController().signal);
  for (let page = 1; page < attempts; page++) result = await store.load(mapFixture.layers[0], new AbortController().signal, true);
  return result;
}
function nextPage(index: number, size: number): FeaturePage {
  const page = structuredClone(pageFixture);
  page.features = Array.from({ length: size }, (_, row) => ({ ...pageFixture.features[0], id: `result:2:${index * size + row}` }));
  page.row_references = page.features.map((feature, row) => ({ feature_id: feature.id, identity: null, row_ordinal: index * size + row }));
  page.page = { next_token: `public-${index + 1}`, has_more: true, resume_mode: "native" };
  if (index > 0) page.limits = { truncated: true, reasons: ["SKIPPED_FEATURES"] };
  return page;
}

describe("불변 snapshot과 table viewport", () => {
  it("layer_1만_feature_상한은_전체_pair와_skip_이유를_보존한다", async () => {
    // given
    const makePage = (index: number) => nextPage(index, 1000);
    // when
    const actual = await pagedScenario(makePage, 11);
    // then
    expect(actual.page.features).toHaveLength(LAYER_LIMIT.features);
    expect(actual.page.row_references).toHaveLength(LAYER_LIMIT.features);
    expect(actual.page.limits).toEqual({ truncated: true, reasons: ["SKIPPED_FEATURES","LAYER_LIMIT"] });
    expect(actual.page.page).toEqual({ next_token: null, has_more: false, resume_mode: "none" });
  });

  it("layer_25만_좌표를_넘는_page는_feature를_분할하지_않는다", async () => {
    // given
    const makePage = (index: number) => {
      const page = nextPage(index, 1);
      page.features[0] = { ...page.features[0], geometry: { type: "LineString", coordinates: Array.from({ length: 100_000 }, (_, row) => [row % 2, row % 2] as [number,number]) } };
      return page;
    };
    // when
    const actual = await pagedScenario(makePage, 3);
    // then
    expect(actual.page.features).toHaveLength(2);
    expect(actual.coordinates).toBe(200_000);
    expect(actual.page.limits.reasons).toContain("LAYER_LIMIT");
    expect(actual.page.features[0].geometry).toMatchObject({ coordinates: expect.any(Array) });
  });

  it("layer_32MiB_상한은_원본_properties_bytes까지_계산한다", async () => {
    // given
    const makePage = (index: number) => {
      const page = nextPage(index, 1);
      page.features[0] = { ...page.features[0], properties: { raw: "x".repeat(7_900_000) } };
      return page;
    };
    // when
    const actual = await pagedScenario(makePage, 5);
    // then
    expect(actual.page.features).toHaveLength(4);
    expect(actual.bytes).toBeLessThanOrEqual(LAYER_LIMIT.bytes);
    expect(actual.page.limits.reasons).toContain("LAYER_LIMIT");
    expect(actual.page.page.has_more).toBe(false);
  });
  it("query_result는_result와_set와_column_index만_보낸다", async () => {
    // given
    const store = new LayerDataStore(harness.service);
    const layer = structuredClone(mapFixture.layers[0]);
    store.mark([layer]);
    // when
    const actual = await store.load(layer, new AbortController().signal);
    // then
    expect(harness.requests[0]).toMatchObject({ operation: "spatial.query_result", connection_id: "inactive-connection", input: { result_id: "original-snapshot", result_set_index: 2, column_index: 4, skip_invalid: false } });
    expect(actual.page.features[0].properties.g).toEqual(pageFixture.features[0].properties.g);
    expect(actual.featureResultIds.get(pageFixture.features[0].id)).toBe("display-cache");
    expect(harness.requests.some(request => request.operation === "query.execute")).toBe(false);
  });

  it("query_result의_지도_이동이나_style_generation은_원본을_재실행하지_않는다", async () => {
    // given
    const store = new LayerDataStore(harness.service);
    const layer = structuredClone(mapFixture.layers[0]);
    store.mark([layer]);
    await store.load(layer, new AbortController().signal);
    const changed = { ...layer, generation: 1 };
    store.mark([changed]);
    // when
    const actual = await store.load(changed, new AbortController().signal);
    // then
    expect(actual.generation).toBe(1);
    expect(harness.requests).toHaveLength(1);
    expect(harness.requests[0].operation).toBe("spatial.query_result");
  });

  it("table은_변경된_viewport와_filter를_서비스로_전달한다", async () => {
    // given
    const store = new LayerDataStore(harness.service);
    const layer = { ...structuredClone(mapFixture.layers[0]), source: { kind: "table" as const, table: { database: null, schema: "공간 schema", table: "roads" }, column: "g", viewport: { ...WORLD, north: 10 }, source_srid: 4326, srid_filter: 0, skip_invalid: false } };
    store.mark([layer]);
    // when
    await store.load(layer, new AbortController().signal);
    // then
    expect(harness.requests[0]).toMatchObject({ operation: "spatial.table_query", input: { table: layer.source.table, viewport: layer.source.viewport, source_srid: 4326, srid_filter: 0 } });
    expect(harness.requests[0].input).not.toHaveProperty("text");
    expect(harness.requests[0].input).not.toHaveProperty("kind");
  });

  it("이전_generation의_늦은_응답은_적용하지_않는다", async () => {
    // given
    let complete!: (value: ReturnType<typeof response>) => void;
    vi.mocked(harness.service.call).mockImplementationOnce(request => new Promise(resolve => { complete = resolve; harness.requests.push(request); }));
    const store = new LayerDataStore(harness.service);
    const layer = structuredClone(mapFixture.layers[0]);
    store.mark([layer]);
    const pending = store.load(layer, new AbortController().signal).catch(error => error);
    store.mark([{ ...layer, generation: 1 }]);
    // when
    complete(response(harness.requests[0], pageFixture, "old-cache"));
    // then
    expect(await pending).toMatchObject({ name: "AbortError" });
    expect(store.get(layer.layer_id)).toBeUndefined();
  });

  it("취소는_SDK에_같은_connection의_AbortSignal로_전달한다", async () => {
    // given
    let signal: AbortSignal | undefined;
    vi.mocked(harness.service.call).mockImplementationOnce((_request, options) => { signal = options?.signal; return new Promise(() => {}); });
    const store = new LayerDataStore(harness.service);
    store.mark(mapFixture.layers);
    void store.load(mapFixture.layers[0], new AbortController().signal);
    // when
    store.cancel();
    // then
    expect(signal?.aborted).toBe(true);
  });

  it("더보기는_이전_page의_detail_handle과_skip_이유를_보존한다", async () => {
    // given
    const first = structuredClone(pageFixture);
    first.page = { next_token: "public-host-token", has_more: true, resume_mode: "native" };
    first.limits = { truncated: true, reasons: ["SKIPPED_FEATURES"] };
    const second = structuredClone(pageFixture);
    second.features[0].id = "original-snapshot:2:20";
    second.row_references[0].feature_id = second.features[0].id;
    second.row_references[0].row_ordinal = 20;
    vi.mocked(harness.service.call).mockImplementationOnce(async request => response(request, first, "first-cache")).mockImplementationOnce(async request => { harness.requests.push(request); return response(request, second, "second-cache"); });
    const store = new LayerDataStore(harness.service);
    store.mark(mapFixture.layers);
    await store.load(mapFixture.layers[0], new AbortController().signal);
    // when
    const actual = await store.load(mapFixture.layers[0], new AbortController().signal, true);
    // then
    expect(actual.page.features).toHaveLength(2);
    expect(actual.page.limits.reasons).toContain("SKIPPED_FEATURES");
    expect(actual.featureResultIds.get(first.features[0].id)).toBe("first-cache");
    expect(actual.featureResultIds.get(second.features[0].id)).toBe("second-cache");
    expect(harness.requests[0].input).toMatchObject({ next_token: "public-host-token" });
  });

  it("같은_feature_id의_다음_page는_기존_행을_덮어쓰지_않는다", async () => {
    // given
    const first = structuredClone(pageFixture);
    first.page = { next_token: "token", has_more: true, resume_mode: "materialized" };
    harness.page = first;
    const store = new LayerDataStore(harness.service);
    store.mark(mapFixture.layers);
    await store.load(mapFixture.layers[0], new AbortController().signal);
    // when
    const actual = store.load(mapFixture.layers[0], new AbortController().signal, true);
    // then
    await expect(actual).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    expect(store.get(mapFixture.layers[0].layer_id)?.page.features).toHaveLength(1);
  });

  it("geography_지원불가_오류는_cast나_조회_우회없이_보여준다", async () => {
    // given
    harness.failNext = "UNSUPPORTED_OPERATION";
    const store = new LayerDataStore(harness.service);
    store.mark(mapFixture.layers);
    // when
    const actual = store.load(mapFixture.layers[0], new AbortController().signal);
    // then
    await expect(actual).rejects.toMatchObject({ code: "UNSUPPORTED_OPERATION" });
    expect(harness.requests).toHaveLength(1);
  });
});
