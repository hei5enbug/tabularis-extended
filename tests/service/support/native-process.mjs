import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { spawn } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { inflateRawSync } from "node:zlib";
import { performance } from "node:perf_hooks";

const FRAME_BYTES = 16 * 1024 * 1024;
const LOG_BYTES = 8 * 1024 * 1024;
const PACKAGE_BYTES = 64 * 1024 * 1024;
const INPUT_NAMES = Object.freeze([
  "TABULARIS_TEST_HOST_BINARY", "TABULARIS_TEST_POSTGIS_PLUGIN",
  "TABULARIS_TEST_POSTGIS_MANIFEST", "TABULARIS_TEST_POSTGIS17_PORT",
  "TABULARIS_TEST_SPATIAL_ZIP",
]);
export const QUERY = "SELECT id, 'native-한글' AS note, ST_SetSRID(ST_MakePoint(10,20),4326) AS geom FROM generate_series(1,3) AS id ORDER BY id";

export class NativeHarnessError extends Error {
  constructor(code, evidenceRoot = null) {
    super(code);
    this.code = code;
    this.evidenceRoot = evidenceRoot;
  }
}

function fail(code) { throw new NativeHarnessError(code); }
export function sha256(bytes) { return createHash("sha256").update(bytes).digest("hex"); }
export function requireCondition(condition, code) { if (!condition) fail(code); }

