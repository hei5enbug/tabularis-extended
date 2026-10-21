import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repositories = [
  ['host', 'tabularis-app-source', 'tabularis', 'b78a40946f072f6b8f2f1a4c80c1e04ff9b54cd4'],
  ['postgresql', 'tabularis-postgresql-plugin', 'tabularis-postgresql-plugin', '963f358e5de037014db37337db3dbaea560aee77'],
  ['sqlserver', 'tabularis-sqlserver-plugin', 'tabularis-sqlserver-plugin', '58c47b374ddc472457d8c946572f4bc8fd930978'],
];
const args = process.argv.slice(2);
if (args.length !== 2 || args[0] !== '--source-parent' || !path.isAbsolute(args[1])) {
  throw new Error('Usage: node scripts/export-upstreams.mjs --source-parent ABSOLUTE_PATH');
}
const records = repositories.map(([id, directory, upstream, base]) => {
  const cwd = path.join(args[1], directory);
  const git = parameters => execFileSync('git', parameters, { cwd, maxBuffer: 32 * 1024 * 1024 });
  if (git(['status', '--porcelain', '--untracked-files=no']).length) throw new Error(`Source has uncommitted changes: ${id}`);
  const commit = git(['rev-parse', 'HEAD']).toString().trim();
  const tree = git(['rev-parse', 'HEAD^{tree}']).toString().trim();
  const patch = git(['diff', '--binary', '--full-index', base, commit, '--']);
  const entry = { id, directory, url: `https://github.com/TabularisDB/${upstream}.git`,
    base_commit: base, source_commit: commit, tree, patch: `patches/${id}.patch`,
    patch_sha256: createHash('sha256').update(patch).digest('hex') };
  const uiPackagePath = path.join(cwd, 'ui', 'package.json');
  if (id === 'sqlserver' && fs.existsSync(uiPackagePath)) {
    const uiPackage = JSON.parse(fs.readFileSync(uiPackagePath, 'utf8'));
    if (uiPackage.devDependencies?.['@tabularis/plugin-api']
        === 'link:../../tabularis-spatial/build-support/sdk/packages/plugin-api') {
      entry.patch_adaptations = [
        'The SQL Server UI uses the Git-tracked Spatial SDK instead of a sibling host SDK.',
      ];
    }
  }
  const portableFixturePath = path.join(cwd, id === 'host'
    ? 'tests/service/support/native-process.mjs' : 'tests/spatial/export_live.rs');
  if (['host', 'postgresql'].includes(id) && fs.existsSync(portableFixturePath)) {
    const fixture = fs.readFileSync(portableFixturePath, 'utf8');
    if ((id === 'host' && fixture.includes('os.tmpdir()'))
        || (id === 'postgresql' && fixture.includes('Command::new("psql")'))) {
      entry.patch_adaptations = [
        'Integration fixtures use OS temporary directories and PATH tools instead of machine-specific paths.',
      ];
    }
  }
  return { patch, entry };
});
fs.mkdirSync(path.join(root, 'integration', 'patches'), { recursive: true });
for (const { patch, entry } of records) fs.writeFileSync(path.join(root, 'integration', entry.patch), patch);
fs.writeFileSync(path.join(root, 'integration', 'upstreams.json'), `${JSON.stringify({ format: 1,
  host_version: '0.26.1-spatial.1', repositories: records.map(record => record.entry) }, null, 2)}\n`);
process.stdout.write('Exported three verified source patches.\n');
