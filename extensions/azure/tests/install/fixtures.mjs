import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { sha256 } from '../../scripts/package/files.mjs';
import { NODE_VERSION } from '../../scripts/package/pins.mjs';
import { defaultTar } from '../../scripts/package/runtime.mjs';
import { directoryLinkType, fixturePaths, pythonExecutable, runtimeCache } from './platform-options.mjs';

export { pythonExecutable } from './platform-options.mjs';

export const ACCEPTED_MANIFEST = {
  id: 'cosmos-nosql', name: 'cosmos-nosql', kind: 'driver', engine: 'cosmos-nosql', version: '0.1.0',
  description: 'Azure Cosmos DB NoSQL 문서 드라이버', executable: 'cosmos-nosql', connection_metadata: true,
  service_protocol: 1, min_runtime_version: '0.26.0', paradigms: ['document'],
  capabilities: { schemas: false, views: false, routines: false, file_based: false, identifier_quote: '"', alter_primary_key: false, manage_tables: false, explain: false, documents_v1: true, query_page_v1: true, cancel_v1: true, metadata_discovery: true, readonly: false },
  data_types: [{ name: 'JSON', category: 'json', requires_length: false, requires_precision: false, default_length: null }], settings: [],
  ui_assets: [{ path: 'ui/dist/style.css', mime: 'text/css' }],
  ui_extensions: ['connection-modal.extra_fields', 'data-grid.toolbar.actions', 'row-edit-modal.footer.before', 'row-editor-sidebar.header.actions', 'settings.plugin.actions'].map(slot => ({ slot, module: 'ui/dist/index.js', ...(slot === 'settings.plugin.actions' ? {} : { driver: 'cosmos-nosql' }) })),
};

export function write(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, typeof value === 'object' && !Buffer.isBuffer(value) ? JSON.stringify(value) : value);
}

function pkg(root, name, version, extra = {}, code = `export default ${JSON.stringify(version)};`) {
  write(path.join(root, 'package.json'), { name, version, type: 'module', exports: './index.js', ...extra });
  write(path.join(root, 'index.js'), code);
  write(path.join(root, 'LICENSE'), `Synthetic fixture license for ${name} ${version}.\n`);
  return root;
}

