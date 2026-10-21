import { fork } from "node:child_process";
import { fileURLToPath } from "node:url";
const driver = fileURLToPath(new URL("./protocol-driver.mjs", import.meta.url));
export const frame = (request) => JSON.stringify(request) + "\n";
export async function protocolScenario({ chunks, requests = [], cancel, mode, startedCount = 1, logLevel, expectedResponses = requests.length + (cancel ? 1 : 0) || 1, eofAfterStarted = false } = {}) {
  const env = { ...(process.env.PATH ? { PATH: process.env.PATH } : {}), ...(process.env.TMPDIR ? { TMPDIR: process.env.TMPDIR } : {}), ...(logLevel ? { AZURE_LOG_LEVEL: logLevel } : {}) };
  const child = fork(driver, mode ? [mode] : [], { silent: true, execArgv: [], env });
  const stdout = [];
  const stderr = [];
  const messages = [];
  let responseLines = 0;
  child.stdout.on("data", (chunk) => { stdout.push(chunk); responseLines += chunk.toString("utf8").split("\n").length - 1; if (responseLines >= expectedResponses) child.stdin.end(); });
  child.stderr.on("data", (chunk) => stderr.push(chunk));
  let started = 0;
  let ready;
  const readiness = new Promise((resolve) => { ready = resolve; });
  child.on("message", (message) => { messages.push(message); if (message?.type === "started" && ++started >= startedCount) ready(); });
  const completion = new Promise((resolve, reject) => {
    const timer = setTimeout(() => { child.kill(); reject(new Error("The synthetic protocol child exceeded its time limit.")); }, 7000);
    child.on("error", (error) => { clearTimeout(timer); reject(error); });
    child.on("close", (code) => { clearTimeout(timer); resolve({ code, stdout: Buffer.concat(stdout).toString("utf8"), stderr: Buffer.concat(stderr).toString("utf8") }); });
  });
  async function write(value) { await new Promise((resolve, reject) => child.stdin.write(value, (error) => error ? reject(error) : resolve())); }
  for (const value of chunks ?? requests.map(frame)) await write(value);
  if (cancel || eofAfterStarted) { await Promise.race([readiness, completion.then(() => { throw new Error("The synthetic child exited before the SDK boundary started."); })]); if (cancel) await write(frame(cancel)); if (eofAfterStarted) child.stdin.end(); }
  const result = await completion;
  return { ...result, messages, responses: result.stdout.trim() ? result.stdout.trim().split("\n").map((line) => JSON.parse(line)) : [] };
}
