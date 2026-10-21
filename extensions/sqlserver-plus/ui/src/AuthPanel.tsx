import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { usePluginService } from "@tabularis/plugin-api";
import {
  AuthFault, AuthScope, boundedId, completed, failedStatusMessage, faultMessage, matchesSaved,
  object, parseStatus, POLL_DURATION_MS, POLL_INTERVAL_MS, POLL_LIMIT, validUuid,
  type AuthSettings, type AuthStatus, type EntraMode, type Persistence,
} from "./auth";

interface Props { connectionId?: string | null; form: AuthSettings }
type Readiness = { ready: boolean; message: string };

function initialReadiness(connectionId: Props["connectionId"], form: AuthSettings): Readiness {
  if (!boundedId(connectionId)) return { ready: false, message: "연결을 먼저 저장한 뒤 로그인해 주세요." };
  if (!validUuid(form.tenant) || !validUuid(form.client)) return { ready: false, message: "테넌트 ID와 클라이언트 ID에 올바른 UUID를 입력해 주세요." };
  return { ready: false, message: "저장된 인증 설정을 확인하고 있습니다." };
}

export default function AuthPanel({ connectionId, form }: Props) {
  const service = usePluginService();
  const scope = useMemo(() => new AuthScope(service, connectionId ?? ""), [service, connectionId]);
  const secret = useRef<HTMLInputElement>(null);
  const poll = useRef({ started: 0, count: 0 });
  const [readiness, setReadiness] = useState(() => initialReadiness(connectionId, form));
  const [persistence, setPersistence] = useState<Persistence>("session");
  const [credentialReady, setCredentialReady] = useState(false);
  const [status, setStatus] = useState<AuthStatus | null>(null);
  const [busy, setBusy] = useState<"begin" | "secret" | "cancel" | "logout" | null>(null);
  const [pollStopped, setPollStopped] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const principal = form.mode === "entra_service_principal";

  useEffect(() => {
    scope.activate();
    const generation = scope.current();
    if (boundedId(connectionId) && validUuid(form.tenant) && validUuid(form.client)) {
      void (async () => {
        try {
          const capabilities = await scope.run(() => service.capabilities(connectionId));
          const required = ["connection.list", "auth.begin", "auth.status", "auth.cancel", "auth.logout"];
          if (principal) required.push("credential.import");
          if (!object(capabilities) || capabilities.service_protocol !== 1 || !Array.isArray(capabilities.operations)
            || required.some((operation) => !capabilities.operations.includes(operation as typeof capabilities.operations[number]))) {
            throw new AuthFault("capability");
          }
          const response = await scope.call("connection.list", {});
          if (!matchesSaved(response.data, connectionId, form)) {
            if (scope.isCurrent(generation)) setReadiness({ ready: false, message: "현재 인증 설정을 연결에 저장한 뒤 로그인해 주세요." });
            return;
          }
          if (scope.isCurrent(generation)) setReadiness({ ready: true, message: "저장된 연결의 인증 설정을 확인했습니다." });
        } catch (failure) {
          if (scope.isCurrent(generation)) setReadiness({ ready: false, message: faultMessage(failure) });
        }
      })();
    }
    const input = secret.current;
    return () => {
      if (input) input.value = "";
      scope.dispose();
    };
  }, [connectionId, form.client, form.tenant, form.source, principal, form.mode, scope, service]);

  const applyStatus = useCallback((next: AuthStatus) => {
    setStatus(next);
    setError(next.state === "failed" ? failedStatusMessage(next.errorCode) : "");
    if (next.state === "authenticated") {
      scope.setJob(null);
      setMessage("로그인했습니다.");
    } else if (next.state === "cancelled") {
      scope.setJob(null);
      setMessage("로그인을 취소했습니다.");
    } else if (next.state === "failed") {
      scope.setJob(null);
      setMessage("");
      if (next.errorCode === "PERMISSION_DENIED") setReadiness({ ready: false, message: "이 연결의 인증을 관리할 권한이 없습니다." });
    } else setMessage(next.state === "interaction_required" ? "Microsoft 로그인 안내를 따라 인증을 완료해 주세요." : "로그인 결과를 기다리고 있습니다.");
  }, [scope]);

  const showFailure = useCallback((failure: unknown) => {
    setError(faultMessage(failure));
    if (failure instanceof AuthFault && failure.kind === "denied") {
      setReadiness({ ready: false, message: "이 연결의 인증을 관리할 권한이 없습니다." });
    }
  }, []);

  useEffect(() => {
    if (!status || !["pending", "interaction_required"].includes(status.state) || pollStopped || busy) return;
    const generation = scope.current();
    let ended = false;
    const timer = setTimeout(() => {
      void (async () => {
        if (poll.current.count >= POLL_LIMIT || Date.now() - poll.current.started >= POLL_DURATION_MS) {
          scope.cancelBestEffort(status.jobId);
          if (!ended && scope.isCurrent(generation)) {
            setPollStopped(true);
            setError("로그인 대기 시간이 끝났습니다. 취소 후 다시 시작해 주세요.");
          }
          return;
        }
        poll.current.count += 1;
        try {
          const response = await scope.call("auth.status", { auth_job_id: status.jobId });
          const next = parseStatus(response.data);
          if (next.jobId !== status.jobId) throw new AuthFault("malformed");
          if (!ended && scope.isCurrent(generation)) applyStatus(next);
        } catch (failure) {
          if (!ended && scope.isCurrent(generation)) {
            setPollStopped(true);
            showFailure(failure);
          }
        }
      })();
    }, POLL_INTERVAL_MS);
    return () => { ended = true; clearTimeout(timer); };
  }, [applyStatus, busy, pollStopped, scope, showFailure, status]);

  const start = async () => {
    if (!readiness.ready || busy || (principal && !credentialReady)) return;
    const previous = scope.stop();
    if (previous) scope.cancelBestEffort(previous);
    const generation = scope.current();
    setBusy("begin"); setError(""); setMessage(""); setStatus(null); setPollStopped(false);
    poll.current = { started: Date.now(), count: 0 };
    try {
      const response = await scope.call("auth.begin", { auth_mode: form.mode as EntraMode, persistence }, true);
      const next = parseStatus(response.data);
      if (scope.isCurrent(generation)) applyStatus(next);
    } catch (failure) {
      if (scope.isCurrent(generation)) showFailure(failure);
    } finally {
      if (scope.isCurrent(generation)) setBusy(null);
    }
  };

  const registerSecret = async () => {
    const input = secret.current;
    const value = input?.value ?? "";
    if (input) input.value = "";
    if (!readiness.ready || busy || value.length === 0) {
      if (readiness.ready && !busy) setError("클라이언트 비밀을 입력해 주세요.");
      return;
    }
    const generation = scope.current();
    setBusy("secret"); setError(""); setCredentialReady(false);
    try {
      await scope.run(async (signal) => completed(await service.importCredential(scope.connectionId, "entra_client_secret", value, { persistence, signal }), scope.connectionId));
      if (scope.isCurrent(generation)) { setCredentialReady(true); setMessage("클라이언트 비밀을 보호된 경로로 전달했습니다."); }
    } catch (failure) {
      if (scope.isCurrent(generation)) showFailure(failure);
    } finally {
      if (scope.isCurrent(generation)) setBusy(null);
    }
  };

  const cancel = async () => {
    const job = scope.stop();
    const generation = scope.current();
    setBusy("cancel"); setPollStopped(true); setMessage(""); setError("");
    if (job === null) {
      setStatus(null); setMessage("진행 중인 로그인 요청을 중단했습니다."); setBusy(null);
      return;
    }
    try {
      const response = await scope.call("auth.cancel", { auth_job_id: job });
      const next = parseStatus(response.data);
      if (next.jobId !== job) throw new AuthFault("malformed");
      if (scope.isCurrent(generation)) applyStatus(next);
    } catch (failure) {
      if (scope.isCurrent(generation)) { scope.setJob(job); showFailure(failure); }
    } finally {
      if (scope.isCurrent(generation)) setBusy(null);
    }
  };

  const logout = async () => {
    if (!readiness.ready) return;
    const job = scope.stop();
    if (job) scope.cancelBestEffort(job);
    const generation = scope.current();
    setBusy("logout"); setPollStopped(true); setStatus(null); setMessage(""); setError("");
    try {
      const response = await scope.call("auth.logout", {});
      if (!object(response.data) || response.data.logged_out !== true) throw new AuthFault("malformed");
      if (scope.isCurrent(generation)) { setMessage("로그아웃했습니다."); setCredentialReady(false); }
    } catch (failure) {
      if (scope.isCurrent(generation)) showFailure(failure);
    } finally {
      if (scope.isCurrent(generation)) setBusy(null);
    }
  };

  const pending = status?.state === "pending" || status?.state === "interaction_required";
  return <section aria-label="Microsoft Entra 인증" style={{ display: "grid", gap: 8 }}>
    <p role="status">{readiness.message}</p>
    <label>인증 정보 보관
      <select aria-label="인증 정보 보관" value={persistence} disabled={busy !== null || pending}
        onChange={(event) => { setPersistence(event.target.value as Persistence); setCredentialReady(false); }}>
        <option value="session">이번 앱 세션</option><option value="keychain">시스템 키체인</option>
      </select>
    </label>
    {principal && <>
      <label>클라이언트 비밀<input aria-label="클라이언트 비밀" type="password" ref={secret} autoComplete="new-password" disabled={!readiness.ready || busy !== null || pending} /></label>
      <button type="button" disabled={!readiness.ready || busy !== null || pending} onClick={() => void registerSecret()}>비밀 등록</button>
    </>}
    <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
      <button type="button" disabled={!readiness.ready || busy !== null || pending || (principal && !credentialReady)} onClick={() => void start()}>로그인</button>
      <button type="button" disabled={!readiness.ready || (busy !== null && busy !== "begin") || (!pending && busy !== "begin")} onClick={() => void cancel()}>로그인 취소</button>
      <button type="button" disabled={!readiness.ready || busy !== null} onClick={() => void logout()}>로그아웃</button>
    </div>
    {status?.challenge && (status.state === "interaction_required" || status.state === "pending") && <div>
      <p>로그인 코드: <strong>{status.challenge.code}</strong></p>
      {status.challenge.url ? <a href={status.challenge.url} target="_blank" rel="noopener noreferrer">Microsoft 로그인 페이지 열기</a>
        : <p>안전한 로그인 주소를 확인할 수 없습니다. 로그인을 취소해 주세요.</p>}
    </div>}
    {message && <p role="status">{message}</p>}
    {error && <p role="alert">{error}</p>}
  </section>;
}
