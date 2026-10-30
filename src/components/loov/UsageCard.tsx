/**
 * 本账号的花费：本月 / 今日、月预算进度、按应用的占比、云端搜索成本。
 * 数字来自 hub /usage（按每个应用单独的 Key 查 new-api 实扣金额，见 src-tauri/src/loov/attribution.rs），
 * 与 new-api 后台一致；本地「用量统计」页是按 token 估算的实时明细。
 */
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronUp } from "lucide-react";
import { cn } from "@/lib/utils";
import { useLoovUsage } from "@/lib/query/loov";

/** 品牌名不翻译；其余走 i18n */
const APP_NAME: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex",
  agy: "Antigravity",
  grok: "Grok Build",
  desktop: "Claude Desktop",
};

/** 云端搜索按字节计费，单笔常常不到一分钱：显示成 <$0.01，不显示一长串小数 */
const usd = (x: number) => (x >= 100 ? `$${x.toFixed(0)}` : x >= 0.01 ? `$${x.toFixed(2)}` : x > 0 ? "<$0.01" : "$0");

export function UsageCard({ enabled }: { enabled: boolean }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const month = useLoovUsage("month", enabled);
  const today = useLoovUsage("today", enabled);
  const m = month.data;
  if (!enabled || (!m && !month.isError)) return null;
  if (!m) {
    return <p className="px-1 text-xs text-fg-3">{t("loov.usage.unavailable")}</p>;
  }

  const appName = (app: string) => APP_NAME[app] ?? t(`loov.usage.apps.${app}`, { defaultValue: app });
  const budget = m.budgetUsd;
  const ratio = budget ? Math.min(1, m.monthUsd / Math.max(budget, 0.0001)) : 0;
  const rows = [...m.byApp, ...(m.search.calls > 0 ? [{ app: "__search", usd: m.search.usd }] : [])];
  const max = Math.max(...rows.map((r) => r.usd), 0.0001);

  return (
    <section className="rounded-xl border border-border bg-surface px-4 py-3">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className="flex w-full items-center justify-between gap-3 text-left"
      >
        <div className="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-0.5 text-sm">
          <span>
            {t("loov.usage.month")} <span className="font-semibold tabular-nums">{usd(m.totalUsd)}</span>
            {budget != null && <span className="text-fg-3 tabular-nums"> / {usd(budget)}</span>}
          </span>
          {today.data && (
            <span className="text-fg-3">
              {t("loov.usage.today")} <span className="tabular-nums">{usd(today.data.totalUsd)}</span>
            </span>
          )}
        </div>
        <span className="flex shrink-0 items-center gap-1 text-caption text-fg-3">
          {t("loov.usage.details")}
          {open ? <ChevronUp className="h-3.5 w-3.5" /> : <ChevronDown className="h-3.5 w-3.5" />}
        </span>
      </button>

      {budget != null && (
        <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-subtle" aria-hidden="true">
          <div
            className={cn("h-full rounded-full", m.budgetBlocked || ratio >= 1 ? "bg-danger" : ratio >= 0.8 ? "bg-warning" : "bg-action")}
            style={{ width: `${Math.round(ratio * 100)}%` }}
          />
        </div>
      )}
      {m.budgetBlocked ? (
        <p role="alert" className="mt-2 text-xs text-danger">
          {t("loov.usage.blocked")}
        </p>
      ) : (
        budget != null &&
        ratio >= 0.8 && (
          <p role="status" className="mt-2 text-xs text-warning-text">
            {t("loov.usage.nearBudget", { percent: Math.round(ratio * 100) })}
          </p>
        )
      )}

      {open && (
        <ul className="mt-3 flex flex-col gap-1.5">
          {rows.length === 0 && <li className="text-xs text-fg-3">{t("loov.usage.none")}</li>}
          {rows.map((r) => (
            <li key={r.app} className="grid grid-cols-[7.5rem_1fr_auto] items-center gap-3 text-xs">
              <span className="truncate text-fg-2">
                {r.app === "__search" ? t("loov.usage.search", { calls: m.search.calls }) : appName(r.app)}
              </span>
              <span className="h-1.5 overflow-hidden rounded-full bg-subtle">
                <span className="block h-full rounded-full bg-action opacity-70" style={{ width: `${Math.max(2, (r.usd / max) * 100)}%` }} />
              </span>
              <span className="tabular-nums text-fg-2">{usd(r.usd)}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
