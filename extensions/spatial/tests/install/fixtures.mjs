import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { packageBundle } from '../../scripts/package/index.mjs';

export const CLI = fileURLToPath(new URL('../../scripts/package/cli.mjs', import.meta.url));
export function directoryLinkType(platform = process.platform) { return platform === 'win32' ? 'junction' : 'dir'; }
export function auditPython(platform = process.platform, env = { TABULARIS_S3_TEST_PYTHON: process.env.TABULARIS_S3_TEST_PYTHON }) {
  const executable = env.TABULARIS_S3_TEST_PYTHON ?? (platform === 'win32' ? undefined : '/usr/bin/python3');
  if (typeof executable !== 'string' || !executable || executable.includes('\0') || !(platform === 'win32' ? path.win32 : path.posix).isAbsolute(executable)) throw new Error('CAPABILITY_UNAVAILABLE');
  return executable;
}
export const MANIFEST = Object.freeze({
  id: 'spatial', name: 'spatial', kind: 'driver', version: '0.1.0', description: '오프라인 공간 지도', service_protocol: 1,
  ui_assets: [{ path: 'ui/dist/style.css', mime: 'text/css' }, { path: 'ui/dist/maplibre-worker.js', mime: 'text/javascript' }],
  min_runtime_version: '0.26.0', ui_extensions: [
    { slot: 'data-grid.toolbar.actions', module: 'ui/dist/index.js' },
    { slot: 'app.map.renderer', module: 'ui/dist/index.js' },
  ],
});

export function write(root, name, bytes) {
  const file = path.join(root, name);
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  fs.writeFileSync(file, bytes, { mode: 0o600 });
  return file;
}

export function json(root, name, value) {
  return write(root, name, `${JSON.stringify(value)}\n`);
}

export function addPackage(root, name, version = '1.0.0', extra = {}, license = `원본 license ${name}\n`) {
  const location = path.join(root, 'node_modules', ...name.split('/'));
  json(location, 'package.json', { name, version, ...extra });
  if (license !== null) write(location, 'LICENSE', license);
  write(location, 'private.secret', 'not-to-be-packaged');
  write(location, 'index.js', 'throw Error("code must not be bundled");');
  return location;
}

export function fixture(t, extra = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-s3-test-공간 ')));
  fs.chmodSync(root, 0o700);
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const source = path.join(root, 'source');
  json(source, 'manifest.json', structuredClone(MANIFEST));
  write(source, 'LICENSE', '원본 root Apache license\n');
  json(source, 'ui/package.json', { name: 'synthetic-ui', version: '0.1.0', devDependencies: { 'maplibre-gl': '6.11.2' } });
  write(source, 'ui/dist/index.js', 'window.synthetic = "한글";\n');
  write(source, 'ui/dist/style.css', 'body { color: #123456; }\n');
  write(source, 'ui/dist/maplibre-worker.js', 'self.onmessage = () => {};\n');
  write(source, 'extra.secret', 'source-secret-canary');
  write(source, 'core/private.rs', 'not source input');
  write(source, 'ui/dist/extra.secret', 'asset-secret-canary');
  const maplibre = addPackage(path.join(source, 'ui'), 'maplibre-gl', '6.11.2', extra);
  return { root, source, maplibre, output: path.join(root, 'map.zip') };
}

export function attempt(f, options = {}) {
  try { return { result: packageBundle({ source: f.source, output: f.output, ...options }), error: null }; } catch (error) {
    return { result: null, error: error.code, message: error.message, remaining: fs.readdirSync(f.root).filter(name => name.startsWith('.tabularis-spatial-package-')) };
  }
}

export function audit(file) {
  const python = auditPython();
  const script = `import sys,json,zipfile,hashlib,base64\nwith zipfile.ZipFile(sys.argv[1]) as z:\n assert z.testzip() is None\n out={i.filename:{'bytes':len(z.read(i)),'sha256':hashlib.sha256(z.read(i)).hexdigest(),'data':base64.b64encode(z.read(i)).decode(),'mode':i.external_attr>>16,'method':i.compress_type,'timestamp':list(i.date_time)} for i in z.infolist()}\n print(json.dumps(out))`;
  const child = spawnSync(python, ['-c', script, file], { encoding: 'utf8', timeout: 10000, maxBuffer: 4 * 1024 * 1024 });
  if (child.status !== 0) throw new Error('independent ZIP audit failed');
  return JSON.parse(child.stdout);
}

export function decode(entry) {
  return Buffer.from(entry.data, 'base64');
}

export function packageAndAudit(f) {
  const result = packageBundle({ source: f.source, output: f.output });
  const files = audit(f.output);
  return { result, files, release: JSON.parse(decode(files['release.json'])), notices: JSON.parse(decode(files['licenses/third-party-notices.json'])) };
}

export function runCLI(args, input = 'stdin-secret-canary') {
  return spawnSync(process.execPath, [CLI, ...args], { input, encoding: 'utf8', timeout: 10000, maxBuffer: 1024 * 1024 });
}
