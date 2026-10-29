import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";
import { LoovPage } from "@/components/loov/LoovPage";
import { compareVersions, upgradeState } from "@/lib/api/loov";
import { server } from "../msw/server";
import { renderWithQueryClient as render } from "../utils/testQueryClient";

const T = "http://tauri.local";

type Calls = Record<string, unknown[]>;

function mockLoov(opts: {
  status?: unknown;
  managed?: boolean;
  loginError?: unknown;
  client?: Record<string, string>;
  apps?: { id: string; supported?: boolean; installed: boolean }[];
  launchError?: unknown;
  calls: Calls;
}) {
  let status: unknown = opts.status ?? { state: "noCredentials" };
  let loginAttempts = 0;
  const record = (cmd: string, body: unknown) => {
    (opts.calls[cmd] ??= []).push(body);
  };
  server.use(
    http.post(`${T}/loov_status`, () => HttpResponse.json(status as Record<string, unknown>)),
    http.post(`${T}/loov_meta`, () =>
      HttpResponse.json({ hubVersion: "0.3.0", announcement: "", client: opts.client ?? {}, maxDevices: 5 }),
    ),
    http.post(`${T}/loov_app_version`, () => HttpResponse.json("0.1.0")),
    http.post(`${T}/loov_managed`, () => HttpResponse.json(opts.managed ?? false)),
    http.post(`${T}/loov_login`, async ({ request }) => {
      const body = await request.json();
      record("loov_login", body);
      loginAttempts += 1;
      if (opts.loginError && loginAttempts === 1) {
        // 测试环境的 invoke mock 把非 2xx 的响应体包成 Error(text)
        return new HttpResponse(JSON.stringify(opts.loginError), { status: 500 });
      }
      status = { state: "ready", username: "alice", device_id: "d1", proxy_url: "", http_port: 1, nodes: ["jp1"] };
      return HttpResponse.json(status as Record<string, unknown>);
    }),
    http.post(`${T}/loov_restore_original`, async ({ request }) => {
      record("loov_restore_original", await request.json());
      status = { state: "noCredentials" };
      return HttpResponse.json({ snapshot: true, claude: true, codex: true });
    }),
    http.post(`${T}/loov_session`, () =>
      HttpResponse.json({
        username: "alice",
        device_id: "d1",
        topup_url: "https://me.loov.cc/console/topup",
        models: ["claude-sonnet-5", "gpt-6-sol"],
        model_catalog: [],
        api_base_url: "https://me.loov.cc",
      }),
    ),
    http.post(`${T}/loov_account`, () =>
      HttpResponse.json({ username: "alice", group: "default", balanceUsd: 1.92, usedUsd: 0.09, topupUrl: "https://me.loov.cc/console/topup" }),
    ),
    http.post(`${T}/loov_apps_status`, () =>
      HttpResponse.json(
        (opts.apps ?? []).map((a) => ({ supported: true, version: a.installed ? "1.0.0" : null, ...a })),
      ),
    ),
    http.post(`${T}/loov_app_install`, async ({ request }) => {
      const body = (await request.json()) as { id: string };
      record("loov_app_install", body);
      opts.apps = (opts.apps ?? []).map((a) => (a.id === body.id ? { ...a, installed: true } : a));
      return HttpResponse.json(null);
    }),
    http.post(`${T}/loov_app_launch`, async ({ request }) => {
      const body = (await request.json()) as { id: string; restart: boolean };
      record("loov_app_launch", body);
      if (opts.launchError && !body.restart) {
        return new HttpResponse(JSON.stringify(opts.launchError), { status: 500 });
      }
      return HttpResponse.json(null);
    }),
    http.post(`${T}/get_universal_provider`, () =>
      HttpResponse.json({
        id: "loov",
        name: "Loov",
        providerType: "newapi",
        apps: { claude: true, codex: true, gemini: false },
        baseUrl: "https://me.loov.cc",
        apiKey: "LOOV_MANAGED",
        models: { claude: { model: "claude-sonnet-5" }, codex: { model: "gpt-6-sol" } },
      }),
    ),
  );
}

async function fillLogin(user: ReturnType<typeof userEvent.setup>) {
  await user.type(await screen.findByLabelText("loov.username"), "alice");
  await user.type(screen.getByLabelText("loov.password"), "password123");
}