function regularFile(filename, maximum) {
  requireCondition(typeof filename === "string" && path.isAbsolute(filename), "EXPLICIT_ABSOLUTE_INPUT_REQUIRED");
  const fd = fs.openSync(filename, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try {
    const stat = fs.fstatSync(fd);
    requireCondition(stat.isFile() && stat.size > 0 && stat.size <= maximum, "INPUT_FILE_INVALID");
    const chunks = [];
    let total = 0;
    const buffer = Buffer.alloc(Math.min(maximum + 1, 65536));
    while (true) {
      const count = fs.readSync(fd, buffer, 0, Math.min(buffer.length, maximum + 1 - total), null);
      if (count === 0) break;
      total += count;
      requireCondition(total <= maximum, "INPUT_FILE_TOO_LARGE");
      chunks.push(Buffer.from(buffer.subarray(0, count)));
    }
    const after = fs.fstatSync(fd);
    requireCondition(stat.dev === after.dev && stat.ino === after.ino && stat.size === total && after.size === total && stat.mtimeMs === after.mtimeMs, "INPUT_FILE_CHANGED");
    return Buffer.concat(chunks, total);
  } finally { fs.closeSync(fd); }
}

export function loadNativeInputs(environment) {
  const values = Object.fromEntries(INPUT_NAMES.map((name) => {
    const value = environment[name];
    requireCondition(typeof value === "string" && value.length > 0, `INPUT_REQUIRED:${name}`);
    return [name, value];
  }));
  for (const name of INPUT_NAMES.filter((name) => !name.endsWith("_PORT"))) {
    requireCondition(path.isAbsolute(values[name]) && fs.lstatSync(values[name]).isFile(), `INPUT_FILE_REQUIRED:${name}`);
  }
  const text = values.TABULARIS_TEST_POSTGIS17_PORT;
  requireCondition(/^\d+$/.test(text) && Number(text) > 0 && Number(text) <= 65535, "INVALID_POSTGIS_PORT");
  requireCondition(path.basename(values.TABULARIS_TEST_POSTGIS_MANIFEST) === ".tabularium", "POSTGIS_MANIFEST_REQUIRED");
  return Object.freeze({
    hostBinary: values.TABULARIS_TEST_HOST_BINARY,
    pluginBinary: values.TABULARIS_TEST_POSTGIS_PLUGIN,
    pluginManifest: values.TABULARIS_TEST_POSTGIS_MANIFEST,
    port: Number(text), spatialZip: values.TABULARIS_TEST_SPATIAL_ZIP,
    environment: values,
  });
}

export function explicitNativeEnvironment() {
  return Object.fromEntries(INPUT_NAMES.map((name) => [name, process.env[name]]));
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function zipFiles(bytes) {
  const end = bytes.length - 22;
  requireCondition(end >= 0 && bytes.readUInt32LE(end) === 0x06054b50 && bytes.readUInt16LE(end + 20) === 0, "INVALID_SPATIAL_ZIP");
  const count = bytes.readUInt16LE(end + 10);
  const centralSize = bytes.readUInt32LE(end + 12);
  let cursor = bytes.readUInt32LE(end + 16);
  requireCondition(bytes.readUInt16LE(end + 4) === 0 && bytes.readUInt16LE(end + 6) === 0 && bytes.readUInt16LE(end + 8) === count && count > 0 && count <= 1024 && cursor + centralSize === end, "INVALID_SPATIAL_ZIP");
  const files = new Map();
  let total = 0;
  for (let index = 0; index < count; index += 1) {
    requireCondition(cursor + 46 <= end && bytes.readUInt32LE(cursor) === 0x02014b50, "INVALID_SPATIAL_ZIP");
    const flags = bytes.readUInt16LE(cursor + 8);
    const method = bytes.readUInt16LE(cursor + 10);
    const crc = bytes.readUInt32LE(cursor + 16);
    const compressed = bytes.readUInt32LE(cursor + 20);
    const length = bytes.readUInt32LE(cursor + 24);
    const nameSize = bytes.readUInt16LE(cursor + 28);
    const extraSize = bytes.readUInt16LE(cursor + 30);
    const commentSize = bytes.readUInt16LE(cursor + 32);
    const mode = bytes.readUInt32LE(cursor + 38) >>> 16;
    const local = bytes.readUInt32LE(cursor + 42);
    requireCondition(cursor + 46 + nameSize + extraSize + commentSize <= end && flags === 0x800 && method === 8 && (mode & 0xf000) === 0x8000 && length <= 16 * 1024 * 1024, "INVALID_SPATIAL_ZIP");
    const nameBytes = bytes.subarray(cursor + 46, cursor + 46 + nameSize);
    const name = new TextDecoder("utf-8", { fatal: true }).decode(nameBytes);
    requireCondition(name.length > 0 && !name.startsWith("/") && !/[\\:\x00-\x1f\x7f]/.test(name) && name.split("/").every((part) => part && part !== "." && part !== "..") && !files.has(name), "INVALID_SPATIAL_ZIP_PATH");
    requireCondition(local + 30 <= cursor && bytes.readUInt32LE(local) === 0x04034b50 && bytes.readUInt16LE(local + 6) === flags && bytes.readUInt16LE(local + 8) === method && bytes.readUInt32LE(local + 14) === crc && bytes.readUInt32LE(local + 18) === compressed && bytes.readUInt32LE(local + 22) === length, "INVALID_SPATIAL_ZIP");
    const localName = bytes.readUInt16LE(local + 26);
    const start = local + 30 + localName + bytes.readUInt16LE(local + 28);
    requireCondition(localName === nameSize && bytes.subarray(local + 30, local + 30 + localName).equals(nameBytes) && start + compressed <= bytes.readUInt32LE(end + 16), "INVALID_SPATIAL_ZIP");
    total += length;
    requireCondition(total <= PACKAGE_BYTES, "SPATIAL_PACKAGE_TOO_LARGE");
    const data = inflateRawSync(bytes.subarray(start, start + compressed), { maxOutputLength: 16 * 1024 * 1024 });
    requireCondition(data.length === length && crc32(data) === crc, "SPATIAL_ZIP_CRC_MISMATCH");
    files.set(name, data);
    cursor += 46 + nameSize + extraSize + commentSize;
  }
  requireCondition(cursor === end, "INVALID_SPATIAL_ZIP");
  return files;
}

function privateWrite(filename, bytes) {
  const fd = fs.openSync(filename, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600);
  try { fs.writeFileSync(fd, bytes); fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
}

function directory(filename) { fs.mkdirSync(filename, { recursive: true, mode: 0o700 }); }

function provision(inputs, root) {
  const profile = path.join(root, "p");
  const plugins = path.join(root, "g");
  const evidence = path.join(root, "e");
  for (const filename of [profile, plugins, evidence, path.join(root, "t")]) directory(filename);
  const manifestBytes = regularFile(inputs.pluginManifest, 1024 * 1024);
  const manifest = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(manifestBytes));
  requireCondition(manifest.id === "postgresql" && manifest.kind === "driver" && manifest.engine === "postgresql" && typeof manifest.executable === "string" && /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(manifest.executable), "POSTGIS_MANIFEST_INVALID");
  const driverRoot = path.join(plugins, "drivers", manifest.id);
  directory(driverRoot);
  privateWrite(path.join(driverRoot, ".tabularium"), manifestBytes);
  privateWrite(path.join(driverRoot, manifest.executable), regularFile(inputs.pluginBinary, 256 * 1024 * 1024));
  fs.chmodSync(path.join(driverRoot, manifest.executable), 0o700);
  const zip = regularFile(inputs.spatialZip, 128 * 1024 * 1024);
  const expectedHash = regularFile(`${inputs.spatialZip}.sha256`, 128).toString("ascii").trim();
  requireCondition(/^[a-f0-9]{64}$/.test(expectedHash) && sha256(zip) === expectedHash, "SPATIAL_ZIP_SHA_MISMATCH");
  const entries = zipFiles(zip);
  const required = [".tabularium", "LICENSE", "ui/dist/index.js", "ui/dist/style.css", "ui/dist/maplibre-worker.js", "licenses/third-party-notices.json", "release.json"];
  requireCondition(required.every((name) => entries.has(name)) && [...entries.keys()].every((name) => required.includes(name) || name.startsWith("licenses/packages/")), "SPATIAL_PACKAGE_LAYOUT_INVALID");
  const spatial = JSON.parse(entries.get(".tabularium").toString("utf8"));
  const release = JSON.parse(entries.get("release.json").toString("utf8"));
  const expectedSlots = [
    { slot: "data-grid.toolbar.actions", module: "ui/dist/index.js", driver: "postgresql" },
    { slot: "app.map.renderer", module: "ui/dist/index.js" },
  ];
  const expectedAssets = [
    { path: "ui/dist/style.css", mime: "text/css" },
    { path: "ui/dist/maplibre-worker.js", mime: "text/javascript" },
  ];
  const declarationsMatch = (actual, expected, key) => Array.isArray(actual) && actual.length === expected.length
    && expected.every((entry) => actual.filter((value) => value?.[key] === entry[key] && Object.keys(value).length === Object.keys(entry).length && Object.entries(entry).every(([name, value]) => value === actual.find((item) => item?.[key] === entry[key])?.[name])).length === 1);
  requireCondition(spatial.id === "spatial" && spatial.kind === "extension" && spatial.service_protocol === 1 && spatial.required_service_protocol === 1
    && spatial.min_runtime_version === "0.27.0+extended.1" && declarationsMatch(spatial.ui_extensions, expectedSlots, "slot") && declarationsMatch(spatial.ui_assets, expectedAssets, "path")
    && Array.isArray(release.files) && release.files.length === entries.size - 1, "SPATIAL_PACKAGE_NOT_READY");
  const seen = new Set();
  for (const file of release.files) {
    const data = entries.get(file.path);
    requireCondition(file.path !== "release.json" && !seen.has(file.path) && data && file.bytes === data.length && file.sha256 === sha256(data), "SPATIAL_RELEASE_SHA_MISMATCH");
    seen.add(file.path);
  }
  const extensionRoot = path.join(plugins, "extensions", "spatial");
  directory(extensionRoot);
  for (const [name, bytes] of entries) {
    const destination = path.join(extensionRoot, ...name.split("/"));
    directory(path.dirname(destination));
    privateWrite(destination, bytes);
  }
  privateWrite(path.join(profile, "config.json"), JSON.stringify({
    activeExternalDrivers: [manifest.id, "spatial"], plugins: { [manifest.id]: { settings: {} }, spatial: { settings: {} } },
    aiEnabled: false, checkForUpdates: false, autoCheckUpdatesOnStartup: false, showWelcome: false,
    mcpReadonlyDefault: true, mcpPreflightExplain: false,
  }));
  return { profile, plugins, evidence, driverId: manifest.id, zipSha256: expectedHash,
    assetSha256: Object.fromEntries(required.filter((name) => name.startsWith("ui/")).map((name) => [name, sha256(entries.get(name))])) };
}

function responseExit(error) {
  if (!error) return 0;
  if (["INVALID_ARGUMENT", "PROTOCOL_MISMATCH"].includes(error.code)) return 2;
  if (["PERMISSION_DENIED", "WRITE_NOT_ALLOWED", "AUTH_REQUIRED", "AUTH_EXPIRED", "INTERACTION_REQUIRED"].includes(error.code)) return 3;
  return error.code === "CANCELLED" ? 130 : 4;
}

export function checkedEnvelope(response, request) {
  requireCondition(response && response.protocol_version === 1 && response.request_id === request.request_id && response.connection_id === request.connection_id && ["succeeded", "failed", "cancelled", "outcome_unknown"].includes(response.status), "SERVICE_ENVELOPE_INVALID");
  requireCondition(response.page && typeof response.page.has_more === "boolean" && ["none", "native", "materialized"].includes(response.page.resume_mode) && (response.page.next_token === null || typeof response.page.next_token === "string" && response.page.next_token.length > 0), "SERVICE_PAGE_INVALID");
  requireCondition(response.limits && typeof response.limits.truncated === "boolean" && Array.isArray(response.limits.reasons) && Array.isArray(response.warnings) && response.metrics && Number.isSafeInteger(response.metrics.elapsed_ms) && response.metrics.elapsed_ms >= 0 && Number.isSafeInteger(response.metrics.retry_count) && response.metrics.retry_count >= 0, "SERVICE_METADATA_INVALID");
  requireCondition(response.error === null || response.error && typeof response.error.code === "string" && typeof response.error.message === "string" && typeof response.error.retryable === "boolean" && ["not_started", "not_applied", "applied", "unknown"].includes(response.error.outcome), "SERVICE_ERROR_INVALID");
  return response;
}

export function succeeded(response) {
  requireCondition(response.status === "succeeded" && response.error === null, "REQUIRED_OPERATION_FAILED");
  return response;
}

export class NativeProcess {
  constructor(fixture, label, args, mode) {
    this.fixture = fixture;
    this.mode = mode;
    this.label = label;
    this.pending = new Map();
    this.buffer = Buffer.alloc(0);
    this.logSize = { stdout: 0, stderr: 0 };
    this.expectedExits = new Set([0]);
    this.failed = null;
    this.exited = null;
    this.closed = null;
    this.logs = Object.fromEntries(["stdout", "stderr"].map((stream) => [stream, fs.openSync(path.join(fixture.evidence, `${label}.${stream}.log`), "wx", 0o600)]));
    this.child = spawn(fixture.inputs.hostBinary, args, { cwd: fixture.root, env: fixture.childEnvironment, shell: false, detached: process.platform !== "win32", stdio: ["pipe", "pipe", "pipe"] });
    this.exitPromise = new Promise((resolve) => {
      this.child.once("error", () => this.break(new NativeHarnessError("NATIVE_SPAWN_FAILED", fixture.root)));
      this.child.once("exit", (code, signal) => { this.exited = { code, signal }; this.break(new NativeHarnessError("NATIVE_PROCESS_EXITED", fixture.root)); });
      this.child.once("close", (code, signal) => { this.closed = { code, signal }; resolve(this.closed); });
    });
    this.child.stdout.on("data", (bytes) => this.output("stdout", bytes));
    this.child.stderr.on("data", (bytes) => this.output("stderr", bytes));
    this.child.stdin.on("error", () => this.break(new NativeHarnessError("NATIVE_STDIN_FAILED", fixture.root)));
  }
  output(stream, bytes) {
    this.logSize[stream] += bytes.length;
    if (this.logSize[stream] > LOG_BYTES) { this.break(new NativeHarnessError("NATIVE_LOG_LIMIT", this.fixture.root)); this.kill("SIGKILL"); return; }
    fs.writeSync(this.logs[stream], bytes);
    if (stream !== "stdout" || ["gui", "command"].includes(this.mode)) return;
    this.buffer = Buffer.concat([this.buffer, bytes]);
    if (this.buffer.length > FRAME_BYTES) { this.break(new NativeHarnessError("NATIVE_FRAME_LIMIT", this.fixture.root)); this.kill("SIGKILL"); return; }
    let end;
    while ((end = this.buffer.indexOf(10)) !== -1) {
      const line = this.buffer.subarray(0, end);
      this.buffer = this.buffer.subarray(end + 1);
      if (!line.length) continue;
      try {
        const value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(line));
        const id = this.mode === "cli" ? value.request_id : value.id;
        const pending = this.pending.get(id);
        requireCondition(pending, "UNEXPECTED_NATIVE_RESPONSE");
        this.pending.delete(id);
        clearTimeout(pending.timer);
        pending.resolve(value);
      } catch (error) { this.break(new NativeHarnessError(error instanceof NativeHarnessError ? error.code : "NATIVE_JSON_INVALID", this.fixture.root)); this.kill("SIGKILL"); return; }
    }
  }
  break(error) {
    this.failed ??= error;
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(error); }
    this.pending.clear();
  }
  kill(signal) {
    if (this.exited || !this.child.pid) return;
    try { if (process.platform === "win32") this.child.kill(signal); else process.kill(-this.child.pid, signal); } catch (error) { if (error.code !== "ESRCH") this.fixture.cleanupErrors.push("OWN_PROCESS_TERMINATION_FAILED"); }
  }
  frame(value, id, timeoutMs = 32000) {
    if (this.failed) return Promise.reject(this.failed);
    const bytes = Buffer.from(`${JSON.stringify(value)}\n`);
    requireCondition(bytes.length <= FRAME_BYTES && this.pending.size < 64 && !this.pending.has(id), "NATIVE_REQUEST_LIMIT");
    const remaining = Math.min(timeoutMs, this.fixture.deadline - performance.now());
    requireCondition(remaining > 0, "NATIVE_RUN_TIMEOUT");
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new NativeHarnessError("NATIVE_REQUEST_TIMEOUT", this.fixture.root));
        this.break(new NativeHarnessError("NATIVE_REQUEST_TIMEOUT", this.fixture.root));
        this.kill("SIGKILL");
      }, remaining);
      this.pending.set(id, { resolve, reject, timer });
      this.child.stdin.write(bytes, (error) => {
        bytes.fill(0);
        if (error && this.pending.has(id)) { this.pending.delete(id); clearTimeout(timer); reject(new NativeHarnessError("NATIVE_SEND_FAILED", this.fixture.root)); this.break(new NativeHarnessError("NATIVE_SEND_FAILED", this.fixture.root)); }
      });
    });
  }
  async rpc(method, params = {}) {
    const id = randomUUID();
    const frame = await this.frame({ jsonrpc: "2.0", id, method, params }, id);
    requireCondition(frame.jsonrpc === "2.0" && frame.id === id && !frame.error && frame.result, "MCP_RPC_FAILED");
    return frame.result;
  }
  async call(request) {
    this.fixture.requests.push({ transport: this.mode, operation: request.operation, request_id: request.request_id, connection_id: request.connection_id });
    const frame = this.mode === "cli" ? await this.frame(request, request.request_id, request.deadline_ms + 2000) : await this.frame({ jsonrpc: "2.0", id: request.request_id, method: "tools/call", params: { name: request.operation, arguments: request } }, request.request_id, request.deadline_ms + 2000);
    let response;
    if (this.mode === "cli") response = frame;
    else {
      requireCondition(frame.jsonrpc === "2.0" && frame.id === request.request_id && !frame.error && frame.result?.structuredContent, "MCP_TOOL_FAILED");
      const tool = frame.result;
      response = tool.structuredContent;
      requireCondition(tool.content?.length === 1 && tool.content[0].type === "text" && JSON.stringify(JSON.parse(tool.content[0].text)) === JSON.stringify(response) && tool.isError === (response.error !== null) && typeof response.presentation_truncated === "boolean", "MCP_PRESENTATION_INVALID");
    }
    checkedEnvelope(response, request);
    this.expectedExits.add(responseExit(response.error));
    this.fixture.responses.push({ transport: this.mode, operation: request.operation, response });
    return response;
  }
  async close() {
    const end = performance.now() + 5000;
    if (this.mode === "gui") this.kill("SIGTERM"); else this.child.stdin.end();
    let timer;
    let escalated = false;
    const escalation = setTimeout(() => { escalated = true; this.kill("SIGKILL"); }, 3000);
    try {
      const exited = await Promise.race([this.exitPromise, new Promise((_, reject) => { timer = setTimeout(() => reject(new NativeHarnessError("NATIVE_SHUTDOWN_TIMEOUT", this.fixture.root)), Math.max(1, end - performance.now())); })]);
      requireCondition(!escalated, "NATIVE_SHUTDOWN_TIMEOUT");
      if (this.mode !== "gui") requireCondition(exited.signal === null && this.expectedExits.has(exited.code), "NATIVE_SHUTDOWN_FAILED");
      return exited;
    } catch (error) {
      this.kill("SIGKILL");
      throw error;
    } finally {
      clearTimeout(timer);
      clearTimeout(escalation);
      this.child.stdout.removeAllListeners("data");
      this.child.stderr.removeAllListeners("data");
      this.child.stdout.destroy(); this.child.stderr.destroy(); this.child.stdin.destroy();
      for (const fd of Object.values(this.logs)) fs.closeSync(fd);
    }
  }
}

