import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

export class RpcProcess {
  #child;
  #pending = new Map();
  #sequence = 0;
  #unexpected = [];
  #stderr = "";
  #closed = false;

  constructor(binary, args = [], env = {}) {
    this.#child = spawn(binary, args, {
      stdio: ["pipe", "pipe", "pipe"],
      env: { PATH: process.env.PATH, RUST_BACKTRACE: "0", ...env },
    });
    this.#child.stderr.on("data", data => {
      this.#stderr = (this.#stderr + data.toString()).slice(-1_048_576);
    });
    const lines = createInterface({ input: this.#child.stdout });
    lines.on("line", line => {
      let response;
      try { response = JSON.parse(line); }
      catch { this.#unexpected.push("invalid JSON on stdout"); return; }
      const pending = this.#pending.get(response.id);
      if (!pending) { this.#unexpected.push("unsolicited response"); return; }
      this.#pending.delete(response.id);
      clearTimeout(pending.timer);
      pending.resolve(response);
    });
    const fail = () => {
      this.#closed = true;
      for (const pending of this.#pending.values()) {
        clearTimeout(pending.timer);
        pending.reject(new Error("RPC process exited before responding"));
      }
      this.#pending.clear();
    };
    this.#child.once("error", fail);
    this.#child.once("exit", fail);
    this.#child.stdin.on("error", fail);
  }

  request(method, params, timeoutMs = 30_000) {
    if (this.#closed) throw new Error("RPC process is closed");
    const id = ++this.#sequence;
    const result = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`RPC ${method} did not respond within ${timeoutMs} ms`));
      }, timeoutMs);
      this.#pending.set(id, { resolve, reject, timer });
    });
    this.#child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    return { id, result };
  }

  notify(method, params) {
    if (!this.#closed) this.#child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }

  get unexpected() { return [...this.#unexpected]; }
  get stderr() { return this.#stderr; }

  async stop() {
    if (this.#closed) return;
    const exited = new Promise(resolve => this.#child.once("exit", resolve));
    this.#child.stdin.end();
    const timer = setTimeout(() => this.#child.kill("SIGKILL"), 3000);
    await exited;
    clearTimeout(timer);
  }
}
