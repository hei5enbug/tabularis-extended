import {describe, expect, it} from 'vitest';
import {
    connectionLabels,
    connectionSaveIssue,
    connectionSaveIssueText,
    isPasswordOnlyEdit,
    type ConnectionEditSnapshot,
} from '../../src/utils/connectionEdit';

function snapshot(overrides: Partial<ConnectionEditSnapshot> = {}): ConnectionEditSnapshot {
    return {
        name: 'Prod',
        driver: 'postgresql-plus',
        params: {
            driver: 'postgresql-plus',
            host: 'db.example.com',
            port: 5432,
            username: 'app',
            password: 'old-secret',
            database: '',
            ssl_mode: 'verify_full',
            extra: {auth_mode: 'sql_password'},
        },
        selectedDatabases: ['orders', 'billing'],
        loadAllDatabases: false,
        detectJsonInTextColumns: false,
        environment: 'production',
        appearance: {},
        tagIds: ['tag-a', 'tag-b'],
        sshMode: 'existing',
        k8sMode: 'inline',
        k8sPaths: {},
        credentialFieldsHidden: false,
        passwordFieldHidden: false,
        ...overrides,
    };
}

describe('connectionEdit', () => {
    describe('isPasswordOnlyEdit', () => {
        it('비밀번호만 바뀌면 비밀번호 전용 저장으로 판단한다', () => {
            // given
            const loaded = snapshot();
            const current = snapshot({
                params: {...loaded.params, password: 'new-secret'},
                selectedDatabases: ['billing', 'orders'],
                tagIds: ['tag-b', 'tag-a'],
            });

            // when
            const actual = isPasswordOnlyEdit(loaded, current);

            // then
            expect(actual).toBe(true);
        });

        it('비밀번호와 함께 다른 항목이 바뀌면 전체 저장으로 판단한다', () => {
            // given
            const loaded = snapshot();
            const changes: Partial<ConnectionEditSnapshot>[] = [
                {params: {...loaded.params, password: 'new', ssl_mode: 'disable'}},
                {params: {...loaded.params, password: 'new', extra: {}}},
                {params: {...loaded.params, password: 'new'}, name: 'Prod 2'},
                {params: {...loaded.params, password: 'new'}, selectedDatabases: ['orders']},
                {
                    params: {...loaded.params, password: 'new'},
                    k8sPaths: {kubectl_path: '/bin/kubectl'},
                },
                {params: {...loaded.params, password: 'new'}, passwordFieldHidden: true},
            ];

            // when
            const actual = changes.map((change) => isPasswordOnlyEdit(loaded, snapshot(change)));

            // then
            expect(actual).toEqual([false, false, false, false, false, false]);
        });

        it('비밀번호가 그대로이거나 비어 있으면 비밀번호 전용 저장으로 판단하지 않는다', () => {
            // given
            const loaded = snapshot();
            const passwords = ['old-secret', '', '   ', undefined];

            // when
            const actual = passwords.map((password) =>
                isPasswordOnlyEdit(loaded, snapshot({params: {...loaded.params, password}})),
            );

            // then
            expect(actual).toEqual([false, false, false, false]);
        });

        it('값이 undefined인 항목과 없는 항목은 같은 값으로 본다', () => {
            // given
            const loaded = snapshot();
            const current = snapshot({
                params: {...loaded.params, password: 'new', ssh_connection_id: undefined},
                k8sPaths: {kubectl_path: '  '},
            });

            // when
            const actual = isPasswordOnlyEdit(loaded, current);

            // then
            expect(actual).toBe(true);
        });
    });

    describe('connectionSaveIssue', () => {
        it('저장 충돌과 복구 필요 오류를 구분하고 나머지는 무시한다', () => {
            // given
            const errors = [
                'CONNECTION_CONFLICT',
                new Error('CONNECTION_RECOVERY_REQUIRED:c1, c2'),
                'CONNECTION_CONFLICT: extra',
                'Connection not found',
            ];

            // when
            const actual = errors.map(connectionSaveIssue);

            // then
            expect(actual).toEqual([
                {kind: 'conflict'},
                {kind: 'recovery', connectionIds: ['c1', 'c2']},
                null,
                null,
            ]);
        });
    });

    describe('connectionLabels', () => {
        it('알려진 연결은 이름으로, 모르는 연결은 id로 표시한다', () => {
            // given
            const ids = ['c1', 'c2'];

            // when
            const actual = connectionLabels(ids, {c1: 'Prod'});

            // then
            expect(actual).toEqual(['Prod', 'c2']);
        });
    });

    describe('connectionSaveIssueText', () => {
        it('복구 필요 오류는 연결 이름을 담은 번역 키로, 충돌은 충돌 키로 바꾸고 다른 오류는 무시한다', () => {
            // given
            const names = {c1: 'Prod', c2: 'Staging'};
            const errors = [
                'CONNECTION_RECOVERY_REQUIRED:c1,c3',
                'CONNECTION_CONFLICT',
                'Connection not found',
            ];

            // when
            const actual = errors.map((error) => connectionSaveIssueText(error, names));

            // then
            expect(actual).toEqual([
                {
                    key: 'connections.saveIssue.recovery',
                    options: {count: 2, names: 'Prod, c3'},
                },
                {key: 'connections.saveIssue.conflict', options: {}},
                null,
            ]);
        });
    });
});
