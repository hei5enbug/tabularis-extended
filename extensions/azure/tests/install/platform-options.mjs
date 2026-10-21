import os from 'node:os';
import path from 'node:path';
import { NODE_PINS, NODE_VERSION } from '../../scripts/package/pins.mjs';

const INPUT_KEYS = ['TABULARIS_C3B_TEST_LAUNCHER', 'TABULARIS_C3B_TEST_NODE_ARCHIVE', 'TABULARIS_C3B_TEST_PYTHON', 'TABULARIS_C3B_TEST_NODE_CACHE'];
const MAC_CACHE = '/tmp/tabularis-runtime-cache/node-v24.21.0';
const DEBUG_TARGET = '/tmp/tabularis-c3b-launcher-target';
const RELEASE_TARGET = '/tmp/tabularis-c3b2b1-launcher-target';

function unavailable() { const error = new Error('CAPABILITY_UNAVAILABLE'); error.code = 'CAPABILITY_UNAVAILABLE'; throw error; }
function inputObject(env) {
  if (!env || typeof env !== 'object' || Array.isArray(env) || Object.keys(env).some(key => ![...INPUT_KEYS, 'SystemRoot'].includes(key))) unavailable();
  return env;
}
function absoluteInput(value, platform) {
  if (typeof value !== 'string' || !value || value.includes('\0') || !(platform === 'win32' ? path.win32 : path.posix).isAbsolute(value)) unavailable();
  return value;
}

export function selectTargets(platform, hostArch) {
  if (!NODE_PINS[`${platform}-${hostArch}`]) unavailable();
  const arches = platform === 'darwin' && hostArch === 'arm64' ? ['arm64', 'x64'] : [hostArch];
  return arches.map(arch => ({ platform, arch }));
}

export function readTestEnvironment(platform = process.platform) {
  const env = {};
  for (const key of INPUT_KEYS) if (process.env[key] !== undefined) env[key] = process.env[key];
  if (platform === 'win32' && process.env.SystemRoot !== undefined) env.SystemRoot = process.env.SystemRoot;
  return env;
}

export function nativeNames(platform, arch) {
  if (!NODE_PINS[`${platform}-${arch}`]) unavailable();
  return {
    executable: platform === 'win32' ? 'cosmos-nosql.exe' : 'cosmos-nosql',
    runtime: platform === 'win32' ? 'runtime/node.exe' : 'runtime/bin/node',
    archive: `node-v${NODE_VERSION}-${platform === 'win32' ? 'win' : platform}-${arch}.${platform === 'win32' ? 'zip' : 'tar.gz'}`,
    pin: NODE_PINS[`${platform}-${arch}`],
  };
}

export function pythonExecutable(platform = process.platform, env = readTestEnvironment(platform)) {
  inputObject(env);
  return absoluteInput(env.TABULARIS_C3B_TEST_PYTHON ?? (platform === 'win32' ? undefined : '/usr/bin/python3'), platform);
}

export function runtimeCache(platform = process.platform, env = readTestEnvironment(platform)) {
  inputObject(env);
  return absoluteInput(env.TABULARIS_C3B_TEST_NODE_CACHE ?? (platform === 'darwin' ? MAC_CACHE : path.join(os.tmpdir(), 'tabularis-runtime-cache', `node-v${NODE_VERSION}`)), platform);
}

export function fixturePaths({ platform = process.platform, arch = process.arch, env = readTestEnvironment(platform), production = false } = {}) {
  const names = nativeNames(platform, arch);
  inputObject(env);
  const paths = platform === 'win32' ? path.win32 : path.posix;
  const cache = runtimeCache(platform, env);
  const triple = arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin';
  const launcher = absoluteInput(env.TABULARIS_C3B_TEST_LAUNCHER ?? (platform === 'darwin' ? production
    ? paths.join(RELEASE_TARGET, triple, 'release', 'tabularis-azure-launcher')
    : paths.join(DEBUG_TARGET, ...(arch === 'x64' ? [triple] : []), 'debug', 'tabularis-azure-launcher') : undefined), platform);
  const runtimeArchive = absoluteInput(env.TABULARIS_C3B_TEST_NODE_ARCHIVE ?? (platform === 'darwin' ? paths.join(cache, names.archive) : undefined), platform);
  const tar = platform === 'win32' ? paths.join(absoluteInput(env.SystemRoot, platform), 'System32', 'tar.exe') : platform === 'darwin' ? '/usr/bin/tar' : '/usr/bin/bsdtar';
  return { platform, arch, launcher, runtimeArchive, python: pythonExecutable(platform, env), tar };
}

export function fixtureEnvironment(profile, platform = process.platform, env = readTestEnvironment(platform)) {
  inputObject(env);
  return { PATH: '', HOME: profile, USERPROFILE: profile, ...(platform === 'win32' ? { SystemRoot: absoluteInput(env.SystemRoot, platform) } : {}) };
}

export function directoryLinkType(platform = process.platform) { return platform === 'win32' ? 'junction' : 'dir'; }
