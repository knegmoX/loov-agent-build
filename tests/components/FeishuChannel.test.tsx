import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";
import { FeishuChannel } from "@/components/loov/FeishuChannel";
import { server } from "../msw/server";
import { renderWithQueryClient as render } from "../utils/testQueryClient";

const T = "http://tauri.local";

describe("FeishuChannel", () => {
  it("offers to connect, then shows the QR dialog from the backend", async () => {
    server.use(
      http.post(`${T}/loov_feishu_status`, () =>
        HttpResponse.json({ configured: false, enabled: false, appId: "", agent: "", cwd: "", allowGroup: false, link: { state: "stopped" } }),
      ),
      http.post(`${T}/loov_feishu_begin`, () =>
        HttpResponse.json({ deviceCode: "d1", url: "https://open.feishu.cn/page/cli?user_code=AB-12", intervalSecs: 60, expiresInSecs: 600, qrSvg: "<svg data-testid='qr'></svg>" }),
      ),
    );
    render(<FeishuChannel enabled />);
    await userEvent.click(await screen.findByRole("button", { name: "loov.feishu.connect" }));
    expect(await screen.findByText("https://open.feishu.cn/page/cli?user_code=AB-12")).toBeInTheDocument();
    expect(screen.getByTestId("qr")).toBeInTheDocument();
  });

  it("shows settings once connected and saves the agent", async () => {
    const calls: unknown[] = [];
    server.use(
      http.post(`${T}/loov_feishu_status`, () =>
        HttpResponse.json({ configured: true, enabled: true, appId: "cli_x", agent: "claude", cwd: "", allowGroup: false, link: { state: "connected" } }),
      ),
      http.post(`${T}/loov_feishu_configure`, async ({ request }) => {
        calls.push(await request.json());
        return HttpResponse.json({ configured: true, enabled: false, appId: "cli_x", agent: "claude", cwd: "", allowGroup: false, link: { state: "stopped" } });
      }),
    );
    render(<FeishuChannel enabled />);
    expect(await screen.findByText("loov.feishu.link.connected")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("switch", { name: "loov.feishu.enable" }));
    await waitFor(() => expect(calls).toContainEqual({ enabled: false }));
  });
});
