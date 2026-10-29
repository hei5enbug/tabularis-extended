import {useCallback, useEffect, useState} from 'react';
import {getCurrentWindow} from '@tauri-apps/api/window';
import {
    getOfficialProfileImportStatus,
    type OfficialProfileImportStatus,
} from '../utils/profileImport';

type PromptPhase = 'checking' | 'open' | 'closed';

function isMainWindow(): boolean {
    try {
        const label = getCurrentWindow().label;
        return !label || label === 'main';
    } catch {
        return false;
    }
}

/**
 * Asks once, at startup of the main window, whether the official profile can
 * still be imported, and opens the first-run import prompt when it can.
 * Other first-run prompts wait while this one is checking or open: any
 * profile write they make would leave the destination non-empty. After an
 * import the prompt stays open until the app restarts.
 */
export function useOfficialProfileImportPrompt() {
    const [checkOnMount] = useState(isMainWindow);
    const [phase, setPhase] = useState<PromptPhase>(checkOnMount ? 'checking' : 'closed');
    const [status, setStatus] = useState<OfficialProfileImportStatus | null>(null);

    useEffect(() => {
        if (!checkOnMount) return;
        let cancelled = false;
        getOfficialProfileImportStatus()
            .then((next) => {
                if (cancelled) return;
                setStatus(next);
                setPhase(next.canImport ? 'open' : 'closed');
            })
            .catch(() => {
                if (!cancelled) setPhase('closed');
            });
        return () => {
            cancelled = true;
        };
    }, [checkOnMount]);

    const dismiss = useCallback(() => setPhase('closed'), []);

    return {
        isOpen: phase === 'open',
        status,
        holdsOtherPrompts: phase !== 'closed',
        dismiss,
    };
}
