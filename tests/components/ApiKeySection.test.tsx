import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ApiKeySection } from "@/components/providers/forms/shared/ApiKeySection";

// 让 t() 原样返回 key：组件若还去翻译推广语，断言能直接看到 key
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

function renderSection(
  props: Partial<React.ComponentProps<typeof ApiKeySection>> = {},
) {
  return render(
    <ApiKeySection
      value=""
      onChange={() => {}}
      category="third_party"
      shouldShowLink
      websiteUrl="https://example.com/keys"
      partnerPromotionKey="kimi"
      {...props}
    />,
  );
}

describe("ApiKeySection", () => {
  // loov: 推广语恒为空，即使预设仍带 partnerPromotionKey
  it("never shows a partner promotion next to the API Key link", () => {
    const { container } = renderSection();

    expect(
      screen.getByRole("link", { name: /providerForm\.getApiKey/ }),
    ).toHaveAttribute("href", "https://example.com/keys");
    expect(
      screen.queryByText(/providerForm\.partnerPromotion/),
    ).not.toBeInTheDocument();
    expect(container.querySelector("p")).toBeNull();
  });

  it("hides the promotion when the API Key link is hidden", () => {
    renderSection({ category: "official", shouldShowLink: false });

    expect(
      screen.queryByText("providerForm.partnerPromotion.kimi"),
    ).not.toBeInTheDocument();
  });

  it("renders nothing extra for presets without a promotion", () => {
    const { container } = renderSection({ partnerPromotionKey: undefined });

    expect(container.querySelector("p")).toBeNull();
  });

  it("turns off autocorrect and auto-capitalization on the key input", () => {
    const { container } = renderSection({ value: "sk-test" });
    const input = container.querySelector("input");

    expect(input).toHaveAttribute("autocorrect", "off");
    expect(input).toHaveAttribute("autocapitalize", "off");
    expect(input).toHaveAttribute("spellcheck", "false");
  });
});
