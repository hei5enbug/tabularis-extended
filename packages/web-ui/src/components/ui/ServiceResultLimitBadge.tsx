import {useTranslation} from 'react-i18next';
import {UpdateTooltip} from './UpdateTooltip';
import type {QueryResult} from '@/types/editor';

/** Mirrors MAX_RESULT_ROWS in src-tauri/src/services/results.rs. */
const SERVICE_RESULT_ROW_LIMIT = 10_000;

interface ServiceResultLimitBadgeProps {
    result: Pick<QueryResult, 'service' | 'truncated'>;
}

/** Warns that a service result snapshot kept only part of what the query returned. */
export function ServiceResultLimitBadge({result}: ServiceResultLimitBadgeProps) {
    const {t} = useTranslation();
    if (!result.service || result.truncated !== true) return null;

    const rowLimit = result.service.limitReasons.includes('row_limit');
    const label = rowLimit
        ? t('serviceQuery.rowLimitBadge', {limit: SERVICE_RESULT_ROW_LIMIT})
        : t('serviceQuery.sizeLimitBadge');
    const hint = rowLimit ? t('serviceQuery.rowLimitHint') : t('serviceQuery.sizeLimitHint');

    return (
        <UpdateTooltip label={hint} className="inline-flex shrink-0">
            <button
                type="button"
                className="px-2 py-0.5 bg-accent-warning/15 text-accent-warning rounded text-[10px] font-semibold uppercase tracking-wide border border-accent-warning/50 whitespace-nowrap shrink-0 cursor-help focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus"
            >
                {label}
            </button>
        </UpdateTooltip>
    );
}
