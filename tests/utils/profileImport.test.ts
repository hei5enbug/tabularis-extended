import { beforeEach, describe, expect, it, vi } from 'vitest';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

import {
  getOfficialProfileImportStatus,
  importOfficialProfile,
  profileImportErrorCode,
  remapConnectionsToPlusDrivers,
} from '../../src/utils/profileImport';

describe('profileImport', () => {
  beforeEach(() => invoke.mockReset());

  it('상태 명령을 인자 없이 호출한다', async () => {
    // given
    invoke.mockResolvedValue({ sourceFound: true });

    // when
    await getOfficialProfileImportStatus();

    // then
    expect(invoke).toHaveBeenCalledWith('official_profile_import_status');
  });

  it('가져오기와 재매핑 명령을 등록된 이름으로 호출한다', async () => {
    // given
    invoke.mockResolvedValue({});

    // when
    await importOfficialProfile();
    await remapConnectionsToPlusDrivers();

    // then
    expect(invoke.mock.calls.map((call) => call[0])).toEqual([
      'import_official_profile',
      'remap_connections_to_plus_drivers',
    ]);
  });

  it('거절된 명령의 오류에서 고정 코드를 읽는다', () => {
    // given
    const rejections = [
      'IMPORT_DESTINATION_NOT_EMPTY: The destination',
      new Error('IMPORT_SECRET_FAILED: x'),
      'something else',
      undefined,
    ];

    // when
    const codes = rejections.map(profileImportErrorCode);

    // then
    expect(codes).toEqual(['IMPORT_DESTINATION_NOT_EMPTY', 'IMPORT_SECRET_FAILED', null, null]);
  });
});
