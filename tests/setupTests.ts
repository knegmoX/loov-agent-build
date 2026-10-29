import "@testing-library/jest-dom";
import { afterAll, afterEach, beforeAll, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import { server } from "./msw/server";
import { resetProviderState } from "./msw/state";
import "./msw/tauriMocks";

beforeAll(async () => {
  server.listen({ onUnhandledRequest: "warn" });
  await i18n.use(initReactI18next).init({
    lng: "zh",
    fallbackLng: "zh",
    resources: {
      zh: { translation: {} },
      en: { translation: {} },
    },
    interpolation: {
      escapeValue: false,
    },
  });
});

// loov: 上游的界面测试针对完整界面（高级模式）；产品里每次打开都从简洁模式开始，测试里固定进高级模式
vi.mock("@/lib/api/loov", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/api/loov")>()),
  readAdvancedMode: () => true,
}));

afterEach(() => {
  cleanup();
  // 页面数据的本地缓存（src/lib/localCache.ts）和用量页记住的选择不能从一个用例带到下一个
  for (const key of Object.keys(localStorage)) {
    if (
      key.startsWith("cc-switch-cache:") ||
      key.startsWith("cc-switch.usage.")
    )
      localStorage.removeItem(key);
  }
  resetProviderState();
  server.resetHandlers();
  vi.clearAllMocks();
});

afterAll(() => {
  server.close();
});
