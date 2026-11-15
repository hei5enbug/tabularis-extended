import DOMPurify from 'dompurify';

describe('monaco 편집기의 DOMPurify', () => {
    it('monaco는 자기 안에 넣어 둔 사본이 아니라 앱이 설치한 dompurify 패키지를 쓴다', async () => {
        // when
        const {default: monacoPurify} =
            await import('monaco-editor/esm/vs/base/browser/dompurify/dompurify.js');

        // then
        expect(monacoPurify).toBe(DOMPurify);
    });
});
