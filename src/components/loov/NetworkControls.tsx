/**
 * 安全通道的节点选择与网络模式（后端 src-tauri/src/loov/netmode.rs、flows::set_selection）。
 *
 * - 节点：默认「自动」（能通的节点里按延迟选最快的）；员工也可以固定一个节点，不通时自动回到「自动」。
 * - 网络模式：直连（只有 Loov 启动的应用走通道）/ 代理（国外网站走通道，国内直连）/ 全局（全部走通道）。
 *   代理、全局会接管系统代理，切回直连、断开、退出 Loov 时还原。
 */
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { Check, ChevronDown, Loader2 } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Button } from "@/components/ui/button";
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
import { loovApi, parseLoovError, type LoovNetMode, type LoovNetView, type LoovStatus } from "@/lib/api/loov";
import { loovKeys } from "@/lib/query/loov";

/** 第一次切到代理 / 全局时说明会接管系统代理；确认过就不再问 */
const ACK_KEY = "loov.netModeAck";

function useApply() {
  const qc = useQueryClient();
  return (status: LoovStatus) => qc.setQueryData(loovKeys.status, status);
}

/** 状态条里的「· 台湾 120ms ▾」：当前节点，点开选节点。 */
export function NodePicker({ net }: { net: LoovNetView }) {
  const { t } = useTranslation();
  const apply = useApply();
  const [busy, setBusy] = useState(false);
  const active = net.nodes.find((n) => n.id === net.active);
  const label = active ? `${active.name}${active.latency_ms != null ? ` ${active.latency_ms}ms` : ""}` : "—";

  const choose = async (node: string | null) => {
    if (node === net.pinned) return;
    setBusy(true);
    try {
      const s = await loovApi.setNode(node);
      apply(s);
      if (s.state === "ready" && s.net.pin_fallback) toast.warning(t("loov.net.pinFallback"));
    } catch (e) {
      toast.error(parseLoovError(e).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild disabled={busy}>
        <button
          type="button"
          aria-label={t("loov.net.pickNode")}
          className="inline-flex items-center gap-0.5 rounded px-1 font-normal text-fg-2 transition-colors hover:bg-subtle hover:text-fg-1"
        >
          {busy ? <Loader2 className="h-3 w-3 animate-spin" /> : null}
          {net.pinned ? label : t("loov.net.autoWith", { node: label })}
          <ChevronDown className="h-3 w-3" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="min-w-[220px]">
        <DropdownMenuItem onSelect={() => void choose(null)}>
          <span className="flex-1">{t("loov.net.auto")}</span>
          {!net.pinned && <Check className="h-3.5 w-3.5" />}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        {net.nodes.map((n) => (
          <DropdownMenuItem key={n.id} onSelect={() => void choose(n.id)}>
            <span className={cn("flex-1", n.latency_ms == null && "text-fg-3")}>{n.name}</span>
            <span className="text-caption tabular-nums text-fg-3">
              {n.latency_ms != null ? `${n.latency_ms}ms` : t("loov.net.unavailable")}
            </span>
            {net.pinned === n.id && <Check className="h-3.5 w-3.5" />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 网络模式三段开关 + 一句说明。 */
export function NetModeSwitch({ net, disabled }: { net: LoovNetView | null; disabled: boolean }) {
  const { t } = useTranslation();
  const apply = useApply();
  const [busy, setBusy] = useState(false);
  const [ask, setAsk] = useState<LoovNetMode | null>(null);
  const mode = net?.mode ?? "direct";

  const set = async (next: LoovNetMode) => {
    setBusy(true);
    try {
      const s = await loovApi.setNetMode(next);
      apply(s);
      if (s.state === "ready" && s.net.sys_error) toast.error(t("loov.net.sysError", { error: s.net.sys_error }), { duration: 10_000 });
    } catch (e) {
      toast.error(parseLoovError(e).message);
    } finally {
      setBusy(false);
    }
  };

  const onChange = (next: LoovNetMode) => {
    if (next === mode) return;
    if (next !== "direct" && localStorage.getItem(ACK_KEY) !== "1") {
      setAsk(next);
      return;
    }
    void set(next);
  };

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
      <SegmentedControl<LoovNetMode>
        size="sm"
        aria-label={t("loov.net.mode")}
        value={mode}
        onValueChange={onChange}
        items={(["direct", "rule", "global"] as const).map((m) => ({
          value: m,
          label: t(`loov.net.modes.${m}`),
          disabled: disabled || busy,
        }))}
      />
      <span className="flex min-w-0 items-center gap-1 text-caption text-fg-3">
        {busy && <Loader2 className="h-3 w-3 shrink-0 animate-spin" />}
        {net?.sys_error ? (
          <span className="text-danger">{t("loov.net.sysError", { error: net.sys_error })}</span>
        ) : (
          t(`loov.net.modeHint.${mode}`)
        )}
      </span>

      <Dialog open={ask !== null} onOpenChange={(open) => !open && setAsk(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("loov.net.ackTitle")}</DialogTitle>
            <DialogDescription>{t("loov.net.ackDesc")}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="neutral" onClick={() => setAsk(null)}>
              {t("common.cancel")}
            </Button>
            <Button
              variant="solid"
              onClick={() => {
                const next = ask;
                setAsk(null);
                localStorage.setItem(ACK_KEY, "1");
                if (next) void set(next);
              }}
            >
              {t("loov.net.ackOk")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
