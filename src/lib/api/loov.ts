/**
 * Loov Agent 账号 / 隧道 API（对应 src-tauri/src/loov/commands.rs）。
 */
import { invoke } from "@tauri-apps/api/core";

export type LoovStatus =
  | { state: "noCredentials" }
  | { state: "starting" }
  | { state: "offline"; reason: string }
  | { state: "reloginRequired"; code: string }
  | { state: "tunnelFailed"; reason: string }
  | {
      state: "ready";
      username: string;
      device_id: string;
      proxy_url: string;
      http_port: number;
      nodes: string[];
      route: "direct" | "proxy";
      via: string | null;
      net: LoovNetView;
    };

/** agent 权限档位：写成各家 agent 自己的原生设置（src-tauri/src/loov/safety.rs） */
export type LoovSafetyLevel = "careful" | "standard" | "full";

export interface LoovPrefs {
  node: string | null;
  mode: LoovNetMode;
  safety: LoovSafetyLevel;
}

/** 网络模式：仅 Loov 应用走通道 / 国外网站走通道 / 全部走通道 */
export type LoovNetMode = "direct" | "rule" | "global";

export interface LoovNodeView {
  id: string;
  name: string;
  latency_ms: number | null;
  error: string | null;
}

export interface LoovNetView {
  nodes: LoovNodeView[];
  active: string | null;
  /** 手选的节点；null = 自动 */
  pinned: string | null;
  /** 手选的节点不可用，这次回到了自动 */
  pin_fallback: boolean;
  mode: LoovNetMode;
  sys_proxy: string | null;
  sys_error: string | null;
}

/** 本账号的花费（hub /usage）：按应用汇总 new-api 实扣金额 + 云端搜索成本，以及月预算 */
export interface LoovUsage {
  range: "today" | "month";
  from: number;
  to: number;
  totalUsd: number;
  /** app：claude / codex / codexdesk / agy / grok / desktop / feishu；other 是老设备的通用 Key */
  byApp: { app: string; usd: number }[];
  search: { calls: number; bytes: number; usd: number };
  budgetUsd: number | null;
  monthUsd: number;
  budgetBlocked: boolean;
}

export interface LoovAccount {
  username: string;
  group: string;
  balanceUsd: number;
  usedUsd: number;
  topupUrl: string;
}

export interface LoovDevice {
  id: string;
  name: string;
  platform: string;
  createdAt: number;
  lastSeenAt: number | null;
  current: boolean;
}

export interface LoovModelEntry {
  id: string;
  kind: string;
  recommended: boolean;
  /** 近 30 分钟的健康度（hub 按 new-api 日志判断）；正常时没有 */
  health?: "degraded" | "down";
}

/** 模型故障时本地路由在同一网关内改用别的模型（src-tauri/src/loov/fallback.rs） */
export const LOOV_FALLBACK_EVENT = "loov-model-fallback";
export interface LoovFallbackNotice {
  from: string;
  to: string;
  app: string;
}

export interface LoovSession {
  username: string;
  device_id: string;
  topup_url: string;
  models: string[];
  model_catalog: LoovModelEntry[];
  api_base_url: string;
}

export interface LoovRestoreReport {
  snapshot: boolean;
  claude: boolean;
  codex: boolean;
}

export interface LoovMeta {
  hubVersion: string;
  announcement: string;
  client: { min?: string; latest?: string; downloadUrl?: string };
  maxDevices: number;
}

export interface ParsedLoovError {
  code: string;
  message: string;
  devices?: LoovDevice[];
}

/**
 * 后端错误统一是扁平的 `{ code, message, devices? }`（src-tauri/src/loov/flows.rs 的 LoovError）。
 * 也兼容 `Error`（message 是同样的 JSON，测试环境的 invoke mock 会这样包）和纯字符串。
 */
export function parseLoovError(err: unknown): ParsedLoovError {
  if (err instanceof Error) {
    try {
      return parseLoovError(JSON.parse(err.message));
    } catch {
      return { code: "UNKNOWN", message: err.message };
    }
  }
  if (typeof err === "string") return { code: "UNKNOWN", message: err };
  if (!err || typeof err !== "object") return { code: "UNKNOWN", message: String(err) };
  const e = err as Record<string, unknown>;
  return {
    code: typeof e.code === "string" ? e.code : "UNKNOWN",
    message: typeof e.message === "string" && e.message ? e.message : "请求失败",
    devices: Array.isArray(e.devices) ? (e.devices as LoovDevice[]) : undefined,
  };
}

/** 启动台上的四个应用（src-tauri/src/loov/apps.rs）。 */
export type LoovAppId = "claude" | "codex" | "agy" | "grok" | "claude-desktop" | "codex-desktop";

