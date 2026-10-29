import {fireEvent, render, screen} from '@testing-library/react';
import {invoke} from '@tauri-apps/api/core';
import {beforeEach, describe, expect, it, vi} from 'vitest';
import {OfficialProfileImportSection} from '../../../src/components/settings/OfficialProfileImportSection';
import type {OfficialProfileImportStatus} from '../../../src/utils/profileImport';

vi.mock('@/hooks/useTabularisClient', () => import('../../support/tauriBackedHooks'));
vi.mock('lucide-react', () => vi.importActual('lucide-react'));

function status(overrides: Partial<OfficialProfileImportStatus> = {}): OfficialProfileImportStatus {
    return {
        sourceFound: true,
        destinationEmpty: true,
        canImport: true,
        sourceDir: '/official',
        destinationDir: '/extended',
        counts: {
            connections: 2,
            groups: 0,
            savedQueries: 0,
            notebooks: 0,
            themes: 0,
            pluginPackages: 0,
        },
        ...overrides,
    };
}

describe('OfficialProfileImportSection', () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    it('아직 가져올 수 있으면 설정에서 가져오기 안내를 열 수 있다', async () => {
        // given
        vi.mocked(invoke).mockResolvedValue(status());
        render(<OfficialProfileImportSection />);
        const button = await screen.findByRole('button', {
            name: 'profileImport.settings.button',
        });

        // when
        fireEvent.click(button);

        // then
        expect(screen.getByText('profileImport.settings.ready')).toBeInTheDocument();
        expect(screen.getByRole('dialog')).toHaveAccessibleName('profileImport.title');
    });

    it.each([
        {
            reason: '이 프로필에 이미 데이터가 있으면',
            current: status({destinationEmpty: false, canImport: false}),
            message: 'profileImport.settings.notEmpty',
        },
        {
            reason: '공식 프로필이 없으면',
            current: status({sourceFound: false, canImport: false}),
            message: 'profileImport.settings.noSource',
        },
    ])('$reason 이유를 보여주고 가져오기 버튼을 막는다', async ({current, message}) => {
        // given
        vi.mocked(invoke).mockResolvedValue(current);

        // when
        render(<OfficialProfileImportSection />);

        // then
        expect(await screen.findByText(message)).toBeInTheDocument();
        expect(screen.getByRole('button', {name: 'profileImport.settings.button'})).toBeDisabled();
    });
});
