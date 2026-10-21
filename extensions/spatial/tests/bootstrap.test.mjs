import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { prepareSources, verifySources } from '../scripts/bootstrap.mjs';

const source = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
function fixture(t) {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-bootstrap-'));
  t.after(() => fs.rmSync(parent, { recursive: true, force: true }));
  const root = path.join(parent, 'tabularis-spatial');
  fs.mkdirSync(root);
  fs.cpSync(path.join(source, 'integration'), path.join(root, 'integration'), { recursive: true });
  return { parent, root };
}

test('패치 checksum이 다르면 checkout을 생성하기 전에 거부한다', t => {
  // given
  const { root, parent } = fixture(t);
  fs.appendFileSync(path.join(root, 'integration/patches/host.patch'), 'changed');
  // when
  let failure;
  try { prepareSources({ root }); } catch (error) { failure = error; }
  // then
  assert.match(failure?.message ?? '', /checksum mismatch/);
  assert.deepEqual(fs.readdirSync(parent), ['tabularis-spatial']);
});

test('기존 checkout의 파일은 bootstrap이 덮어쓰거나 삭제하지 않는다', t => {
  // given
  const { root, parent } = fixture(t);
  const existing = path.join(parent, 'tabularis-app-source');
  fs.mkdirSync(existing);
  fs.writeFileSync(path.join(existing, 'keep.txt'), 'keep');
  // when
  let failure;
  try { prepareSources({ root }); } catch (error) { failure = error; }
  // then
  assert.match(failure?.message ?? '', /Existing checkout is preserved/);
  assert.equal(fs.readFileSync(path.join(existing, 'keep.txt'), 'utf8'), 'keep');
  assert.equal(fs.existsSync(path.join(parent, '.tabularis-upstreams-bootstrap.lock')), false);
});

test('다른 bootstrap의 잠금이 있으면 잠금과 기존 파일을 보존한다', t => {
  // given
  const { root, parent } = fixture(t);
  const lock = path.join(parent, '.tabularis-upstreams-bootstrap.lock');
  fs.writeFileSync(lock, 'other owner', { flag: 'wx' });
  // when
  let failure;
  try { prepareSources({ root }); } catch (error) { failure = error; }
  // then
  assert.equal(failure?.code, 'EEXIST');
  assert.equal(fs.readFileSync(lock, 'utf8'), 'other owner');
  assert.equal(fs.existsSync(path.join(parent, 'tabularis-app-source')), false);
});

test('세 upstream 패치는 고정 revision과 checksum을 모두 가진다', () => {
  // given
  const root = source;
  // when
  const manifest = verifySources(root);
  // then
  assert.deepEqual(manifest.repositories.map(entry => entry.id), ['host', 'postgresql', 'sqlserver']);
  assert.equal(manifest.host_version, '0.26.1-spatial.1');
});
