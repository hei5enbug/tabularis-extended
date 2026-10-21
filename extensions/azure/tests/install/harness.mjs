import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { absolute, regularWithin } from '../../scripts/package/files.mjs';
import { fail } from '../../scripts/package/errors.mjs';
import { fixtureEnvironment, nativeNames, pythonExecutable } from './platform-options.mjs';

export const POISONING_KEYS = ['NODE_OPTIONS', 'NODE_PATH', 'NODE_TLS_REJECT_UNAUTHORIZED', 'NODE_EXTRA_CA_CERTS', 'NODE_USE_SYSTEM_CA', 'SSL_CERT_FILE', 'SSL_CERT_DIR', 'OPENSSL_CONF'];

export function prepareInstallSmoke({ bundle, platform = process.platform, arch = process.arch, python = pythonExecutable() }) {
  absolute(bundle);
  absolute(python);
  const names = nativeNames(platform, arch);
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-install-한글 space-'));
  fs.chmodSync(root, 0o700);
  const installed = path.join(root, 'bundle');
  const profile = path.join(root, 'profile');
  fs.mkdirSync(installed, { mode: 0o700 });
  fs.mkdirSync(profile, { mode: 0o700 });
  let disposed = false;
  return {
    root, installed, profile, platform, arch,
    extraction: { executable: python, args: ['-c', PYTHON_EXTRACT, bundle, installed] },
    launch() {
      if (disposed) fail('INVALID_FIXTURE');
      const runtime = regularWithin(installed, names.runtime);
      const executable = regularWithin(installed, names.executable);
      return { executable, runtime, args: [], cwd: installed, env: { ...fixtureEnvironment(profile), NODE_OPTIONS: '--invalid-canary', NODE_PATH: 'secret-canary', NODE_TLS_REJECT_UNAUTHORIZED: '0', NODE_EXTRA_CA_CERTS: 'secret-canary', NODE_USE_SYSTEM_CA: '1', SSL_CERT_FILE: 'secret-canary', SSL_CERT_DIR: 'secret-canary', OPENSSL_CONF: 'secret-canary' } };
    },
    dispose() {
      if (!disposed) fs.rmSync(root, { recursive: true, force: true });
      disposed = true;
    },
  };
}

const PYTHON_EXTRACT = `import os, pathlib, stat, sys, zipfile
with zipfile.ZipFile(sys.argv[1]) as archive:
    seen = set()
    total = 0
    for item in archive.infolist():
        name = item.filename
        parts = name.split('/')
        mode = item.external_attr >> 16
        if name in seen or name.startswith('/') or any(p in ('', '.', '..') for p in parts) or any(c in name for c in ('\\\\', ':', '\\x00')) or not stat.S_ISREG(mode):
            raise ValueError('unsafe archive')
        seen.add(name)
        total += item.file_size
        if len(seen) > 50000 or total > 512*1024*1024 or item.file_size > 256*1024*1024:
            raise ValueError('archive limit')
        target = pathlib.Path(sys.argv[2]).joinpath(*parts)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(archive.read(item))
        os.chmod(target, mode & 0o777)
`;

export async function runInstallSmoke(prepared, { input = '', timeoutMs = 60000 } = {}) {
  const { executable, args, cwd, env } = prepared.launch();
  return new Promise((resolve, reject) => {
    const child = spawn(executable, args, { cwd, env, shell: false, stdio: ['pipe', 'pipe', 'pipe'] });
    const stdout = [];
    const stderr = [];
    let bytes = 0;
    const timer = setTimeout(() => child.kill('SIGKILL'), timeoutMs);
    child.stdout.on('data', data => {
      bytes += data.length;
      if (bytes > 8 * 1024 * 1024) child.kill('SIGKILL');
      else stdout.push(data);
    });
    child.stderr.on('data', data => {
      bytes += data.length;
      if (bytes > 8 * 1024 * 1024) child.kill('SIGKILL');
      else stderr.push(data);
    });
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('close', (code, signal) => { clearTimeout(timer); resolve({ code, signal, stdout: Buffer.concat(stdout).toString('utf8'), stderr: Buffer.concat(stderr).toString('utf8') }); });
    child.stdin.on('error', () => {});
    child.stdin.end(input);
  });
}
