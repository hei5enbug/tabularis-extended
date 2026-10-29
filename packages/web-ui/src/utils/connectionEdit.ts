import {toErrorMessage} from './errors';

/** Exact error a save rejects with when the connection changed since it was read. */
export const CONNECTION_CONFLICT = 'CONNECTION_CONFLICT';
/** Prefix of the error a save rejects with when a failed save could not restore the previous secrets. */
export const CONNECTION_RECOVERY_REQUIRED_PREFIX = 'CONNECTION_RECOVERY_REQUIRED:';

export type K8sPathOptions = {
    kubectl_path?: string;
    kubeconfig_path?: string;
};

/** Everything the connection editor can save, as loaded or as currently edited. */
export interface ConnectionEditSnapshot {
    name: string;
    driver: string;
    params: Readonly<Record<string, unknown>>;
    selectedDatabases: readonly string[];
    loadAllDatabases: boolean;
    detectJsonInTextColumns: boolean;
    environment: string;
    appearance: unknown;
    tagIds: readonly string[];
    sshMode: string;
    k8sMode: string;
    k8sPaths: K8sPathOptions;
    credentialFieldsHidden: boolean;
    passwordFieldHidden: boolean;
}

export type ConnectionSaveIssue = {kind: 'conflict'} | {kind: 'recovery'; connectionIds: string[]};

function canonical(value: unknown): unknown {
    if (Array.isArray(value)) return value.map(canonical);
    if (value !== null && typeof value === 'object') {
        const entries = Object.entries(value as Record<string, unknown>)
            .filter(([, item]) => item !== undefined)
            .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
            .map(([key, item]) => [key, canonical(item)] as const);
        return Object.fromEntries(entries);
    }
    return value;
}

function sameValue(left: unknown, right: unknown): boolean {
    return JSON.stringify(canonical(left)) === JSON.stringify(canonical(right));
}

export function normalizeK8sPaths(options?: K8sPathOptions | null): K8sPathOptions {
    return {
        kubectl_path: options?.kubectl_path?.trim() || undefined,
        kubeconfig_path: options?.kubeconfig_path?.trim() || undefined,
    };
}

function withoutPassword(snapshot: ConnectionEditSnapshot) {
    const params: Record<string, unknown> = {...snapshot.params};
    delete params.password;
    return {
        ...snapshot,
        params,
        selectedDatabases: [...snapshot.selectedDatabases].sort(),
        tagIds: [...snapshot.tagIds].sort(),
        k8sPaths: normalizeK8sPaths(snapshot.k8sPaths),
    };
}

/**
 * True when the only difference between the loaded and the edited connection
 * is a new, non-blank password, so the save can change the password alone.
 */
export function isPasswordOnlyEdit(
    loaded: ConnectionEditSnapshot,
    current: ConnectionEditSnapshot,
): boolean {
    const password = current.params.password;
    if (typeof password !== 'string' || password.trim() === '') return false;
    if (password === loaded.params.password) return false;
    return sameValue(withoutPassword(loaded), withoutPassword(current));
}

/** Recognises the save failures the editor reports with a dedicated message. */
export function connectionSaveIssue(error: unknown): ConnectionSaveIssue | null {
    const text = toErrorMessage(error).trim();
    if (text === CONNECTION_CONFLICT) return {kind: 'conflict'};
    if (text.startsWith(CONNECTION_RECOVERY_REQUIRED_PREFIX)) {
        const connectionIds = text
            .slice(CONNECTION_RECOVERY_REQUIRED_PREFIX.length)
            .split(',')
            .map((id) => id.trim())
            .filter(Boolean);
        return {kind: 'recovery', connectionIds};
    }
    return null;
}

/** Names the given connections, falling back to the id when no name is known. */
export function connectionLabels(
    connectionIds: readonly string[],
    names: Readonly<Record<string, string | undefined>>,
): string[] {
    return connectionIds.map((id) => names[id]?.trim() || id);
}

/** Translation key and values that describe a save issue outside the editor. */
export interface ConnectionSaveIssueText {
    key: string;
    options: Record<string, unknown>;
}

/**
 * Describes a failed connection change that needs the user to act, naming the
 * affected connections; `null` for any other error.
 */
export function connectionSaveIssueText(
    error: unknown,
    names: Readonly<Record<string, string | undefined>>,
): ConnectionSaveIssueText | null {
    const issue = connectionSaveIssue(error);
    if (!issue) return null;
    if (issue.kind === 'conflict') {
        return {key: 'connections.saveIssue.conflict', options: {}};
    }
    const labels = connectionLabels(issue.connectionIds, names);
    return {
        key: 'connections.saveIssue.recovery',
        options: {count: labels.length, names: labels.join(', ')},
    };
}
