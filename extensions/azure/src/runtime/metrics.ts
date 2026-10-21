import type { Metrics } from "@tabularis/service-contracts/types";
import { AsyncLocalStorage } from "node:async_hooks";
import type { RpcRequestContext } from "./contracts.js";

const requestMetrics = new AsyncLocalStorage<MetricsAccumulator>();
export function withRequestMetrics<T>(context: RpcRequestContext, action: () => T): T { return context.metrics ? requestMetrics.run(context.metrics, action) : action(); }
export function observeTransportCharge(charge: string | undefined): void { requestMetrics.getStore()?.addTransport({ headers: { "x-ms-request-charge": charge } }); }
export interface RequestTransportLease { allowed: boolean; complete(charge?: string): void }
export function admitRequestTransport(): RequestTransportLease { return requestMetrics.getStore()?.admitTransport() ?? { allowed: true, complete() {} }; }

function object(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null ? value as Record<string, unknown> : undefined;
}
export function measuredCharge(value: unknown): number | null {
  const record = object(value);
  const direct = record?.requestCharge;
  if (typeof direct === "number" && Number.isFinite(direct) && direct >= 0) return direct;
  const header = object(record?.headers)?.["x-ms-request-charge"];
  if (typeof header !== "string" && typeof header !== "number") return null;
  if (typeof header === "string" && !header.trim()) return null;
  const parsed = Number(header);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : null;
}
export function sdkRetryCount(value: unknown): number {
  const record = object(value);
  const diagnostics = object(record?.diagnostics);
  const statistics = object(diagnostics?.clientSideRequestStatistics);
  const attempts = object(statistics?.retryDiagnostics)?.failedAttempts;
  if (Array.isArray(attempts)) return attempts.length;
  const header = object(record?.headers)?.["x-ms-throttle-retry-count"];
  const parsed = typeof header === "number" || typeof header === "string" ? Number(header) : 0;
  return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : 0;
}
export class MetricsAccumulator {
  private charge = 0;
  private measured = false;
  private unknown = false;
  private retries = 0;
  private transportObserved = false;
  private transportSealed = false;
  private pendingTransports = 0;
  private transportWaiters = new Set<() => void>();
  private finalMetrics: Metrics | undefined;
  constructor(private readonly startedAt = Date.now(), private readonly now: () => number = Date.now) {}
  add(value: unknown): void {
    if (this.finalMetrics) return;
    if (!this.transportObserved) this.addCharge(value);
    this.retries += sdkRetryCount(value);
  }
  addTransport(value: unknown): void {
    if (this.finalMetrics) return;
    this.transportObserved = true;
    this.addCharge(value);
  }
  admitTransport(): RequestTransportLease {
    if (this.transportSealed) return { allowed: false, complete() {} };
    this.transportObserved = true;
    this.pendingTransports++;
    let completed = false;
    return { allowed: true, complete: (charge) => {
      if (completed) return;
      completed = true;
      this.addTransport({ headers: { "x-ms-request-charge": charge } });
      this.pendingTransports--;
      if (!this.pendingTransports) for (const resolve of [...this.transportWaiters]) resolve();
    } };
  }
  sealTransport(): void { this.transportSealed = true; }
  async drainTransport(signal: AbortSignal): Promise<void> {
    if (signal.aborted) throw signal.reason;
    if (!this.pendingTransports) return;
    await new Promise<void>((resolve, reject) => {
      const cleanup = () => { this.transportWaiters.delete(done); signal.removeEventListener("abort", aborted); };
      const done = () => { cleanup(); resolve(); };
      const aborted = () => { cleanup(); reject(signal.reason); };
      this.transportWaiters.add(done);
      signal.addEventListener("abort", aborted, { once: true });
    });
  }
  finalize(): Metrics {
    this.sealTransport();
    if (!this.finalMetrics) {
      if (this.pendingTransports) this.unknown = true;
      this.finalMetrics = this.snapshot();
    }
    return this.snapshot();
  }
  private addCharge(value: unknown): void {
    const charge = measuredCharge(value);
    if (charge === null) this.unknown = true;
    else { this.measured = true; this.charge += charge; if (!Number.isFinite(this.charge)) this.unknown = true; }
  }
  get knownCharge(): number { return this.charge; }
  get chargeIsComplete(): boolean { return this.measured && !this.unknown; }
  snapshot(): Metrics {
    if (this.finalMetrics) return { ...this.finalMetrics };
    return { elapsed_ms: Math.max(0, Math.floor(this.now() - this.startedAt)), request_charge: this.measured && !this.unknown ? this.charge : null, retry_count: this.retries };
  }
}
