import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import {
  ArrowRight,
  ExternalLink,
  LogOut,
  MonitorSmartphone,
  RefreshCw,
  RotateCcw,
  ShieldCheck,
  ShieldOff,
  Ticket,
  Wallet,
} from "lucide-react";
import loovLogo from "@/assets/icons/logo.svg";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/lib/toast";
import { refreshLoovManaged } from "@/lib/loovManaged";
import { AppLauncher } from "./AppLauncher";
import {
  loovApi,
  parseLoovError,
  upgradeState,
  type LoovDevice,
  type LoovStatus,
} from "@/lib/api/loov";
import {
  loovKeys,
  useLoovAccount,
  useLoovAppVersion,
  useLoovDevices,
  useLoovManaged,
  useLoovMeta,
  useLoovSession,
  useLoovStatus,
} from "@/lib/query/loov";

interface LoovPageProps {
  /** 简洁模式下整页显示（无侧栏）；高级模式下作为一个全局页 */
  embedded?: boolean;
  onToggleAdvanced?: () => void;
  advanced?: boolean;
}

function isLoggedIn(status: LoovStatus | undefined): boolean {
  return (
    !!status &&
    (status.state === "ready" || status.state === "starting" || status.state === "tunnelFailed" || status.state === "offline")
  );
}

export function LoovPage({ embedded = false, onToggleAdvanced, advanced = false }: LoovPageProps) {
  const { t } = useTranslation();
  const { data: status } = useLoovStatus();
  const loggedIn = isLoggedIn(status);
  const { data: meta } = useLoovMeta();
  const { data: appVersion } = useLoovAppVersion();
  const upgrade = upgradeState(appVersion, meta?.client);
  const downloadUrl = meta?.client?.downloadUrl;

  return (
    <div
      id="main-content"
      className={
        embedded
          ? "min-h-0 flex-1 overflow-y-auto scroll-stable"
          : "flex h-screen w-full flex-col overflow-y-auto bg-app text-fg-1"
      }
    >
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-6 px-6 pb-10 pt-8">
        <header className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <img src={loovLogo} alt="" draggable={false} className="h-10 w-10 shrink-0" />
            <div>
              <h1 className="text-page font-semibold leading-tight">Loov Agent</h1>
              <p className="text-sm text-fg-3">{t("loov.tagline")}</p>
            </div>
          </div>
          {onToggleAdvanced && (
            <Button variant="quiet" size="regular" onClick={onToggleAdvanced}>
              {advanced ? t("loov.simpleMode") : t("loov.advancedMode")}
              <ArrowRight className="h-3.5 w-3.5" />
            </Button>
          )}
        </header>

        {upgrade !== "none" && (
          <div
            role="status"
            className={`flex items-center justify-between gap-3 rounded-lg border px-4 py-3 text-sm ${
              upgrade === "required"
                ? "border-danger/40 bg-danger/10 text-danger"
                : "border-border bg-subtle text-fg-2"
            }`}
          >
            <span>
              {upgrade === "required"
                ? t("loov.upgradeRequired", { min: meta?.client?.min, current: appVersion })
                : t("loov.upgradeAvailable", { latest: meta?.client?.latest, current: appVersion })}
            </span>
            {downloadUrl && (
              <Button variant={upgrade === "required" ? "solid" : "neutral"} size="regular" onClick={() => void loovApi.openExternal(downloadUrl)}>
                {t("loov.download")}
                <ExternalLink className="h-3.5 w-3.5" />
              </Button>
            )}
          </div>
        )}

        {meta?.announcement && (
          <div className="rounded-lg border border-border bg-subtle px-4 py-3 text-sm text-fg-2">
            {meta.announcement}
          </div>
        )}

        {loggedIn ? (
          <HomePanel status={status!} />
        ) : (
          <LoginCard status={status} blocked={upgrade === "required"} />
        )}
      </div>
    </div>
  );
}

// ───────────────────────────── 登录 / 注册 ─────────────────────────────

