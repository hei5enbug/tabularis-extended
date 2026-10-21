import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { POISONING_KEYS, prepareInstallSmoke, runInstallSmoke } from './harness.mjs';
import { hashFile } from '../../scripts/package/files.mjs';
import { NODE_PINS } from '../../scripts/package/pins.mjs';
import { fixtureEnvironment, fixturePaths, selectTargets } from './platform-options.mjs';

const source = fileURLToPath(new URL('../../', import.meta.url));
const CANARY = 'production-protocol-secret-canary';
const FRAME_LIMIT = 8 * 1024 * 1024;

export function productionOptions(arch, { platform = process.platform, hostArch = process.arch, env } = {}) {
  if (!selectTargets(platform, hostArch).some(target => target.arch === arch)) throw new Error('CAPABILITY_UNAVAILABLE');
  const paths = fixturePaths({ platform, arch, env, production: true });
  const root = fs.mkdtempSync(path.join(os.tmpdir(), `tabularis-c3b2c-production-${arch}-`));
  fs.chmodSync(root, 0o700);
  const profile = path.join(root, 'cli-profile');
  fs.mkdirSync(profile, { mode: 0o700 });
  return {
    root, profile, ...paths, source,
    output: path.join(root, `cosmos-nosql-0.1.0-${platform}-${arch}.zip`),
  };
}

function environment(profile) { return fixtureEnvironment(profile); }

function execute(executable, args, { cwd = source, env, timeout = 60_000 } = {}) {
  return spawnSync(executable, args, { cwd, env, shell: false, encoding: 'utf8', timeout, maxBuffer: FRAME_LIMIT });
}

function packageCLI(options) {
  const args = ['scripts/package/cli.mjs', '--platform', options.platform, '--arch', options.arch, '--source', options.source,
    '--launcher', options.launcher, '--runtime-archive', options.runtimeArchive, '--output', options.output, '--tar', options.tar];
  return execute(process.execPath, args, { env: environment(options.profile) });
}

function auditArchive(options) {
  const driverHash = hashFile(path.join(source, 'dist/index.js'));
  const licenseHash = hashFile(path.join(source, 'LICENSE'));
  const result = execute(options.python, ['-c', AUDIT_ARCHIVE, options.output, options.platform, options.arch,
    NODE_PINS[`${options.platform}-${options.arch}`], driverHash, licenseHash, fileURLToPath(new URL('../../../../', import.meta.url))], { env: environment(options.profile) });
  if (result.status !== 0 || result.stderr) throw new Error('Production archive audit failed.');
  return JSON.parse(result.stdout);
}

function frame(id, method, params) { return `${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`; }
function protocolInput(shutdown) {
  return frame('native', 'initialize', { settings: {}, service_protocol: 1 })
    + frame('wrong', 'initialize', { settings: {}, service_protocol: 2 })
    + frame('unknown', 'production_unknown', { value: CANARY })
    + (shutdown ? frame('shutdown', 'shutdown', {}) : '');
}

function responses(result) {
  if (result.stdout && !result.stdout.endsWith('\n')) throw new Error('Production stdout is not newline framed.');
  const parsed = result.stdout.trim() ? result.stdout.trim().split('\n').map(line => JSON.parse(line)) : [];
  return {
    ...result, responses: parsed, byId: Object.fromEntries(parsed.map(response => [response.id, response])),
    canaryPresent: `${result.stdout}${result.stderr}`.includes(CANARY) || `${result.stdout}${result.stderr}`.includes('secret-canary'),
  };
}

