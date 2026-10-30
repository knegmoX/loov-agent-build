# 架构与安全模型

## 1. 组件
```text
员工电脑（Loov Agent）
  ├ WebView 前端：简洁模式只有 Loov 页（src/components/loov/），高级模式是完整的 CC Switch
  ├ Rust 后端
  │   ├ src-tauri/src/loov/        Loov 专属逻辑（见第 3 节）
  │   ├ 本地路由 127.0.0.1:15721   上游 CC Switch 的代理服务，Loov 加了令牌鉴权与 Key 注入
  │   └ 上游 HTTP 客户端           全局代理指向 xray sidecar（隧道）
  └ xray sidecar                   127.0.0.1 随机端口：http 与 socks 入口，带随机口令
        └─ REALITY + ML-DSA-65 + VLESS Encryption ─→ 台湾 / 东京节点 ─→ VPC 内网 ─→ me.loov.cc
           （直连节点不通时经员工本机代理拨号，链路仍端到端加密）

服务器（新加坡）
  ├ nginx  me.loov.cc            /hub/ 指向 loov-hub，其余路径指向 new-api
  ├ loov-hub 127.0.0.1:3005      账号、设备、隧道参数、余额、兑换、/meta、MCP 搜索
  ├ new-api 127.0.0.1:3001       计费与模型路由，后端经 vertex-proxy 访问 Vertex AI
  └ Remnawave（台湾 VPC）         节点与隧道用户管理，hub 通过内网 API 调用
```

## 2. 安全模型
| 规则 | 实现 |
|---|---|
| 真实 new-api Key 不落盘 | 供应商行里只存占位 `LOOV_MANAGED`，凭据文件里的 bootstrap 也会清掉 Key。启动时用设备令牌向 hub 换 Key，`proxy/forwarder.rs` 转发时经 `loov::resolve_placeholder_key` 换成内存里的 Key；hub 不可达就是离线状态 |
| CLI 配置里没有可复用的凭据 | Claude Code 和 Codex 配置里写的是本地路由令牌 `lat_…`，只对 127.0.0.1:15721 有效 |
| 失败时不直连 | 数据库里的上游代理固定为黑洞 `http://127.0.0.1:1`，隧道地址只应用到内存。Claude 的 `HTTPS_PROXY` 也写成隧道或黑洞 |
| 登录类请求直连 hub | `hub_client` 不经过隧道和系统代理，隧道凭据由登录接口下发 |
| 设备级吊销 | 每台设备一枚 new-api Key 加一个 Remnawave 用户。吊销后 Key 与隧道同时失效 |
| 凭据加密 | `~/.loov-agent/loov/credentials.enc`（设备令牌、节点参数）用随机数据密钥加密。`credentials.key` 里的数据密钥在 Windows 上由 DPAPI 保护，其它平台用机器绑定密钥（机器 ID ‖ 用户名派生，不是秘密）：**只防文件被拷走，不防同一用户下的其他进程**。能被拿走的是可吊销的设备令牌，不是 Key。macOS 钥匙串等有开发者签名后再接 |
| 系统环境变量不绕过 | 员工 shell / 注册表里的 `CLAUDE_CODE_USE_VERTEX` 等协议选择器、`ANTHROPIC_API_KEY`、`ANTHROPIC_MODEL` 会被 Claude Code 读到。接管期间代理契约（`live/project/claude.rs`）补写中和值（`provision::neutralize_claude_env`），`settings.json` 的 env 优先于系统环境。**员工自己的 Vertex / Bedrock 配置从不删除**：用 Loov 时被压住、走 new-api 计费，「恢复原配置」后照常直连。横幅不把 `ANTHROPIC_VERTEX_*` 等当冲突，接管期间也只提示仍会影响链路的变量 |
| 默认模型 | Loov 页选的 Claude 默认模型换成档位别名（`sonnet` / `opus` / `haiku`）写进 `ANTHROPIC_MODEL`，否则 Claude Code 自选 opus 档。Codex 下拉含 OpenAI 与 Claude 模型；默认取 hub 推荐的 OpenAI 模型，没有就取推荐的 Claude 模型（hub `data/model-kinds.json` 的 `recommended`，热改） |
| 未连时的报错 | Loov 供应商在未登录 / 隧道未连时本地路由回 400 并写明原因（CLI 不重试），且不计入熔断器 |
| 启动顺序 | 黑洞 → Loov 起隧道 → 上游接管 → 加固键。各挂载点打 `loov::startup_mark` 标记，上游 `controller::startup` 入口检查；顺序错了调试构建 panic、发布构建阻断出站。源码顺序由单测 `lib_rs_mount_points_stay_in_order` 守住 |
| 不破坏员工原有配置 | 首次登录前快照 Claude 与 Codex 配置。「恢复原配置」只把受管字段写回原值。接管过本机后，退出时不调用上游的「指回直连」 |