const READY = { state: "ready", username: "alice", device_id: "d1", proxy_url: "", http_port: 1, nodes: ["jp4"], route: "direct", via: null };
const APP_IDS = ["claude", "codex", "agy", "grok", "claude-desktop", "codex-desktop"];
const ALL_INSTALLED = APP_IDS.map((id) => ({ id, installed: true }));

describe("LoovPage launcher", () => {
  it("shows the six agents and launches an installed one with a click", async () => {
    const calls: Calls = {};
    mockLoov({ calls, status: READY, managed: true, apps: ALL_INSTALLED });
    const user = userEvent.setup();
    render(<LoovPage />);

    const tiles = await screen.findAllByRole("button", { name: "loov.apps.launchAria" });
    expect(tiles).toHaveLength(6);
    for (const name of ["Claude Code", "Codex", "Antigravity", "Grok Build", "Claude Desktop", "Codex 桌面版"]) {
      expect(screen.getByText(name)).toBeInTheDocument();
    }
    await user.click(tiles[0]);
    await waitFor(() => expect(calls.loov_app_launch).toEqual([{ id: "claude", restart: false }]));
  });

  it("installs a missing agent in place, then launches it", async () => {
    const calls: Calls = {};
    mockLoov({
      calls,
      status: READY,
      managed: true,
      apps: APP_IDS.map((id) => ({ id, installed: id !== "codex" })),
    });
    const user = userEvent.setup();
    render(<LoovPage />);

    await user.click(await screen.findByRole("button", { name: "loov.apps.installAria" }));
    await waitFor(() => expect(calls.loov_app_install).toEqual([{ id: "codex" }]));
    await waitFor(() => expect(calls.loov_app_launch).toEqual([{ id: "codex", restart: false }]));
  });

  it("asks before restarting a running Claude Desktop", async () => {
    const calls: Calls = {};
    mockLoov({
      calls,
      status: READY,
      managed: true,
      apps: ALL_INSTALLED,
      launchError: { code: "DESKTOP_RESTART_REQUIRED", message: "需要重启" },
    });
    const user = userEvent.setup();
    render(<LoovPage />);

    const tiles = await screen.findAllByRole("button", { name: "loov.apps.launchAria" });
    await user.click(tiles[4]);
    await user.click(await screen.findByRole("button", { name: "loov.apps.restartNow" }));
    await waitFor(() =>
      expect(calls.loov_app_launch).toEqual([
        { id: "claude-desktop", restart: false },
        { id: "claude-desktop", restart: true },
      ]),
    );
  });

  it("greys out Claude Desktop where it is not available", async () => {
    const calls: Calls = {};
    mockLoov({
      calls,
      status: READY,
      managed: true,
      apps: APP_IDS.map((id) =>
        id === "claude-desktop" ? { id, supported: false, installed: false } : { id, installed: true },
      ),
    });
    render(<LoovPage />);
    expect(await screen.findByText("loov.apps.unsupported")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "loov.apps.installAria" })).toBeDisabled();
  });

  it("explains how Codex desktop shares the Codex model", async () => {
    const calls: Calls = {};
    mockLoov({ calls, status: READY, managed: true, apps: ALL_INSTALLED });
    render(<LoovPage />);
    expect(await screen.findByText("loov.apps.codexDesktopNote")).toBeInTheDocument();
  });
});

