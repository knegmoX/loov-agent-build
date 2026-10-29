# dev-04 构建与发布

## 流水线
| 文件 | 触发 | 内容 |
|---|---|---|
| `.github/workflows/loov-ci.yml` | 推送到 main、PR、手动 | Linux 上类型检查、Loov 页测试、loov 单测 |
| `.github/workflows/loov-build.yml` | 手动（参数 `platforms`）、推送 `loov-v*` 标签 | 多平台打包；标签触发时另外创建 GitHub Release |

**构建矩阵**
| 平台 | runner | target | 产物 |
|---|---|---|---|
| windows | windows-2022 | x86_64-pc-windows-msvc | NSIS 安装包，按用户安装，不需要管理员权限 |
| macos-arm64 | macos-14 | aarch64-apple-darwin | dmg 与 app.zip，ad-hoc 签名 |
| macos-x64 | macos-14，交叉编译 | x86_64-apple-darwin | dmg 与 app.zip，ad-hoc 签名，不跑测试 |
| linux | ubuntu-22.04 | x86_64-unknown-linux-gnu | deb 与 AppImage。22.04 的 glibc 较老，能运行的发行版更多 |

**手动运行**：
```bash
gh workflow run loov-build.yml -R knegmoX/loov-agent -f platforms=windows,linux
gh workflow run loov-build.yml -R knegmoX/loov-agent -f platforms=all
```
私有仓库的 macOS 分钟数按 10 倍计费，排查问题时先只跑 windows 和 linux。

## Windows 安装界面

`src-tauri/windows/installer.nsi` 是自定义 NSIS 模板（`bundle.windows.nsis.template`），基于 tauri-bundler 2.8.1 的上游模板，只换界面：一个白底安装页（logo、名称、标语、安装位置、桌面快捷方式）→ 只有进度条的安装页 → 完成页（立即启动）。安装、升级（直接覆盖，与更新器一致）、WiX 旧版卸载、卸载逻辑保持上游原样，改动处标了 `Loov:`。

- `windows/hooks.nsh`：提供素材目录 `LOOV_ASSETS`；安装后通知资源管理器刷新图标缓存（升级后桌面图标不更新的问题）
- `windows/assets/logo-{72,108,144}.bmp`：按 100%/150%/200% 缩放选用，由 `windows/gen-assets.py` 从 `icons/icon.png` 生成，换图标后要重新生成
- **升级 `@tauri-apps/cli` 时**要对照新版 tauri-bundler 的 `installer.nsi` 合并上游改动（模板变量、安装逻辑）
- 本地预览：用 makensis + wine 在 Xvfb 里跑（渲染脚本与截图流程见 loov-ops，2026-10-26 做过新装与 0.1.4→0.1.6 升级两种场景）

## xray 版本管理
- `src-tauri/xray.lock.json` 按平台记录官方 zip 与解包后可执行文件的 sha256。构建脚本用它校验下载的文件。
- 运行时只对外部来源的 xray（`LOOV_XRAY_BIN` 或用户目录 `~/.loov-agent/loov/bin/`）比对锁文件；随包附带的 sidecar 与主程序同在应用目录内，不再比对哈希。原因是 macOS 打包时 ad-hoc 签名会改写二进制，0.1.0 的 macOS 包因此无法启动隧道，0.1.1 修复。
- `scripts/fetch-xray.sh [target] [本地目录]`：不给本地目录时从官方 release 下载，校验 zip 和可执行文件，然后放进 `src-tauri/binaries/` 与 `src-tauri/xray-assets/`。这两个目录都不进 git。
- **升级步骤**：
  1. 下载四个平台的 zip，用官方 `.dgst` 核对。
  2. 解包，计算可执行文件的 sha256，更新锁文件。
  3. 本机跑 `client_e2e.sh` 验证新版本与节点兼容，节点用 ML-DSA-65 和 VLESS Encryption，对 xray 版本有要求。
  4. 提交，触发构建。

## 版本号
`package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml` 三处保持一致，当前为 0.1.1。前端通过 `loov_app_version` 取 tauri.conf.json 的版本，与 hub `/meta` 的 `client.min` 和 `client.latest` 比较：低于 min 时禁止登录并提示下载，低于 latest 时只提示。

## 发布步骤
1. 三处版本号一起改，提交。
2. `git tag loov-vX.Y.Z && git push origin loov-vX.Y.Z`，流水线会构建全平台并创建 Release，附带 SHA256SUMS。
3. 在服务器上执行 `python3 /mnt/data/workspace/loov-ops/scripts/publish_download.py X.Y.Z`。脚本会：
   - 下载 Release 并按 SHA256SUMS 校验；
   - 运行 `verify_release.py` 拆开全部安装包，核对各平台**一致**：版本号、应用名与标识、主程序内嵌的前端资源文件名（Vite 内容哈希，相同即同一份前端构建）、托盘文案（Loov Agent，无 CC Switch）、内嵌托盘图标与应用图标（与 tag 下仓库文件逐字节比对）、随包 xray（Windows/Linux 哈希在锁文件内，macOS 核对架构）、xray-assets、dmg 与 app.zip 的主程序、Windows 安装程序的图标与安装界面 logo（确认用的是 Loov 模板）。任何一项不过就中止，下载页和 hub 都不动；单独运行：`python3 loov-ops/scripts/verify_release.py X.Y.Z`；
   - 以友好文件名安装到 `/var/www/loov-download/X.Y.Z/`，并重新生成下载页；
   - 把 hub `data/meta.json` 的 `client.latest` 设为新版本、`downloadUrl` 设为 https://me.loov.cc/download/，改前先备份。30 秒内生效，不用重启。
4. 需要强制升级时，手动提高 `meta.json` 里的 `client.min`。

**各平台一致的前提**：`.gitattributes` 全局 `* text=auto eol=lf`。Windows 构建机默认 `core.autocrlf=true`，没写 eol 的文本（0.1.4 时的 `*.svg`）会被转成 CRLF，前端资源哈希随之和 macOS / Linux 不同。新增文本类型无需再单独声明。

**下载页**：https://me.loov.cc/download/ 由 nginx 静态提供，配置是 `/etc/nginx/snippets/loov-download.conf`（源文件在 `loov-ops/loov-download.nginx.conf`）。私有仓库的 Release 员工无权下载，所以对员工分发一律走这个地址。注册仍需邀请码，安装包里不含任何密钥。

## 代码签名（延后到正式版）
- **macOS**：需要 Apple Developer ID（每年 99 美元）与公证。目前用 ad-hoc 签名，员工首次打开要右键选「打开」。
- **Windows**：需要 OV 或 EV 代码签名证书。目前未签名，会出现 SmartScreen 提示，要选「仍要运行」。另外要关注 Windows Defender 会不会误报 xray。
- 签名后可以启用自动更新（Tauri updater 加自建更新源），macOS 也可以改用钥匙串保存凭据密钥。