export function request(operation, input = {}, overrides = {}) {
  return { protocol_version: 1, operation, request_id: randomUUID(), connection_id: null,
    session_id: null, map_id: null, expected_version: null, deadline_ms: 30000,
    input: structuredClone(input), ...overrides };
}

export function createNativeFixture(inputs) {
  requireCondition(process.platform === "darwin", "NATIVE_MACOS_FIXTURE_REQUIRED");
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "tabularis-service-fixture-"));
  fs.chmodSync(root, 0o700);
  const fixture = { root, inputs, deadline: performance.now() + 10 * 60 * 1000,
    processes: [], requests: [], responses: [], cleanupErrors: [] };
  try { Object.assign(fixture, provision(inputs, root)); }
  catch (error) {
    const code = error instanceof NativeHarnessError ? error.code : "NATIVE_PROVISION_FAILED";
    const cleanupErrors = [];
    for (const name of ["p", "g", "t"]) {
      try { fs.rmSync(path.join(root, name), { recursive: true, force: true }); }
      catch { cleanupErrors.push("FIXTURE_PREPARATION_CLEANUP_FAILED"); }
    }
    try {
      directory(path.join(root, "e"));
      privateWrite(path.join(root, "e", "preparation-failure.json"), JSON.stringify({ error: code, evidence_kind: "native_fixture_preparation", native_execution: false, cleanup_errors: cleanupErrors }));
    } catch { cleanupErrors.push("FIXTURE_PREPARATION_REPORT_FAILED"); }
    console.error(JSON.stringify({ error: code, evidence_root: root, cleanup_errors: cleanupErrors }));
    throw new NativeHarnessError(code, root);
  }
  fixture.childEnvironment = { TABULARIS_DATA_DIR: fixture.profile, TABULARIS_PLUGIN_DIR: fixture.plugins,
    PATH: "/usr/bin:/bin:/usr/sbin:/sbin", TMPDIR: path.join(root, "t") };
  let interruptTimer;
  const interrupt = () => {
    fixture.cleanupErrors.push("NATIVE_RUN_INTERRUPTED");
    for (const child of fixture.processes) child.kill("SIGTERM");
    interruptTimer = setTimeout(() => { for (const child of fixture.processes) child.kill("SIGKILL"); }, 3500);
  };
  process.once("SIGINT", interrupt); process.once("SIGTERM", interrupt);
  fixture.start = (label, mode = label, args = mode === "cli" ? ["service", "--stdio"] : mode === "mcp" ? ["--mcp"] : []) => {
    const child = new NativeProcess(fixture, label, args, mode);
    fixture.processes.push(child);
    return child;
  };
  fixture.finish = async (evidence) => {
    const terminations = [];
    for (const child of [...fixture.processes].reverse()) {
      try { terminations.push({ label: child.label, ...await child.close() }); }
      catch (error) { fixture.cleanupErrors.push(error instanceof NativeHarnessError ? error.code : "NATIVE_CLEANUP_FAILED"); }
    }
    clearTimeout(interruptTimer);
    process.removeListener("SIGINT", interrupt); process.removeListener("SIGTERM", interrupt);
    const report = { evidence_kind: "actual_native_product_with_isolated_app_identifier", integration_complete: false,
      profile_isolated: true, gui_identifier: "dev.tabularis.spatial-verification-01a1000d", postgis_port: inputs.port,
      spatial_zip_sha256: fixture.zipSha256, assets_sha256: fixture.assetSha256, requests: fixture.requests,
      responses: fixture.responses, terminations, cleanup_errors: fixture.cleanupErrors, ...evidence };
    privateWrite(path.join(fixture.evidence, "report.json"), JSON.stringify(report, null, 2));
    fs.rmSync(fixture.profile, { recursive: true }); fs.rmSync(fixture.plugins, { recursive: true }); fs.rmSync(path.join(root, "t"), { recursive: true });
    return { evidenceRoot: fixture.evidence, cleanupErrors: fixture.cleanupErrors, terminations };
  };
  return fixture;
}

