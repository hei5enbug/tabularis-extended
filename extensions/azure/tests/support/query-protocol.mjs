import { fork } from "node:child_process";
import { fileURLToPath } from "node:url";
const driver = fileURLToPath(new URL("./query-driver.mjs", import.meta.url));

export async function queryProtocol({ requests, cancel, logLevel } = {}) {
  const child = fork(driver, [], { silent: true, execArgv: [], env: { ...(process.env.PATH ? { PATH: process.env.PATH } : {}), ...(process.env.TMPDIR ? { TMPDIR: process.env.TMPDIR } : {}), ...(logLevel ? { AZURE_LOG_LEVEL: logLevel } : {}) } });
  const output = [];
  const errors = [];
  let responseCount = 0;
  const count = requests.length + (cancel ? 1 : 0);
  child.stdout.on("data", (chunk) => { output.push(chunk); responseCount += chunk.toString("utf8").split("\n").length - 1; if (responseCount >= count) child.stdin.end(); });
  child.stderr.on("data", (chunk) => errors.push(chunk));
  let ready;
  const started = new Promise((resolve) => { ready = resolve; });
  child.on("message", (message) => { if (message.type === "started") ready(); });
  const completion = new Promise((resolve, reject) => {
    const timer = setTimeout(() => { child.kill(); reject(new Error("The synthetic query child exceeded its deadline.")); }, 10_000);
    child.on("error", (error) => { clearTimeout(timer); reject(error); });
    child.on("close", (code) => { clearTimeout(timer); resolve({ code, stdout: Buffer.concat(output).toString("utf8"), stderr: Buffer.concat(errors).toString("utf8") }); });
  });
  for (const request of requests) child.stdin.write(JSON.stringify(request) + "\n");
  if (cancel) { await Promise.race([started, completion.then(() => { throw new Error("The query child exited before cancellation." ); })]); child.stdin.write(JSON.stringify(cancel) + "\n"); }
  const result = await completion;
  return { ...result, responses: result.stdout.trim().split("\n").map((line) => JSON.parse(line)) };
}
