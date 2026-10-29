# dev-03 隐私整流器

## 目标
CLI 发往公司 new-api 的请求里，有些字段能识别员工的机器或代码仓库。在本地路由里统一改写这些字段，又不能破坏会话粘性、缓存和计费。new-api 默认的渠道亲和规则会把 `X-Stainless-*`、`User-Agent`、`X-Codex-Turn-Metadata` 等头**继续转给最终的模型方**，所以必须在客户端改写。

## 一、生效范围
- **只作用于 Loov 托管的供应商**：id 为 `universal-claude-loov` 或 `universal-codex-loov`，或者转发时命中了 Key 占位 `LOOV_MANAGED`。
- 排除 Copilot、Codex OAuth、官方直连透传，这些路径不经过 Loov。
- **开关** `PrivacyConfig`：settings 表的键是 `privacy_config`，默认值 `{headers: true, metadata: true}`。它独立于 `RectifierConfig.enabled`，关掉整流器不会关掉隐私改写。本轮不做界面。
- **实现取舍**：开关不穿过 `RequestForwarder` 的构造函数，否则会牵动上游大量测试。改为 loov 模块持有进程级配置，在建立链路时（`flows::bring_up`）从数据库加载；forwarder 里按供应商 id 判断是否生效。

## 二、插入点
| 位置 | 时机 | 说明 |
|---|---|---|
| `proxy/forwarder.rs` 约 2701 行（`chatgpt-account-id` 处理之后） | 出站头定稿后、hyper 与 reqwest 两条发送路径分叉前 | 只改 `ordered_headers`，一处覆盖两条路径。在 map 里改值时，hyper 路径保留客户端原来的大小写 |
| `proxy/forwarder.rs` 约 2072 行（请求体覆盖之后） | `session.rs` 与 Copilot 都已读完 `metadata` 之后 | 改写请求体 |
故障转移和整流器重试每次都会重新调用 `forward()`，改写会自动重新执行。

新文件 `src-tauri/src/loov/privacy.rs`（新代码集中在 loov 目录），包含两个纯函数：
```rust
pub fn rewrite_headers(h: &mut http::HeaderMap) -> Vec<&'static str>;
pub fn rewrite_body(body: &mut serde_json::Value) -> Vec<&'static str>;
```
返回值是被改写字段的名字列表，用于日志。

## 三、改写规则
| 位置 | 字段 | 规则 |
|---|---|---|
| 头 | `user-agent: claude-cli/<ver> (…)` | 保留版本，括号部分统一成 `(external, cli)` |
| 头 | `user-agent: codex_*/<ver> (<系统> <版本>; <架构>) <终端>` | 保留 `codex_xxx/<ver>`，去掉系统、架构与终端 |
| 头 | `x-stainless-os`、`-arch`、`-runtime`、`-runtime-version`、`anthropic-client-os` | 换成固定值 `MacOS`、`arm64`、`node`、`v22.12.0`，出现才改 |
| 头 | `x-codex-installation-id` | 换成伪 UUID |
| 头 | `x-codex-turn-metadata` | 删除（含 git 远端 URL 与提交哈希） |
| 头 | `x-oai-attestation`、`x-forwarded-for`、`x-real-ip` | 删除 |
| 请求体 | `metadata.user_id`，新版 JSON 字符串 | `device_id` 换成伪名，`account_uuid` 置为空串；**保留** `session_id`、`parent_session_id` 与其余键，键顺序不变 |
| 请求体 | `metadata.user_id`，旧版 `user_<h>_account_<a>_session_<s>` | 换掉 `<h>`，`<a>` 置空，`<s>` 保留 |
| 请求体 | `client_metadata["x-codex-installation-id"]` 及其中的 turn metadata | installation-id 换成伪 UUID，删掉 `workspaces` |

**不能动的**：`anthropic-version` 与 `anthropic-beta`（new-api 用来路由和阶梯计费）、`x-app`、`x-stainless-lang`、`package-version`、`retry-count`、`timeout`、`x-claude-code-session-id`、`session_id`、`thread_id`、`x-codex-turn-state`、`x-codex-window-id`、`openai-beta`、`originator`、`prompt_cache_key`。

**伪名算法**：`HMAC-SHA256(本机随机密钥, 原值)`。密钥存在 `~/.loov-agent/loov/privacy.key`（0600，不进数据库，不会被 WebDAV/S3 同步），首次使用时生成。输出保持原来的格式：64 位 hex 就输出 hex，UUID 就按 UUID 格式输出。这样改写结果对同一台机器确定且稳定，又无法逆推原值。

**为什么 `metadata.user_id` 要做确定性改写**：new-api 的 Claude 亲和规则以它为 key。每个请求随机会让会话粘性和多渠道缓存失效；整台机器用一个固定值，又会把所有会话钉在同一个渠道上。保留 `session_id` 就能兼顾，本地 `session.rs` 在改写前已经读完，不受影响。

## 四、日志
只记录 `[Privacy] rewrote: [user-agent, x-stainless-os, metadata.user_id]` 这样的字段名列表，不记录原值，也不记录伪名。

## 五、测试
1. `rewrite_headers` 每条规则一个单测；没命中的头保持不变；大小写不同的头名也能处理。
2. `metadata.user_id`：新旧两种格式；同一会话改写两次结果相同，不同会话结果不同；改写后 `session.rs` 的 `parse_session_from_user_id` 仍能取出原 session_id；输出里不含原 device_id。
3. `client_metadata` 单测。
4. forwarder 集成测试，仿照 `mod codex_stack` 的假上游：分别用 Loov 的 Claude 与 Codex 供应商，断言上游收到的头和请求体；再用一个非 Loov 供应商做对照，断言完全透传。
5. 整流器重试后的第二次请求同样经过改写。
6. 现有的 `claude_stacked_requests_keep_sdk_headers` 与 `stacked_requests_drop_chatgpt_identity…` 继续通过。
7. 捕获日志，断言里面不出现原 UA 和原 device_id。
8. E2E 抓包确认 Codex 实际发送的 `client_metadata` 与 `x-codex-turn-metadata` 字段。字段名目前只从二进制里的字符串推断，需要真实请求核对。