export function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-package-fixture-한글 space-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const source = path.join(root, 'source');
  write(path.join(source, 'manifest.json'), ACCEPTED_MANIFEST);
  write(path.join(source, 'LICENSE'), fs.readFileSync(new URL('../../LICENSE', import.meta.url)));
  write(path.join(source, 'package.json'), { name: 'fixture-driver', version: '0.1.0', type: 'module', dependencies: { '@azure/cosmos': '4.10.1', '@tabularis/service-contracts': '1.0.0', first: '1', second: '1', cycle: '1' }, optionalDependencies: { absent: '1', installedOptional: '1' }, devDependencies: { development: '1' } });
  write(path.join(source, 'dist', 'index.js'), `import cosmos from '@azure/cosmos';\nimport { schema } from '@tabularis/service-contracts';\nimport first from 'first';\nimport second from 'second';\nimport cycle from 'cycle';\nimport { helper } from './helper.js';\nprocess.stdout.write(JSON.stringify({cosmos,schema,first,second,cycle,helper,argv:process.argv.slice(2),node:process.version,envAbsent:['NODE_OPTIONS','NODE_PATH','NODE_TLS_REJECT_UNAUTHORIZED','NODE_EXTRA_CA_CERTS','NODE_USE_SYSTEM_CA','SSL_CERT_FILE','SSL_CERT_DIR','OPENSSL_CONF'].every(k=>process.env[k]===undefined)})+'\\n');\nprocess.stdin.setEncoding('utf8');process.stdin.on('data',chunk=>process.stdout.write(chunk));\n`);
  write(path.join(source, 'dist', 'helper.js'), 'export const helper = "relative import works";');
  write(path.join(source, 'ui', 'dist', 'index.js'), 'export default function FixtureUI() {}');
  write(path.join(source, 'ui', 'dist', 'azure-ui.css'), 'body { color: #123456; }');
  pkg(path.join(source, 'node_modules', '@azure', 'cosmos'), '@azure/cosmos', '4.10.1');
  const contracts = pkg(path.join(root, 'workspace', 'service-contracts'), '@tabularis/service-contracts', '1.0.0', { exports: { '.': { import: './dist/index.js' } } });
  write(path.join(contracts, 'dist', 'index.js'), `import fs from 'node:fs';export const schema=JSON.parse(fs.readFileSync(new URL('../schemas/fixture.json',import.meta.url),'utf8')).title;`);
  write(path.join(contracts, 'schemas', 'fixture.json'), { title: 'materialized contract' });
  fs.mkdirSync(path.join(source, 'node_modules', '@tabularis'), { recursive: true });
  fs.symlinkSync(contracts, path.join(source, 'node_modules', '@tabularis', 'service-contracts'), directoryLinkType());
  const first = pkg(path.join(source, 'node_modules', 'first'), 'first', '1.0.0', { dependencies: { shared: '1' }, peerDependencies: { peer: '1' } }, `import shared from 'shared';import peer from 'peer';export default shared+':'+peer;`);
  pkg(path.join(first, 'node_modules', 'shared'), 'shared', '1.0.0');
  pkg(path.join(first, 'node_modules', 'peer'), 'peer', '1.0.0');
  const second = pkg(path.join(source, 'node_modules', 'second'), 'second', '1.0.0', { dependencies: { shared: '2' }, peerDependencies: { missingPeer: '1' }, peerDependenciesMeta: { missingPeer: { optional: true } } }, `import shared from 'shared';export default shared;`);
  pkg(path.join(second, 'node_modules', 'shared'), 'shared', '2.0.0');
  const cycle = pkg(path.join(source, 'node_modules', 'cycle'), 'cycle', '1.0.0', { dependencies: { leaf: '1' } }, `import { label } from 'leaf';export default label;`);
  const leaf = pkg(path.join(cycle, 'node_modules', 'leaf'), 'leaf', '1.0.0', { dependencies: { cycle: '1' } }, 'export const label = "bounded cycle";');
  fs.mkdirSync(path.join(leaf, 'node_modules'), { recursive: true });
  fs.symlinkSync(cycle, path.join(leaf, 'node_modules', 'cycle'), directoryLinkType());
  pkg(path.join(source, 'node_modules', 'development'), 'development', '1.0.0');
  pkg(path.join(source, 'node_modules', 'installedOptional'), 'installedOptional', '1.0.0');
  write(path.join(first, '.env'), 'SYNTHETIC_SECRET_CANARY=hidden');
  write(path.join(first, 'tests', 'state.json'), { secret: 'SYNTHETIC_SECRET_CANARY' });
  const launcher = path.join(root, 'launcher');
  write(launcher, 'synthetic launcher');
  const archiveRoot = `node-v${NODE_VERSION}-darwin-arm64`;
  write(path.join(root, 'archive', archiveRoot, 'bin', 'node'), Buffer.from([0xcf, 0xfa, 0xed, 0xfe, 1]));
  write(path.join(root, 'archive', archiveRoot, 'LICENSE'), 'Synthetic runtime license.');
  const runtimeArchive = path.join(root, 'runtime.tar.gz');
  const tarExecutable = defaultTar();
  const tar = spawnSync(tarExecutable, ['-czf', '../runtime.tar.gz', archiveRoot], { cwd: path.join(root, 'archive'), shell: false });
  if (tar.status !== 0) throw new Error('fixture archive unavailable', { cause: tar.error ?? tar.stderr?.toString('utf8') });
  const expectedRuntimePin = sha256(fs.readFileSync(runtimeArchive));
  return { root, source, launcher, contracts, options: { platform: 'darwin', arch: 'arm64', source, launcher, runtimeArchive, expectedRuntimePin, output: path.join(root, 'bundle.zip'), tar: tarExecutable } };
}

export function python(args) {
  return spawnSync(pythonExecutable(), args, { shell: false, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, timeout: 60000 });
}

export function inspectArchive(file) {
  const result = python(['-c', `import json, sys, zipfile
with zipfile.ZipFile(sys.argv[1]) as z:
 print(json.dumps({'bad':z.testzip(),'entries':[{'name':i.filename,'mode':i.external_attr>>16,'compression':i.compress_type,'time':i.date_time,'bytes':i.file_size} for i in z.infolist()],'manifest':json.loads(z.read('.tabularium')),'release':json.loads(z.read('release.json')),'notices':json.loads(z.read('licenses/third-party-notices.json'))}))`, file]);
  if (result.status !== 0) throw new Error('independent ZIP verification failed');
  return JSON.parse(result.stdout);
}

export function capture(action) {
  try { return { value: action(), error: null }; } catch (error) { return { value: null, error: error.code ?? error.message }; }
}

export const NODE_CACHE = runtimeCache();

export function actualOptions(value, arch) {
  return { ...value.options, ...fixturePaths({ arch }), expectedRuntimePin: undefined };
}