export async function connectFixture(fixture, cli) {
  succeeded(await cli.call(request("connection.list")));
  const response = succeeded(await cli.call(request("connection.create", {
    name: "Native spatial verification", driver: fixture.driverId, credential_ref: null,
    settings: { host: "127.0.0.1", port: fixture.inputs.port, username: "postgres", database: "spatial_fixture", ssl_mode: "disable", save_in_keychain: false },
  })));
  const id = response.data.connection_id;
  requireCondition(typeof id === "string" && id.length > 0 && response.data.driver === fixture.driverId && response.data.allow_writes === false, "SAVED_FIXTURE_CONNECTION_INVALID");
  succeeded(await cli.call(request("connection.test", {}, { connection_id: id })));
  return id;
}

export function queryInput() { return { language: "sql", text: QUERY, parameters: [], page_size: 1, database: "spatial_fixture" }; }
export function querySource(resultId) { return { kind: "query_result", result_id: resultId, result_set_index: 0, column_index: 2, source_srid: 4326, longitude_mode: "preserve", skip_invalid: false }; }

export async function pollUntil(check, deadline) {
  while (performance.now() < deadline) {
    const value = await check();
    if (value) return value;
    const remaining = deadline - performance.now();
    if (remaining > 0) await new Promise((resolve) => setTimeout(resolve, Math.min(100, remaining)));
  }
  fail("NATIVE_READINESS_TIMEOUT");
}

