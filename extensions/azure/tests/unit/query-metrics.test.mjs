import test from "node:test";
import assert from "node:assert/strict";
import { admitRequestTransport, MetricsAccumulator, withRequestMetrics } from "../../dist/runtime/metrics.js";

test("닫힌 요청의 송신은 네트워크 비용을 새로 기록하지 않는다", () => {
  // given
  const metrics = new MetricsAccumulator();
  metrics.add({ requestCharge: 2 });
  metrics.sealTransport();

  // when
  const actual = withRequestMetrics({ metrics }, admitRequestTransport);

  // then
  assert.equal(actual.allowed, false);
  assert.equal(metrics.knownCharge, 2);
  assert.equal(metrics.snapshot().request_charge, 2);
});

test("같은 송신의 완료를 두 번 알려도 RU를 한 번만 합산한다", () => {
  // given
  const metrics = new MetricsAccumulator();
  const lease = metrics.admitTransport();
  lease.complete("3");

  // when
  lease.complete("7");

  // then
  assert.equal(metrics.knownCharge, 3);
  assert.equal(metrics.snapshot().request_charge, 3);
});

test("이미 시작한 송신이 끝나면 대기를 해제하고 SDK 비용을 중복하지 않는다", async () => {
  // given
  const metrics = new MetricsAccumulator();
  const lease = metrics.admitTransport();
  metrics.sealTransport();
  const waiting = metrics.drainTransport(new AbortController().signal);
  metrics.add({ requestCharge: 10 });

  // when
  lease.complete("4");

  // then
  await assert.doesNotReject(waiting);
  assert.equal(metrics.snapshot().request_charge, 4);
});

test("송신을 기다리는 요청을 취소하면 무기한 대기하지 않는다", async () => {
  // given
  const metrics = new MetricsAccumulator();
  metrics.admitTransport();
  metrics.sealTransport();
  const controller = new AbortController();
  const waiting = metrics.drainTransport(controller.signal);

  // when
  controller.abort(new Error("synthetic-cancel"));

  // then
  await assert.rejects(waiting, /synthetic-cancel/);
});

test("미완료 송신의 총 RU는 확정 후 null로 고정하고 늦은 완료를 무시한다", () => {
  // given
  let now = 10;
  const metrics = new MetricsAccumulator(0, () => now);
  metrics.add({ requestCharge: 2 });
  const lease = metrics.admitTransport();
  const frozen = metrics.finalize();
  now = 30;

  // when
  lease.complete("5");

  // then
  assert.deepEqual(metrics.snapshot(), frozen);
  assert.equal(frozen.request_charge, null);
  assert.equal(metrics.knownCharge, 2);
  assert.equal(metrics.chargeIsComplete, false);
  assert.equal(metrics.admitTransport().allowed, false);
});
