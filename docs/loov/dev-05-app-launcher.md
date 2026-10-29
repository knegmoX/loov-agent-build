# 启动台：六个应用一键启动，全部经 new-api 计费（0.1.7）

## 目标
打开 Loov Agent 直接进入简洁页（每次都是，不再记住上次的高级模式）。简洁页的主体是启动台，共六张卡片，分两类：
- 终端 agent：Claude Code、Codex、Antigravity（agy 命令行）、Grok Build（`grok` 命令行）
- 桌面应用：Claude Desktop、Codex 桌面版（即 ChatGPT 桌面 App 的 Codex 模式）

xAI 没有 Windows/macOS 桌面版 Grok（只有手机 App），桌面上只有 Grok Build 命令行，所以 Grok 以终端 agent 的形式接入。

卡片的交互：

- 已安装：点卡片就启动。
- 未安装：点卡片就地安装，图标外圈显示进度；装完自动启动。
- 卡片上的小下拉是这个应用的默认模型。Claude Desktop 没有下拉，它的模型在 Desktop 自己的菜单里选。

**原则**：客户端里每个应用启动后都必须走公司 new-api 统一计费，员工不做任何二次配置。新增应用入口时，必须同时设计它接入计费的方式；接不进计费的应用不放进客户端。

## 计费链路
| 应用 | 怎么接入 | 请求路径 |
|---|---|---|
| Claude Code | 登录时进入路由模式（`provision::attach`），`settings.json` 指向本地路由 | `claude` → 本地路由 `/v1/messages` → 隧道 → new-api |
| Codex | 同上，`config.toml` 指向本地路由 | `codex` → 本地路由 `/v1/responses` → 隧道 → new-api |
| Antigravity | 不进 Gemini 的路由模式：那会改写员工 Gemini CLI 的 `~/.gemini/.env`。启动时用私有 `--gemini_dir`（`~/.loov-agent/agy/loov/`，`modelProvider: gemini`），环境变量 `GOOGLE_GEMINI_BASE_URL=<本地路由>/loov/gemini`、`GEMINI_API_KEY=lat_…`；`--model` 传员工选的模型 | `agy` → 本地路由 `/loov/gemini/v1beta/...`（`loov::gemini_route` 固定发往 `universal-gemini-loov`）→ 隧道 → new-api 原生 Gemini 接口 |
| Grok Build | 启动时进入 Grok 的路由模式（`provision::attach_grok`）。`loov-grok` 供应商写一张模型表 `loov`，`~/.grok/config.toml` 指向本地路由 `/grokbuild/v1`，令牌是 `lat_` | `grok` → 本地路由 `/grokbuild/v1/responses` → 隧道 → new-api。选 Claude / GPT 时走 new-api 原生 `/v1/responses`；选 Gemini 时供应商标成 `openai_chat`，由本地路由转成 Chat Completions（new-api 的 Gemini 不支持 responses） |
| Claude Desktop | 第一次从启动台启动时进入 Desktop 的路由模式（`provision::attach_desktop`），写 3p 网关配置，指向本地路由 `/claude-desktop`，带本地网关 token | Desktop → 本地路由 `/claude-desktop/v1/messages` → 隧道 → new-api |

| Codex 桌面版 | 和 Codex 命令行共用 `~/.codex/config.toml`，登录时 Loov 已写好路由，不需要额外配置。正在运行时同样会询问是否重启 | 桌面版 → 本地路由 `/v1/responses` → 隧道 → new-api。0.1.8 起 Loov 写 `model_catalog_json`（见 dev-06），桌面版不登录 ChatGPT 也能在模型选择器里选公司模型；万一发来不在 Loov 目录里的模型，forwarder 用 `provision::coerce_codex_model` 换成员工选的 Codex 模型 |

这些路径的上游 Key 都是占位（`LOOV_MANAGED`），由 forwarder 在内存里换成真实 Key（`provision::is_loov_provider` 认这四个供应商）。agy 把本地令牌放在 `x-goog-api-key` 头里，`local_auth::bearer_of` 已支持这个头。

agy 自己的模型表里有 new-api 上没有的模型（3.7 Flash、3.1 Pro，以及生成标题用的 `gemini-3.1-flash-lite-preview`）。`gemini_route` 会把这些模型改写成员工选的 Gemini，避免返回 404。

**不向 Google 分享数据**：agy 首次引导的服务条款页默认勾选「允许 Google 收集交互数据」。实测（agy 1.4.7，API Key 模式）这个选择不落盘，`settings.json` 预写 `telemetryEnabled: false` 也不影响，每轮对话都会往 Clearcut（`play.googleapis.com`）上报。所以 Loov 在隧道路由里直接丢弃这个域名（`tunnel::TELEMETRY_BLOCK`），agy 经 Loov 启动时上报一律失败，与员工在引导里怎么选无关。私有 profile 里仍预写 `telemetryEnabled: false`，留给以后认这个键的版本。服务条款由员工自己确认，Loov 不代为接受。功能开关服务 `antigravity-unleash.goog` 没有拦截。