export async function runArtifactSave(fixture, id, destination) {
  requireCondition(/^artifact-[a-f0-9-]{36}$/.test(id) && path.isAbsolute(destination), "ARTIFACT_SAVE_INPUT_INVALID");
  const child = fixture.start(`artifact-save-${randomUUID()}`, "command", ["service", "artifact-save", id, "--output", destination]);
  const end = Math.min(fixture.deadline, performance.now() + 120000);
  const result = await pollUntil(async () => child.closed, end);
  requireCondition(result.code === 0 && result.signal === null, "ARTIFACT_SAVE_FAILED");
  const metadata = JSON.parse(regularFile(path.join(fixture.evidence, `${child.label}.stdout.log`), 1024 * 1024).toString("utf8"));
  const bytes = regularFile(destination, 1024 * 1024);
  requireCondition(metadata.artifact_id === id && metadata.bytes === bytes.length && metadata.checksum === sha256(bytes), "ARTIFACT_SAVE_CHECKSUM_INVALID");
  return { metadata, bytes };
}

export async function readMcpArtifact(mcp, id) {
  const uri = `tabularis://artifact/${id}`;
  const response = await mcp.rpc("resources/read", { uri });
  requireCondition(response.contents?.length === 1 && response.contents[0].uri === uri && response.contents[0].mimeType === "application/json", "ARTIFACT_METADATA_INVALID");
  const metadata = JSON.parse(response.contents[0].text);
  requireCondition(metadata.artifact_id === id && Number.isSafeInteger(metadata.bytes) && metadata.bytes > 0 && metadata.bytes <= 1024 * 1024, "ARTIFACT_METADATA_INVALID");
  const chunks = [];
  let offset = 0;
  while (offset < metadata.bytes) {
    const chunkUri = `${uri}?offset=${offset}&limit=65536`;
    const chunk = await mcp.rpc("resources/read", { uri: chunkUri });
    requireCondition(chunk.artifact_id === id && chunk.offset === offset && chunk.contents?.length === 1 && chunk.contents[0].uri === chunkUri && chunk.contents[0].mimeType === "application/octet-stream" && typeof chunk.contents[0].blob === "string", "ARTIFACT_CHUNK_INVALID");
    const bytes = Buffer.from(chunk.contents[0].blob, "base64");
    requireCondition(bytes.length > 0 && bytes.length <= 65536 && bytes.toString("base64") === chunk.contents[0].blob && sha256(bytes) === chunk.chunk_checksum && chunk.checksum === metadata.checksum && offset + bytes.length <= metadata.bytes && chunk.eof === (offset + bytes.length === metadata.bytes), "ARTIFACT_CHUNK_CHECKSUM_INVALID");
    chunks.push(bytes); offset += bytes.length;
  }
  const bytes = Buffer.concat(chunks, offset);
  requireCondition(sha256(bytes) === metadata.checksum, "ARTIFACT_CHECKSUM_INVALID");
  return { metadata, bytes, chunkCount: chunks.length };
}
