# 节点选择、网络模式与「每个 agent 都能选全部模型」（0.1.8）

## 节点选择
- 默认「自动」：登录 / 重连时并发探测 hub 下发的全部节点（`tunnel::probe`，同时记下每个节点的耗时），能通的节点交给 xray 的 leastPing balancer。
- 员工可以在状态条的节点下拉里固定一个节点（`loov_set_node`）。
  - 选择存在 `<loov>/prefs.json`（`netmode::Prefs`）。
  - 手选的节点这次探测没通过时回到自动，状态里带 `pin_fallback`，界面提示「已回到自动」。
- 换节点、换模式都不重新探测：`tunnel::reconfigure` 沿用探测结果，用**原来的端口和口令**重启 xray。已启动的应用环境变量里带着 Loov 的代理地址，不受影响。手选的节点之前没通过探测时整条链路重建一次（`flows::reconnect`）。
- 状态条显示当前节点名和探测耗时（`Status::Ready.net`，`NetView`）：手选时显示那个节点；自动时显示探测最快的那个，xray 的 leastPing 用的是同一类测量。

## 网络模式
| 模式 | 系统代理 | 经安全通道的流量 |
|---|---|---|
| 直连（默认） | 不动 | 只有 Loov 自己（new-api、安装包、经 Loov 启动的应用） |
| 代理 | 指向 `sys-in` | 国外网站；`geosite:cn` / `geoip:cn` 与内网直连 |
| 全局 | 指向 `sys-in` | 除内网外的全部流量 |

- **sys-in 入站**
  - 是免认证的 HTTP（17890）和 SOCKS（17891）入站，只监听 127.0.0.1；端口被占时随机分配。
  - 系统代理不支持用户名口令，所以单独开这个入站。Loov 自己用的带口令入站不变。
  - 直连模式下不开。
- **sys-in 的路由规则**
  - 排在最前面，自成一套：员工自己的上网流量，内网直连，不套给 Loov 流量的内网丢弃和遥测拦截。
  - 代理模式用 `IPIfNonMatch`：两份域名表都没认出的域名先解析，国内 IP 直连。其余规则在第一轮就按入站命中，不会多做 DNS 解析。
- **系统代理写在哪**（`netmode.rs`）
  - Windows：HKCU Internet Settings（`ProxyServer` / `ProxyOverride`，接管期间拿掉 `AutoConfigURL`），然后 `InternetSetOptionW` 通知生效。
  - macOS：`networksetup`，对每个启用的网络服务设置。
  - Linux：GNOME `gsettings`，其他桌面会提示失败。
- **备份与还原**
  - 第一次接管前，原设置备份到 `<loov>/sysproxy-backup.json`。
  - 切回直连、隧道重建 / 意外退出、登出、恢复原配置、退出程序时，都按备份原样还原。
  - 启动时如果发现上次留下的备份（崩溃、断电），先还原再连。
- **防断网**
  - 每 15 秒检查一次 xray 是否还活着（`flows::spawn_watchdog`）。
  - 意外退出时先还原系统代理、出站回黑洞，再自动重连。
- `sysproxy::detect` 跳过指向 sys-in 的系统代理，免得把自己当成上游。
- 第一次切到代理 / 全局时，界面说明会接管系统代理，员工确认后才切换。

## 每个 agent 都能选 Loov 的全部模型
启动台卡片上的下拉只是默认模型；员工在各应用自己的模型选择器里可以切换 Loov 的全部模型，都经 new-api 计费。

