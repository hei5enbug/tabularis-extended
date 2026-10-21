import { expect, test } from "vitest";
import { boundedId, completed, matchesSaved, parseStatus, savedSettings, settings, validUuid } from "./auth";
import { CONNECTION_ID, TENANT, CLIENT, authData, response, savedConnection } from "./testFixtures";

test.each([
  ["기존 통합 인증", { integrated_auth: "true" }, "integrated"],
  ["기존 암호 인증", {}, "sql_password"],
  ["명시적 Entra 사용자", { auth_mode: "entra_user", integrated_auth: "true" }, "entra_user"],
  ["알 수 없는 방식", { auth_mode: "other" }, null],
])("%s 설정을 안전한 인증 모드로 해석한다", (_title, extra, expected) => {
  // given
  const input = extra as Record<string, string>;
  // when
  const actual = settings(input);
  // then
  expect(actual.mode).toBe(expected);
});

test.each([
  ["올바른 UUID", TENANT, true], ["잘못된 UUID", "tenant", false],
  ["공백 포함 UUID", ` ${TENANT}`, false], ["빈 UUID", "", false],
])("%s를 검증한다", (_title, input, expected) => {
  // given
  const value = input as string;
  // when
  const actual = validUuid(value);
  // then
  expect(actual).toBe(expected);
});

test.each([
  ["정상 ID", "saved-id", true], ["빈 ID", "", false], ["공백 ID", " ", false],
  ["긴 ID", "x".repeat(129), false], ["제어 문자 ID", "a\u0085", false],
])("%s의 길이와 제어 문자를 검증한다", (_title, input, expected) => {
  // given
  const value = input;
  // when
  const actual = boundedId(value);
  // then
  expect(actual).toBe(expected);
});

test("저장된 인증 설정은 extra 값이 있으면 직접 필드보다 우선한다", () => {
  // given
  const input = { auth_mode: "sql_password", tenant_id: "other", extra: { auth_mode: "entra_user", tenant_id: TENANT, client_id: CLIENT } };
  // when
  const actual = savedSettings(input);
  // then
  expect(actual).toEqual({ mode: "entra_user", tenant: TENANT, client: CLIENT });
});

test.each([
  ["다른 연결", { connections: [savedConnection("other")] }],
  ["다른 드라이버", { connections: [{ ...savedConnection(), driver: "postgresql" }] }],
  ["중복 연결 ID", { connections: [savedConnection(), savedConnection()] }],
  ["저장 안 된 테넌트", { connections: [{ ...savedConnection(), settings: { auth_mode: "entra_user", tenant_id: "other", client_id: CLIENT } }] }],
  ["잘못된 저장 필드", { connections: [{ ...savedConnection(), settings: { auth_mode: 1 } }] }],
])("%s 설정으로 인증을 시작할 수 없다", (_title, data) => {
  // given
  const form = { mode: "entra_user" as const, tenant: TENANT, client: CLIENT };
  // when
  const actual = matchesSaved(data, CONNECTION_ID, form);
  // then
  expect(actual).toBe(false);
});

test("challenge 없는 인증 상태는 정상 응답이다", () => {
  // given
  const input = authData("pending");
  // when
  const actual = parseStatus(input);
  // then
  expect(actual.state).toBe("pending");
  expect(actual.challenge).toBeNull();
});

test.each([
  ["다른 호스트", "https://evil.example/devicelogin"],
  ["명시적 포트", "https://microsoft.com:443/devicelogin"],
  ["사용자 정보", "https://user@microsoft.com/devicelogin"],
  ["쿼리 문자열", "https://microsoft.com/devicelogin?x=1"],
  ["주소 조각", "https://microsoft.com/devicelogin#x"],
  ["HTTP 주소", "http://microsoft.com/devicelogin"],
])("%s 로그인 주소에는 링크를 만들지 않는다", (_title, url) => {
  // given
  const input = { ...authData("interaction_required"), challenge: { verification_uri: url, user_code: "ABCD-EFGH" } };
  // when
  const actual = parseStatus(input);
  // then
  expect(actual.challenge?.url).toBeNull();
});

test.each([
  ["긴 코드", "x".repeat(129)], ["제어 문자 코드", "CODE\n"], ["빈 코드", ""], ["숫자 코드", 1234],
])("%s가 있는 challenge를 거부한다", (_title, code) => {
  // given
  const input = { ...authData("interaction_required"), challenge: { verification_uri: "https://microsoft.com/devicelogin", user_code: code } };
  // when
  const action = () => parseStatus(input);
  // then
  expect(action).toThrow("malformed");
});

test.each([
  ["결과 미확정", { status: "outcome_unknown" }],
  ["부분 적용 실패", { status: "failed", error: { code: "AUTH_FAILED", message: "SECRET", outcome: "applied" } }],
  ["다른 연결 응답", { connection_id: "other" }],
  ["다른 요청 응답", { request_id: "other" }],
  ["잘린 응답", { limits: { truncated: true, reasons: ["bytes"] } }],
])("%s를 인증 성공으로 수용하지 않는다", (_title, overrides) => {
  // given
  const input = { ...response("request", authData("authenticated")), ...overrides };
  // when
  const action = () => completed(input, CONNECTION_ID, "request");
  // then
  expect(action).toThrow();
});
