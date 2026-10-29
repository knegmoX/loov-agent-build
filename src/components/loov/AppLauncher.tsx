/**
 * Loov 启动台：Claude Code / Codex / Antigravity / Grok Build 四个终端 agent，Claude Desktop / Codex 桌面版两个桌面应用。
 *
 * 员工打开 Loov 就是为了马上用某个 agent，所以整张卡片就是启动按钮：装了就启动，没装就地安装
 * （图标外圈显示进度），装完自动启动。卡片上的小下拉是这个应用的默认模型。
 * 四个应用都经 Loov 走 new-api 计费（后端 src-tauri/src/loov/apps.rs），员工不需要任何配置。
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { Download, Loader2, Monitor } from "lucide-react";
import { AppGlyph } from "@/components/shell/AppGlyph";
import type { AppId } from "@/lib/api";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/lib/toast";
import { cn } from "@/lib/utils";
import { universalProvidersApi } from "@/lib/api";
import {
  isAgentGemini,
  loovApi,
  parseLoovError,
  LOOV_INSTALL_EVENT,
  LOOV_PROVIDER_ID,
  type LoovAppId,
  type LoovAppStatus,
  type LoovInstallProgress,
  type LoovModelEntry,
} from "@/lib/api/loov";
import { loovKeys, useLoovApps } from "@/lib/query/loov";

const ORDER: LoovAppId[] = ["claude", "codex", "agy", "grok", "claude-desktop", "codex-desktop"];
const NODE_DOWNLOAD_URL = "https://nodejs.org/zh-cn/download";

/** 品牌名不翻译 */
const NAME: Record<LoovAppId, string> = {
  claude: "Claude Code",
  codex: "Codex",
  agy: "Antigravity",
  grok: "Grok Build",
  "claude-desktop": "Claude Desktop",
  "codex-desktop": "Codex 桌面版",
};
/** 图标沿用上游的应用图标；Codex 桌面版借 Codex 的图标再加桌面角标 */
const GLYPH: Record<LoovAppId, AppId> = {
  claude: "claude",
  codex: "codex",
  agy: "agy",
  grok: "grokbuild",
  "claude-desktop": "claude-desktop",
  "codex-desktop": "codex",
};

/** 模型存在哪：前三个在 Loov 统一供应商里，Grok 单独存（后端 loov_app_set_model） */
type ModelSlot = "claude" | "codex" | "gemini" | "grok";
const SLOT_OF: Partial<Record<LoovAppId, ModelSlot>> = {
  claude: "claude",
  codex: "codex",
  agy: "gemini",
  grok: "grok",
};
/** 卡片下方的小字说明（替代模型下拉） */
const NOTE: Partial<Record<LoovAppId, string>> = {
  "claude-desktop": "loov.apps.desktopNote",
  "codex-desktop": "loov.apps.codexDesktopNote",
};
/** 安装包很大（Codex 桌面版约 1GB），安装时提示员工耐心等 */
const BIG_DOWNLOAD: LoovAppId[] = ["codex-desktop"];

type Busy =
  | { kind: "install"; stage: LoovInstallProgress["stage"]; percent: number | null }
  | { kind: "launch" };

interface AppLauncherProps {
  models: string[];
  catalog: LoovModelEntry[];
  tunnelReady: boolean;
}

