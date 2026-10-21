import { createHash, randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { makePerformancePages, performanceExpectations, validatePerformance } from './performance-data.mjs';

export const EVIDENCE = 'actual_macos_wkwebview_with_mock_service';
export const CHECKS = ['open_ack', 'real_worker', 'worker_geojson', 'real_webgl', 'canvas_size', 'feature_options', 'null_empty', 'css_loaded', 'raw_detail', 'safe_text', 'stale_rejected', 'updated_ack', 'close_ack', 'worker_terminated', 'assets_disposed', 'css_removed', 'modal_unmounted', 'root_shutdown', 'unsubscribed', 'no_external_requests', 'no_static_imports', 'query_snapshot_only'];
export const MAX_REPORT_BYTES = 64 * 1024;
export const MAX_OUTPUT_BYTES = 1024 * 1024;
const HERE = dirname(fileURLToPath(import.meta.url));
const SOURCE = resolve(HERE, '../../..');
const UI = join(SOURCE, 'ui');
const fixedEnvironment = { PATH: '/usr/bin:/bin', LANG: 'en_US.UTF-8' };

function failure(code) { return Object.assign(new Error(code), { code }); }
export function validateReport(raw, { performanceMode = false } = {}) {
  const bytes = Buffer.isBuffer(raw) ? raw : Buffer.from(raw);
  if (bytes.length > MAX_REPORT_BYTES) throw failure('REPORT_TOO_LARGE');
  let report;
  try { report = JSON.parse(bytes.toString('utf8')); } catch { throw failure('INVALID_REPORT'); }
  if (!report || typeof report !== 'object' || Array.isArray(report) || typeof report.pass !== 'boolean' || report.evidence !== EVIDENCE) throw failure('INVALID_REPORT');
  if (report.pass) {
    if (!report.checks || Object.keys(report.checks).length !== CHECKS.length || !CHECKS.every(name => report.checks[name] === true)) throw failure('INVALID_REPORT');
    for (const [name, version, generation] of [['open_ack', 0, 1], ['update_ack', 2, 2], ['close_ack', 2, 3], ['reopen_ack', 2, 4], ['reclose_ack', 2, 5]]) {
      const ack = report[name];
      if (!ack || ack.gui_applied !== true || ack.state_version !== version || ack.rendered_version !== version || ack.generation !== generation) throw failure('INVALID_REPORT');
    }
    const metrics = report.metrics;
    if (!metrics || !['workers_started', 'worker_messages', 'worker_geojson', 'gl_contexts', 'gl_draws'].every(name => Number.isSafeInteger(metrics[name]) && metrics[name] > 0)
      || metrics.gl_contexts < 2 || metrics.workers_terminated !== metrics.workers_started || metrics.assets_created !== 4 || metrics.assets_disposed !== 4
      || metrics.subscriptions !== 1 || metrics.unsubscribed !== 1 || metrics.external_attempts !== 0 || metrics.worker_raw_canary !== false
      || report.feature_count !== 9 || report.geometry_types?.length !== 7
      || new Set(report.geometry_types).size !== 7 || !['Point', 'LineString', 'Polygon', 'MultiPoint', 'MultiLineString', 'MultiPolygon', 'GeometryCollection'].every(type => report.geometry_types.includes(type))) throw failure('INVALID_REPORT');
    const cycles = report.cycles;
    if (!Array.isArray(cycles) || cycles.length !== 2 || !cycles.every(cycle => Number.isSafeInteger(cycle.workers_started) && cycle.workers_started > 0 && cycle.workers_terminated === cycle.workers_started && cycle.css_removed === true && cycle.canvas_removed === true && cycle.modal_unmounted === true && cycle.ack_cleanup_complete === true)
      || cycles[0].assets_created !== 2 || cycles[0].assets_disposed !== 2 || cycles[1].assets_created !== 4 || cycles[1].assets_disposed !== 4
      || cycles[1].workers_started <= cycles[0].workers_started || cycles[1].workers_started !== metrics.workers_started || cycles[1].gl_draws <= cycles[0].gl_draws) throw failure('INVALID_REPORT');
  }
  if (performanceMode) {
    if (report.mode !== 'performance') throw failure('INVALID_PERFORMANCE_REPORT');
    if (report.pass) validatePerformance(report.performance, performanceExpectations(makePerformancePages()));
  }
  return report;
}

export function fixtureHtml({ performanceMode = false } = {}) {
  return '<!doctype html><html lang="ko"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="icon" href="data:,"><title>V1 WKWebView fixture</title><style>html,body{margin:0;background:#111827;color:#f9fafb;font-family:system-ui}body{padding:16px}button,input,select{font:inherit}[role=dialog]{width:100%}</style></head><body><div id="plugin-root"></div><script>window.addEventListener("error",function(){window.webkit.messageHandlers.v1Result.postMessage({pass:false,code:"SHIM_INITIALIZATION_FAILED",evidence:"' + EVIDENCE + '"})});</script><script src="/shim.js"></script><script src="/production/index.js"></script><script>V1Fixture.' + (performanceMode ? 'startPerformance' : 'start') + '();</script></body></html>';
}

export function routeFor(method, rawPath, host, port) {
  const allowed = ['/index.html', '/shim.js', '/production/index.js', '/style.css', '/maplibre-worker.js'];
  return method === 'GET' && host === `127.0.0.1:${port}` && allowed.includes(rawPath) ? rawPath : null;
}

export function boundedProcess(executable, args, { cwd, timeout = 60000, maximum = MAX_OUTPUT_BYTES } = {}) {
  if (!isAbsolute(executable) || timeout < 1 || timeout > 60000 || maximum < 1 || maximum > MAX_OUTPUT_BYTES) return Promise.reject(failure('INVALID_PROCESS_BOUNDS'));
  return new Promise((resolveResult, reject) => {
    const child = spawn(executable, args, { cwd, env: { ...fixedEnvironment, ...(cwd ? { TMPDIR: cwd } : {}) }, stdio: ['ignore', 'pipe', 'pipe'], shell: false });
    let total = 0, output = [], errors = [], terminal = null;
    const timer = setTimeout(() => { terminal = failure('PROCESS_TIMEOUT'); child.kill('SIGKILL'); }, timeout);
    const collect = (target, data) => {
      const remaining = Math.max(0, maximum - total);
      if (remaining) target.push(data.subarray(0, remaining));
      total += data.length;
      if (total > maximum && !terminal) { terminal = failure('OUTPUT_LIMIT'); child.kill('SIGKILL'); }
    };
    child.stdout.on('data', data => collect(output, data));
    child.stderr.on('data', data => collect(errors, data));
    child.on('error', () => { terminal = failure('PROCESS_START_FAILED'); });
    child.on('close', (code, signal) => {
      clearTimeout(timer);
      const result = { code, signal, stdout: Buffer.concat(output), stderr: Buffer.concat(errors) };
      if (terminal) reject(Object.assign(terminal, { result })); else resolveResult(result);
    });
  });
}

async function productionAssets() {
  const files = {};
  for (const name of ['index.js', 'style.css', 'maplibre-worker.js']) {
    const path = join(UI, 'dist', name);
    const information = await stat(path);
    if (!information.isFile() || information.size < 1 || information.size > 16 * 1024 * 1024) throw failure('INVALID_PRODUCTION_ASSET');
    const bytes = await readFile(path);
    files[name] = { bytes, size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') };
  }
  return files;
}
function assetEvidence(files) { return Object.fromEntries(Object.entries(files).map(([name, value]) => [name, { bytes: value.size, sha256: value.sha256 }])); }

export async function buildFixture(root) {
  const require = createRequire(join(UI, '__v1_fixture__.cjs'));
  const vitePackage = require.resolve('vite/package.json');
  const { build } = await import(pathToFileURL(join(dirname(vitePackage), 'dist/node/index.js')).href);
  const output = join(root, 'fixture-build');
  await mkdir(output, { mode: 0o700 });
  await build({ configFile: false, root: UI, logLevel: 'silent', publicDir: false,
    resolve: { alias: [
      { find: 'react/jsx-runtime', replacement: require.resolve('react/jsx-runtime') },
      { find: 'react-dom/client', replacement: require.resolve('react-dom/client') },
      { find: /^react-dom$/, replacement: require.resolve('react-dom') },
      { find: /^react$/, replacement: require.resolve('react') },
    ] },
    define: { 'process.env.NODE_ENV': JSON.stringify('production') }, esbuild: { tsconfigRaw: { compilerOptions: { jsx: 'automatic' } } },
    build: { outDir: output, emptyOutDir: false, sourcemap: false, minify: false, lib: { entry: join(HERE, 'fixture.tsx'), name: 'V1Fixture', formats: ['iife'], fileName: () => 'fixture.js' }, rollupOptions: { output: { inlineDynamicImports: true } } },
  });
  const react = JSON.parse(await readFile(require.resolve('react/package.json'), 'utf8')).version;
  if (react !== '19.2.4') throw failure('UNEXPECTED_REACT_VERSION');
  return { bytes: await readFile(join(output, 'fixture.js')), react };
}

async function serve(files, shim, options) {
  const requests = [];
  let port;
  const table = new Map([
    ['/index.html', [Buffer.from(fixtureHtml(options)), 'text/html; charset=utf-8']],
    ['/shim.js', [shim, 'text/javascript; charset=utf-8']],
    ['/production/index.js', [files['index.js'].bytes, 'text/javascript; charset=utf-8']],
    ['/style.css', [files['style.css'].bytes, 'text/css; charset=utf-8']],
    ['/maplibre-worker.js', [files['maplibre-worker.js'].bytes, 'text/javascript; charset=utf-8']],
  ]);
  const server = createServer((request, response) => {
    const route = routeFor(request.method, request.url, request.headers.host, port);
    if (requests.length >= 128) { response.writeHead(429); response.end(); return; }
    requests.push({ route: route ?? 'unlisted', status: route ? 200 : 404 });
    if (!route) { response.writeHead(404); response.end(); return; }
    const [bytes, type] = table.get(route);
    response.writeHead(200, { 'Content-Type': type, 'Content-Length': bytes.length, 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' }); response.end(bytes);
  });
  await new Promise((resolveListen, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolveListen); });
  port = server.address().port;
  return { port, requests, close: () => new Promise(resolveClose => { server.close(resolveClose); server.closeAllConnections(); }) };
}

export async function runHarness({ performanceMode = false } = {}) {
  if (process.platform !== 'darwin') throw failure('MACOS_REQUIRED');
  const root = await mkdtemp('/tmp/tabularis-v1-');
  const log = `/tmp/${performanceMode ? 'tabularis-x1-spatial-performance' : 'tabularis-v1'}-${randomUUID()}`;
  let server, native, compile;
  let before = {};
  try {
    before = await productionAssets();
    const fixture = await buildFixture(root);
    const executable = join(root, 'WebViewHarness');
    compile = await boundedProcess('/usr/bin/xcrun', ['swiftc', join(HERE, 'WebViewHarness.swift'), '-framework', 'Cocoa', '-framework', 'WebKit', '-module-cache-path', join(root, 'module-cache'), '-Xcc', `-fmodules-cache-path=${join(root, 'clang-cache')}`, '-o', executable], { cwd: root });
    await writeFile(`${log}-compile.log`, Buffer.concat([compile.stdout, compile.stderr]), { mode: 0o600 });
    if (compile.code !== 0) throw failure('SWIFT_COMPILE_FAILED');
    server = await serve(before, fixture.bytes, { performanceMode });
    native = await boundedProcess(executable, [`http://127.0.0.1:${server.port}/index.html`, root], { cwd: root });
    await writeFile(`${log}-native.stdout.log`, native.stdout, { mode: 0o600 });
    await writeFile(`${log}-native.stderr.log`, native.stderr, { mode: 0o600 });
    const report = validateReport(native.stdout, { performanceMode });
    const after = await productionAssets();
    const drift = JSON.stringify(assetEvidence(before)) !== JSON.stringify(assetEvidence(after));
    const evidence = { ...report, production_assets: assetEvidence(before), assets_drift: drift, react: fixture.react, maplibre_version_in_bundle: before['index.js'].bytes.includes(Buffer.from('6.11.2')), node: process.version, architecture: process.arch, requests: server.requests, native_exit_code: native.code, logs: log };
    await writeFile(`${log}-result.json`, JSON.stringify(evidence, null, 2) + '\n', { mode: 0o600 });
    if (native.code !== 0 || report.pass !== true) throw Object.assign(failure(report.code ?? 'NATIVE_VERIFICATION_FAILED'), { evidence });
    if (drift || server.requests.some(request => request.status !== 200) || !evidence.maplibre_version_in_bundle) throw Object.assign(failure('ASSET_OR_REQUEST_DRIFT'), { evidence });
    return evidence;
  } catch (error) {
    if (error.result) {
      await writeFile(`${log}-process.stdout.log`, error.result.stdout, { mode: 0o600 });
      await writeFile(`${log}-process.stderr.log`, error.result.stderr, { mode: 0o600 });
    }
    await writeFile(`${log}-failure.json`, JSON.stringify({ code: error.code ?? 'HARNESS_FAILED', evidence: EVIDENCE, logs: log, production_assets: assetEvidence(before) }, null, 2) + '\n', { mode: 0o600 });
    throw Object.assign(error, { logs: log });
  } finally {
    await server?.close();
    await rm(root, { recursive: true, force: true });
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const performanceMode = process.argv.length === 3 && process.argv[2] === '--performance';
  if (process.argv.length !== 2 && !performanceMode) { process.stderr.write('INVALID_ARGUMENT\n'); process.exitCode = 1; }
  else {
    try { process.stdout.write(JSON.stringify(await runHarness({ performanceMode }), null, 2) + '\n'); }
    catch (error) { process.stderr.write(JSON.stringify({ code: error.code ?? 'HARNESS_FAILED', evidence: EVIDENCE, logs: error.logs ?? null }) + '\n'); process.exitCode = 1; }
  }
}
