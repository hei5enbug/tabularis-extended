import { describe, expect, it } from "vitest";
import {
  connectionChangeRows,
  isLockedApproval,
  parseServiceApprovalRequest,
} from "../../src/utils/serviceApproval";

describe("serviceApproval", () => {
  describe("isLockedApproval", () => {
    it("수정 불가 표시나 서비스 표시가 있는 승인만 잠근다", () => {
      // given
      const approvals = [
        { editable: false, queryKind: "write" },
        {
          queryKind: "service:connection.update",
          service: { operation: "connection.update", requestHash: "h", credentialRetarget: false },
        },
        { editable: true, queryKind: "write" },
        { queryKind: "ddl" },
      ];

      // when
      const actual = approvals.map(isLockedApproval);

      // then
      expect(actual).toEqual([true, true, false, false]);
    });
  });

  describe("parseServiceApprovalRequest", () => {
    it("서비스 요청 요약에서 작업, 대상 연결, 자격 증명 재지정 여부를 읽는다", () => {
      // given
      const approval = {
        query: JSON.stringify({
          operation: "connection.update",
          input: { patch: { settings: { host: "elsewhere.example" } } },
          connection_id: "fixture",
          credential_retarget: true,
        }),
        queryKind: "service:connection.update",
        connectionId: "fixture",
      };

      // when
      const actual = parseServiceApprovalRequest(approval);

      // then
      expect(actual).toEqual({
        operation: "connection.update",
        input: { patch: { settings: { host: "elsewhere.example" } } },
        connectionId: "fixture",
        credentialRetarget: true,
      });
    });

    it("JSON이 아닌 승인 내용은 서비스 요청으로 읽지 않는다", () => {
      // given
      const approval = { query: "DELETE FROM users", queryKind: "write", connectionId: "c1" };

      // when
      const actual = parseServiceApprovalRequest(approval);

      // then
      expect(actual).toBeNull();
    });
  });

  describe("connectionChangeRows", () => {
    it("연결 생성 요청의 모든 설정을 점 경로로 펼친다", () => {
      // given
      const request = {
        operation: "connection.create",
        input: {
          name: "Created",
          driver: "postgresql-plus",
          settings: { host: "localhost", port: 5432, extra: { auth_mode: "sql_password" } },
          credential_ref: null,
        },
        connectionId: null,
        credentialRetarget: false,
      };

      // when
      const actual = connectionChangeRows(request);

      // then
      expect(actual).toEqual([
        { field: "name", value: "Created" },
        { field: "driver", value: "postgresql-plus" },
        { field: "settings.host", value: "localhost" },
        { field: "settings.port", value: "5432" },
        { field: "settings.extra.auth_mode", value: "sql_password" },
      ]);
    });

    it("연결 수정 요청은 patch 항목만 펼치고 다른 작업은 null을 돌려준다", () => {
      // given
      const update = {
        operation: "connection.update",
        input: { patch: { name: "Renamed", settings: { ssl_mode: "require" } } },
        connectionId: "c1",
        credentialRetarget: false,
      };
      const rowInsert = { ...update, operation: "row.insert" };

      // when
      const actual = [connectionChangeRows(update), connectionChangeRows(rowInsert)];

      // then
      expect(actual).toEqual([
        [
          { field: "name", value: "Renamed" },
          { field: "settings.ssl_mode", value: "require" },
        ],
        null,
      ]);
    });
  });
});
