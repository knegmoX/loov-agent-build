# 服务器模式（无界面）

员工可以把 Loov Agent 装在云服务器等没有桌面的 Linux 机器上：登录、安全通道、计费都和桌面版一样，员工在 SSH 里直接用 claude / codex，费用照样记在 Loov。

## 安装与启动
```bash
sudo apt install ./LoovAgent-<版本>-linux-x64.deb   # deb 会带上 webkit 等依赖；也可以用 AppImage（--appimage-extract-and-run）
loov-agent service install                           # 写 systemd 用户服务
systemctl --user daemon-reload
systemctl --user enable --now loov-agent
sudo loginctl enable-linger $USER                    # 退出 SSH 后继续运行
loov-agent login                                     # 用户名、密码（不回显）；脚本里用 --password-stdin
loov-agent status
```
之后直接运行 `claude`、`codex`：它们的配置在登录时已经指向本机路由。agy、Grok 用 `loov-agent run agy` / `loov-agent run grok`：要在启动时带上私有配置和环境变量。

## 命令
| 命令 | 说明 |
|---|---|
| `serve` | 常驻服务，systemd 管理 |
| `service install` | 写 `~/.config/systemd/user/loov-agent.service` |
| `login` / `register --invite` / `logout` | 账号 |
| `status [--json]` | 安全通道、节点、用户 |
| `refresh` | 重新拉配置、重建安全通道（也可以 `kill -HUP`） |
| `node <id\|auto>` | 固定节点或改回自动 |
| `usage [today\|month]` | 花费（按应用） |
| `devices` | 已登录的设备 |
| `install <claude\|codex\|agy\|grok>` | 经安全通道安装 CLI agent |
| `run <应用> [-- 参数…]` | 在当前终端运行 agent（`exec` 替换进程） |
| `safety <careful\|standard\|full>` | agent 权限档位 |
| `restore` | 恢复登录前的配置并停用 Loov |

## 设计
- **入口**：`main.rs` 在初始化 Tauri / GTK 之前按第一个参数分流到 `headless::main`；不是这些子命令就启动桌面版。
- **共用启动顺序**：
  - 桌面版和 `serve` 共用 `core_boot`：出站客户端（已登录时固定黑洞）→ 拉配置、建隧道 → 上游接上代理模式的应用 → 写加固键。
  - 顺序只在这一处定义，由 `startup_mount_points_stay_in_order` 测试校验两个入口都按这个顺序调用。
- **为什么要常驻进程**：Loov 的状态（凭据、隧道、本地路由）在常驻进程里。
  - 其余子命令经 `~/.loov-agent/loov/ctl.sock` 通信：只有属主能读写（0600），一条连接一个请求，按行 JSON。
  - 短命的命令行进程不能自己起一份，否则会留下孤儿 xray、抢路由端口。
- **同一时间只能运行一个**：桌面版和 `serve` 都要先拿到 `~/.loov-agent/loov/core.lock`（`File::try_lock`）。后启动的那个会被拒绝：桌面版弹提示后退出，`serve` 退出码 2。
- **信号**：`serve` 自己处理信号。SIGTERM / SIGINT 正常停止（还原系统代理、停 xray、停路由，退出码 0，systemd 不当成失败）；SIGHUP 重新拉配置。桌面版那套「收到信号直接退出」的处理在服务器模式下不装。
- **系统代理**：服务器上没有系统代理可接管。代理 / 全局模式照样开 sys-in，`status --json` 里给出地址，员工可以自己 `export HTTPS_PROXY`。
- **凭据密钥**：用户名改为取进程实际用户（`getpwuid`），不依赖 `$USER`。systemd、cron、`env -i` 启动时也能解开凭据。
- **安装进度**：`apps::install` 的进度回调（`apps::Progress`）在桌面版发界面事件，在服务器模式经控制套接字逐行转发，打到 stderr。

## 验证（2026-10-30，本机干净环境：`env -i`、隔离 HOME、没有 DISPLAY / DBus）
- `serve` 启动后再起一个，被锁拒绝（退出码 2）。
- `login --password-stdin` 成功，`status` 显示已连接，套接字权限是 0600。
- 直接运行 `claude -p`、`codex exec`，以及 `run agy -- -p`，都回 OK。`usage today` 按应用列出花费。
- `safety careful` 立即改写 Claude 的 `permissions.defaultMode` 和 Codex 的 `approval_policy` / `sandbox_mode`。
- `restore` 恢复成功；SIGHUP 触发刷新；SIGTERM 退出码 0，套接字文件删除，没有残留 xray。
