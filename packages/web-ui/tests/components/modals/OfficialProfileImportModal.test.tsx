import {fireEvent, render, screen, waitFor, within} from '@testing-library/react';
import {invoke} from '@tauri-apps/api/core';
import {beforeEach, describe, expect, it, vi} from 'vitest';
import {OfficialProfileImportModal} from '../../../src/components/modals/OfficialProfileImportModal';
import {useOfficialProfileImportPrompt} from '../../../src/hooks/useOfficialProfileImportPrompt';
import {isProfileImportAwaitingRestart} from '../../../src/utils/profileImportSession';
import type {
    OfficialProfileImportReport,
    OfficialProfileImportStatus,
} from '../../../src/utils/profileImport';

vi.mock('lucide-react', () => vi.importActual('lucide-react'));
vi.mock('@/hooks/useTabularisClient', () => import('../../support/tauriBackedHooks'));

const counts = {
    connections: 3,
    groups: 1,
    savedQueries: 4,
    notebooks: 2,
    themes: 1,
    pluginPackages: 2,
};

function status(overrides: Partial<OfficialProfileImportStatus> = {}): OfficialProfileImportStatus {
    return {
        sourceFound: true,
        destinationEmpty: true,
        canImport: true,
        sourceDir: '/Users/me/Library/Application Support/tabularis',
        destinationDir: '/Users/me/Library/Application Support/tabularis-extended',
        counts,
        ...overrides,
    };
}

const report: OfficialProfileImportReport = {
    backupDir: '/backups/official-1',
    counts,
    secretsStored: 3,
    pluginPackagesCopied: ['postgresql-plus'],
    pluginPackagesSkipped: ['broken-driver'],
    skippedLinks: 0,
    remap: {
        remapped: ['c1'],
        unconverted: ['c2'],
        verifiedPackages: ['postgresql-plus'],
    },
};

function mockCommands(handlers: Record<string, () => Promise<unknown>>) {
    vi.mocked(invoke).mockImplementation((command) => {
        const handler = handlers[command];
        return handler ? handler() : Promise.resolve(undefined);
    });
}

function renderDialog(current: OfficialProfileImportStatus = status()) {
    const onClose = vi.fn();
    render(<OfficialProfileImportModal isOpen={true} onClose={onClose} status={current} />);
    return {onClose};
}

function countOf(label: string): string | null {
    return screen.getByText(label).nextElementSibling?.textContent ?? null;
}

function FirstRunPrompt() {
    const prompt = useOfficialProfileImportPrompt();
    return (
        <>
            <span data-testid="holds-other-prompts">{String(prompt.holdsOtherPrompts)}</span>
            <OfficialProfileImportModal
                isOpen={prompt.isOpen}
                onClose={prompt.dismiss}
                status={prompt.status}
            />
        </>
    );
}

