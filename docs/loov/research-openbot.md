# OpenBot 调研：对 Loov Agent 的启示与路线建议（2026-10-30）

调研对象：[nightly-labs/openbot](https://github.com/nightly-labs/openbot) v0.54.0（只读阅读源码与文档），以及 OpenAI（learn.chatgpt.com/docs）和 Anthropic（code.claude.com/docs）官方文档的最新动向。文中 `[推断]` 为判断，其余均有文件路径或官方页面依据。

**许可证提醒**：OpenBot 是 PolyForm Noncommercial（只开放源码、禁止商用），Loov 只借鉴思路，不复制代码。

## 一、OpenBot 是什么（事实）
- **定位与热度**
  - 自我定位是「本地优先的常驻 AI 同事工作台」，用户用自己已经付费的 ChatGPT / Claude / Gemini / Grok 订阅，把它们变成多个长期在线、能互相协作的 agent。
  - 2026-08 建仓，4.1k star，到 10-29 已发布 v0.54.0，几乎每天发版。
- **核心概念**
  - **Agent**：有名字、指令、记忆，各自有独立工作区（`~/OpenBot/Agents/<id>`），切换模型时上下文不丢。
  - **任务队列**：先进先出，可暂停 / 恢复，崩溃后不丢；agent 执行中途可以插话改方向。
  - **协作**：agent 之间互发消息、群聊；有共享文件夹 `~/OpenBot/Shared`。
  - **浏览器与电脑**：内置浏览器（Electron 内嵌页面，经 CDP 驱动）、电脑操控（cua-driver），密码保险箱刷卡前要人工审批。
  - **自动化**：Workflows 可视化编排、定时 routine、webhook。
  - **远程**：Slack / Discord / Telegram 桥接；手机和网页通过 WebRTC 点对点直连本机，Cloudflare 只做信令，DTLS 端到端加密，并校验本机的 Ed25519 公钥。
- **怎么驱动各家 agent**：都走官方的结构化接口，不开终端（`src/backend/provider-drivers.ts`）。
  - Codex：`codex app-server`（JSON-RPC）
  - Claude：`claude-agent-sdk`，指向锁定版本的 claude 可执行文件
  - Grok、Antigravity、OpenCode、Cursor、Cline：统一走 **ACP**（Agent Client Protocol）
- **CLI 版本管理**
  - `native-runtime.lock.json` 按平台锁定版本和 sha256；每小时检查一次上游，校验后才启用，启用失败就保留旧版。
  - 远程黑名单 `provider-runtime-blocklist.json`：上游出坏版本时不用发新版就能拦住。
  - CI 每 6 小时跑一次「各家 CLI 金丝雀测试」，连续两次失败自动开 issue。
- **模型与成本**
  - 每个 agent 最多配 8 个备用模型，撞上额度或报错时自动切换，额度恢复后切回。
  - Usage 页面显示 token 数和估算成本。
- **权限**
  - 三档：仅限工作区 / 先问再做 / 完全访问。
  - 但当前默认是 `danger-full-access` + `approvalPolicy: never`，首次同意后不再逐条确认。
  - macOS 用 sandbox-exec 限制，Linux / Windows 上的部分 provider 只能选完全访问。
- **工程实践**
  - 测试以端到端为主：约 150 个 e2e / 冒烟脚本，配假 provider。
  - 静态检查：Biome、约 20 条自定义「anti-slop」规则、knip、类型基线只降不升；还限制给 agent 的提示词和工具总字节数。
  - 发布：附 SBOM 和构建来源证明；macOS 公证，Windows 用 Azure Trusted Signing。
  - 用 agent 开发自身：每个目录有 AGENTS.md，`.agents/skills` 里有 ship / babysit / triage 等技能。
- **商业模式**
  - 许可证是 **PolyForm Noncommercial**（只开放源码，不是开源）；CLA 要求贡献者把版权转让给公司。
  - 个人免费；工作用途收费：Starter €10/月，Team €25/月含 2 人、每多 1 人 +€15；另售托管服务器 €20–100/月。
  - **它自己不卖模型额度**。文档里自己承认，让团队成员共用一个 Claude 订阅属于「高风险」，建议改用 API Key。

## 二、和 Loov 的定位对比
| 维度 | OpenBot | Loov Agent |
|---|---|---|
| 谁付模型费 | 员工自己的订阅（有合规风险） | 公司 new-api 统一计费，零配置 |
| 网络 | 依赖用户自己的网络 | 自带抗量子安全通道，国内可用 |
| 应用形态 | 把各家 CLI 嵌进自己的对话界面 | 启动各家原生应用（CLI、桌面版），路由到公司网关 |
| 覆盖面 | 7+ 家 CLI，不管桌面版 App | 6 个应用，含 Claude Desktop 和 Codex 桌面版 |
| 管理 | 面向个人和小团队 | 企业集中管理（设备、额度、恢复原配置） |

**结论**：两者不是正面竞争。
- OpenBot 解决的是「把个人订阅用成团队」，Loov 解决的是「公司合规地给员工发 AI 额度」。
- 它最值得学的是**常驻同事形态**和**工程上的稳定性**；它的订阅共享模式恰好是我们应该避开的。[推断]

## 三、未来 1–2 年各家 agent 的走向（有官方依据）
1. **从「你去用的工具」变成「常驻的同事」**
   - OpenAI 的 **dots**：常驻 agent，有自己的云电脑，跨对话记住任务，能自己定时醒来，把工作拆给并行的后台 agent，在手机和 Slack 里指挥（learn.chatgpt.com/docs/dots）。
   - Claude Code 有 **Projects**（并行的云端会话共享记忆）、**agent teams**、云端定时任务（scheduled tasks，原名 routines）、**Remote Control**（手机接管本机会话）（code.claude.com/docs）。
   - OpenBot 做的是同一件事的本地开源版。
2. **一个会话在多个端之间流转**：桌面、云端、手机、Slack / Teams 之间无缝切换，例如 Claude 的 `--teleport` 和 ChatGPT 桌面 / 网页 / 手机线程同步。
3. **官方开始正式支持企业网关**
   - Codex 有「Connect to a gateway」「model catalogs」「Bedrock via LiteLLM」等文档；Claude Desktop 支持第三方网关。
   - 这说明 Loov 这种「网关统一计费」的路子和厂商方向一致，接入只会越来越稳。
   - 另一面是 ChatGPT Enterprise、Claude Team 这类厂商自己的企业版是竞争对手。Loov 的护城河在于：多厂商统一、国内网络、统一计费和审计。[推断]
4. **协议在收敛**
   - 工具层：MCP。
   - 客户端与 agent 之间：ACP（Grok、Antigravity、OpenCode、Cursor、Cline 都支持）。
   - 技能：跨厂商的 `~/.agents/skills`、AGENTS.md。
   - Codex 和 Claude 都有面向组织的插件管理。
5. **浏览器和电脑操控成为标配**：ChatGPT 的 Computer Use、Claude 的 Chrome 集成、OpenBot 内置浏览器。
6. **成本治理变重要**：按插件 / 用户统计用量和成本（Claude 有 plugin cost 统计）、自动切备用模型、按用户设额度上限。

## 四、我们能学什么（按优先级）

### P0：接下来 1–2 个版本，成本低、收益直接
1. **托管并锁定各家 CLI 版本**
   - **现状问题**：现在用 npm / 官方脚本装进员工系统，版本不受控，之前出过 Codex 升级失败、Claude 自动更新等问题。
   - **做法**：
     - 装到 `~/.loov-agent/runtimes/<工具>/<版本>`，用锁文件记录版本和 sha256。
     - hub 的 meta 里下发推荐版本和黑名单（hub 已经有 `/hub/v1/meta`，热更新）。
     - CI 加一个「new-api 加各 CLI 金丝雀」定时测试，复用现有 `client_e2e.sh` 和 Actions。
2. **模型故障自动切换**
   - **现状问题**：GPT-6 系列上游已经 500 一周，员工选了 GPT 只会看到报错。
   - **做法**：
     - 本地路由碰到上游 5xx / 额度错误时，按 hub 下发的备用模型链切换（例如 gpt-6-sol → claude-sonnet-5），并给出提示。
     - hub 根据 new-api 渠道状态，在启动台的模型下拉里把故障模型标成「暂不可用」。
     - 上游 CC Switch 本来就有故障转移队列，可以复用。
3. **客户端显示用量和成本**
   - 数据现成：new-api 日志里有。
   - hub 加一个按应用 / 按模型汇总的接口，启动台显示「本月已用 $x · 各应用占比」，并支持额度预警。
4. **公司级技能 / MCP 目录**
   - 把现在下发 `loov-search` MCP 的机制扩展成 hub 维护的目录：公司统一的 skills 写进 `~/.agents/skills` 和 `~/.claude/skills`，MCP 按需开启，员工一键装。

### P1：一个季度内
5. **把 agent 嵌进 Loov，而不只是打开终端**
   - **做法**：参照 OpenBot 的驱动层，用 ACP（一条协议覆盖 Grok、Antigravity，以后还有 OpenCode）、Claude Agent SDK、`codex app-server` 在 Loov 里直接开会话。
   - **这样能做到**：任务队列、「需要你确认」提醒、用量按会话归属；也是后面做常驻同事的地基。
   - **起步**：先做一家试点，建议 Claude Agent SDK 或 ACP。新模块放 `src-tauri/src/loov/agents/`，计费照旧走本地路由。
6. **策略中心**
   - hub 下发各家的托管配置：Claude managed settings、Codex requirements.toml。
   - 统一权限档位（仅工作区 / 先问 / 完全访问）、允许的 MCP 和插件、沙箱开关。
   - 企业合规审计的核心卖点。
7. **定时任务 / routine**
   - 用各家的无头模式（`claude -p`、`codex exec`）在本机定时跑日报、代码审查等，照样经 new-api 计费。

### P2：6–12 个月
8. **常驻同事 + 团队共享 agent**
   - agent 有记忆和任务队列，可以分享给同事。
   - **飞书桥接**：公司用飞书，已有 openclaw-lark 的经验，在飞书里 @ agent 派任务。
9. **手机远程**：hub 做信令，WebRTC 点对点端到端加密直连员工电脑。也可以先等厂商的 Remote Control / dots 成熟，看能不能走公司网关。
10. **云端执行环境**：在 GCP 上给 agent 跑托管沙箱，对标 OpenBot 托管服务器和 Codex Cloud；员工电脑关机任务也能继续。

### 工程实践直接借鉴
- 端到端优先，配假 provider；我们已有 `loov_e2e.rs`，可以补一个假 new-api，让 CI 不依赖真实额度。
- 每个目录一个 AGENTS.md；给 agent 的上下文设字节预算；类型和告警只降不升。
- 发布附 SBOM 和构建来源证明（GitHub `attest-build-provenance`）；Windows 签名用 Azure Trusted / Artifact Signing，与上次结论一致。
- 北极星指标：「每周至少有一次成功计费请求的活跃设备数」，new-api 日志可以直接算。

### 明确不学
- **不复制它的代码**：PolyForm Noncommercial 禁止商用，公司内部使用也算商用。只借鉴思路，代码自己写。
- **不走订阅共享**：OpenBot 自己都标成高风险，公司统一计费才是 Loov 的价值所在。
- **不默认完全访问且不审批**：企业场景要默认「先问」。