export interface LoovAppStatus {
  id: LoovAppId;
  /** 本平台能否使用（Claude Desktop 没有 Linux 版） */
  supported: boolean;
  installed: boolean;
  version: string | null;
  /** 当前模型（Grok Build 的模型单独存在它的供应商里） */
  model: string | null;
}

/** 安装进度事件 `loov-app-install`。percent 为空表示不确定进度（npm、安装脚本）。 */
export interface LoovInstallProgress {
  app: LoovAppId;
  stage: "download" | "install" | "configure" | "done";
  percent: number | null;
}

export const LOOV_INSTALL_EVENT = "loov-app-install";

/** 能给 Antigravity 当对话模型的 Gemini（与后端 provision::is_agent_gemini 同一规则）。 */
export function isAgentGemini(id: string): boolean {
  return id.startsWith("gemini-") && !["embedding", "banana", "image", "tts", "live", "veo"].some((k) => id.includes(k));
}

export const loovApi = {
  status: () => invoke<LoovStatus>("loov_status"),
  session: () => invoke<LoovSession | null>("loov_session"),
  login: (username: string, password: string, evictOldest = false) =>
    invoke<LoovStatus>("loov_login", { username, password, evictOldest }),
  register: (invite: string, username: string, password: string, evictOldest = false) =>
    invoke<LoovStatus>("loov_register", { invite, username, password, evictOldest }),
  logout: () => invoke<void>("loov_logout"),
  refresh: () => invoke<LoovStatus>("loov_refresh"),
  account: () => invoke<LoovAccount>("loov_account"),
  usage: (range: "today" | "month") => invoke<LoovUsage>("loov_usage", { range }),
  redeem: (code: string) => invoke<number>("loov_redeem", { code }),
  devices: () => invoke<LoovDevice[]>("loov_devices"),
  revokeDevice: (id: string) => invoke<void>("loov_revoke_device", { id }),
  meta: () => invoke<LoovMeta>("loov_meta"),
  managed: () => invoke<boolean>("loov_managed"),
  appVersion: () => invoke<string>("loov_app_version"),
  restoreOriginal: () => invoke<LoovRestoreReport>("loov_restore_original"),
  openExternal: (url: string) => invoke<boolean>("open_external", { url }),
  syncUniversal: (id: string) => invoke<boolean>("sync_universal_provider", { id }),
  appsStatus: () => invoke<LoovAppStatus[]>("loov_apps_status"),
  appInstall: (id: LoovAppId) => invoke<void>("loov_app_install", { id }),
  appLaunch: (id: LoovAppId, restart = false) => invoke<void>("loov_app_launch", { id, restart }),
  appSetModel: (id: LoovAppId, model: string) => invoke<void>("loov_app_set_model", { id, model }),
  setNode: (node: string | null) => invoke<LoovStatus>("loov_set_node", { node }),
  setNetMode: (mode: LoovNetMode) => invoke<LoovStatus>("loov_set_net_mode", { mode }),
  prefs: () => invoke<LoovPrefs>("loov_prefs"),
  setSafety: (level: LoovSafetyLevel) => invoke<void>("loov_set_safety", { level }),
};

/** 比较点分版本号（只看数字段，缺省按 0）：a<b 返回负数，相等 0，a>b 正数。 */
export function compareVersions(a: string, b: string): number {
  const pa = a.replace(/^v/, "").split(/[.+-]/).map((x) => parseInt(x, 10) || 0);
  const pb = b.replace(/^v/, "").split(/[.+-]/).map((x) => parseInt(x, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length, 3); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

/** 根据 hub /meta 判断当前客户端是否需要 / 建议升级。 */
export function upgradeState(
  current: string | undefined,
  client: LoovMeta["client"] | undefined,
): "required" | "available" | "none" {
  if (!current || !client) return "none";
  if (client.min && compareVersions(current, client.min) < 0) return "required";
  if (client.latest && compareVersions(current, client.latest) < 0) return "available";
  return "none";
}

export const LOOV_PROVIDER_ID = "loov";
export const LOOV_ADVANCED_KEY = "loov-advanced-mode";

/**
 * 打开客户端总是先到简洁页（启动台）：员工打开它就是为了马上用某个 agent。
 * 高级模式只在本次会话内有效，不再记住（旧版存的 `loov-advanced-mode` 一并清掉）。
 */
export function readAdvancedMode(): boolean {
  try {
    localStorage.removeItem(LOOV_ADVANCED_KEY);
  } catch {
    // ignore
  }
  return false;
}