Claude Desktop 正在运行时，配置要重启才生效：后端返回 `DESKTOP_RESTART_REQUIRED`，前端询问员工后再带 `restart=true` 重来。「恢复原配置」会退出 Desktop 的路由模式，并删除 `loov-claude-desktop` 供应商。

## 安装
- CLI 复用上游的工具安装（`commands::run_tool_install_with_env`）：Claude Code 优先用官方安装脚本、npm 兜底；agy 用官方安装脚本；Codex 只能用 npm。安装进程带 `HTTPS_PROXY`，指向隧道（国内直连 npm、Google 不稳定）。没有 Node.js 时 Codex 返回 `NODE_REQUIRED`，前端给出下载 Node.js 的入口。
- Claude Desktop 经隧道下载官方安装包：
  - macOS：从 `downloads.claude.ai/releases/darwin/universal/RELEASES.json` 取 zip，`ditto` 解到 `/Applications`；不可写时解到 `~/Applications`。
  - Windows：`claude.ai/api/desktop/win32/<arch>/msix/latest/redirect` 下载 MSIX，再用 `Add-AppxPackage` 按用户安装。
  - Codex 桌面版（OpenAI 现在的桌面 App，名字叫 ChatGPT）：
    - macOS：下载 `persistent.oaistatic.com/codex-app-prod/ChatGPT.dmg`（只有 Apple 芯片版本），挂载后拷 `ChatGPT.app`，按 bundle id `com.openai.codex` 检测。
    - Windows：商店 9PLM9XGG6VKS，包名 `OpenAI.Codex`。先用 winget 静默装（以检测到包为准，20 分钟超时），失败时经隧道下载官方 MSIX 再 `Add-AppxPackage`。详见 dev-06。
  - 桌面应用的检测、安装、运行检测、退出、启动都由 `apps::desktop::Spec` 描述，Claude Desktop 与 Codex 桌面版共用一套实现。
  - Linux 没有官方版，卡片置灰。
- 隧道就绪时，上游「应用」页的安装 / 升级也带隧道代理（`apps::tunnel_proxy_env`）。Windows 的官方安装脚本（如 Codex 的 `irm https://chatgpt.com/codex/install.ps1 | iex`）跑在 PowerShell 5.1 里，`irm` 不读 `HTTPS_PROXY`，所以 `powershell_encoded_command` 统一加了前导，把环境里的代理设成 .NET 默认代理。国内直连 chatgpt.com 不通，没有这个前导就会报「无法连接到远程服务器」。
- 进度事件 `loov-app-install`：`{app, stage: download|install|configure|done, percent}`。npm 和安装脚本没有百分比，前端转圈显示。

## 代码位置
| 文件 | 内容 |
|---|---|
| `src-tauri/src/loov/apps.rs` | 检测、安装、启动；Claude Desktop 各平台实现 |
| `src-tauri/src/loov/gemini_route.rs` | agy 的 `/loov/gemini` 入口与模型改写 |
| `src-tauri/src/loov/provision.rs` | Gemini 选模（`pick_models`、`is_agent_gemini`）、`desktop_provider`、`attach_desktop` |
| `src-tauri/src/commands/misc.rs` | `launch_loov_cli`（复用上游终端启动器）、`run_tool_install_with_env`、`loov_probe_tool` |
| `src-tauri/src/proxy/handlers.rs`、`server.rs` | `handle_gemini_with`（可指定供应商）与 `/loov/gemini/*path` 路由 |
| `src/components/loov/AppLauncher.tsx` | 启动台卡片、进度环、模型下拉、Desktop 重启确认 |

## 验证
- 单测：`cargo test --lib loov::`，以及 `pnpm vitest run tests/components/LoovPage.test.tsx`（启动、就地安装后启动、Desktop 重启确认、不支持的平台）。
- E2E `full_flow_login_tunnel_agents_logout` 第 6b、6c 步：
  - `/loov/gemini` 不带令牌返回 401，带令牌返回 200；未知模型被改写。
  - 用 `agy_launch_spec` 的参数真跑 `agy -p`，strace 确认只连本机。
  - Desktop 接入后 profile 指向本地路由且不含真实 Key；`/claude-desktop` 带网关 token 调用成功。
  - 恢复原配置后 Desktop 退出路由模式。
- Windows / macOS 的 Claude Desktop 安装与启动只能在真机上验收。
