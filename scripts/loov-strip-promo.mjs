#!/usr/bin/env node
// loov: 清理供应商预设里的推广内容（规格见 docs/loov/dev-01-preset-cleanup.md 第一、二节）。
//   1. 按行删除 isPartner / primePartner / partnerPromotionKey（gemini 的 "google-official" 除外，
//      Rust 侧 gemini_auth.rs、agy_direct.rs 靠它识别鉴权方式）；
//   2. 清理 URL 里的返利 / 追踪参数，推广写在路径里的换成该预设的 websiteUrl 或站点根地址；
//   3. 按 JSON 方式删除 5 个 locale 里的 providerForm.partnerPromotion。
// 不删预设条目（presetId 按数组下标生成）。可重复执行，第二次运行应输出「无改动」。
// 合并上游时预设与 locale 冲突一律取上游版本，再重跑本脚本和 `pnpm vitest run tests/loov`。
//
// Usage: node scripts/loov-strip-promo.mjs [--check]
//   --check  只报告不写入；有待清理内容时退出码为 1

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// 清理后有的长链接变短，上游 prettier 会把折行的属性收回一行；不跑 prettier 会留下 format:check 告警
let prettier = null;
try {
  prettier = await import("prettier");
} catch {
  console.warn("未找到 prettier，预设文件未格式化，请手动运行 pnpm format");
}

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const CHECK_ONLY = process.argv.includes("--check");

const PRESET_FILES = [
  "claude",
  "claudeDesktop",
  "codex",
  "gemini",
  "grokBuild",
  "hermes",
  "opencode",
  "openclaw",
  "pi",
].map((name) => `src/config/${name}ProviderPresets.ts`);

const LOCALE_FILES = ["en", "ja", "zh", "zh-TW", "tr"].map(
  (name) => `src/i18n/locales/${name}.json`,
);

const KEPT_PROMOTION_KEYS = new Set(["google-official"]);

// 查询参数名（小写比较）
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

function isPromoParam(name, value) {
  const key = name.toLowerCase();
  if (PROMO_PARAMS.has(key) || key.startsWith("utm_")) return true;
  if (key === "from") return /^CH_/i.test(value) || /cc-?switch/i.test(value);
  if (key === "source")
    return /^ccs$/i.test(value) || /cc-?switch/i.test(value);
  return false;
}

function isPromoPath(pathname) {
  return (
    /^\/i\/[^/]+\/?$/.test(pathname) ||
    /^\/invite\/[^/]+\/?$/.test(pathname) ||
    /^\/r\/cc-?switch\/?$/i.test(pathname) ||
    /^\/activity\/cc-?switch\/?$/i.test(pathname)
  );
}

