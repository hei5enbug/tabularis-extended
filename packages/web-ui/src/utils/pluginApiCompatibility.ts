interface SemanticVersion {
    readonly major: number;
    readonly minor: number;
    readonly patch: number;
}

const SEMANTIC_VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

function parseSemanticVersion(value: string): SemanticVersion | null {
    const match = SEMANTIC_VERSION.exec(value);
    if (!match) return null;
    return {
        major: Number(match[1]),
        minor: Number(match[2]),
        patch: Number(match[3]),
    };
}

function compareVersions(left: SemanticVersion, right: SemanticVersion): number {
    return left.major - right.major || left.minor - right.minor || left.patch - right.patch;
}

/**
 * 0.x host minors that also run bundles built for an older minor, because they only add optional
 * fields to it. Tabularis Extended's 0.2 adds the password-only hiding to the 0.1 contract.
 */
const EXTENDED_ZERO_MINORS: Readonly<Record<number, number>> = {2: 1};

/**
 * Check whether a host can execute a bundle built for the declared plugin API.
 * Legacy entries without a declaration remain supported. During the unstable
 * 0.x line, minor versions are treated as compatibility boundaries.
 */
export function isPluginApiCompatible(hostVersion: string, requiredVersion?: string): boolean {
    if (requiredVersion === undefined) return true;
    const host = parseSemanticVersion(hostVersion);
    const required = parseSemanticVersion(requiredVersion);
    if (!host || !required || host.major !== required.major) return false;
    if (host.major === 0 && host.minor !== required.minor) {
        return EXTENDED_ZERO_MINORS[host.minor] === required.minor;
    }
    return compareVersions(host, required) >= 0;
}