## 3. `src-tauri/src/loov/` 模块
| 模块 | 职责 |
|---|---|
| `mod.rs` | 运行状态 `Status`、hub 地址、设备名、常量 |
| `hub_client.rs` | loov-hub 契约类型与 HTTP 客户端，错误统一为 `{code, message, devices?}` |
| `credentials.rs` | 凭据 v2 读写、密钥封装（DPAPI 或机器密钥）、v1 自动迁移 |
| `tunnel.rs` | 生成 xray 配置、按 `xray.lock.json` 校验二进制、启动 sidecar、清理残留进程；逐节点探测（直连 / 经本机代理）后选节点和走法，再做端口独占与 5 连发健康检查 |
| `sysproxy.rs` | 读本机代理（环境变量、Windows「Internet 选项」、macOS `scutil --proxy`），供 xray 经 `sockopt.dialerProxy` 拨号 |
| `local_auth.rs` | 生成与轮换 `lat_` 令牌、本地路由鉴权中间件、识别占位令牌 |
| `provision.rs` | 写统一供应商（含 Gemini 子供应商）与 Claude Desktop 供应商、聚合成员（GPT / Gemini，见 dev-06）、进入聚合模式、写 Claude 与 Codex 加固键（Claude 加固键含 `CLAUDE_CODE_AUTO_MODE_SERVER=0`：上游不支持 auto mode 服务端分类检查，不关每个会话都会提示 isn't eligible） |
| `apps.rs` | 启动台：六个应用的检测、安装（经隧道下载）、启动（见 dev-05） |
| `attribution.rs` | 判定请求来自哪个应用，选用该应用的 new-api Key（见 dev-07） |
| `fallback.rs` | 模型故障时在同一网关内换模型重试（见 dev-07） |
| `safety.rs` | agent 权限档位，写成各家原生设置（见 dev-07） |
| `netmode.rs` | 节点偏好、网络模式；代理 / 全局模式接管系统代理并负责还原（见 dev-06） |
| `gemini_route.rs` | Antigravity 的本地入口 `/loov/gemini`，固定发往 Loov 的 Gemini 子供应商 |
| `restore.rs` | 登录前快照、恢复原配置、`managed()` 判断、官方直连入口判定 `hide_provider` |
| `mcp_proxy.rs` | 云端搜索 MCP 的本地透传 `/loov/mcp`：lat_ 换成 ldt_，经隧道发往 hub |
| `privacy.rs` | 隐私整流器：改写出站头与 `metadata.user_id` 等字段 |
| `flows.rs` | 启动、登录、注册、登出、刷新、关闭的编排 |
| `commands.rs` | Tauri 命令与测试钩子 |

## 4. 上游挂载点
上游文件里的改动都带 `// loov:` 注释。合并上游后先执行下面的命令，核对数量与位置：

```bash
grep -rn '// loov' src-tauri/src src --include=*.rs --include=*.ts --include=*.tsx | grep -v src-tauri/src/loov/
```

| 文件 | 作用 |
|---|---|
| `src-tauri/src/lib.rs` | 模块声明、命令注册、启动时固定黑洞代理、在上游接管前先跑 `loov::flows::startup`、接管后写加固键、退出与重启清理、不注册 updater、深链接 scheme |
| `src-tauri/src/proxy/server.rs` | 本地路由加 `require_bearer` 中间件、`/loov/mcp` 与 `/loov/gemini/*path` 路由 |
| `src-tauri/src/proxy/handlers.rs` | `handle_gemini_with`：可指定固定供应商（agy 入口用） |
| `src-tauri/src/commands/misc.rs` | `launch_loov_cli`、`run_tool_install_with_env`、`loov_probe_tool`（启动台复用上游终端启动与工具安装） |
| `src-tauri/src/proxy/forwarder.rs` | 把 Key 占位换成真实 Key；隐私整流器的两个调用点；对照测试 `loov_privacy_rewrites_only_loov_providers` |
| `src-tauri/src/live/project/claude.rs`、`codex.rs` | 代理凭据写成 `lat_`，检测时也认 `lat_`；Claude 代理契约补写中和值 |
| `src-tauri/src/mode/controller.rs` | `startup` 入口的启动顺序检查 |
| `src-tauri/src/proxy/http_client.rs` | `get_proxy_port` 改为 crate 内可见（本机代理检测排除自己） |
| `src-tauri/src/services/proxy.rs`、`codex_config.rs` | 判断是否已接管时也认 `lat_` |
| `src-tauri/src/config.rs` | 配置目录名常量 `APP_DIR_NAME = ".loov-agent"` |
| `src-tauri/src/commands/settings.rs` | 两个更新命令改为桩 |
| `src-tauri/src/deeplink/parser.rs` | scheme `loovagent` |
| `src-tauri/src/tray.rs` | 官网链接；接管期间不列官方直连 / 订阅类供应商 |
| `src/components/providers/forms/ProviderForm.tsx`、`ClaudeDesktopProviderForm.tsx`、`src/components/mcp/UnifiedMcpPanel.tsx` | 接管期间过滤官方预设；锁定 Loov 托管的 MCP 条目 |
| `src/App.tsx`、`src/components/shell/Sidebar.tsx`、`src/lib/navigation.ts` | 简洁模式（每次打开都从简洁模式开始）与 Loov 导航（侧栏「Loov Agent」渐变字）；接管期间过滤环境变量冲突横幅 |
| `src/components/settings/AboutSection.tsx`、`src/lib/updater.ts` | 关闭邀 Star 与更新检查 |

