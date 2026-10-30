# 计费可信、模型回退、agent 权限档位（0.1.9 / hub 0.5.0）

原则：Loov 不干涉 agent 本身，只做集合、基础配置和计费。所有出站都计费、都能归属到「用户 / 设备 / 应用」，金额以 new-api 为准。

## 按应用计费
- hub 给每台设备的每个应用各建一把 new-api Key，令牌名格式 `loov-<设备id>-<应用>`（共享额度的格式是 `loov-<用户名前 18 位>-<设备id>-<应用>`，不超过 50 字符）。
  - 应用：`claude`、`codex`、`codexdesk`、`agy`、`grok`、`desktop`、`feishu`。
  - bootstrap 下发 `api.keys`。老设备在下一次 bootstrap 时补建；建失败时客户端退回通用 Key `api.key`。
  - 吊销设备时一并删除这些 Key。本地保留 Key 名，当月用量照样能统计到。
- **客户端**（`loov/attribution.rs`）：转发时判定请求属于哪个应用，注入对应的 Key。判定依据依次是：
  1. 请求头 `x-loov-app`：Loov 自己写进配置的，比如 Grok 的额外模型表、以后的飞书渠道。
  2. 供应商行：每个应用有自己的 Loov 供应商和聚合成员。
  3. Codex 的 `originator` 头：区分命令行和桌面版。

  `x-loov-app` 只在本机用，转发前删掉。应用 Key 和通用 Key 一样只在内存里，不落盘。
- **Grok 的额外模型表**：经本地 Codex 入口转发，表里带 `extra_headers = { x-loov-app = "grok" }`。计费和本地用量日志都记在 Grok 名下。

## 用量
- **hub `GET /hub/v1/usage?range=today|month`**：
  - 用 hub 代持的长期令牌，按每个 Key 名查 new-api `/api/log/self/stat`（消费日志、精确匹配 token_name），得到实扣金额，按应用汇总。
  - 统计范围包括本月中途吊销的设备。
  - 另加云端搜索成本、月预算和本月已用。缓存 60 秒。
- **客户端**：Loov 页的用量卡片（`UsageCard.tsx`）显示本月 / 今日、预算进度、按应用的占比和云端搜索。高级模式的「用量统计」页沿用上游的本地估算明细。
- new-api 的「模型价格」页是关闭的（`/api/pricing` 返回 pricing is disabled），读倍率配置需要 root 权限，所以本地价格表没有和 new-api 对齐。**以 hub `/usage` 的数字为准。**

## 云端搜索按成本收费
- **成本**：SearXNG 和抓取跑在现有服务器上，计算资源的边际成本约为 0，主要成本是把结果发回客户端的出站带宽。
- **计量**：hub 记下每次 `web_search` / `web_fetch` 返回的字节数（`mcp_usage.bytes_out`）。
- **计价**：单价 = 字节 × `HUB_SEARCH_USD_PER_GB`（默认 0.12）+ 次数 × `HUB_SEARCH_USD_PER_CALL`（默认 0）。
- **结算**：每轮维护时结算昨天及以前的用量，按「用户 × 日」从钱包扣一次额度（new-api `add_quota` 的 `subtract` 模式）。不足 1 个额度单位的记为已结算但不扣。
- **实测**：一次搜索返回约 600 字节，成本约 $0.00000007，远低于一分钱。

## 月预算
- **设置**：`node dist/cli.js user budget <用户名> <美元|none>`。
- **执行**：维护任务每 10 分钟检查一次。本月花费达到预算时，停用该用户全部设备的 Key（new-api 令牌状态改为停用）；调高预算或进入新月份后自动恢复。
- **客户端提示**：用到 80% 时提醒；停用后显示「本月预算已用完」。

## 内置工具核实（2026-10-30，new-api 日志）
- Codex 自带的网页搜索是 OpenAI 托管工具：GPT 请求直接返回 400「Hosted tools are not supported on Google Cloud」；转给 Claude 时被丢弃。
- 因此 Codex 加固键加了 `web_search = "disabled"`，模型改用 Loov 下发的云端搜索 MCP。恢复原配置时还原。
- 近 30 天没有单独的网页搜索计费记录。生图按模型（gemini-nano-banana-2.1）正常计费。

## 模型回退
- **为什么要自己做**：上游 CC Switch 的故障转移只能跨供应商，而且在聚合模式下整体关闭；Loov 的应用都在聚合模式。
- **怎么做**：在 forwarder 里对 Loov 供应商单独处理（`loov/fallback.rs`）。上游报 5xx 或 429、并且还没向客户端写数据时，按 hub 下发的回退链（`modelFallbacks`，来自 hub `data/model-kinds.json` 的 `fallbacks`，热更新）依次改用别的模型，最多 2 个，仍经 new-api 计费。
- **跳过与限制**：跳过 hub 标为 `down` 的模型；agy 的原生 Gemini 入口只回退到 Gemini 模型。
- **提示员工**：界面弹一次「X 暂时不可用，已自动改用 Y（按 Y 计费）」，价格可能不同，不能静默换。
- **实测**：GPT-6 上游 500 期间，Claude Code 里选 gpt-6-sol，自动改用 claude-sonnet-5，返回成功。

## 模型健康度
- **判断**：hub 维护任务按 new-api 日志统计近 30 分钟每个模型的错误和成功次数。有错误（≥3）且没有成功的是 `down`；错误多于成功的是 `degraded`。
- **手工指定**：可以在 `model-kinds.json` 的 `health` 里写，优先于自动判断。
- **展示**：随 `modelCatalog` 下发，启动台模型下拉里标「暂不可用 / 不稳定」。客户端每 5 分钟刷新一次模型目录。

## agent 权限档位（`loov/safety.rs`）
Loov 不做自己的沙箱，只把一个全局档位写成各家 agent 自己的原生设置：

| 档位 | Claude Code `permissions.defaultMode` | Codex `approval_policy` / `sandbox_mode` | Grok `[ui] permission_mode` | agy 启动参数 |
|---|---|---|---|---|
| 谨慎 | `default` | `on-request` / `read-only` | `ask` | 不加参数 |
| 标准（默认） | `acceptEdits` | `on-request` / `workspace-write` | `ask` | `--mode accept-edits` |
| 完全 | `bypassPermissions` | `never` / `danger-full-access` | `always-approve` | `--dangerously-skip-permissions` |

- **不覆盖员工的值**：只在文件里没有这个键、或者还是 Loov 上次写的值时才写。写过的值记在 prefs 的 `safety_applied` 里。
- **恢复原配置**：有快照时改回原值，没有快照时只删 Loov 写的值。
- **切到「完全」**：需要二次确认。
