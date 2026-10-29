import {useCallback, useEffect, useState} from 'react';
import {useTranslation} from 'react-i18next';
import {Download, Loader2} from 'lucide-react';
import {OfficialProfileImportModal} from '@/components/modals/OfficialProfileImportModal';
import {
    getOfficialProfileImportStatus,
    type OfficialProfileImportStatus,
} from '@/utils/profileImport';
import {toErrorMessage} from '@/utils/errors';
import {SettingRow, SettingSection} from './SettingControls';

const buttonClass =
    'flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-base border border-strong text-sm text-secondary hover:text-accent hover:border-accent-primary/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:text-secondary disabled:hover:border-strong focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus';

/** Runs the official profile import later, while the profile is still empty. */
export function OfficialProfileImportSection() {
    const {t} = useTranslation();
    const [status, setStatus] = useState<OfficialProfileImportStatus | null>(null);
    const [loadError, setLoadError] = useState<string | null>(null);
    const [isModalOpen, setIsModalOpen] = useState(false);

    const loadStatus = useCallback(
        () =>
            getOfficialProfileImportStatus().then(
                (next) => {
                    setStatus(next);
                    setLoadError(null);
                },
                (error: unknown) => setLoadError(toErrorMessage(error)),
            ),
        [],
    );

    useEffect(() => {
        void loadStatus();
    }, [loadStatus]);

    const statusText = loadError
        ? t('profileImport.settings.statusFailed', {error: loadError})
        : !status
          ? null
          : status.canImport
            ? t('profileImport.settings.ready', {count: status.counts.connections})
            : !status.sourceFound
              ? t('profileImport.settings.noSource')
              : t('profileImport.settings.notEmpty');

    return (
        <SettingSection title={t('profileImport.settings.title')}>
            <SettingRow
                label={t('profileImport.settings.label')}
                description={t('profileImport.settings.description')}
                vertical
            >
                <p className="text-xs text-secondary" aria-live="polite">
                    {statusText ?? (
                        <Loader2
                            size={14}
                            className="animate-spin"
                            aria-label={t('common.loading')}
                        />
                    )}
                </p>
                <button
                    type="button"
                    onClick={() => setIsModalOpen(true)}
                    disabled={!status?.canImport}
                    className={`${buttonClass} mt-3`}
                >
                    <Download size={14} />
                    {t('profileImport.settings.button')}
                </button>
            </SettingRow>
            <OfficialProfileImportModal
                isOpen={isModalOpen}
                status={status}
                onImported={() => void loadStatus()}
                onClose={() => {
                    setIsModalOpen(false);
                    void loadStatus();
                }}
            />
        </SettingSection>
    );
}
