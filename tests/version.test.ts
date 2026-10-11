import { describe, it, expect } from 'vitest';
import { APP_VERSION } from '@/version';
import { parse, valid } from 'semver';

describe('version', () => {
  it('should export a version string', () => {
    expect(typeof APP_VERSION).toBe('string');
    expect(APP_VERSION).toBeTruthy();
  });

  it('빌드 메타데이터를 포함한 정식 SemVer 버전을 제공한다', () => {
    // given
    const version = APP_VERSION;
    // when
    const normalized = valid(version);
    // then
    expect(normalized).not.toBeNull();
  });

  it('주 버전과 부 버전과 패치 번호는 음수가 아닌 정수다', () => {
    // given
    const version = APP_VERSION;
    // when
    const parsed = parse(version);
    // then
    expect(parsed).not.toBeNull();
    for (const part of [parsed!.major, parsed!.minor, parsed!.patch]) {
      expect(Number.isInteger(part)).toBe(true);
      expect(part).toBeGreaterThanOrEqual(0);
    }
  });

  it('should not contain whitespace', () => {
    expect(APP_VERSION.trim()).toBe(APP_VERSION);
    expect(APP_VERSION).not.toContain(' ');
    expect(APP_VERSION).not.toContain('\t');
    expect(APP_VERSION).not.toContain('\n');
  });
});
