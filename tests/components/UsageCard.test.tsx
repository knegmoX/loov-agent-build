import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";
import { UsageCard } from "@/components/loov/UsageCard";
import { server } from "../msw/server";
import { renderWithQueryClient as render } from "../utils/testQueryClient";

const T = "http://tauri.local";

function mockUsage(month: Record<string, unknown>, todayUsd = 0.5) {
  server.use(
    http.post(`${T}/loov_usage`, async ({ request }) => {
      const { range } = (await request.json()) as { range: string };
      const base = { from: 0, to: 0, byApp: [], search: { calls: 0, bytes: 0, usd: 0 }, budgetUsd: null, monthUsd: 0, budgetBlocked: false };
      return HttpResponse.json(range === "today" ? { ...base, range, totalUsd: todayUsd } : { ...base, range, ...month });
    }),
  );
}

describe("UsageCard", () => {
  it("shows month and today totals, and per-app details with search", async () => {
    mockUsage({
      totalUsd: 12.5,
      monthUsd: 12.5,
      byApp: [
        { app: "claude", usd: 10 },
        { app: "codexdesk", usd: 2.4 },
      ],
      search: { calls: 7, bytes: 100, usd: 0.0001 },
    });
    render(<UsageCard enabled />);
    expect(await screen.findByText("$12.50")).toBeInTheDocument();
    expect(await screen.findByText("$0.50")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { expanded: false }));
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
    expect(screen.getByText(/codexdesk/)).toBeInTheDocument();
    expect(screen.getByText("loov.usage.search")).toBeInTheDocument();
  });

  it("warns near the budget and shows the blocked state", async () => {
    mockUsage({ totalUsd: 45, monthUsd: 45, budgetUsd: 50 });
    const { unmount } = render(<UsageCard enabled />);
    expect(await screen.findByRole("status")).toHaveTextContent("loov.usage.nearBudget");
    unmount();
    mockUsage({ totalUsd: 51, monthUsd: 51, budgetUsd: 50, budgetBlocked: true });
    render(<UsageCard enabled />);
    expect(await screen.findByRole("alert")).toHaveTextContent("loov.usage.blocked");
  });

  it("renders nothing while logged out", async () => {
    mockUsage({ totalUsd: 1, monthUsd: 1 });
    const { container } = render(<UsageCard enabled={false} />);
    await waitFor(() => expect(container).toBeEmptyDOMElement());
  });
});
