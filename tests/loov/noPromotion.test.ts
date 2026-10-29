import { readFileSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";

// loov: 守护测试——预设里不得出现返利 / 追踪链接与合作伙伴推广字段（规格 docs/loov/dev-01-preset-cleanup.md）。
// 失败时先跑 `node scripts/loov-strip-promo.mjs`，脚本漏掉的新花样再补进脚本和这里的规则。

const PRESET_MODULES = {
  claude: () => import("@/config/claudeProviderPresets"),
  claudeDesktop: () => import("@/config/claudeDesktopProviderPresets"),
  codex: () => import("@/config/codexProviderPresets"),
  gemini: () => import("@/config/geminiProviderPresets"),
  grokBuild: () => import("@/config/grokBuildProviderPresets"),
  hermes: () => import("@/config/hermesProviderPresets"),
  opencode: () => import("@/config/opencodeProviderPresets"),
  openclaw: () => import("@/config/openclawProviderPresets"),
  pi: () => import("@/config/piProviderPresets"),
} as const;

const LOCALES = ["en", "ja", "zh", "zh-TW", "tr"] as const;

const PROMO_FIELDS = ["isPartner", "primePartner", "partnerPromotionKey"];
// Rust 侧 gemini_auth.rs / agy_direct.rs 靠它识别 Google 官方鉴权，必须保留
const KEPT_PROMOTION_KEY = "google-official";

const PROMO_PARAMS = new Set([
  "aff",
  "ref",
  "invite",
  "invitecode",
  "invite_code",
  "track_id",
  "ytag",
  "ch",
  "rc",
  "ac",
  "code",
]);

const PROMO_PATHS = [
  /^\/i\/[^/]+\/?$/,
  /^\/invite\/[^/]+\/?$/,
  /^\/r\/cc-?switch\/?$/i,
  /^\/activity\/cc-?switch\/?$/i,
];

const URL_RE = /https?:\/\/[^\s"'`<>\\]+/g;

function promoParamsOf(params: URLSearchParams): string[] {
  return [...params].flatMap(([name, value]) => {
    const key = name.toLowerCase();
    const hit =
      PROMO_PARAMS.has(key) ||
      key.startsWith("utm_") ||
      (key === "from" && (/^CH_/i.test(value) || /cc-?switch/i.test(value))) ||
      (key === "source" && (/^ccs$/i.test(value) || /cc-?switch/i.test(value)));
    return hit ? [`${name}=${value}`] : [];
  });
}

/** 返回该 URL 的推广问题，空数组表示干净 */
function promoProblems(raw: string): string[] {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return [];
  }
  const problems = promoParamsOf(url.searchParams);
  const hashQueryAt = url.hash.indexOf("?");
  if (hashQueryAt >= 0) {
    problems.push(
      ...promoParamsOf(new URLSearchParams(url.hash.slice(hashQueryAt + 1))),
    );
  }
  if (PROMO_PATHS.some((re) => re.test(url.pathname))) {
    problems.push(`path ${url.pathname}`);
  }
  return problems;
}

interface Visit {
  path: string;
  key: string;
  value: unknown;
}

function* walk(
  value: unknown,
  path: string,
  seen = new WeakSet<object>(),
): Generator<Visit> {
  if (value === null || typeof value !== "object") return;
  if (seen.has(value)) return;
  seen.add(value);
  for (const [key, child] of Object.entries(value)) {
    const childPath = `${path}.${key}`;
    yield { path: childPath, key, value: child };
    yield* walk(child, childPath, seen);
  }
}

async function loadExports() {
  return Promise.all(
    Object.entries(PRESET_MODULES).map(async ([name, load]) => {
      const mod: Record<string, unknown> = await load();
      const exported = Object.fromEntries(
        Object.entries(mod).filter(
          ([, value]) => value !== null && typeof value === "object",
        ),
      );
      return { name, exported };
    }),
  );
}

describe("loov: 预设不含推广内容", () => {
  it("每个预设模块都导出了预设数组（防止守护测试空转）", async () => {
    for (const { name, exported } of await loadExports()) {
      const arrays = Object.values(exported).filter(Array.isArray);
      expect(
        arrays.some((list) => list.length > 0),
        `${name} 没有可检查的预设数组`,
      ).toBe(true);
    }
  });

  it("所有 URL 不含返利 / 追踪参数与推广路径", async () => {
    const violations: string[] = [];
    let scanned = 0;
    for (const { name, exported } of await loadExports()) {
      for (const visit of walk(exported, name)) {
        if (typeof visit.value !== "string") continue;
        for (const raw of visit.value.match(URL_RE) ?? []) {
          scanned += 1;
          const problems = promoProblems(raw);
          if (problems.length) {
            violations.push(`${visit.path}: ${raw} (${problems.join(", ")})`);
          }
        }
      }
    }
    expect(scanned).toBeGreaterThan(100);
    expect(violations).toEqual([]);
  });

  it("业务参数不被误判为推广", () => {
    expect(
      promoProblems(
        "https://console.volcengine.com/ark/region:ark+cn-beijing/apiKey?apikey=%7B%7D",
      ),
    ).toEqual([]);
    expect(
      promoProblems("https://vercel.com/d?to=%2F%5Bteam%5D%2F%7E%2Fai-gateway"),
    ).toEqual([]);
    expect(
      promoProblems(
        "https://huggingface.co/settings/tokens/new?ownUserPermissions=inference.serverless.write",
      ),
    ).toEqual([]);
    expect(promoProblems("https://aicodewith.ai/login?tab=register")).toEqual(
      [],
    );
    expect(promoProblems("https://example.com/r/?aff=x")).not.toEqual([]);
    expect(promoProblems("https://example.com/i/ABC")).not.toEqual([]);
  });

  it("除 gemini 的 google-official 外没有合作伙伴推广字段", async () => {
    const violations: string[] = [];
    let keptGoogleOfficial = 0;
    for (const { name, exported } of await loadExports()) {
      for (const visit of walk(exported, name)) {
        if (!PROMO_FIELDS.includes(visit.key)) continue;
        if (
          name === "gemini" &&
          visit.key === "partnerPromotionKey" &&
          visit.value === KEPT_PROMOTION_KEY
        ) {
          keptGoogleOfficial += 1;
          continue;
        }
        violations.push(`${visit.path} = ${JSON.stringify(visit.value)}`);
      }
    }
    expect(violations).toEqual([]);
    expect(keptGoogleOfficial).toBeGreaterThan(0);
  });

  it.each(LOCALES)("%s locale 没有 providerForm.partnerPromotion", (locale) => {
    const file = path.resolve(
      __dirname,
      "..",
      "..",
      "src",
      "i18n",
      "locales",
      `${locale}.json`,
    );
    const data = JSON.parse(readFileSync(file, "utf8"));
    expect(data.providerForm).toBeDefined();
    expect(data.providerForm).not.toHaveProperty("partnerPromotion");
  });
});
