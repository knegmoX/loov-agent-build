import { renderHook, waitFor } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { afterEach, describe, expect, it } from "vitest";
import { providerPresets } from "@/config/claudeProviderPresets";
import { codexProviderPresets } from "@/config/codexProviderPresets";
import { claudeDesktopProviderPresets } from "@/config/claudeDesktopProviderPresets";
import { geminiProviderPresets } from "@/config/geminiProviderPresets";
import {
  __resetLoovManagedForTest,
  isLoovBlockedPreset,
  isLoovBlockedProvider,
  useLoovManagedFlag,
} from "@/lib/loovManaged";
import { server } from "../msw/server";

describe("isLoovBlockedPreset", () => {
  it("blocks official and subscription presets", () => {
    const names = (list: { name: string }[]) =>
      list.filter(isLoovBlockedPreset).map((p) => p.name);
    const claude = names(providerPresets);
    expect(claude).toContain("Claude Official");
    expect(claude.some((n) => /copilot/i.test(n))).toBe(true);
    const codex = names(codexProviderPresets);
    expect(codex).toContain("OpenAI Official");
    expect(codexProviderPresets.filter(isLoovBlockedPreset).some((p) => (p as { providerType?: string }).providerType === "codex_oauth")).toBe(true);
    expect(names(claudeDesktopProviderPresets).length).toBeGreaterThan(0);
    expect(names(geminiProviderPresets)).toContain("Google Official");
  });

  it("keeps ordinary third-party and aggregator presets", () => {
    const blocked = providerPresets.filter(isLoovBlockedPreset).length;
    expect(blocked).toBeLessThan(10);
    expect(isLoovBlockedPreset({ name: "x", category: "aggregator" })).toBe(false);
    expect(isLoovBlockedPreset({ name: "x", category: "third_party" })).toBe(false);
    expect(isLoovBlockedPreset(null)).toBe(false);
  });
});

describe("isLoovBlockedProvider", () => {
  it("matches stored official and subscription providers", () => {
    expect(isLoovBlockedProvider({ id: "claude-official", category: "official", settingsConfig: {} })).toBe(true);
    expect(isLoovBlockedProvider({ id: "x", meta: { providerType: "codex_oauth" }, settingsConfig: {} })).toBe(true);
    expect(isLoovBlockedProvider({ id: "x", settingsConfig: { env: { ANTHROPIC_BASE_URL: "https://api.githubcopilot.com" } } })).toBe(true);
    expect(isLoovBlockedProvider({ id: "universal-claude-loov", category: "aggregator", settingsConfig: { env: { ANTHROPIC_BASE_URL: "https://me.loov.cc" } } })).toBe(false);
    expect(isLoovBlockedProvider({ id: "default", category: "custom", settingsConfig: {} })).toBe(false);
  });
});

describe("useLoovManagedFlag", () => {
  afterEach(() => __resetLoovManagedForTest(undefined));

  it("reflects the backend managed flag", async () => {
    __resetLoovManagedForTest(undefined);
    server.use(http.post("http://tauri.local/loov_managed", () => HttpResponse.json(true)));
    const { result } = renderHook(() => useLoovManagedFlag());
    await waitFor(() => expect(result.current).toBe(true));
  });

  it("falls back to not managed when the command is unavailable", async () => {
    __resetLoovManagedForTest(undefined);
    server.use(http.post("http://tauri.local/loov_managed", () => new HttpResponse("nope", { status: 500 })));
    const { result } = renderHook(() => useLoovManagedFlag());
    await new Promise((r) => setTimeout(r, 50));
    expect(result.current).toBe(false);
  });
});
