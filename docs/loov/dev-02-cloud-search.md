# dev-02 云端搜索

## 目标
让 Claude Code 和 Codex 在隧道内能搜索外网、抓取网页。两者原生的 WebSearch 都依赖模型方的服务端工具，而公司的 Vertex 链路被组织策略拦截，用不了。所以由公司服务器提供搜索，以 MCP 的形式下发给两个 CLI。

## 架构
```text
Claude Code / Codex
  └ MCP（Streamable HTTP）http://127.0.0.1:15721/loov/mcp，携带 Bearer lat_…
      └ 本地路由 /loov/mcp：校验 lat_，换成设备令牌 ldt_，经隧道透传
          └ https://me.loov.cc/hub/v1/mcp（loov-hub）
              ├ web_search → SearXNG 127.0.0.1:8888（服务器本机容器）
              └ web_fetch  → 服务器直接抓取（带 SSRF 防护）
```
CLI 配置里只出现 `lat_`。设备令牌和隧道凭据只在客户端内存里，符合[安全模型](architecture.md)。

## 一、SearXNG（服务器）
部署文件放在 loov-hub 仓库的 `deploy/searxng/`：`docker-compose.yml` 和 `settings.yml`。

| 配置 | 值 |
|---|---|
| 容器名 | `loov-searxng`，镜像固定版本 |
| 端口 | `127.0.0.1:8888:8080`，不对公网开放 |
| 资源 | `mem_limit 350m`，`restart unless-stopped` |
| `settings.yml` | `use_default_settings: true`；`search.formats` 加 `json`；`server.limiter: false`（只有 hub 访问）；`server.secret_key` 随机生成 |
| 引擎 | 用 `disabled: true` 禁用 yahoo、qwant、startpage、brave、duckduckgo，它们在云 IP 上持续被限流。**不要用 `remove`**，会让容器崩溃（见服务器记忆） |
| 自检 | `curl -s 'http://127.0.0.1:8888/search?q=test&format=json'` 能返回 results |

## 二、hub：`POST /hub/v1/mcp`
**协议**：MCP Streamable HTTP，无状态，只返回 JSON，不开 SSE。
- `POST` 收 JSON-RPC 请求，返回 `application/json`。通知类消息（没有 id）返回 202，响应体为空。
- `GET`、`DELETE` 返回 405，表示不提供 SSE 流。
- 支持的方法：`initialize`、`notifications/initialized`、`ping`、`tools/list`、`tools/call`，其它方法返回 `-32601`。
- `initialize` 按客户端请求的 `protocolVersion` 回显，支持 `2025-06-18` 和 `2025-03-26`，其它版本回 `2025-06-18`。`capabilities` 为 `{tools:{}}`，`serverInfo` 为 `{name:"loov-search", version}`。
- 手写实现，不引入 MCP SDK。代码放在 `src/mcp.ts`（协议）、`src/search.ts`（SearXNG）、`src/fetch.ts`（抓取）。

**鉴权与限额**
- 鉴权复用现有的 `requireDevice`（`Authorization: Bearer ldt_…`）。设备失效时返回 401，客户端会改成 502。
- 限额：每设备每分钟 30 次、每天 1000 次。超限时返回工具错误（`isError: true`），文案为中文。
- 审计只记录每台设备每天的调用次数，不记录查询内容和 URL。

**工具 `web_search`**
- 输入：`query`（字符串，1 到 400 字符）、`max_results`（1 到 10，默认 5）、`time_range`（day / week / month / year，可选）、`language`（可选，例如 zh-CN）。
- 处理：请求 SearXNG `/search?format=json`，超时 10 秒，取前 N 条。
- 输出：`content: [{type:"text", text}]`，每条结果是「序号. 标题 / URL / 摘要」。无结果时说明没有找到。

**工具 `web_fetch`**
- 输入：`url`（http 或 https）、`max_chars`（1000 到 20000，默认 8000）。
- SSRF 防护：
  - 只允许 http 和 https，端口只允许 80 和 443。
  - 解析出的所有地址都要检查，拒绝私网 10/8、172.16/12、192.168/16，环回、链路本地 169.254/16 与 fe80::/10，CGNAT 100.64/10，ULA fc00::/7，0.0.0.0/8，组播，以及 IPv4 映射的 IPv6 地址。
  - 不自动跟随重定向，手动最多跟 5 跳，每一跳都重新检查。
  - 连接时把 Host 固定到已校验的地址上，防止 DNS 重绑定。
