/**
 * loov: Loov 是否接管了本机 CLI 配置（有登录前快照或已登录）。
 * 接管期间隐藏官方直连 / 订阅类入口（docs/loov/dev-01-preset-cleanup.md 第四节）。
 * 不依赖 react-query：上游不少组件测试没有 QueryClientProvider。查询失败一律按「未接管」处理。
 */
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

let cached: boolean | undefined;
const listeners = new Set<(v: boolean) => void>();

export async function refreshLoovManaged(): Promise<boolean> {
  try {
    cached = Boolean(await invoke<boolean>("loov_managed"));
  } catch {
    cached = false;
  }
  listeners.forEach((fn) => fn(cached!));
  return cached;
}

export function useLoovManagedFlag(): boolean {
  const [value, setValue] = useState<boolean>(cached ?? false);
  useEffect(() => {
    listeners.add(setValue);
    if (cached === undefined) void refreshLoovManaged();
    else setValue(cached);
    return () => {
      listeners.delete(setValue);
    };
  }, []);
  return value;
}

const BLOCKED_PROVIDER_TYPES = new Set(["github_copilot", "codex_oauth", "xai_oauth"]);

/** 官方直连 / 订阅类预设（会让 CLI 绕开公司 API）。 */
export function isLoovBlockedPreset(preset: unknown): boolean {
  if (!preset || typeof preset !== "object") return false;
  const p = preset as { category?: unknown; providerType?: unknown };
  return p.category === "official" || (typeof p.providerType === "string" && BLOCKED_PROVIDER_TYPES.has(p.providerType));
}

/** 已入库的官方直连 / 订阅类供应商（与 Rust 侧 loov::restore::blocked_provider 一致）。 */
export function isLoovBlockedProvider(provider: unknown): boolean {
  if (!provider || typeof provider !== "object") return false;
  const p = provider as {
    category?: unknown;
    meta?: { providerType?: unknown } | null;
    settingsConfig?: { env?: Record<string, unknown> } | null;
  };
  if (p.category === "official") return true;
  const t = p.meta?.providerType;
  if (typeof t === "string" && BLOCKED_PROVIDER_TYPES.has(t)) return true;
  const base = p.settingsConfig?.env?.ANTHROPIC_BASE_URL;
  return typeof base === "string" && base.includes("githubcopilot.com");
}

/**
 * 接管期间不需要处理的系统环境变量：Claude Code 的 settings.json 里已写入同名键（地址、令牌、模型档位）
 * 或已被中和（Rust 侧 loov::provision::neutralize_claude_env 把协议选择器置 0，Vertex/Bedrock/Foundry
 * 专属变量随之失效）；Codex 走自定义 provider + 令牌，不读 OPENAI_*。Gemini / Grok 不归 Loov 管。
 */
const LOOV_INERT_ENV = /^(ANTHROPIC_(BASE_URL|AUTH_TOKEN|API_KEY|MODEL|SMALL_FAST_MODEL|DEFAULT_\w+|VERTEX_\w+|BEDROCK_\w+|FOUNDRY_\w+)|OPENAI_\w+|GEMINI_\w+|GOOGLE_GEMINI_\w+|XAI_API_KEY|GROK_DEFAULT_MODEL)$/i;

/**
 * 员工自己的云厂商配置（Vertex / Bedrock / Foundry）：不用 Loov 时他们的 Claude Code 靠它直连，
 * 用 Loov 时被 settings.json 里的中和值压住、不必改动。无论是否接管都不当成冲突，更不能提示删除。
 */
const OWN_CLOUD_ENV = /^ANTHROPIC_(VERTEX|BEDROCK|FOUNDRY)_\w+$/i;

/** 接管期间只保留仍可能影响 Loov 链路的冲突（如 ANTHROPIC_CUSTOM_HEADERS）；员工自己的云厂商变量始终不提示。 */
export function filterLoovEnvConflicts<T extends { varName: string }>(conflicts: T[], managed: boolean): T[] {
  return conflicts.filter((c) => !OWN_CLOUD_ENV.test(c.varName) && !(managed && LOOV_INERT_ENV.test(c.varName)));
}

/** 测试用：重置缓存。 */
export function __resetLoovManagedForTest(value?: boolean) {
  cached = value;
}
