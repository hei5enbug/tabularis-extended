import { useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { JobStatus, ServiceRequest, ServiceResponse } from "@tabularis/service-contracts";
import type { PluginMapApplyRequest, PluginMapApplyAck, PluginCredentialSlot, UsePluginServiceReturn } from "../../packages/plugin-api/src/types";

export interface ServiceCapabilities {
  service_protocol: 1;
  operations: ServiceRequest["operation"][];
  spatial_v1: boolean;
  documents_v1: boolean;
  cancel_v1: boolean;
  gui_instance_id?: string;
}

const terminal = (status: JobStatus) => status !== "queued" && status !== "running";
const pause = () => new Promise<void>((resolve) => setTimeout(resolve, 50));

export function usePluginService() {
  return useMemo(() => {
    const service: UsePluginServiceReturn = {
    async executeWrite(request: ServiceRequest<"query.execute">, options?: { signal?: AbortSignal }): Promise<ServiceResponse> {
      if (options?.signal?.aborted) throw new DOMException("The request was aborted.", "AbortError");
      const clonedRequest = structuredClone(request);
      if (!clonedRequest || clonedRequest.protocol_version !== 1 || clonedRequest.operation !== "query.execute") throw new Error("A valid query write request is required.");
      for (const identity of [clonedRequest.connection_id, clonedRequest.request_id]) {
        if (typeof identity !== "string" || !identity.trim() || identity.length > 128 || identity.includes("\0")) throw new Error("A valid query write identity is required.");
      }
      const registered: unknown = await invoke("service_register_query_write_intent", { request: clonedRequest })
        .catch(() => { throw new Error("The query write intent could not be registered."); });
      if (registered !== true) throw new Error("The query write intent could not be registered.");
      if (options?.signal?.aborted) throw new DOMException("The request was aborted.", "AbortError");
      return service.call(clonedRequest, options);
    },
    async saveArtifact(artifactId: string): Promise<boolean> {
      if (typeof artifactId !== "string" || !artifactId.trim() || artifactId.length > 128 || artifactId.includes("\0")) throw new Error("A valid artifact identity is required.");
      const saved: unknown = await invoke("service_save_artifact", { artifactId });
      if (typeof saved !== "boolean") throw new Error("The artifact save response is invalid.");
      return saved;
    },
    async call(request: ServiceRequest, options?: { signal?: AbortSignal }): Promise<ServiceResponse> {
      if (options?.signal?.aborted) throw new DOMException("The request was aborted.", "AbortError");
      const duration = Math.min(120000, Math.max(1, request.deadline_ms ?? 30000));
      const deadline = performance.now() + duration + 2000;
      let jobId: string | null = null;
      let owner: string | null = null;
      let cancelRequested = false;
      const expired = Symbol("service deadline");
      const bounded = async <T,>(pending: Promise<T>): Promise<T> => {
        let timer: ReturnType<typeof setTimeout> | undefined;
        try {
          return await Promise.race([pending, new Promise<never>((_, reject) => {
            timer = setTimeout(() => reject(expired), Math.max(0, deadline - performance.now()));
          })]);
        } finally { if (timer !== undefined) clearTimeout(timer); }
      };
      const unknown = (error: ServiceResponse["error"] = null): ServiceResponse => ({
        protocol_version: 1, request_id: request.request_id, connection_id: request.connection_id ?? null,
        status: "outcome_unknown", job_id: jobId, result_id: null, data: null,
        page: { next_token: null, has_more: false, resume_mode: "none" },
        limits: { truncated: false, reasons: [] }, metrics: { elapsed_ms: 0, request_charge: null, retry_count: 0 }, warnings: [],
        error: error ? { ...error, outcome: "unknown" } : { code: "OUTCOME_UNKNOWN", message: "The service request deadline elapsed before its outcome was confirmed.", outcome: "unknown", retryable: false, details: null },
      });
      const control = (operation: "job.get" | "job.result" | "job.cancel"): ServiceRequest => ({
        protocol_version: 1, operation, request_id: crypto.randomUUID(), connection_id: request.connection_id,
        deadline_ms: Math.min(30000, Math.max(1, Math.floor(deadline - performance.now()))),
        input: { job_id: jobId!, owner_instance_id: owner! },
      });
      const cancel = () => {
        if (cancelRequested || !jobId || !owner) return;
        cancelRequested = true;
        void invoke<ServiceResponse>("service_call", { request: control("job.cancel") }).catch(() => undefined);
      };
      options?.signal?.addEventListener("abort", cancel);
      try {
        let response = await bounded(invoke<ServiceResponse>("service_call", { request }));
        if (terminal(response.status)) return response;
        jobId = response.job_id;
        const data = response.data;
        const candidate = data && typeof data === "object" && !Array.isArray(data) ? data.owner_instance_id : null;
        if (!jobId || typeof candidate !== "string") throw new Error("The service did not return a job owner.");
        owner = candidate;
        if (options?.signal?.aborted) cancel();
        for (;;) {
          const progress = await bounded(invoke<ServiceResponse>("service_call", { request: control("job.get") }));
          if (progress.error) return unknown(progress.error);
          const progressData = progress.data;
          const status = progressData && typeof progressData === "object" && !Array.isArray(progressData) ? progressData.status : null;
          if (typeof status !== "string") throw new Error("The service returned an invalid job status.");
          if (status !== "queued" && status !== "running") {
            response = await bounded(invoke<ServiceResponse>("service_call", { request: control("job.result") }));
            if (response.error) return unknown(response.error);
            const resultData = response.data;
            const result = resultData && typeof resultData === "object" && !Array.isArray(resultData) ? resultData.response : null;
            if (!result || typeof result !== "object" || Array.isArray(result)) throw new Error("The terminal job response is unavailable.");
            if (result.request_id !== request.request_id || result.connection_id !== (request.connection_id ?? null) || result.protocol_version !== 1 || result.status === "queued" || result.status === "running") throw new Error("The terminal job response does not match this request.");
            return result as unknown as ServiceResponse;
          }
          await bounded(pause());
        }
      } catch (error) {
        if (error !== expired) throw error;
        cancel();
        return unknown();
      } finally { options?.signal?.removeEventListener("abort", cancel); }
    },
    capabilities(connectionId: string): Promise<ServiceCapabilities> {
      return invoke("service_capabilities", { connectionId });
    },
    async importCredential(connectionId: string, slot: PluginCredentialSlot, secret: string, options: { persistence: "keychain" | "session"; signal?: AbortSignal }): Promise<ServiceResponse> {
      const requestId = crypto.randomUUID();
      try {
        if (options.signal?.aborted) throw new DOMException("The request was aborted.", "AbortError");
        const channel: unknown = await invoke("service_gui_secret_register", { connectionId, requestId, secret })
          .catch(() => { throw new Error("The credential input could not be registered."); });
        if (!channel || typeof channel !== "object" || Array.isArray(channel) || Object.keys(channel).length !== 2 || Object.keys(channel).some((key) => key !== "kind" && key !== "channel_id") ||
          !("kind" in channel) || channel.kind !== "gui_buffer" || !("channel_id" in channel) || typeof channel.channel_id !== "string" ||
          !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(channel.channel_id)) {
          throw new Error("The credential input could not be registered.");
        }
        const request: ServiceRequest<"credential.import"> = {
          protocol_version: 1, operation: "credential.import", request_id: requestId,
          connection_id: connectionId, session_id: null, map_id: null, expected_version: null, deadline_ms: 30000,
          input: { slot, input_channel: { kind: "gui_buffer", channel_id: channel.channel_id }, persistence: options.persistence },
        };
        return await service.call(request, { signal: options.signal });
      } finally {
        await invoke("service_gui_secret_discard", { requestId }).catch(() => undefined);
      }
    },
    async subscribeMap(handler: (request: PluginMapApplyRequest) => Promise<PluginMapApplyAck>): Promise<() => void> {
      let active = true;
      const subscriptionId = crypto.randomUUID();
      const pending = new Set<() => void>();
      const unlisten = await listen<PluginMapApplyRequest>("service-map-apply", ({ payload }) => {
        if (!active) return;
        const failure: PluginMapApplyAck = { state_version: payload.state_version, rendered_version: null, generation: payload.generation, gui_applied: false };
        let acknowledged = false;
        const acknowledge = (ack: PluginMapApplyAck) => {
          if (acknowledged) return;
          acknowledged = true;
          pending.delete(cancel);
          void invoke("service_map_ack", { mapId: payload.map_id, ack }).catch(() => undefined);
        };
        const cancel = () => acknowledge(failure);
        pending.add(cancel);
        void Promise.resolve().then(() => handler(payload)).then((ack) => {
          const valid = ack.state_version === payload.state_version && ack.generation === payload.generation &&
            typeof ack.gui_applied === "boolean" && (ack.rendered_version === null || (Number.isSafeInteger(ack.rendered_version) && ack.rendered_version >= 0)) &&
            (!ack.gui_applied || ack.rendered_version === payload.state_version);
          acknowledge(valid ? { state_version: ack.state_version, rendered_version: ack.rendered_version, generation: ack.generation, gui_applied: ack.gui_applied } : failure);
        }).catch(cancel);
      });
      try {
        await invoke("service_map_renderer", { subscriptionId, ready: true });
      } catch (error) {
        active = false;
        unlisten();
        for (const cancel of pending) cancel();
        throw error;
      }
      return () => {
        if (!active) return;
        active = false;
        unlisten();
        for (const cancel of pending) cancel();
        void invoke("service_map_renderer", { subscriptionId, ready: false }).catch(() => undefined);
      };
    },
    };
    return service;
  }, []);
}
