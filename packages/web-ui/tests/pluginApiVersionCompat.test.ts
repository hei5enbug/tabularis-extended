import {afterEach, describe, expect, it, vi} from 'vitest';
import {assertHostCompat} from '../../plugin-api/src/host';
import {MIN_HOST_VERSION} from '../../plugin-api/src/version';

describe('플러그인 SDK API 버전 호환', () => {
    afterEach(() => {
        vi.unstubAllGlobals();
    });

    it('sandbox guest가 알리는 host API 0.4.0은 SDK 최소 API 버전을 만족한다', () => {
        // given
        vi.stubGlobal('__TABULARIS_API_VERSION__', '0.4.0');
        // when
        const actual = (() => {
            try {
                assertHostCompat();
                return null;
            } catch (error) {
                return error;
            }
        })();
        // then
        expect(actual).toBeNull();
        expect(MIN_HOST_VERSION).toBe('0.4.0');
    });

    it.each([undefined, '0.1.1', '0.2.0'])('host API %s는 SDK 최소 버전에 미달한다', (version) => {
        // given
        vi.stubGlobal('__TABULARIS_API_VERSION__', version);
        // when
        const actual = (() => {
            try {
                assertHostCompat();
                return null;
            } catch (error) {
                return error;
            }
        })();
        // then
        expect(actual).toBeInstanceOf(Error);
        expect(actual).toMatchObject({message: expect.stringContaining('required 0.4.0')});
    });
});
