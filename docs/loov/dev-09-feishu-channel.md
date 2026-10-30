# 飞书渠道

在飞书里私聊机器人，就像在终端里用某个 agent。处理在员工自己的 Loov（桌面版或服务器模式）里进行，费用照样经 new-api 计费，记在「飞书渠道」名下。

## 配置（扫码即用）
- **桌面版**：Loov 页 →「飞书渠道」→「连接飞书」→ 用飞书扫二维码确认。
- **服务器**：`loov-agent feishu connect`，终端里显示二维码。

扫码会在员工自己的飞书里创建一个「个人助手」应用，**不需要管理员审批**。完成后自动连上；agent 默认 Claude Code，可以在界面或用 `feishu agent codex` 修改。

| 设置 | 说明 |
|---|---|
| 处理消息的 agent | Claude Code / Codex / Antigravity / Grok Build |
| 工作目录 | 默认是主目录 |
| 群聊 | 默认关闭；开启后在群里 @ 机器人也能用，仍只认创建者本人 |
| 启用 | 关掉就断开长连接，配置保留 |

## 飞书里的用法
直接发消息就是在用 agent，回复以流式卡片显示。还可以发这些命令：
- `/new`：开新会话
- `/agent claude|codex|agy|grok`：换 agent
- `/cwd <路径>`：换工作目录
- `/stop`：停止当前任务
- `/status`：查看当前设置
- `/help`：查看帮助

## 架构（`src-tauri/src/loov/feishu/`）
```
飞书 ──长连接(WSS)── Loov 核心（桌面版或 serve）
                      ├─ link.rs    取地址 / 心跳 / ACK / 分片 / message_id 去重 / 退避重连
                      ├─ mod.rs     配置、会话、命令、只认创建者、流式卡片、生命周期
                      ├─ runner.rs  各家官方无头接口跑 agent
                      ├─ api.rs     扫码创建应用、token、消息、CardKit
                      └─ frame.rs   pbbp2 帧编解码（手写 protobuf）
```
- **扫码创建**：`accounts.feishu.cn/oauth/v1/app/registration` 的设备码流程，和官方 lark-cli 相同：`archetype=PersonalAgent`，并请求返回 `open_id` 和 `tenant_brand`。
  - 员工确认前，轮询返回 HTTP 400 + `authorization_pending`，要读响应体，不能当作失败。
  - 拿到的 App Secret 用 Loov 的机器密钥加密，存在 `<loov>/feishu.json`。
- **长连接**：
  - `POST /callback/ws/endpoint` 拿到地址和心跳间隔，然后建 WebSocket。
  - 数据帧 3 秒内原样回 `{"code":200}` 作为应答。大事件会分片（`sum` / `seq`），要拼起来。
  - 按 `message_id` 去重，记录保留 12 小时 / 5000 条，跨重连也有效（重连后飞书可能重放旧事件）。
  - 只处理 `im.message.receive_v1`，已读回执等其他事件忽略。
  - 断线后退避重连：2 秒起，最长 60 秒。
- **执行**（`runner.rs`）：命令和环境取自 `apps::cli_spec`，与启动台、服务器模式同一份，请求都经本地路由和 new-api。

| agent | 调用 | 会话延续 |
|---|---|---|
| Claude Code | `claude -p … --output-format stream-json --verbose --permission-mode <档位>` | `--resume <session_id>` |
| Codex | `codex exec [resume] --json --skip-git-repo-check -c sandbox_mode=… [<thread_id>] <提示>` | `exec resume` |
| Antigravity / Grok | 各自的 `-p` 打印模式 | 无 |

- **计费归属**：
  - Claude Code 带 `ANTHROPIC_CUSTOM_HEADERS: x-loov-app: feishu`。
  - Codex 带 `-c model_providers.<当前供应商>.http_headers={ x-loov-app = "feishu" }`。
  - 本地路由据此选用 `loov-<设备>-feishu` Key，见 dev-07。
- **权限**：
  - 按 agent 权限档位执行。无头模式下需要询问的操作一律拒绝，「完全」档才放行。
  - 「谨慎」档下 Codex 是只读沙箱。
  - 审批卡片（在飞书里点「同意 / 拒绝」）留作下一步。
- **流式卡片**：
  - 回调只记下最新全文，每 400ms 更新一次卡片，`sequence` 严格递增。
  - 正文超过约 9000 字时只保留结尾。
  - 流式模式 10 分钟后会自动关闭，所以 9 分钟时续开；结束时关掉流式模式。
  - 卡片发不出去时，退回用文本回复。
- **串行**：每个飞书会话一个队列，同一会话一次只跑一轮；`/stop` 不排队，直接结束当前这一轮。
- **生命周期**：Loov 的看门狗每 15 秒调一次 `feishu::ensure`。安全通道就绪且渠道已启用就连上，否则断开；改配置后立即调用一次。
- **同一个应用只连一台设备**：飞书会把事件随机投给多条连接中的一条。

## 验证（2026-10-30）
- **扫码创建**（`loov-ops/scripts/feishu_spike.py`）：一次通过、免审批；机器人私聊、长连接地址、CardKit 都可用。
- **长连接收发**（`feishu_ws_spike.mjs`）：收到员工的私聊，并成功回复。
- **端到端**（服务器模式，干净环境）：
  - 员工私聊「用一句话介绍你自己」，Claude Code 以流式卡片回复。
  - 发 `/agent codex` 后切到 Codex，「1+1」由 Codex 回答，会话 id 已记下。
  - new-api 日志里能看到 `loov-<设备>-feishu` 的计费记录。
- **测试**：单测覆盖帧编解码、消息解析（@ 占位去除、富文本）、去重与分片、各家参数、Claude / Codex 输出解析、二维码；前端测试覆盖连接与设置。