function LoginCard({ status, blocked = false }: { status: LoovStatus | undefined; blocked?: boolean }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [mode, setMode] = useState<"login" | "register">("login");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [invite, setInvite] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [limitDevices, setLimitDevices] = useState<LoovDevice[] | null>(null);

  const hint =
    status?.state === "reloginRequired"
      ? t("loov.reloginHint")
      : null;

  const submit = async (evictOldest = false) => {
    setBusy(true);
    setError(null);
    try {
      const u = username.trim();
      if (mode === "register") {
        await loovApi.register(invite.trim(), u, password, evictOldest);
      } else {
        await loovApi.login(u, password, evictOldest);
      }
      setLimitDevices(null);
      void qc.invalidateQueries({ queryKey: ["loov"] });
      void refreshLoovManaged();
      toast.success(t("loov.loginSuccess"));
    } catch (e) {
      const err = parseLoovError(e);
      if (err.code === "DEVICE_LIMIT") {
        setLimitDevices(err.devices ?? []);
      } else if (err.code === "REGISTERED_RETRY_LOGIN") {
        setMode("login");
        setError(err.message);
      } else {
        setError(err.message);
      }
    } finally {
      setBusy(false);
    }
  };

  const canSubmit =
    /^[A-Za-z0-9_]{3,20}$/.test(username.trim()) &&
    password.length >= 8 &&
    (mode === "login" || invite.trim().length >= 6);

  return (
    <div className="rounded-xl border border-border bg-surface p-6 shadow-sm">
      <div className="mb-5 flex gap-1 rounded-lg bg-subtle p-1 text-sm">
        {(["login", "register"] as const).map((m) => (
          <button
            key={m}
            type="button"
            className={`flex-1 rounded-md px-3 py-1.5 transition ${
              mode === m ? "bg-surface font-semibold text-fg-1 shadow-sm" : "text-fg-3 hover:text-fg-1"
            }`}
            onClick={() => {
              setMode(m);
              setError(null);
            }}
          >
            {m === "login" ? t("loov.login") : t("loov.register")}
          </button>
        ))}
      </div>

      <form
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          if (canSubmit && !busy && !blocked) void submit(false);
        }}
      >
        {mode === "register" && (
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="loov-invite">{t("loov.invite")}</Label>
            <Input
              id="loov-invite"
              autoComplete="off"
              placeholder="LOOV-XXXX-XXXX-XXXX"
              value={invite}
              onChange={(e) => setInvite(e.target.value.toUpperCase())}
            />
          </div>
        )}
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="loov-username">{t("loov.username")}</Label>
          <Input
            id="loov-username"
            autoComplete="username"
            placeholder={t("loov.usernameHint")}
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="loov-password">{t("loov.password")}</Label>
          <Input
            id="loov-password"
            type="password"
            autoComplete={mode === "login" ? "current-password" : "new-password"}
            placeholder={t("loov.passwordHint")}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </div>

        {(error || hint) && (
          <p className="rounded-md bg-danger/10 px-3 py-2 text-sm text-danger" role="alert">
            {error ?? hint}
          </p>
        )}

        <Button type="submit" variant="solid" size="regular" disabled={!canSubmit || busy || blocked}>
          {busy ? t("common.loading") : mode === "login" ? t("loov.loginAction") : t("loov.registerAction")}
        </Button>
        <p className="text-center text-xs text-fg-3">{t("loov.privacyNote")}</p>
      </form>
      <RestoreOriginal variant="link" />

      <Dialog open={limitDevices !== null} onOpenChange={(open) => !open && setLimitDevices(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("loov.deviceLimitTitle")}</DialogTitle>
            <DialogDescription>{t("loov.deviceLimitDesc")}</DialogDescription>
          </DialogHeader>
          <ul className="max-h-60 divide-y divide-border overflow-y-auto rounded-md border border-border text-sm">
            {(limitDevices ?? []).map((d, i) => (
              <li key={d.id} className="flex items-center justify-between px-3 py-2">
                <span className="flex items-center gap-2">
                  <MonitorSmartphone className="h-4 w-4 text-fg-3" />
                  {d.name}
                  <span className="text-xs text-fg-3">{d.platform}</span>
                  {i === 0 && <Badge variant="outline">{t("loov.oldest")}</Badge>}
                </span>
                <span className="text-xs text-fg-3">{formatSeen(d.lastSeenAt ?? d.createdAt, t)}</span>
              </li>
            ))}
          </ul>
          <DialogFooter>
            <Button variant="neutral" onClick={() => setLimitDevices(null)}>
              {t("common.cancel")}
            </Button>
            <Button variant="solid" disabled={busy} onClick={() => void submit(true)}>
              {t("loov.evictAndLogin")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

// ───────────────────────────── 已登录首页 ─────────────────────────────

/**
 * 第一性原理：员工打开 Loov 是为了马上用某个 agent。所以首页 = 一行状态（通道、余额）+ 启动台，
 * 其余（兑换码、设备、恢复、登出）收在页脚。
 */
function HomePanel({ status }: { status: LoovStatus }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { data: session } = useLoovSession(true);
  const { data: account, isError: accountError } = useLoovAccount(true);
  const [devicesOpen, setDevicesOpen] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [redeemOpen, setRedeemOpen] = useState(false);
  const [redeemCode, setRedeemCode] = useState("");

  const refresh = () => void qc.invalidateQueries({ queryKey: ["loov"] });

  const run = async (key: string, fn: () => Promise<unknown>, okMsg?: string) => {
    setBusy(key);
    try {
      await fn();
      if (okMsg) toast.success(okMsg);
      refresh();
    } catch (e) {
      toast.error(parseLoovError(e).message);
    } finally {
      setBusy(null);
    }
  };

  const topupUrl = account?.topupUrl || session?.topup_url || "";
  const ready = status.state === "ready";
  const failed = status.state === "tunnelFailed" || status.state === "offline";

  return (
    <div className="flex flex-col gap-4">
      {/* 状态条：通道 · 节点 | 余额 · 充值 */}
      <section
        className={`rounded-xl border px-4 py-3 ${failed ? "border-danger/40 bg-danger/5" : "border-border bg-surface"}`}
      >
        <div className="flex items-center justify-between gap-3">
          <div className="flex min-w-0 items-center gap-2.5">
            {ready ? (
              <ShieldCheck className="h-4 w-4 shrink-0 text-success" />
            ) : status.state === "starting" ? (
              <RefreshCw className="h-4 w-4 shrink-0 animate-spin text-fg-3" />
            ) : (
              <ShieldOff className="h-4 w-4 shrink-0 text-danger" />
            )}
            <div className="min-w-0">
              <div className="truncate text-sm font-medium">
                {ready ? t("loov.tunnelReady") : status.state === "starting" ? t("loov.tunnelConnecting") : t("loov.tunnelDown")}
                {ready && (
                  <span className="font-normal text-fg-3">
                    {" · "}
                    {status.nodes.join(" / ")}
                    {status.route === "proxy" ? ` · ${t("loov.tunnelViaProxy", { proxy: status.via ?? "" })}` : ""}
                  </span>
                )}
              </div>
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {!ready && (
              <Button
                variant="neutral"
                size="compact"
                disabled={busy === "refresh" || status.state === "starting"}
                onClick={() => void run("refresh", loovApi.refresh)}
              >
                <RefreshCw className={`h-3.5 w-3.5 ${busy === "refresh" ? "animate-spin" : ""}`} />
                {t("loov.retry")}
              </Button>
            )}
            <div className="flex items-center gap-1.5 text-sm">
              <Wallet className="h-4 w-4 text-fg-3" />
              <span className="font-semibold tabular-nums">
                {account ? `$${account.balanceUsd.toFixed(2)}` : accountError ? "—" : "…"}
              </span>
            </div>
            {topupUrl && (
              <Button variant="quiet" size="compact" onClick={() => void loovApi.openExternal(topupUrl)}>
                {t("loov.topup")}
                <ExternalLink className="h-3 w-3" />
              </Button>
            )}
          </div>
        </div>
        {failed && <p className="mt-2 whitespace-pre-line text-xs text-danger">{status.reason}</p>}
      </section>

      {/* 启动台 */}
      <AppLauncher models={session?.models ?? []} catalog={session?.model_catalog ?? []} tunnelReady={ready} />
      {ready && <p className="px-1 text-xs text-fg-3">{t("loov.apps.hint")}</p>}

      {/* 页脚：兑换码 / 设备 / 恢复 / 登出 */}
      <section className="flex flex-col gap-3 border-t border-border pt-3">
        <div className="flex flex-wrap items-center justify-between gap-1">
          <div className="flex items-center gap-1">
            <Button variant="quiet" size="compact" onClick={() => setRedeemOpen((o) => !o)}>
              <Ticket className="h-3.5 w-3.5" />
              {t("loov.redeem")}
            </Button>
            <Button variant="quiet" size="compact" onClick={() => setDevicesOpen(true)}>
              <MonitorSmartphone className="h-3.5 w-3.5" />
              {t("loov.devices")}
            </Button>
          </div>
          <div className="flex items-center gap-1">
            <RestoreOriginal variant="button" />
            <Button
              variant="quiet"
              size="compact"
              disabled={busy === "logout"}
              onClick={() => void run("logout", loovApi.logout, t("loov.loggedOut"))}
            >
              <LogOut className="h-3.5 w-3.5" />
              {t("loov.logout")}
            </Button>
          </div>
        </div>
        {redeemOpen && (
          <form
            className="flex gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              const code = redeemCode.trim();
              if (!code) return;
              void run("redeem", async () => {
                const added = await loovApi.redeem(code);
                setRedeemCode("");
                setRedeemOpen(false);
                toast.success(t("loov.redeemed", { value: added.toFixed(2) }));
              });
            }}
          >
            <Input
              autoFocus
              placeholder={t("loov.redeemPlaceholder")}
              value={redeemCode}
              onChange={(e) => setRedeemCode(e.target.value)}
            />
            <Button type="submit" variant="neutral" disabled={!redeemCode.trim() || busy === "redeem"}>
              {t("loov.redeem")}
            </Button>
          </form>
        )}
      </section>

      <DevicesDialog open={devicesOpen} onOpenChange={setDevicesOpen} />
    </div>
  );
}