## 5. 流程

**启动**（`lib.rs` 中、上游 `controller::startup` 之前）
1. 有凭据时把数据库里的上游代理固定为黑洞。
2. 用 `loov::flows::startup` 拉取 bootstrap，8 秒超时。失败时缓存里没有 Key（0.1.3 起不落盘），进入离线状态。
3. 建隧道（`tunnel::establish`）：xray 带上每个节点的探测入站启动，直连和经本机代理两条路并发各探一次；按「直连可用 > 经代理可用」、同档按下发顺序选节点。全部节点直连都通时沿用这个进程，否则按结果重启。之后做 5 连发健康检查，再把上游代理应用到内存里的隧道地址，写统一供应商。全部不通时状态里给出每个节点的原因（TCP 不通还是握手被干扰）。
4. 上游按持久化的模式自动接管，Loov 再补写加固键。

**登录**
1. 首次登录前先拍快照（`restore::snapshot_if_absent`），失败就不登录。
2. 调 hub 登录，保存凭据，并轮换 `lat_` 令牌。
3. 启动隧道并自检，进入路由模式，写加固键。

**登出**：hub 吊销设备（失败也继续）→ 停 xray → 上游代理切回黑洞 → 删除凭据 → Claude 的 `HTTPS_PROXY` 写成黑洞。路由模式保持接上，所以仍然不会直连。

**恢复原配置**：先登出 → 退出 Claude、Codex 与 Claude Desktop 的路由模式 → 按快照写回受管字段 → 删除 Loov 供应商、本地令牌与黑洞代理 → 归档快照。

**退出应用**：停 xray。如果 `restore::managed()` 为真，就不调用上游的 `detach_all`。被 SIGTERM / SIGINT / SIGHUP 结束时由 `flows::install_signal_cleanup` 先停 xray 再退出。

**启动中的界面**：建隧道期间状态为 `Starting`（Loov 页显示「正在连接安全通道…」并每秒刷新）；离开 `Starting` 时 `LoovStartupSync` 让全部查询失效，避免上游路由状态停在「未运行」的误报。登录 / 注册被拒时撤掉本次刚拍的快照。

## 6. 落盘文件
| 路径 | 内容 |
|---|---|
| `~/.loov-agent/cc-switch.db` | 上游数据库，另有 Loov 的 settings 键：`loov_local_token`、`privacy_config`；`mcp_servers` 表里有 `loov-search` 条目 |
| `~/.loov-agent/loov/privacy.key` | 隐私整流器的伪名密钥（0600） |
| `~/.loov-agent/loov/credentials.enc`、`credentials.key` | 加密的凭据与被封装的数据密钥 |
| `~/.loov-agent/loov/original/` | 登录前快照，恢复后改名为 `original.restored-<时间>` |
| `~/.loov-agent/loov/run/` | xray 配置（0600）、pid 文件与日志 |
| `~/.claude/settings.json`、`~/.claude.json` | Claude Code 配置与 MCP 列表，受管字段由 Loov 与 CC Switch 写入 |
| `~/.codex/config.toml` | Codex 配置、MCP 列表与加固键 |

## 7. 测试入口
| 范围 | 命令 |
|---|---|
| loov 单测 | `cd src-tauri && cargo test --features test-hooks --lib loov::` |
| 前端 Loov 页 | `pnpm vitest run tests/components/LoovPage.test.tsx` |
| 真实链路 E2E | `LOOV_E2E=1 … cargo test --features test-hooks --test loov_e2e`，在服务器上用 `loov-ops/scripts/client_e2e.sh` |
| 界面验收 | 必须用隔离 HOME：`HOME=/tmp/gui-home loov-agent` |