async function installedProduction(options) {
  const prepared = prepareInstallSmoke({ bundle: options.output, platform: options.platform, arch: options.arch, python: options.python });
  let actual;
  try {
    const extracted = execute(prepared.extraction.executable, prepared.extraction.args, { env: environment(prepared.profile) });
    if (extracted.status !== 0 || extracted.stderr) throw new Error('Production fixture extraction failed.');
    const config = prepared.launch();
    const version = execute(config.runtime, ['-v'], { cwd: prepared.installed, env: environment(prepared.profile) });
    const identity = execute(config.runtime, ['-p', 'JSON.stringify({version:process.version,arch:process.arch,platform:process.platform})'], { cwd: prepared.installed, env: environment(prepared.profile) });
    const eof = responses(await runInstallSmoke(prepared, { input: protocolInput(false), timeoutMs: 60_000 }));
    const shutdown = responses(await runInstallSmoke(prepared, { input: protocolInput(true), timeoutMs: 60_000 }));
    actual = {
      version, identity: { ...identity, data: identity.status === 0 ? JSON.parse(identity.stdout) : null }, eof, shutdown,
      path: config.env.PATH, isolatedProfile: config.env.HOME === prepared.profile && config.env.USERPROFILE === prepared.profile,
      poisonCount: POISONING_KEYS.filter(key => Object.hasOwn(config.env, key)).length,
      runtimeHash: hashFile(config.runtime), launcherHash: hashFile(config.executable), hostArch: process.arch,
    };
  } finally { prepared.dispose(); }
  return { ...actual, cleaned: !fs.existsSync(prepared.root) };
}

export async function productionSmoke(options) {
  const packaged = packageCLI(options);
  if (packaged.status !== 0 || packaged.stderr) {
    fs.writeFileSync(path.join(options.root, 'package-failure.json'), JSON.stringify(packaged, null, 2));
    throw new Error(`Production CLI packaging failed: ${packaged.stderr.trim() || 'unknown error'}`);
  }
  const metadata = JSON.parse(packaged.stdout);
  const originalHash = hashFile(options.output);
  const duplicate = packageCLI(options);
  const preservedHash = hashFile(options.output);
  const audit = auditArchive(options);
  const smoke = await installedProduction(options);
  const result = {
    root: options.root, output: options.output, outputMode: process.platform === 'win32' ? null : fs.statSync(options.output).mode & 0o777,
    rootMode: process.platform === 'win32' ? null : fs.statSync(options.root).mode & 0o777,
    outputRegular: fs.lstatSync(options.output).isFile() && !fs.lstatSync(options.output).isSymbolicLink(),
    rootDirectory: fs.lstatSync(options.root).isDirectory() && !fs.lstatSync(options.root).isSymbolicLink(), checksum: fs.readFileSync(`${options.output}.sha256`, 'utf8').trim(),
    metadata, originalHash, duplicate: { status: duplicate.status, stdout: duplicate.stdout, stderr: duplicate.stderr },
    preservedHash, audit, smoke,
  };
  fs.writeFileSync(path.join(options.root, 'release.json'), `${JSON.stringify(audit.release, null, 2)}\n`, { mode: 0o600 });
  fs.writeFileSync(path.join(options.root, 'production-report.json'), `${JSON.stringify(result, null, 2)}\n`, { mode: 0o600 });
  process.stdout.write(`Production artifact ${options.arch}: ${options.output} SHA256=${originalHash}\n`);
  return result;
}