function safeDecode(value) {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

// 按字符串处理查询串，保留原有编码（如 fenno 的 redirect=/purchase?tab=…%26group=16）
function stripQuery(query) {
  let removedRef = false;
  const kept = query.split("&").filter((part) => {
    if (!part) return false;
    const eq = part.indexOf("=");
    const name = eq >= 0 ? part.slice(0, eq) : part;
    const value = eq >= 0 ? safeDecode(part.slice(eq + 1)) : "";
    if (!isPromoParam(safeDecode(name), value)) return true;
    if (name.toLowerCase() === "ref") removedRef = true;
    return false;
  });
  return { query: kept.join("&"), removedRef };
}

function splitUrl(url) {
  const hashAt = url.indexOf("#");
  const beforeHash = hashAt >= 0 ? url.slice(0, hashAt) : url;
  const hash = hashAt >= 0 ? url.slice(hashAt) : "";
  const queryAt = beforeHash.indexOf("?");
  return {
    base: queryAt >= 0 ? beforeHash.slice(0, queryAt) : beforeHash,
    query: queryAt >= 0 ? beforeHash.slice(queryAt + 1) : null,
    hash,
  };
}

function siteRoot(url) {
  try {
    return new URL(url).origin;
  } catch {
    return url;
  }
}

/**
 * 返回 { url, promoPath }。promoPath 为 true 时调用方需把整个链接换成 websiteUrl / 站点根地址。
 */
function cleanUrl(url) {
  const { base, query, hash } = splitUrl(url);
  let changed = false;
  let removedRef = false;
  let nextQuery = query;
  let nextHash = hash;

  if (query !== null) {
    const stripped = stripQuery(query);
    removedRef = stripped.removedRef;
    if (stripped.query !== query) {
      changed = true;
      nextQuery = stripped.query;
    }
  }

  // SPA 哈希路由里的查询串（#/register?aff=…）
  const hashQueryAt = hash.indexOf("?");
  if (hashQueryAt >= 0) {
    const stripped = stripQuery(hash.slice(hashQueryAt + 1));
    removedRef ||= stripped.removedRef;
    const rebuilt =
      hash.slice(0, hashQueryAt) + (stripped.query ? `?${stripped.query}` : "");
    if (rebuilt !== hash) {
      changed = true;
      nextHash = rebuilt;
    }
  }

  let pathname = "";
  try {
    pathname = new URL(base).pathname;
  } catch {
    // 模板串等非完整 URL，只做参数清理
  }
  if (isPromoPath(pathname) || (removedRef && /^\/login\/?$/.test(pathname))) {
    return { url, promoPath: true };
  }

  if (!changed) return { url, promoPath: false };
  if (nextHash === "#/" || nextHash === "#") nextHash = "";
  return {
    url: base + (nextQuery ? `?${nextQuery}` : "") + nextHash,
    promoPath: false,
  };
}

const URL_RE = /https?:\/\/[^\s"'`<>\\]+/g;

const indentOf = (line) => line.match(/^\s*/)[0].length;

// 在同一对象里找 websiteUrl：同缩进为兄弟属性，更深为嵌套内容，遇到更浅的行即越过对象边界
function findSiblingWebsiteUrl(lines, index) {
  const indent = indentOf(lines[index]);
  for (const step of [-1, 1]) {
    for (let i = index + step; i >= 0 && i < lines.length; i += step) {
      const line = lines[i];
      if (!line.trim()) continue;
      const lineIndent = indentOf(line);
      if (lineIndent < indent) break;
      if (lineIndent !== indent) continue;
      const match = line.match(
        /^\s*websiteUrl:\s*["'`](https?:\/\/[^"'`]+)["'`]/,
      );
      if (match) return match[1];
    }
  }
  return null;
}

function replacementFor(lines, index, promoUrl) {
  const website = findSiblingWebsiteUrl(lines, index);
  if (website) {
    const cleaned = cleanUrl(website);
    if (!cleaned.promoPath) return cleaned.url;
    return siteRoot(website);
  }
  return siteRoot(promoUrl);
}

const FIELD_RE =
  /^\s*(?:(isPartner|primePartner):\s*true|partnerPromotionKey:\s*["']([^"']+)["'])\s*,?\s*(?:\/\/.*)?$/;

async function stripPresetFile(relPath) {
  const absPath = join(ROOT, relPath);
  const source = readFileSync(absPath, "utf8");
  const lines = source.split("\n");
  const out = [];
  let removedFields = 0;
  let cleanedUrls = 0;

  lines.forEach((line, index) => {
    const field = line.match(FIELD_RE);
    if (field && !(field[2] && KEPT_PROMOTION_KEYS.has(field[2]))) {
      removedFields += 1;
      return;
    }
    const next = line.replace(URL_RE, (url) => {
      const cleaned = cleanUrl(url);
      const result = cleaned.promoPath
        ? replacementFor(lines, index, url)
        : cleaned.url;
      if (result !== url) cleanedUrls += 1;
      return result;
    });
    out.push(next);
  });

  let output = out.join("\n");
  if (output !== source && prettier) {
    const options = await prettier.resolveConfig(absPath);
    output = await prettier.format(output, { ...options, filepath: absPath });
  }
  if (output !== source && !CHECK_ONLY) writeFileSync(absPath, output);
  return { removedFields, cleanedUrls };
}

function stripLocaleFile(relPath) {
  const absPath = join(ROOT, relPath);
  const source = readFileSync(absPath, "utf8");
  const data = JSON.parse(source);
  if (!data.providerForm || !("partnerPromotion" in data.providerForm)) {
    return false;
  }
  delete data.providerForm.partnerPromotion;
  if (!CHECK_ONLY) {
    writeFileSync(absPath, `${JSON.stringify(data, null, 2)}\n`);
  }
  return true;
}

let dirty = false;

for (const file of PRESET_FILES) {
  const { removedFields, cleanedUrls } = await stripPresetFile(file);
  if (removedFields || cleanedUrls) {
    dirty = true;
    console.log(
      `${file}: 删除推广字段 ${removedFields} 行，清理 URL ${cleanedUrls} 处`,
    );
  }
}

for (const file of LOCALE_FILES) {
  if (stripLocaleFile(file)) {
    dirty = true;
    console.log(`${file}: 删除 providerForm.partnerPromotion`);
  }
}

if (!dirty) {
  console.log("无改动");
} else if (CHECK_ONLY) {
  console.log("（--check：未写入）");
  process.exitCode = 1;
}
