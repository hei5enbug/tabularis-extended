import test from 'node:test';
import assert from 'node:assert/strict';
import { assertOwnerAcl, ownerAclAllowed } from '../../scripts/live/windows-acl.mjs';

const currentSid = 'S-1-5-21-100-200-300-1001';
const systemSid = 'S-1-5-18';
const administratorsSid = 'S-1-5-32-544';

function acl(overrides = {}) {
  return {
    currentSid,
    ownerSid: currentSid,
    daclPresent: true,
    aces: [
      { type: 'allow', sid: currentSid },
      { type: 'allow', sid: systemSid },
      { type: 'allow', sid: administratorsSid },
      { type: 'deny', sid: 'S-1-5-21-9-9-9-9' },
    ],
    ...overrides,
  };
}

test('현재 사용자, SYSTEM, Administrators만 허용하는 ACL 정책을 통과시킨다', () => {
  // given
  const value = acl();
  // when
  const result = ownerAclAllowed(value);
  // then
  assert.equal(result, true);
});

test('소유자가 현재 사용자가 아니면 ACL 정책을 거부한다', () => {
  // given
  const value = acl({ ownerSid: 'S-1-5-21-9-9-9-10' });
  // when
  const result = ownerAclAllowed(value);
  // then
  assert.equal(result, false);
});

test('광범위한 SID의 allow ACE가 있으면 ACL 정책을 거부한다', () => {
  // given
  const value = acl({ aces: [{ type: 'allow', sid: 'S-1-1-0' }] });
  // when
  const result = ownerAclAllowed(value);
  // then
  assert.equal(result, false);
});

for (const [title, change] of [
  ['null DACL', value => ({ ...value, daclPresent: false })],
  ['알 수 없는 ACE', value => ({ ...value, aces: [{ type: 'callback_allow', sid: currentSid }] })],
]) {
  test(`${title}일 때 ACL 정책은 보수적으로 거부한다`, () => {
    // given
    const value = change(acl());
    // when
    const result = ownerAclAllowed(value);
    // then
    assert.equal(result, false);
  });
}

test('PowerShell ACL 조회 실패의 상세 오류를 노출하지 않는다', () => {
  // given
  const target = 'C:\\private\\config.json';
  const secret = 'synthetic-sensitive-value';
  const exec = () => { throw new Error(`${target} ${secret}`); };
  // when
  let failure;
  try { assertOwnerAcl(target, { platform: 'win32', systemRoot: 'C:\\Windows', exec }); } catch (error) { failure = error; }
  // then
  assert.equal(failure.message, 'OWNER_ACL_CHECK_FAILED');
  assert.ok(!failure.message.includes(target));
  assert.ok(!failure.message.includes(secret));
});