const AUDIT_ARCHIVE = `import hashlib,json,pathlib,stat,sys,zipfile
bundle,platform,arch,pin,driver_hash,license_hash,workspace=sys.argv[1:]
with zipfile.ZipFile(bundle) as archive:
    entries=archive.infolist()
    names=[entry.filename for entry in entries]
    assert len(names)==len(set(names)) and len(names)<=50000
    assert archive.testzip() is None
    release=json.loads(archive.read('release.json'))
    ledger={item['path']:item for item in release['files']}
    assert len(ledger)==len(release['files']) and set(ledger)==set(names)-{'release.json'}
    assert release['node']=='24.21.0' and release['sdk']=='4.10.1'
    assert release['service_protocol']==1 and release['min_runtime_version']=='0.26.0'
    assert release['platform']==platform and release['arch']==arch and release['runtime_archive_sha256']==pin
    total=0
    for entry in entries:
        name=entry.filename
        assert not name.startswith('/') and all(part not in ('','.', '..') for part in name.split('/'))
        assert not any(char in name for char in ('\\\\', ':', '\\x00')) and stat.S_ISREG(entry.external_attr>>16)
        assert entry.file_size<=256*1024*1024
        data=archive.read(entry)
        total+=len(data)
        assert total<=512*1024*1024 and workspace.encode() not in data
        if name in ledger:
            assert ledger[name]['bytes']==len(data) and ledger[name]['sha256']==hashlib.sha256(data).hexdigest()
    manifest=json.loads(archive.read('.tabularium'))
    assert manifest['version']=='0.1.0' and manifest['min_runtime_version']=='0.26.0' and manifest['service_protocol']==1
    executable='cosmos-nosql.exe' if platform=='win32' else 'cosmos-nosql'
    runtime='runtime/node.exe' if platform=='win32' else 'runtime/bin/node'
    assert manifest['executable']==executable and manifest['paradigms']==['document']
    assert executable in names and runtime in names
    assert len(manifest['ui_extensions'])==5 and all(item['module']=='ui/dist/index.js' and ('driver' not in item if item['slot']=='settings.plugin.actions' else item.get('driver')=='cosmos-nosql') for item in manifest['ui_extensions'])
    assert {item['slot'] for item in manifest['ui_extensions']}=={'connection-modal.extra_fields','data-grid.toolbar.actions','row-edit-modal.footer.before','row-editor-sidebar.header.actions','settings.plugin.actions'}
    assert all(manifest['capabilities'][key] is True for key in ('documents_v1','query_page_v1','cancel_v1','metadata_discovery'))
    assert len(archive.read('ui/dist/index.js'))>0 and len(archive.read('ui/dist/style.css'))>0
    assert hashlib.sha256(archive.read('dist/driver.mjs')).hexdigest()==driver_hash
    assert hashlib.sha256(archive.read('LICENSE')).hexdigest()==license_hash
    sdk=json.loads(archive.read('node_modules/@azure/cosmos/package.json'))
    contracts=json.loads(archive.read('node_modules/@tabularis/service-contracts/package.json'))
    assert sdk['version']=='4.10.1' and contracts['name']=='@tabularis/service-contracts'
    assert b'Apache License' in archive.read('node_modules/@tabularis/service-contracts/LICENSE')
    assert len(archive.read('licenses/node/LICENSE'))>0
    notices=json.loads(archive.read('licenses/third-party-notices.json'))
    for item in notices:
        if item['name'] in ('priorityqueuejs','semaphore'):
            assert set(item)=={'name','version','sha256','license','license_source','template_source'}
            assert item['license']=='MIT' and item['license_source']=='embedded_readme'
            assert item['template_source']=='https://raw.githubusercontent.com/spdx/license-list-data/main/text/MIT.txt'
            prefix='licenses/packages/'+item['name']+'/'+item['version']+'/'+item['sha256']+'/'
            assert all(prefix+name in names for name in ('UPSTREAM-PACKAGE.json','UPSTREAM-README.md','MIT-TEMPLATE.txt','provenance.json'))
            provenance=json.loads(archive.read(prefix+'provenance.json'))
            assert provenance['name']==item['name'] and provenance['version']==item['version']
            assert provenance['original_license_file_present'] is False and provenance['template']['is_template'] is True
            for upstream in provenance['upstream_files'].values():
                data=archive.read(prefix+upstream['file'])
                assert len(data)==upstream['bytes'] and hashlib.sha256(data).hexdigest()==upstream['sha256']
            assert hashlib.sha256(archive.read(prefix+'MIT-TEMPLATE.txt')).hexdigest()==provenance['template']['sha256']
        else:
            assert set(item)=={'name','version','sha256'}
    assert any(item['name']=='node' and item['version']=='24.21.0' for item in notices)
    assert any(item['name']=='@azure/cosmos' and item['version']=='4.10.1' for item in notices)
    assert any(item['name']=='@tabularis/service-contracts' for item in notices)
    print(json.dumps({'files':len(entries),'recorded_files':len(ledger),'bytes':total,'symlinks':0,'workspace_paths':0,'sdk':sdk['version'],'contracts':contracts['name'],'root_license_sha256':license_hash,'driver_sha256':driver_hash,'release':release}))
`;