export function AppLauncher({ models, catalog, tunnelReady }: AppLauncherProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { data: apps } = useLoovApps(true);
  const [busy, setBusy] = useState<Partial<Record<LoovAppId, Busy>>>({});
  // 要重启才能接入 Loov 的桌面应用（Claude Desktop / Codex 桌面版）
  const [restartAsk, setRestartAsk] = useState<LoovAppId | null>(null);
  // 启动后延迟清 busy 的定时器（每个应用一个）：再次启动时先取消上一次的，免得把新的 busy 提前清掉
  const busyTimers = useRef<Partial<Record<LoovAppId, ReturnType<typeof setTimeout>>>>({});
  const picks = useModelPicks();

  const lists = useModelLists(models, catalog);

  // 安装进度事件：只更新正在安装的那张卡
  useEffect(() => {
    const timers = busyTimers.current;
    return () => Object.values(timers).forEach((t) => clearTimeout(t));
  }, []);

  useEffect(() => {
    const off = listen<LoovInstallProgress>(LOOV_INSTALL_EVENT, (e) => {
      const { app, stage, percent } = e.payload;
      setBusy((b) => (b[app]?.kind === "install" ? { ...b, [app]: { kind: "install", stage, percent } } : b));
    });
    return () => {
      void off.then((f) => f());
    };
  }, []);

  const setAppBusy = (id: LoovAppId, value: Busy | undefined) =>
    setBusy((b) => {
      const next = { ...b };
      if (value) next[id] = value;
      else delete next[id];
      return next;
    });

  const launch = async (id: LoovAppId, restart = false) => {
    clearTimeout(busyTimers.current[id]);
    setAppBusy(id, { kind: "launch" });
    try {
      await loovApi.appLaunch(id, restart);
      toast.success(t("loov.apps.launched", { name: NAME[id] }));
    } catch (e) {
      const err = parseLoovError(e);
      if (err.code === "DESKTOP_RESTART_REQUIRED") setRestartAsk(id);
      else toast.error(err.message);
    } finally {
      // 终端 / 应用窗口要一两秒才出来，状态多留一会儿，免得员工以为没反应又点一次
      busyTimers.current[id] = setTimeout(() => setAppBusy(id, undefined), 1200);
    }
  };

  const install = async (id: LoovAppId) => {
    setAppBusy(id, { kind: "install", stage: "install", percent: null });
    try {
      await loovApi.appInstall(id);
      await qc.invalidateQueries({ queryKey: loovKeys.apps });
      toast.success(t("loov.apps.installed", { name: NAME[id] }));
      // 直接切到启动态，中间不闪一下「未安装」
      if (tunnelReady) void launch(id);
      else setAppBusy(id, undefined);
    } catch (e) {
      setAppBusy(id, undefined);
      const err = parseLoovError(e);
      if (err.code === "NODE_REQUIRED") {
        toast.error(err.message, {
          duration: 10_000,
          action: { label: t("loov.apps.getNode"), onClick: () => void loovApi.openExternal(NODE_DOWNLOAD_URL) },
        });
      } else {
        toast.error(err.message, { duration: 8_000 });
      }
    }
  };

  const byId = new Map((apps ?? []).map((a) => [a.id, a]));

  return (
    <>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
        {ORDER.map((id) => {
          const slot = SLOT_OF[id];
          const status = byId.get(id);
          return (
            <AppTile
              key={id}
              id={id}
              status={status}
              busy={busy[id]}
              tunnelReady={tunnelReady}
              onLaunch={() => void launch(id)}
              onInstall={() => void install(id)}
              model={slot === "grok" ? (status?.model ?? undefined) : slot ? picks.value[slot] : undefined}
              modelOptions={slot ? lists[slot] : undefined}
              onModelChange={
                slot === "grok"
                  ? (m) =>
                      void loovApi
                        .appSetModel(id, m)
                        .then(() => {
                          toast.success(t("loov.modelSaved"));
                          void qc.invalidateQueries({ queryKey: loovKeys.apps });
                        })
                        .catch((e) => toast.error(parseLoovError(e).message))
                  : slot
                    ? (m) => void picks.save(slot, m)
                    : undefined
              }
              note={NOTE[id] ? t(NOTE[id]!) : undefined}
              installHint={BIG_DOWNLOAD.includes(id) ? t("loov.apps.bigDownload") : undefined}
            />
          );
        })}
      </div>

      <Dialog open={restartAsk !== null} onOpenChange={(open) => !open && setRestartAsk(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("loov.apps.restartTitle", { name: restartAsk ? NAME[restartAsk] : "" })}</DialogTitle>
            <DialogDescription>{t("loov.apps.restartDesc", { name: restartAsk ? NAME[restartAsk] : "" })}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="neutral" onClick={() => setRestartAsk(null)}>
              {t("loov.apps.restartLater")}
            </Button>
            <Button
              variant="solid"
              onClick={() => {
                const id = restartAsk;
                setRestartAsk(null);
                if (id) void launch(id, true);
              }}
            >
              {t("loov.apps.restartNow")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

// ───────────────────────────── 卡片 ─────────────────────────────

interface AppTileProps {
  id: LoovAppId;
  status: LoovAppStatus | undefined;
  busy: Busy | undefined;
  tunnelReady: boolean;
  onLaunch: () => void;
  onInstall: () => void;
  model?: string;
  modelOptions?: string[];
  onModelChange?: (model: string) => void;
  note?: string;
  installHint?: string;
}

function AppTile({
  id,
  status,
  busy,
  tunnelReady,
  onLaunch,
  onInstall,
  model,
  modelOptions,
  onModelChange,
  note,
  installHint,
}: AppTileProps) {
  const { t } = useTranslation();
  const name = NAME[id];
  const checking = !status;
  const unsupported = status?.supported === false;
  const installed = !!status?.installed;
  const disabled = checking || unsupported || !!busy;

  const line = (() => {
    if (busy?.kind === "install") {
      if (busy.stage === "download")
        return busy.percent != null ? t("loov.apps.downloading", { percent: Math.round(busy.percent) }) : t("loov.apps.downloadingPlain");
      if (busy.stage === "configure") return t("loov.apps.configuring");
      return t("loov.apps.installing");
    }
    if (busy?.kind === "launch") return t("loov.apps.launching");
    if (checking) return t("loov.apps.checking");
    if (unsupported) return t("loov.apps.unsupported");
    if (!installed) return t("loov.apps.notInstalled");
    if (!tunnelReady) return t("loov.apps.waitTunnel");
    return t("loov.apps.ready");
  })();

  const action = installed ? onLaunch : onInstall;
  const label = installed ? t("loov.apps.launchAria", { name }) : t("loov.apps.installAria", { name });

  return (
    <div
      className={cn(
        "group relative flex min-h-[116px] flex-col justify-between rounded-xl border border-border bg-surface p-4 transition-[border-color,box-shadow,transform] duration-200",
        !disabled && "hover:-translate-y-0.5 hover:border-border-strong hover:shadow-v7-md",
        unsupported && "opacity-55",
        busy?.kind === "install" && "border-action",
      )}
    >
      {/* 整张卡片是一个按钮；模型下拉浮在它上面（不嵌套交互元素） */}
      <button
        type="button"
        aria-label={label}
        disabled={disabled}
        onClick={action}
        className="absolute inset-0 rounded-xl focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-default"
      />
      <div className="pointer-events-none flex items-start justify-between gap-3">
        <GlyphWithProgress id={id} busy={busy} />
        {!installed && !unsupported && !busy && !checking && (
          <span className="flex items-center gap-1 rounded-full bg-subtle px-2 py-0.5 text-caption text-fg-2">
            <Download className="h-3 w-3" />
            {t("loov.apps.install")}
          </span>
        )}
      </div>
      <div className="pointer-events-none mt-3 min-w-0">
        <div className="truncate font-semibold leading-tight">{name}</div>
        <div
          className={cn(
            "mt-0.5 flex items-center gap-1 truncate text-caption tabular-nums",
            busy ? "text-action-text" : "text-fg-3",
          )}
          aria-live="polite"
        >
          {installed && !busy && tunnelReady && <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-success" />}
          {line}
        </div>
      </div>
      {busy?.kind === "install" && installHint && (
        <p className="pointer-events-none relative mt-2 text-caption leading-snug text-fg-3">{installHint}</p>
      )}
      {installed && note && !busy && (
        <p className="pointer-events-none relative mt-2 text-caption leading-snug text-fg-3">{note}</p>
      )}
      {installed && modelOptions && modelOptions.length > 0 && onModelChange && (
        <div className="relative z-10 mt-2">
          <Select value={model || undefined} onValueChange={onModelChange} disabled={!!busy}>
            <SelectTrigger
              aria-label={t("loov.apps.modelFor", { name })}
              className="h-7 w-full justify-between rounded-md border-transparent bg-subtle px-2 text-caption text-fg-2 hover:text-fg-1"
            >
              <SelectValue placeholder={t("loov.pickModel")} />
            </SelectTrigger>
            <SelectContent>
              {modelOptions.map((m) => (
                <SelectItem key={m} value={m}>
                  {m}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )}
    </div>
  );
}

/** 应用图标；安装中外圈是进度环（有百分比就按比例，没有就转圈），启动中轻微呼吸。 */
function GlyphWithProgress({ id, busy }: { id: LoovAppId; busy: Busy | undefined }) {
  const size = 44;
  const r = 20;
  const c = 2 * Math.PI * r;
  const installing = busy?.kind === "install";
  const percent = installing ? busy.percent : null;
  const determinate = installing && percent != null && busy.stage === "download";
  return (
    <span className="relative inline-flex items-center justify-center" style={{ width: size, height: size }}>
      <span
        className={cn(
          "flex h-10 w-10 items-center justify-center rounded-[12px] bg-subtle transition-transform",
          busy?.kind === "launch" && "animate-pulse",
        )}
      >
        <AppGlyph app={GLYPH[id]} size={24} badgeClassName="bg-subtle" />
        {id === "codex-desktop" && (
          <span className="absolute bottom-0.5 right-0.5 flex h-[13px] w-[13px] items-center justify-center rounded-[3px] bg-subtle text-fg-2">
            <Monitor className="h-[10px] w-[10px]" strokeWidth={3} />
          </span>
        )}
      </span>
      {installing && (
        <svg
          className={cn("absolute inset-0 -rotate-90", !determinate && "animate-spin [animation-duration:1.1s]")}
          width={size}
          height={size}
          viewBox={`0 0 ${size} ${size}`}
          aria-hidden="true"
        >
          <circle cx={size / 2} cy={size / 2} r={r} fill="none" strokeWidth="2.5" className="stroke-border" />
          <circle
            cx={size / 2}
            cy={size / 2}
            r={r}
            fill="none"
            strokeWidth="2.5"
            strokeLinecap="round"
            className="stroke-action transition-[stroke-dashoffset] duration-300"
            strokeDasharray={c}
            strokeDashoffset={determinate ? c * (1 - (percent ?? 0) / 100) : c * 0.72}
          />
        </svg>
      )}
      {busy?.kind === "launch" && (
        <Loader2 className="absolute -bottom-1 -right-1 h-4 w-4 animate-spin rounded-full bg-surface p-0.5 text-action" />
      )}
    </span>
  );
}

// ───────────────────────────── 模型 ─────────────────────────────

function useModelLists(models: string[], catalog: LoovModelEntry[]): Record<ModelSlot, string[]> {
  return useMemo(() => {
    const kindOf = (id: string) => {
      const k = catalog.find((m) => m.id === id)?.kind;
      if (k) return k;
      if (id.startsWith("claude-")) return "anthropic";
      if (id.startsWith("gemini-")) return "gemini";
      if (id.startsWith("gpt-") || /^o\d/.test(id) || id.startsWith("codex")) return "openai";
      return "other";
    };
    const ids = catalog.length ? catalog.map((m) => m.id) : models;
    const anthropic = ids.filter((m) => kindOf(m) === "anthropic");
    const openai = ids.filter((m) => kindOf(m) === "openai");
    const gemini = ids.filter((m) => kindOf(m) === "gemini" && isAgentGemini(m));
    return {
      claude: anthropic,
      // Grok Build：new-api 上的 Claude / GPT / Gemini 都能用（Gemini 由本地路由转成 Chat Completions）
      grok: [...anthropic, ...openai, ...gemini],
      // Codex 走 new-api 原生 /v1/responses，Claude 模型同样可用（GPT 渠道故障时可切过去）；OpenAI 模型排前面
      codex: [...openai, ...anthropic],
      gemini,
    };
  }, [models, catalog]);
}

/** 三个 CLI 的默认模型，存在 Loov 统一供应商里，改完同步到各子供应商。 */
function useModelPicks() {
  const { t } = useTranslation();
  const [value, setValue] = useState<Partial<Record<Exclude<ModelSlot, "grok">, string>>>({});

  useEffect(() => {
    let alive = true;
    universalProvidersApi
      .get(LOOV_PROVIDER_ID)
      .then((p) => {
        if (!alive || !p) return;
        setValue({
          claude: p.models.claude?.model ?? "",
          codex: p.models.codex?.model ?? "",
          gemini: p.models.gemini?.model ?? "",
        });
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  const save = async (slot: Exclude<ModelSlot, "grok">, model: string) => {
    setValue((v) => ({ ...v, [slot]: model }));
    try {
      const p = await universalProvidersApi.get(LOOV_PROVIDER_ID);
      if (!p) return;
      if (slot === "claude") {
        p.models.claude = { ...(p.models.claude ?? {}), model, sonnetModel: model };
      } else if (slot === "codex") {
        p.models.codex = { ...(p.models.codex ?? {}), model };
      } else {
        p.models.gemini = { ...(p.models.gemini ?? {}), model };
        p.apps.gemini = true;
      }
      await universalProvidersApi.upsert(p);
      await loovApi.syncUniversal(LOOV_PROVIDER_ID);
      toast.success(t("loov.modelSaved"));
    } catch (e) {
      toast.error(parseLoovError(e).message);
    }
  };

  return { value, save };
}
