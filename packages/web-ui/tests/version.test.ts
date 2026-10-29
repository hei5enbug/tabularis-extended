import {describe, it, expect} from 'vitest';
import {APP_VERSION} from '@/version';
import packageJson from '../../../package.json';

describe('version', () => {
    it('should export a version string', () => {
        expect(typeof APP_VERSION).toBe('string');
        expect(APP_VERSION).toBeTruthy();
    });

    it('should follow semantic versioning format', () => {
        // Semantic versioning: MAJOR.MINOR.PATCH with optional build metadata
        const semverRegex = /^\d+\.\d+\.\d+(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/;
        expect(APP_VERSION).toMatch(semverRegex);
    });

    it('should have numeric version components', () => {
        const parts = APP_VERSION.split('+')[0].split('.');
        expect(parts).toHaveLength(3);

        parts.forEach((part) => {
            expect(Number.isInteger(Number(part))).toBe(true);
            expect(Number(part)).toBeGreaterThanOrEqual(0);
        });
    });

    it('should carry the fork build metadata of the package version', () => {
        expect(APP_VERSION).toMatch(/^\d+\.\d+\.\d+\+extended\.\d+$/);
        expect(APP_VERSION).toBe(packageJson.version);
    });

    it('should not contain whitespace', () => {
        expect(APP_VERSION.trim()).toBe(APP_VERSION);
        expect(APP_VERSION).not.toContain(' ');
        expect(APP_VERSION).not.toContain('\t');
        expect(APP_VERSION).not.toContain('\n');
    });
});
