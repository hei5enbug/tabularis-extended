import {render, screen} from '@testing-library/react';
import {describe, expect, it, vi} from 'vitest';
import {createInstance} from 'i18next';
import en from '@/i18n/locales/en.json';
import {ServiceResultLimitBadge} from '@/components/ui/ServiceResultLimitBadge';
import type {ServiceResultReference} from '@/types/editor';

const i18n = createInstance();
await i18n.init({lng: 'en', resources: {en: {translation: en}}});
vi.mock('react-i18next', () => ({
    useTranslation: () => ({t: i18n.t.bind(i18n)}),
}));

function serviceReference(limitReasons: string[]): ServiceResultReference {
    return {
        resultId: 'result-1',
        resultSetIndex: 0,
        resultGeneration: 1,
        resultRowOffset: 0,
        connectionId: 'conn-1',
        driverId: 'postgresql-plus',
        resultRevision: 'rev-1',
        resultSetCount: 1,
        pageOffsets: [0],
        pageSize: 100,
        nextToken: null,
        resumeMode: 'materialized',
        kind: 'tabular',
        columnMetadata: [],
        warnings: [],
        limitReasons,
    };
}

describe('ServiceResultLimitBadge', () => {
    it('row_limit로 잘린 서비스 결과에는 10,000행만 담겼다고 알린다', () => {
        // given
        const result = {service: serviceReference(['row_limit']), truncated: true};

        // when
        render(<ServiceResultLimitBadge result={result} />);

        // then
        expect(screen.getByRole('button', {name: 'First 10,000 rows'})).toBeInTheDocument();
    });

    it('서비스 결과가 아닌 잘린 결과에는 아무것도 표시하지 않는다', () => {
        // given
        const result = {truncated: true};

        // when
        const {container} = render(<ServiceResultLimitBadge result={result} />);

        // then
        expect(container).toBeEmptyDOMElement();
    });

    it('잘리지 않은 서비스 결과에는 아무것도 표시하지 않는다', () => {
        // given
        const result = {service: serviceReference([]), truncated: false};

        // when
        const {container} = render(<ServiceResultLimitBadge result={result} />);

        // then
        expect(container).toBeEmptyDOMElement();
    });
});
