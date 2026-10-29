import {act, renderHook, waitFor} from '@testing-library/react';
import {describe, expect, it, vi} from 'vitest';
import {invoke} from '@tauri-apps/api/core';
import {usePendingApprovals} from '../../src/hooks/useAiActivity';

vi.mock('@tauri-apps/api/event', () => ({listen: vi.fn().mockResolvedValue(vi.fn())}));
vi.mock('@/hooks/useTabularisClient', () => import('../support/tauriBackedHooks'));

it('서비스 승인은 수정 SQL을 보내지 않고 원래 승인 ID를 사용한다', async () => {
    // Given
    vi.mocked(invoke)
        .mockResolvedValueOnce([{id: 'approval-a', queryKind: 'service:query.execute'}])
        .mockResolvedValue(null);
    const hook = renderHook(usePendingApprovals);
    await waitFor(() => expect(hook.result.current.pending).toHaveLength(1));
    const decide = async () => {
        await act(async () => {
            await hook.result.current.decide({
                approvalId: 'approval-a',
                decision: 'approve',
                reason: 'explicit',
                editedQuery: 'ALTER SECRET',
            });
        });
    };
    // When
    await decide();
    // Then
    expect(invoke).toHaveBeenCalledWith('service_approve', {
        approvalId: 'approval-a',
        decision: 'approve',
        reason: 'explicit',
    });
    expect(invoke).not.toHaveBeenCalledWith('decide_pending_approval', expect.anything());
    expect(hook.result.current.pending).toHaveLength(0);
});

describe('승인 거절', () => {
    it('서비스 닫기 결정은 SQL 없이 deny로 전달한다', async () => {
        // Given
        vi.mocked(invoke)
            .mockReset()
            .mockResolvedValueOnce([{id: 'approval-b', queryKind: 'service:row.insert'}])
            .mockResolvedValue(null);
        const hook = renderHook(usePendingApprovals);
        await waitFor(() => expect(hook.result.current.pending).toHaveLength(1));
        const decide = async () => {
            await act(async () => {
                await hook.result.current.decide({
                    approvalId: 'approval-b',
                    decision: 'deny',
                    reason: 'dismissed',
                });
            });
        };
        // When
        await decide();
        // Then
        expect(invoke).toHaveBeenCalledWith('service_approve', {
            approvalId: 'approval-b',
            decision: 'deny',
            reason: 'dismissed',
        });
        expect(hook.result.current.pending).toHaveLength(0);
    });
});
