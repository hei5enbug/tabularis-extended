import type { Writable } from "node:stream";

export const MAX_FRAME_BYTES = 16 * 1024 * 1024;
export type Frame = { kind: "json"; text: string } | { kind: "error"; code: "FRAME_TOO_LARGE" | "INVALID_UTF8" | "INCOMPLETE_FRAME" };
export class LineFramer {
  private buffers: Buffer[] = [];
  private bytes = 0;
  private discarding = false;
  constructor(private readonly limit = MAX_FRAME_BYTES) {}
  push(chunk: Uint8Array | string): Frame[] {
    return [...this.frames(chunk)];
  }
  *frames(chunk: Uint8Array | string): Generator<Frame> {
    const buffer = typeof chunk === "string" ? Buffer.from(chunk, "utf8") : Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength);
    let start = 0;
    while (start < buffer.length) {
      const newline = buffer.indexOf(10, start);
      const end = newline < 0 ? buffer.length : newline;
      const part = buffer.subarray(start, end);
      if (!this.discarding) {
        if (this.bytes + part.length > this.limit) {
          this.buffers = []; this.bytes = 0; this.discarding = true;
          yield { kind: "error", code: "FRAME_TOO_LARGE" };
        } else if (part.length) { this.buffers.push(part); this.bytes += part.length; }
      }
      if (newline >= 0) {
        if (!this.discarding) {
          const joined = Buffer.concat(this.buffers, this.bytes);
          const line = joined.at(-1) === 13 ? joined.subarray(0, -1) : joined;
          try { yield { kind: "json", text: new TextDecoder("utf-8", { fatal: true }).decode(line) }; }
          catch { yield { kind: "error", code: "INVALID_UTF8" }; }
        }
        this.buffers = []; this.bytes = 0; this.discarding = false;
      }
      if (newline < 0) break;
      start = newline + 1;
    }
  }
  finish(): Frame[] {
    const incomplete = this.bytes > 0 && !this.discarding;
    this.buffers = []; this.bytes = 0; this.discarding = false;
    return incomplete ? [{ kind: "error", code: "INCOMPLETE_FRAME" }] : [];
  }
}
export class JsonLineWriter {
  private tail: Promise<void> = Promise.resolve();
  private failure: Error | undefined;
  queuedBytes = 0;
  queueLength = 0;
  constructor(private readonly output: Writable, private readonly limit = MAX_FRAME_BYTES) {
    output.on("error", () => { this.failure = new Error("The RPC output stream failed."); });
  }
  write(value: unknown): Promise<void> {
    let payload = JSON.stringify(value);
    if (payload === undefined) return Promise.reject(new Error("The RPC response is not serializable."));
    if (Buffer.byteLength(payload, "utf8") > this.limit) {
      const response = typeof value === "object" && value !== null ? value as Record<string, unknown> : {};
      const id = typeof response.id === "string" || typeof response.id === "number" ? response.id : null;
      payload = JSON.stringify({ jsonrpc: "2.0", id, error: { code: -32000, message: "The response exceeds the frame limit.", data: { code: "RESOURCE_LIMIT", message: "The response exceeds the frame limit.", retryable: false, outcome: "unknown", details: null } } });
    }
    const bytes = Buffer.byteLength(payload, "utf8") + 1;
    this.queuedBytes += bytes;
    this.queueLength++;
    const next = this.tail.then(async () => {
      if (this.failure) throw this.failure;
      await new Promise<void>((resolve, reject) => {
        this.output.write(payload + "\n", (error) => error ? reject(error) : resolve());
      });
    }).finally(() => { this.queuedBytes -= bytes; this.queueLength--; });
    this.tail = next.catch(() => { this.failure = new Error("The RPC output stream failed."); });
    return next;
  }
  async flush(): Promise<void> { await this.tail; if (this.failure) throw this.failure; }
}
