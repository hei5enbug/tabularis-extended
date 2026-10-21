import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import test from 'node:test';
import { fixture, packageAndAudit, attempt, write } from './fixtures.mjs';

function boundary(f, limit, exact) {
  return {
    exact: attempt(f, { output: path.join(f.root, 'exact.zip'), limits: { [limit]: exact } }),
    above: attempt(f, { output: path.join(f.root, 'above.zip'), limits: { [limit]: exact + 1 } }),
    below: attempt(f, { output: path.join(f.root, 'below.zip'), limits: { [limit]: exact - 1 } }),
  };
}

for (const limit of ['bytes', 'files', 'zipBytes']) {
  test(`${limit} 정확한 상한과 한 단위 차이를 검사하면 경계에서만 거부한다`, t => {
    // given
    const f = fixture(t);
    const baseline = packageAndAudit(f);
    const exact = limit === 'bytes' ? Object.values(baseline.files).reduce((sum, file) => sum + file.bytes, 0) : limit === 'files' ? baseline.result.files : baseline.result.bytes;
    // when
    const actual = boundary(f, limit, exact);
    // then
    assert.equal(actual.exact.error, null);
    assert.equal(actual.above.error, null);
    assert.equal(actual.exact.result.sha256, baseline.result.sha256);
    assert.equal(actual.above.result.sha256, baseline.result.sha256);
    assert.equal(actual.below.error, limit === 'zipBytes' ? 'ZIP_TOO_LARGE' : 'PACKAGE_TOO_LARGE');
    assert.equal(fs.existsSync(path.join(f.root, 'below.zip')), false);
  });
}

test('asset file 상한이 정확한 크기이면 허용하고 한 byte 적으면 거부한다', t => {
  // given
  const f = fixture(t);
  write(f.source, 'ui/dist/index.js', Buffer.alloc(4096, 65));
  // when
  const actual = boundary(f, 'fileBytes', 4096);
  // then
  assert.equal(actual.exact.error, null);
  assert.equal(actual.above.error, null);
  assert.equal(actual.below.error, 'PACKAGE_TOO_LARGE');
});

test('license 한 MiB의 정확한 기본 상한은 변경 없이 허용된다', t => {
  // given
  const f = fixture(t);
  write(f.maplibre, 'LICENSE', Buffer.alloc(1024 * 1024, 65));
  // when
  const actual = attempt(f);
  // then
  assert.equal(actual.error, null);
  assert.equal(actual.result.license_packages, 1);
});

test('generated release JSON도 낮춘 JSON 상한을 넘어갈 수 없다', t => {
  // given
  const f = fixture(t);
  // when
  const actual = attempt(f, { limits: { jsonBytes: 1000 } });
  // then
  assert.equal(actual.error, 'PACKAGE_TOO_LARGE');
  assert.equal(fs.existsSync(f.output), false);
});

test('UTF8이 아닌 manifest는 replacement 문자로 보정하지 않는다', t => {
  // given
  const f = fixture(t);
  const file = path.join(f.source, 'manifest.json');
  const bytes = fs.readFileSync(file);
  const index = bytes.indexOf(Buffer.from('오프라인'));
  bytes[index] = 255;
  fs.writeFileSync(file, bytes);
  // when
  const actual = attempt(f);
  // then
  assert.equal(actual.error, 'INVALID_JSON');
  assert.equal(fs.existsSync(f.output), false);
});

test('installed license root의 항목 수가 낮춘 file 상한을 넘으면 조기 거부한다', t => {
  // given
  const f = fixture(t);
  for (let index = 0; index < 20; index += 1) write(f.maplibre, `not-input-${index}`, 'ignored');
  // when
  const actual = attempt(f, { limits: { files: 20 } });
  // then
  assert.equal(actual.error, 'PACKAGE_TOO_LARGE');
  assert.equal(fs.existsSync(f.output), false);
});
