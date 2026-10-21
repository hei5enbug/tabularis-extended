import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import type { PluginCredentialSlot, UsePluginServiceReturn } from "../packages/plugin-api/src/types";
import type { SlotContext as SdkSlotContext, TypedSlotProps } from "../packages/plugin-api/src/slots";
import type { SlotContext as HostSlotContext } from "../src/types/pluginSlots";
import type { ServiceRequest } from "@tabularis/service-contracts";
import { usePluginService } from "../src/hooks/usePluginService";

describe("credential SDK representation", () => {
  it("SDK와_host는_저장된_행_ordinal과_공통_credential_slot_타입을_공유한다", () => {
    // Given
    const slots: PluginCredentialSlot[] = ["db_password", "cosmos_account_key", "entra_client_secret", "entra_refresh_token"];
    const canonical: ServiceRequest<"credential.import">["input"]["slot"][] = slots;
    const context: SdkSlotContext = { resultId: "result-a", resultSetIndex: 0, resultGeneration: 3, resultRowOrdinal: 120 };
    const host: HostSlotContext = context;
    const typed: TypedSlotProps<"app.map.renderer">["context"] = host;
    const hook: () => UsePluginServiceReturn = usePluginService;
    // When
    const actual = { slots: canonical, sdk: context.resultRowOrdinal, host: host.resultRowOrdinal, typed: typed.resultRowOrdinal, hook };
    // Then
    expect(actual.slots).toEqual(slots);
    expect(actual.sdk).toBe(120);
    expect(actual.host).toBe(120);
    expect(actual.typed).toBe(120);
    expect(actual.hook).toBe(usePluginService);
  });

  it("SDK는_공통_slot을_참조하고_타입을_public_barrel에_노출한다", () => {
    // Given
    const files = ["packages/plugin-api/src/types.ts", "packages/plugin-api/src/index.ts"];
    // When
    const actual = files.map((file) => readFileSync(file, "utf8"));
    // Then
    expect(actual[0]).toContain('PluginCredentialSlot = ServiceRequest<"credential.import">["input"]["slot"]');
    expect(actual[0]).toContain("importCredential(connectionId: string, slot: PluginCredentialSlot");
    expect(actual[1]).toContain("PluginCredentialSlot,");
  });
});