| 应用 | 做法 |
|---|---|
| Claude Code | 进**聚合（Stack）模式**：默认路由是 Loov 的 Claude 子供应商；成员 `loov-claude-gpt`（`apiFormat=openai_responses`）和 `loov-claude-gemini`（`openai_chat`）发布 GPT / Gemini。上游把全部模型写进 `settings.json` 的 `modelPicker`，`/model` 里可选 |
| Codex（命令行、桌面版） | 聚合模式：默认路由是 Loov 的 Codex 子供应商，它的 `modelCatalog`（GPT + Claude）由 new-api 原生 `/v1/responses` 承接；成员 `loov-codex-gemini`（`openai_chat`）发布 Gemini。全部写进 `model_catalog_json`，桌面版不登录 ChatGPT 也能在模型选择器里选 |
| Claude Desktop | 聚合模式：成员 `loov-desktop-gpt` / `loov-desktop-gemini`，Desktop 菜单里显示成「Loov GPT / gpt-…」 |
| Grok Build | 上游一个供应商只投影一张表。Loov 另给每个模型写一张 `[model.loov-<模型>]`，都指向本地路由的 Codex 入口（`/v1/responses`）：Claude / GPT 原样发，Gemini 用 Codex 聚合里 Gemini 成员的 id（`ccs-<key>/<模型>`）。每次从 Loov 页启动 Grok 时同步（`provision::sync_grok_models`），恢复原配置时删除 |
| Antigravity | agy 的 `/model` 只列它内置的模型，没法加。`gemini_route` 按档位换成 Loov 的：内置 Pro → Loov 的 Pro 档（如 gemini-4-argon），内置 Flash → Loov 的 Flash，员工在 agy 里切 Pro / Flash 就真的换了模型 |

- 成员行由 `provision::save_members` 在每次 `apply`（登录 / 刷新）时写入；hub 目录里没有这一家模型时删掉。
- `attach` / `attach_desktop` 调 `enter_stack` 同步成员。
- 成员 id 都在 `is_loov_provider` 里，真实 Key 注入守卫和隐私整流对它们同样生效。
- 恢复原配置时先把成员移出聚合名单，再删除成员行。

实测（2026-10-29）：new-api 的 `/v1/responses` 不支持 Gemini（返回「not served by this proxy role」），所以 Gemini 一律转成 Chat Completions。GPT-6 系列上游目前一律 500（与本功能无关），链路只能验证到「请求正确发到 new-api」。

## Codex 桌面版（修复）
- Windows：商店 9PLM9XGG6VKS 的上架名是 ChatGPT，装下来的 MSIX 包名是 `OpenAI.Codex`（PFN `OpenAI.Codex_2p2nqsd0c76g0`），0.1.7 按 `*ChatGPT*` 检测，所以永远显示未安装。
- Windows 安装顺序：
  1. 先用 winget 从商店装，每 3 秒检测一次，以检测到包为准，20 分钟超时。
  2. 失败时经隧道下载官方 MSIX（`persistent.oaistatic.com/codex-app-prod/ChatGPT-{x64|arm64}.msix`，约 1GB，有进度），然后 `Add-AppxPackage`。
- 任何桌面应用安装完都要能检测到，检测不到就报错，不再提示「已安装」。
- macOS：官方包是 `codex-app-prod/ChatGPT.dmg`（约 800MB，只有 Apple 芯片版本），里面是 `ChatGPT.app`，bundle id `com.openai.codex`。旧版 ChatGPT（`com.openai.chat`，同名）不读 config.toml，不算安装。Intel Mac 上这张卡置灰。

## 代码位置
| 文件 | 内容 |
|---|---|
| `src-tauri/src/loov/netmode.rs` | 偏好、sys-in 端口、系统代理备份 / 接管 / 还原（三个平台） |
| `src-tauri/src/loov/tunnel.rs` | `SysLayout`、`Selection`、`establish`（手选节点）、`reconfigure` |
| `src-tauri/src/loov/flows.rs` | `set_selection`、`reconnect`、`spawn_watchdog`、生命周期里的系统代理还原 |
| `src-tauri/src/loov/provision.rs` | `MEMBERS`、`member_provider`、`enter_stack`、`sync_grok_models`、`codex_model_catalog` |
| `src/components/loov/NetworkControls.tsx` | 节点下拉、网络模式三段开关 |
