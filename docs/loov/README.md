# Loov Agent 开发文档

Loov Agent 是 CC Switch（MIT）的内部 fork。员工安装后登录公司账号，Claude Code 与 Codex 的请求就会经抗量子加密隧道发往公司的 new-api。本目录是开发文档，仓库根目录的 `LOOV.md` 是简介。

## 文档索引
| 文档 | 内容 |
|---|---|
| [architecture.md](architecture.md) | 组件与数据流、安全模型、上游挂载点、启动 / 登录 / 登出 / 退出流程、落盘文件 |
| [dev-01-preset-cleanup.md](dev-01-preset-cleanup.md) | 清理第三方推广与官方直连入口 |
| [dev-02-cloud-search.md](dev-02-cloud-search.md) | 云端搜索：SearXNG + hub MCP + 客户端透传 |
| [dev-03-privacy-rectifier.md](dev-03-privacy-rectifier.md) | 隐私整流器：改写请求里的本机指纹 |
| [dev-04-build-release.md](dev-04-build-release.md) | 多平台构建、xray 版本管理与发布 |
| [dev-05-app-launcher.md](dev-05-app-launcher.md) | 启动台：Claude Code / Codex / Antigravity / Claude Desktop 一键启动与安装，全部经 new-api 计费 |
| [dev-06-network-and-models.md](dev-06-network-and-models.md) | 节点选择、直连 / 代理 / 全局三种网络模式；每个 agent 都能在自己的模型选择器里选 Loov 的全部模型 |
| [dev-07-billing-and-reliability.md](dev-07-billing-and-reliability.md) | 按应用计费（每应用一把 Key）、用量与预算、云端搜索按成本结算、模型回退与健康度、agent 权限档位 |
| [dev-08-headless-server.md](dev-08-headless-server.md) | 服务器模式：`loov-agent serve` 常驻进程 + 命令行（login / status / run …），systemd 用户服务 |
| [research-openbot.md](research-openbot.md) | OpenBot 调研：常驻 AI 同事形态、各家 agent 1–2 年趋势、Loov 路线建议（P0/P1/P2） |

服务端 loov-hub 的接口契约见 hub 仓库的 `docs/API.md`。历史调研与测试报告放在服务器的 `/mnt/data/research/ccswitch/`：`REPORT.md` 是调研，`PLAN.md` 是最初的计划，`TEST_REPORT.md` 和 `M2_REPORT.md` 是测试与实施报告。

## 计划项状态（2026-10-26）

### 已完成
| 里程碑 | 内容 |
|---|---|
| M0 | 东京节点抗量子隧道：REALITY + X25519MLKEM768 + ML-DSA-65 + VLESS Encryption |
| M1 | loov-hub：邀请码注册、设备开通与吊销、隧道参数下发、余额与兑换码、封禁同步、限流 |
| H | hub 0.3.0：踢出最旧设备、`/meta` 版本与公告、带分类的模型目录 |
| M2a | 客户端后端：登录、xray sidecar、本地路由鉴权、Key 只在内存、fail-closed、CLI 加固键 |
| M2b | 简洁模式 Loov 页：登录、余额、充值、兑换、模型选择、设备管理 |
| M3 | Linux 包；Windows 与 macOS 构建流水线；xray 锁文件；Windows 用 DPAPI 保护凭据；升级提示 |
| 修复 | 首次登录前快照与「恢复原配置」，在真机验收事故后补上 |

### 第三轮（2026-10-24）已完成
| 项目 | 文档 | 结果 |
|---|---|---|
| 清理预设中的推广内容 | dev-01 | 删除推广字段 746 行、清理 URL 437 处；带守护测试与可重放脚本 |
| 接管期间隐藏官方与订阅直连入口 | dev-01 | 官方种子、托盘、授权中心、预设选择、供应商列表都已过滤 |
| 云端搜索：web_search 与 web_fetch | dev-02 | hub 0.4.0 已上线；SearXNG 部署在服务器本机；客户端自动下发 MCP |
| 隐私整流器 | dev-03 | 改写 UA、x-stainless-*、Codex 安装 ID 与 turn metadata、metadata.user_id |
| Windows、macOS 安装包与发布 | dev-04 | `loov-v0.1.1`（四个平台）。0.1.0 的 macOS 包因 ad-hoc 签名改写内置 xray 而无法启动隧道，已在 0.1.1 修复 |
| 北京网络连通性（0.1.3） | architecture §2 / §5 | 实测北京直连东京 9443 不通。新增台湾节点 tw1（9443，经 VPC 回连）与东京备用端口 jp2（2053）；客户端逐节点探测，直连不通时自动经本机代理拨号，全部不通时报出每个节点的原因 |
| 系统环境变量冲突（0.1.3） | architecture §2 | 接管期间中和 shell / 注册表里的 `CLAUDE_CODE_USE_*`、`ANTHROPIC_API_KEY`、`ANTHROPIC_MODEL`；横幅只提示仍会影响链路的变量 |
| 审计 S8-01 / S8-02 / S6-02 / S1-02（0.1.3） | architecture §2 | 本地鉴权与 Key 注入决策抽成纯函数并进 CI；启动顺序标记与源码顺序测试；Key 不再落盘 |
| Linux 包实测问题（0.1.4） | ACCEPTANCE_0.1.3_LINUX.md | Codex 可选 Claude 模型并默认 claude-sonnet-5；启动中状态与路由误报；Claude 默认模型生效；未连时 400 明确报错且不计熔断；失败登录不留快照；日志脱敏；信号退出停 xray；横幅不再提示员工自己的 Vertex 变量；品牌与设备名 |

### 延后
| 项目 | 原因或条件 |
|---|---|
| 代码签名与公证 | 等正式版本。需要 Apple 开发者账号与 Windows 签名证书 |
| 自动更新 | 依赖代码签名。目前由 hub `/meta` 提示下载新版 |
| macOS 钥匙串 | 签名后再切换。未签名的 app 每次更新后访问钥匙串都会弹授权框 |
| 在线充值 | 等易支付或 Stripe 的商户资料。目前是兑换码加跳转网页充值 |
| 路径假名化、TUN 全局模式、更多 Agent | 原计划 M4，按需再排 |