- 限制：响应最大 2MB，总时长 15 秒，content-type 只接受 text/*、application/json、application/xhtml+xml。
- 转换：HTML 去掉 script、style、nav 等标签后取正文，保留标题与链接文本，压缩空白，截断到 `max_chars`，并注明是否截断。

**部署**：备份 hub.db → 重建镜像 → 版本升到 0.4.0。nginx 不用改：响应是 JSON，32k 的请求体上限足够。

## 三、客户端
**本地路由透传**：新文件 `src-tauri/src/loov/mcp_proxy.rs`，路由 `/loov/mcp` 接受全部方法。
- 挂在 `proxy/server.rs` 的 `require_bearer` layer 之前，自动要求 `lat_`。
- 请求头：剥掉 host、content-length、connection、transfer-encoding、authorization、x-api-key、proxy-*；保留 content-type、accept、mcp-session-id、mcp-protocol-version、last-event-id；注入 `Authorization: Bearer <ldt_>`。
- 用 `http_client::get()`（隧道）请求 `{hub_base}/v1/mcp`。请求超时设为 24 小时，首包外层 30 秒。
- 响应：透传状态码、content-type、content-encoding、mcp-session-id。复用 `create_logged_passthrough_stream` 与 `strip_hop_by_hop_response_headers`。
- 异常：未登录或隧道未就绪时返回 503 JSON；hub 返回 401 时改成 502 并去掉 `www-authenticate`，避免 Claude Code 走 OAuth 发现；日志里不出现任何令牌。

**MCP 下发**：`provision::apply_mcp`
- 先 `lock_switch_for_app("claude")`，再锁 `codex`，然后调 `McpService::upsert_server`。
- 条目内容：id `loov-search`，name「Loov 云端搜索」，`server = {type:"http", url:"http://127.0.0.1:<路由端口>/loov/mcp", headers:{Authorization:"Bearer <lat_>"}, startup_timeout_sec:20, tool_timeout_sec:120}`，`apps = {claude, codex}`，`tags = ["loov"]`。
- 调用时机：`attach_and_harden`、`post_startup`、`refresh` 的重建分支，都放在 `harden_codex` 之后（要先确保 `~/.codex` 存在）。登录时轮换了 `lat_`，所以每次都要重写。
- `restore_original` 调 `McpService::delete_server("loov-search")`。登出时保留条目，handler 会返回 503。

**前端**
- `UnifiedMcpPanel` 对 tag 为 `loov` 的条目隐藏编辑与删除，显示「Loov 托管」，防止 url 被改到别处、把 `lat_` 发出去。
- Loov 页在通道状态区显示「云端搜索：可用 / 未连接」。

**已知限制**：员工的 shell 里如果设置了 `HTTPS_PROXY` 却没有 `NO_PROXY`，Codex 的 MCP 客户端可能把 127.0.0.1 的请求也发给那个代理，导致失败。这种情况写进常见问题。

## 四、测试
**hub（vitest）**
- 协议：initialize 版本协商、initialized 返回 202、tools/list 的形状、未知方法 -32601、非法 JSON 返回 -32700。
- 工具：使用假 SearXNG，验证结果格式化；验证限额与工具错误。
- SSRF：10.0.0.1、169.254.169.254、[::1]、0.0.0.0、DNS 解析到内网、重定向到内网、非 80/443 端口、超大响应、超时。

**客户端（cargo test）**
- `mcp_proxy`：使用假上游，验证 POST JSON 透传、`mcp-session-id` 往返、`lat_` 被换成 `ldt_`、未登录 503、401 改成 502。
- `provision`：断言 `~/.claude.json` 与 `config.toml` 的落盘形状（Codex 用 `http_headers`，不写 `type`）。

**E2E**（`tests/loov_e2e.rs`）
- 登录后经本地路由调 `initialize`、`tools/list`、`web_search`，能返回结果。
- 恢复原配置后，`loov-search` 条目消失。