describe('OfficialProfileImportModal', () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    it('첫 화면은 복사할 항목의 개수와 공식 앱이 그대로라는 안내, 키체인 안내를 보여준다', () => {
        // given
        const current = status();

        // when
        renderDialog(current);

        // then
        expect(screen.getByRole('dialog')).toHaveAccessibleName('profileImport.title');
        expect(countOf('profileImport.items.connections')).toBe('3');
        expect(countOf('profileImport.items.savedQueries')).toBe('4');
        expect(screen.getByText('profileImport.items.settings')).toBeInTheDocument();
        expect(screen.getByText('profileImport.untouched')).toBeInTheDocument();
        expect(screen.getByText('profileImport.keychainNotice')).toBeInTheDocument();
        expect(screen.getByText('profileImport.notNow')).toBeInTheDocument();
    });

    it('가져오기가 끝나면 개수, 건너뛴 플러그인 패키지, 드라이버 전환 결과를 보여준다', async () => {
        // given
        mockCommands({
            import_official_profile: () => Promise.resolve(report),
            get_connections: () =>
                Promise.resolve([
                    {id: 'c1', name: 'Orders'},
                    {id: 'c2', name: 'Legacy SQL Server'},
                ]),
        });
        renderDialog();

        // when
        fireEvent.click(screen.getByRole('button', {name: 'profileImport.import'}));

        // then
        expect(await screen.findByText('profileImport.result.title')).toBeInTheDocument();
        expect(countOf('profileImport.items.connections')).toBe('3');
        expect(countOf('profileImport.result.pluginPackagesCopied')).toBe('1');
        expect(countOf('profileImport.result.secretsStored')).toBe('3');
        expect(countOf('profileImport.result.remapped')).toBe('1');
        expect(screen.getByText('profileImport.result.pluginPackagesSkipped')).toBeInTheDocument();
        expect(screen.getByText('broken-driver')).toBeInTheDocument();
        expect(screen.getByText('profileImport.result.unconverted')).toBeInTheDocument();
        expect(screen.getByText('Legacy SQL Server')).toBeInTheDocument();
        expect(
            screen.getByRole('button', {name: 'profileImport.result.restart'}),
        ).toBeInTheDocument();
    });

    it('가져오기가 끝나면 닫기 버튼과 Escape 없이 재시작만 할 수 있고 설정 저장을 막는다', async () => {
        // given
        mockCommands({
            import_official_profile: () => Promise.resolve({...report, remap: null}),
        });
        const {onClose} = renderDialog();
        fireEvent.click(screen.getByRole('button', {name: 'profileImport.import'}));
        const dialog = screen.getByRole('dialog');
        await within(dialog).findByText('profileImport.result.title');

        // when
        fireEvent.keyDown(window, {key: 'Escape'});

        // then
        expect(onClose).not.toHaveBeenCalled();
        expect(screen.getByRole('dialog')).toBeInTheDocument();
        expect(
            within(dialog).queryByRole('button', {name: 'common.close'}),
        ).not.toBeInTheDocument();
        expect(
            within(dialog)
                .getAllByRole('button')
                .map((button) => button.textContent),
        ).toEqual(['profileImport.result.restart']);
        expect(isProfileImportAwaitingRestart()).toBe(true);
    });

    it('재시작을 누르면 relaunch_app을 호출한다', async () => {
        // given
        mockCommands({
            import_official_profile: () => Promise.resolve({...report, remap: null}),
        });
        renderDialog();
        fireEvent.click(screen.getByRole('button', {name: 'profileImport.import'}));
        const restart = await screen.findByRole('button', {
            name: 'profileImport.result.restart',
        });

        // when
        fireEvent.click(restart);

        // then
        await waitFor(() => expect(invoke).toHaveBeenCalledWith('relaunch_app'));
    });

    it('재시작에 실패하면 오류를 보여주고 창을 연 채로 다시 시도할 수 있다', async () => {
        // given
        let relaunches = 0;
        mockCommands({
            import_official_profile: () => Promise.resolve({...report, remap: null}),
            relaunch_app: () => {
                relaunches += 1;
                return Promise.reject('restart denied');
            },
        });
        const {onClose} = renderDialog();
        fireEvent.click(screen.getByRole('button', {name: 'profileImport.import'}));
        fireEvent.click(await screen.findByRole('button', {name: 'profileImport.result.restart'}));
        const retry = await screen.findByRole('button', {
            name: 'profileImport.result.retryRestart',
        });

        // when
        fireEvent.click(retry);

        // then
        await waitFor(() => expect(relaunches).toBe(2));
        const alert = screen.getByRole('alert');
        expect(alert).toHaveTextContent('profileImport.result.restartFailed');
        expect(alert).toHaveTextContent('restart denied');
        expect(onClose).not.toHaveBeenCalled();
        expect(screen.getByRole('dialog')).toBeInTheDocument();
    });

    it('가져오기가 오류 코드로 실패하면 그 코드에 맞는 안내를 보여준다', async () => {
        // given
        mockCommands({
            import_official_profile: () =>
                Promise.reject('IMPORT_SECRET_FAILED: The keychain refused a write'),
        });
        renderDialog();

        // when
        fireEvent.click(screen.getByRole('button', {name: 'profileImport.import'}));

        // then
        const alert = await screen.findByRole('alert');
        expect(alert).toHaveTextContent('profileImport.failedTitle');
        expect(alert).toHaveTextContent('profileImport.errors.IMPORT_SECRET_FAILED');
        expect(screen.queryByText('profileImport.result.title')).not.toBeInTheDocument();
    });

    it('코드가 없는 실패는 일반 실패 안내로 보여준다', async () => {
        // given
        mockCommands({
            import_official_profile: () => Promise.reject('The profile import task failed.'),
        });
        renderDialog();

        // when
        fireEvent.click(screen.getByRole('button', {name: 'profileImport.import'}));

        // then
        expect(await screen.findByRole('alert')).toHaveTextContent('profileImport.errors.unknown');
    });
});

describe('첫 실행 프로필 가져오기 안내', () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    it('가져올 수 있다고 보고되면 안내를 열고 다른 첫 실행 안내를 잠시 막는다', async () => {
        // given
        mockCommands({
            official_profile_import_status: () => Promise.resolve(status()),
        });

        // when
        render(<FirstRunPrompt />);

        // then
        expect(await screen.findByRole('dialog')).toBeInTheDocument();
        expect(screen.getByTestId('holds-other-prompts')).toHaveTextContent('true');
    });

    it.each([
        {reason: '공식 프로필이 없으면', current: status({sourceFound: false, canImport: false})},
        {
            reason: '이 프로필에 이미 데이터가 있으면',
            current: status({destinationEmpty: false, canImport: false}),
        },
    ])('$reason 안내를 열지 않고 다른 안내도 막지 않는다', async ({current}) => {
        // given
        mockCommands({
            official_profile_import_status: () => Promise.resolve(current),
        });

        // when
        render(<FirstRunPrompt />);

        // then
        await waitFor(() =>
            expect(screen.getByTestId('holds-other-prompts')).toHaveTextContent('false'),
        );
        expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });

    it('나중에를 누르면 안내를 닫고 아무것도 가져오지 않는다', async () => {
        // given
        mockCommands({
            official_profile_import_status: () => Promise.resolve(status()),
        });
        render(<FirstRunPrompt />);
        const dialog = await screen.findByRole('dialog');

        // when
        fireEvent.click(within(dialog).getByText('profileImport.notNow'));

        // then
        expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
        expect(screen.getByTestId('holds-other-prompts')).toHaveTextContent('false');
        expect(invoke).not.toHaveBeenCalledWith('import_official_profile');
    });

    it('가져오기를 마친 뒤에는 재시작 전까지 안내가 열린 채로 다른 첫 실행 안내를 막는다', async () => {
        // given
        mockCommands({
            official_profile_import_status: () => Promise.resolve(status()),
            import_official_profile: () => Promise.resolve({...report, remap: null}),
        });
        render(<FirstRunPrompt />);

        // when
        fireEvent.click(await screen.findByRole('button', {name: 'profileImport.import'}));

        // then
        const dialog = await screen.findByRole('dialog');
        await within(dialog).findByText('profileImport.result.title');
        expect(screen.getByTestId('holds-other-prompts')).toHaveTextContent('true');
    });
});
