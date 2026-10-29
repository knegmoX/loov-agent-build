# dev-01 清理推广内容与官方直连入口

## 目标
员工在 Loov 客户端里看到的界面和内置数据，不能含第三方推广、返利链接、合作伙伴推广语，也不能有 CC Switch 自身的推广，例如邀 Star、上游下载站。Loov 接管本机期间，不提供绕开公司 API 的官方或订阅直连入口。MIT 版权声明保留，并在界面上能看到。

## 原则
1. **保留预设条目，只删推广。** 预设的 presetId 按数组下标生成（`ProviderForm.tsx` 约 890 行），删条目会让已保存的 presetId 错位。Rust 侧的余额、用量、托盘也按名称或域名识别这些供应商。
2. **改动可以重放。** 上游每天都在加赞助商。清理用脚本完成，再加一个守护测试兜底。合并上游时，预设文件的冲突一律取上游版本，然后重跑脚本。
3. **单点防护。** 推广语只有一个渲染点，就是 `shared/ApiKeySection.tsx`。在那里直接置空，即使上游以后新加的推广漏清，也不会显示出来。

## 一、预设数据清理
**范围**：`src/config/` 下 9 个预设文件：claude、claudeDesktop、codex、gemini、grokBuild、hermes、opencode、openclaw、pi。

**删除字段**
- 删掉 `isPartner: true`、`primePartner: true`、`partnerPromotionKey: "…"` 这几行。
- **例外**：gemini 的 `partnerPromotionKey: "google-official"` 必须保留。`gemini_auth.rs`、`agy_direct.rs`、`GeminiFormFields.tsx` 靠它识别鉴权方式。
- 类型定义里的字段保留，减少与上游的冲突。

**URL 清理规则**：作用于 `websiteUrl`、`apiKeyUrl` 以及同一对象里的其它 URL 字段。
| 情形 | 处理 |
|---|---|
| 查询参数含 `aff`、`ref`、`invite`、`invitecode`、`inviteCode`、`utm_*`、`track_id`、`ytag`、`from=CH_*`、`ch`、`rc`、`ac`、`code`、`source=ccs`，或火山 activity 的邀请参数 | 删掉这些参数；没有剩余参数时连 `?` 一起删 |
| 推广写在路径里：`/i/<码>`、`/invite/<码>`、`/r/ccswitch`、`/activity/ccswitch`、`/login?ref=…` | 换成该预设的 `websiteUrl` 或站点根地址 |
| 业务参数：`?apikey=%7B%7D`、vercel 的 `?to=`、huggingface 的 `?ownUserPermissions=`、`?tab=register` | 保留 |

**语言文件**：5 个 locale（en、ja、zh、zh-TW、tr）删掉 `providerForm.partnerPromotion` 整块。

## 二、可重放与守护
- **`scripts/loov-strip-promo.mjs`**：按行删推广字段，按上面的规则清理 URL，按 JSON 方式删 i18n 推广块。重复执行时结果不变。
- **`tests/loov/noPromotion.test.ts`**：导入全部预设导出，遍历所有 URL 字段，断言不含上表中的追踪参数与路径；断言 locale 里没有 `partnerPromotion`；断言除 google-official 外没有 `partnerPromotionKey`，也没有 `isPartner` 或 `primePartner`。
- **合并上游的流程**：预设与 locale 冲突一律取上游版本 → `node scripts/loov-strip-promo.mjs` → `pnpm vitest run tests/loov`。

## 三、界面入口
| 位置 | 处理 |
|---|---|
| `shared/ApiKeySection.tsx` | 推广语恒为空，加 `// loov:` |
| `AboutSection.tsx` | 「更新日志」和「下载」改用 hub `/meta` 的 `downloadUrl`，缺省为 https://me.loov.cc；删除邀 Star 代码；标题旁注明「基于 CC Switch（MIT）」，并提供查看许可的入口 |
| `DatabaseUpgrade.tsx` | 「打开发布页」改为同一个下载地址。这个界面在两种模式下都可能出现 |
| `tauri.conf.json` | `bundle.resources` 加入 `LICENSE`，安装包里也带上 MIT 许可 |
| 只在仓库里的文件：README_*、assets/partners、docs/user-manual、flatpak、deplink.html | 不动，以减少合并冲突。它们不进安装包 |

## 四、官方与订阅直连入口
**生效条件**：`loov::restore::managed()` 为真，也就是有登录前快照或者已登录。恢复原配置后，上游行为自动恢复。

**属于官方或订阅入口的**：category 为 `official` 的供应商（Claude Official、Claude Desktop Official、OpenAI Official、Google Official、Grok Official、Azure OpenAI、Nous Research），以及 providerType 为 `github_copilot`、`codex_oauth`、`xai_oauth` 的供应商。

| 位置 | 处理 |
|---|---|
| `lib.rs` 的 `init_default_official_providers` | managed 时不预置官方种子 |
| `tray.rs` 的托盘应用列表 | managed 时过滤掉上述供应商 |
| `Sidebar.tsx` 的授权中心 | managed 时不显示 |
| `ProviderForm.tsx`、`ClaudeDesktopProviderForm.tsx` 的预设选择 | managed 时过滤掉上述预设，判断函数为 `isLoovBlockedPreset` |

## 五、需要同步的测试
| 测试 | 原因 |
|---|---|
| `tests/config/subrouterProviderPresets.test.ts`、`src/config/ppioProviderPresets.test.ts` | 断言了推广链接与字段 |
| `tests/components/GrokBuildProviderForm.test.tsx` | 断言了 aff 链接 |
| `tests/components/__snapshots__/ProviderForm.presetRows.golden.test.tsx.snap` | 用 `-u` 重新生成，确认只有 URL 变化 |
| `tests/components/ApiKeySection.test.tsx` | 改为断言不显示推广语 |

**验收**：`pnpm typecheck` 通过、全量 vitest 通过、守护测试通过。在简洁模式与高级模式下手工检查，看不到合作伙伴推广语和追踪链接；managed 时看不到官方直连入口。
