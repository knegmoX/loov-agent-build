/**
 * 飞书渠道：用飞书扫码即可把飞书和一个 agent 打通（后端 src-tauri/src/loov/feishu）。
 * 扫码会在员工自己的飞书里创建一个「个人助手」应用（不需要管理员审批）；之后在飞书里私聊这个机器人，
 * Loov 在本机用选定的 agent 处理并以流式卡片回复，费用记在「飞书渠道」名下。只有创建者本人能用。
 */
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { Loader2, MessageSquare } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { toast } from "@/lib/toast";
import { cn } from "@/lib/utils";
import { loovApi, parseLoovError, type LoovFeishuRegistration } from "@/lib/api/loov";
import { loovKeys, useLoovFeishu } from "@/lib/query/loov";

const AGENTS: { id: string; name: string }[] = [
  { id: "claude", name: "Claude Code" },
  { id: "codex", name: "Codex" },
  { id: "agy", name: "Antigravity" },
  { id: "grok", name: "Grok Build" },
];

export function FeishuChannel({ enabled }: { enabled: boolean }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { data: st } = useLoovFeishu(enabled);
  const [reg, setReg] = useState<LoovFeishuRegistration | null>(null);
  const [starting, setStarting] = useState(false);
  const [cwd, setCwd] = useState("");
  const timer = useRef<ReturnType<typeof setTimeout>>();

  useEffect(() => setCwd(st?.cwd ?? ""), [st?.cwd]);
  useEffect(() => () => clearTimeout(timer.current), []);

  const refresh = () => qc.invalidateQueries({ queryKey: loovKeys.feishu });

  const connect = async () => {
    setStarting(true);
    try {
      const r = await loovApi.feishuBegin();
      setReg(r);
      const deadline = Date.now() + r.expiresInSecs * 1000;
      const tick = async () => {
        if (Date.now() > deadline) {
          setReg(null);
          toast.error(t("loov.feishu.expired"));
          return;
        }
        try {
          if (await loovApi.feishuPoll(r.deviceCode)) {
            setReg(null);
            toast.success(t("loov.feishu.connected"));
            void refresh();
            return;
          }
        } catch (e) {
          setReg(null);
          toast.error(parseLoovError(e).message);
          return;
        }
        timer.current = setTimeout(() => void tick(), r.intervalSecs * 1000);
      };
      timer.current = setTimeout(() => void tick(), r.intervalSecs * 1000);
    } catch (e) {
      toast.error(parseLoovError(e).message);
    } finally {
      setStarting(false);
    }
  };

  const configure = async (opts: Parameters<typeof loovApi.feishuConfigure>[0]) => {
    try {
      await loovApi.feishuConfigure(opts);
      void refresh();
    } catch (e) {
      toast.error(parseLoovError(e).message);
    }
  };

  if (!enabled) return null;
  const link = st?.link.state ?? "stopped";
  const dot = link === "connected" ? "bg-success" : link === "retrying" ? "bg-warning" : "bg-fg-3";

  return (
    <section className="rounded-xl border border-border bg-surface px-4 py-3">
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2.5">
          <MessageSquare className="h-4 w-4 shrink-0 text-fg-2" />
          <div className="min-w-0">
            <div className="flex items-center gap-1.5 text-sm font-medium">
              {t("loov.feishu.title")}
              {st?.configured && (
                <span className="flex items-center gap-1 text-caption font-normal text-fg-3">
                  <span className={cn("h-1.5 w-1.5 rounded-full", dot)} />
                  {t(`loov.feishu.link.${st.enabled ? link : "off"}`)}
                </span>
              )}
            </div>
            <p className="truncate text-caption text-fg-3">
              {st?.configured ? t("loov.feishu.hintConnected") : t("loov.feishu.hint")}
            </p>
          </div>
        </div>
        {st?.configured ? (
          <Switch
            checked={st.enabled}
            aria-label={t("loov.feishu.enable")}
            onCheckedChange={(v) => void configure({ enabled: v })}
          />
        ) : (
          <Button variant="solid" size="compact" disabled={starting} onClick={() => void connect()}>
            {starting && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
            {t("loov.feishu.connect")}
          </Button>
        )}
      </div>

      {st?.configured && (
        <div className="mt-3 grid gap-2 border-t border-border pt-3 text-xs sm:grid-cols-[auto_1fr] sm:items-center">
          <span className="text-fg-2">{t("loov.feishu.agent")}</span>
          <Select value={st.agent || "claude"} onValueChange={(v) => void configure({ agent: v })}>
            <SelectTrigger className="h-7 w-48 text-caption" aria-label={t("loov.feishu.agent")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {AGENTS.map((a) => (
                <SelectItem key={a.id} value={a.id}>
                  {a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <span className="text-fg-2">{t("loov.feishu.cwd")}</span>
          <div className="flex items-center gap-2">
            <Input
              className="h-7 text-caption"
              value={cwd}
              placeholder={t("loov.feishu.cwdPlaceholder")}
              onChange={(e) => setCwd(e.target.value)}
              onBlur={() => cwd !== (st.cwd ?? "") && void configure({ cwd })}
            />
          </div>
          <span className="text-fg-2">{t("loov.feishu.group")}</span>
          <div className="flex items-center gap-2">
            <Switch
              checked={st.allowGroup}
              aria-label={t("loov.feishu.group")}
              onCheckedChange={(v) => void configure({ allowGroup: v })}
            />
            <span className="text-fg-3">{t("loov.feishu.groupHint")}</span>
          </div>
          <span />
          <div className="flex items-center justify-between gap-2 text-fg-3">
            <span>{t("loov.feishu.commands")}</span>
            <Button
              variant="quiet"
              size="compact"
              onClick={() =>
                void loovApi
                  .feishuDisconnect()
                  .then(() => {
                    toast.success(t("loov.feishu.disconnected"));
                    void refresh();
                  })
                  .catch((e) => toast.error(parseLoovError(e).message))
              }
            >
              {t("loov.feishu.disconnect")}
            </Button>
          </div>
        </div>
      )}

      <Dialog open={reg !== null} onOpenChange={(open) => !open && (clearTimeout(timer.current), setReg(null))}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("loov.feishu.scanTitle")}</DialogTitle>
            <DialogDescription>{t("loov.feishu.scanDesc")}</DialogDescription>
          </DialogHeader>
          {reg && (
            <div className="flex flex-col items-center gap-3 px-6 pb-6">
              {/* 二维码是后端用 qrcode 库在本机生成的 SVG（内容只是飞书的确认链接） */}
              <div className="h-52 w-52 rounded-lg bg-white p-2 [&>svg]:h-full [&>svg]:w-full" dangerouslySetInnerHTML={{ __html: reg.qrSvg }} />
              <button type="button" className="break-all text-center text-caption text-action-text underline-offset-2 hover:underline" onClick={() => void loovApi.openExternal(reg.url)}>
                {reg.url}
              </button>
              <p className="flex items-center gap-1.5 text-caption text-fg-3">
                <Loader2 className="h-3 w-3 animate-spin" />
                {t("loov.feishu.waiting")}
              </p>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </section>
  );
}
