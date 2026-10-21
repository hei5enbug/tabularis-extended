import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import { azureCliCommand } from '../../dist/runtime/compat.js';

for (const platform of ['darwin', 'linux']) {
  test(`${platform} CLI는 설치 위치를 가정하지 않고 PATH의 절대 경로에서 찾는다`, () => {
    // given
    const directory = path.posix.join(path.posix.sep, 'cli-fixture', 'bin');
    const expected = path.posix.join(directory, 'az');
    // when
    const actual = azureCliCommand(platform, directory, file => file === expected);
    // then
    assert.deepEqual(actual, { executable: expected, prefix: [] });
  });
}
test('빈 PATH와 상대 경로에서는 다른 설치 위치나 현재 디렉터리를 추측하지 않는다', () => {
  // given
  const searchPath = ':relative:';
  const checked = [];
  // when
  let error;
  try { azureCliCommand('darwin', searchPath, file => { checked.push(file); return true; }); } catch (value) { error = value; }
  // then
  assert.equal(error?.code, 'AUTH_REQUIRED');
  assert.deepEqual(checked, []);
});
test('Windows az.cmd는 PATH에서 찾은 배포본의 Python으로 셸 없이 실행한다', () => {
  // given
  const directory = path.win32.resolve('cli-fixture', 'wbin');
  const python = path.win32.resolve(directory, '..', 'python.exe');
  const files = new Set([path.win32.join(directory, 'az.cmd'), python]);
  // when
  const actual = azureCliCommand('win32', directory, file => files.has(file));
  // then
  assert.deepEqual(actual, { executable: python, prefix: ['-IBm', 'azure.cli'] });
});