describe("LoovPage", () => {
  it("logs in and shows the home panel with balance and tunnel state", async () => {
    const calls: Calls = {};
    mockLoov({ calls });
    const user = userEvent.setup();
    render(<LoovPage />);

    const submit = await screen.findByRole("button", { name: "loov.loginAction" });
    expect(submit).toBeDisabled();
    await fillLogin(user);
    expect(submit).toBeEnabled();
    await user.click(submit);

    await waitFor(() => expect(calls.loov_login).toHaveLength(1));
    expect(calls.loov_login[0]).toEqual({ username: "alice", password: "password123", evictOldest: false });
    expect(await screen.findByText("loov.tunnelReady")).toBeInTheDocument();
    expect(await screen.findByText("$1.92")).toBeInTheDocument();
  });

  it("offers to evict the oldest device when the device limit is reached", async () => {
    const calls: Calls = {};
    mockLoov({
      calls,
      loginError: {
        code: "DEVICE_LIMIT",
        message: "最多同时登录 5 台设备",
        devices: [
          { id: "a", name: "old-laptop", platform: "linux", createdAt: 1, lastSeenAt: null, current: false },
          { id: "b", name: "desk", platform: "windows", createdAt: 2, lastSeenAt: 3, current: false },
        ],
      },
    });
    const user = userEvent.setup();
    render(<LoovPage />);
    await fillLogin(user);
    await user.click(screen.getByRole("button", { name: "loov.loginAction" }));

    expect(await screen.findByText("loov.deviceLimitTitle")).toBeInTheDocument();
    expect(screen.getByText("old-laptop")).toBeInTheDocument();
    expect(screen.getByText("loov.oldest")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "loov.evictAndLogin" }));

    await waitFor(() => expect(calls.loov_login).toHaveLength(2));
    expect(calls.loov_login[1]).toMatchObject({ evictOldest: true });
    expect(await screen.findByText("loov.tunnelReady")).toBeInTheDocument();
  });

  it("shows plain hub error messages", async () => {
    const calls: Calls = {};
    mockLoov({ calls, loginError: { code: "INVALID_CREDENTIALS", message: "用户名或密码错误" } });
    const user = userEvent.setup();
    render(<LoovPage />);
    await fillLogin(user);
    await user.click(screen.getByRole("button", { name: "loov.loginAction" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("用户名或密码错误");
  });

  it("hides the restore entry until Loov has managed this machine", async () => {
    mockLoov({ calls: {}, managed: false });
    render(<LoovPage />);
    await screen.findByRole("button", { name: "loov.loginAction" });
    expect(screen.queryByText("loov.restoreLink")).not.toBeInTheDocument();
  });

  it("restores the original CLI config after confirmation", async () => {
    const calls: Calls = {};
    mockLoov({ calls, managed: true });
    const user = userEvent.setup();
    render(<LoovPage />);

    await user.click(await screen.findByText("loov.restoreLink"));
    expect(await screen.findByText("loov.restoreTitle")).toBeInTheDocument();
    expect(calls.loov_restore_original).toBeUndefined();
    await user.click(screen.getByRole("button", { name: "loov.restoreConfirm" }));
    await waitFor(() => expect(calls.loov_restore_original).toHaveLength(1));
  });

  it("blocks sign-in when the client is older than the hub minimum", async () => {
    mockLoov({ calls: {}, client: { min: "0.2.0", latest: "0.2.0", downloadUrl: "https://example.com/dl" } });
    const user = userEvent.setup();
    render(<LoovPage />);
    expect(await screen.findByText("loov.upgradeRequired")).toBeInTheDocument();
    await fillLogin(user);
    expect(screen.getByRole("button", { name: "loov.loginAction" })).toBeDisabled();
    expect(screen.getByRole("button", { name: /loov.download/ })).toBeInTheDocument();
  });

  it("only suggests an upgrade when a newer optional version exists", async () => {
    mockLoov({ calls: {}, client: { min: "0.0.0", latest: "0.1.5" } });
    const user = userEvent.setup();
    render(<LoovPage />);
    expect(await screen.findByText("loov.upgradeAvailable")).toBeInTheDocument();
    await fillLogin(user);
    expect(screen.getByRole("button", { name: "loov.loginAction" })).toBeEnabled();
  });
});

describe("version helpers", () => {
  it("compares dotted versions numerically", () => {
    expect(compareVersions("0.1.0", "0.1.0")).toBe(0);
    expect(compareVersions("0.1.9", "0.1.10")).toBeLessThan(0);
    expect(compareVersions("v1.0", "0.9.9")).toBeGreaterThan(0);
    expect(compareVersions("0.1.0-beta", "0.1.0")).toBe(0);
  });

  it("derives the upgrade state", () => {
    expect(upgradeState("0.1.0", { min: "0.0.0", latest: "0.0.0" })).toBe("none");
    expect(upgradeState("0.1.0", { min: "0.0.0", latest: "0.2.0" })).toBe("available");
    expect(upgradeState("0.1.0", { min: "0.2.0", latest: "0.2.0" })).toBe("required");
    expect(upgradeState(undefined, { min: "9.9.9" })).toBe("none");
  });
});