/** 停用 Loov 并把 Claude Code / Codex 配置恢复到首次登录前（只在接管过本机时显示）。 */
function RestoreOriginal({ variant }: { variant: "link" | "button" }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { data: managed } = useLoovManaged();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  if (!managed) return null;

  const confirm = async () => {
    setBusy(true);
    try {
      const report = await loovApi.restoreOriginal();
      toast.success(report.snapshot ? t("loov.restored") : t("loov.restoredNoSnapshot"));
      setOpen(false);
      void qc.invalidateQueries({ queryKey: ["loov"] });
      void refreshLoovManaged();
    } catch (e) {
      toast.error(parseLoovError(e).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      {variant === "link" ? (
        <button
          type="button"
          className="mt-4 w-full text-center text-xs text-fg-3 underline-offset-4 hover:text-fg-1 hover:underline"
          onClick={() => setOpen(true)}
        >
          {t("loov.restoreLink")}
        </button>
      ) : (
        <Button variant="quiet" size="compact" onClick={() => setOpen(true)}>
          <RotateCcw className="h-3.5 w-3.5" />
          {t("loov.restore")}
        </Button>
      )}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("loov.restoreTitle")}</DialogTitle>
            <DialogDescription>{t("loov.restoreDesc")}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="neutral" onClick={() => setOpen(false)}>
              {t("common.cancel")}
            </Button>
            <Button variant="solid" disabled={busy} onClick={() => void confirm()}>
              {busy ? t("common.loading") : t("loov.restoreConfirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

function DevicesDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { data: devices, isLoading } = useLoovDevices(open);
  const [busy, setBusy] = useState<string | null>(null);

  const revoke = async (d: LoovDevice) => {
    setBusy(d.id);
    try {
      await loovApi.revokeDevice(d.id);
      toast.success(t("loov.deviceRevoked", { name: d.name }));
      if (d.current) {
        void qc.invalidateQueries({ queryKey: ["loov"] });
        onOpenChange(false);
      } else {
        void qc.invalidateQueries({ queryKey: loovKeys.devices });
      }
    } catch (e) {
      toast.error(parseLoovError(e).message);
    } finally {
      setBusy(null);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("loov.devices")}</DialogTitle>
          <DialogDescription>{t("loov.devicesDesc")}</DialogDescription>
        </DialogHeader>
        {isLoading ? (
          <p className="text-sm text-fg-3">{t("common.loading")}</p>
        ) : (
          <ul className="max-h-72 divide-y divide-border overflow-y-auto rounded-md border border-border text-sm">
            {(devices ?? []).map((d) => (
              <li key={d.id} className="flex items-center justify-between gap-3 px-3 py-2">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="truncate font-medium">{d.name}</span>
                    {d.current && (
                      <Badge variant="outline" className="shrink-0 whitespace-nowrap">
                        {t("loov.thisDevice")}
                      </Badge>
                    )}
                  </div>
                  <div className="text-xs text-fg-3">
                    {d.platform} · {formatSeen(d.lastSeenAt ?? d.createdAt, t)}
                  </div>
                </div>
                <Button
                  variant="neutral"
                  size="regular"
                  disabled={busy === d.id}
                  onClick={() => void revoke(d)}
                >
                  {d.current ? t("loov.logout") : t("loov.remove")}
                </Button>
              </li>
            ))}
          </ul>
        )}
      </DialogContent>
    </Dialog>
  );
}

function formatSeen(ts: number, t: (k: string, o?: Record<string, unknown>) => string): string {
  if (!ts) return "";
  const d = new Date(ts * 1000);
  const diff = Date.now() - d.getTime();
  if (diff < 60_000) return t("loov.justNow");
  if (diff < 3_600_000) return t("loov.minutesAgo", { n: Math.floor(diff / 60_000) });
  if (diff < 86_400_000) return t("loov.hoursAgo", { n: Math.floor(diff / 3_600_000) });
  return d.toLocaleDateString();
}

