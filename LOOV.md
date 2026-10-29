# Loov Agent（基于 CC Switch 的内部客户端）

Loov Agent 是 [CC Switch](https://github.com/farion1231/cc-switch)（MIT）的内部 fork。员工安装后登录公司账号，Claude Code 与 Codex 的请求就会经抗量子加密隧道发往公司的 new-api，不需要自己注册、买 Key 或配代理。上游的 LICENSE 与版权声明保持不变。

## 和上游的差别
- **新代码集中在 `src-tauri/src/loov/`**：hub 客户端、凭据加密存储、xray sidecar、本地路由鉴权、供应商写入、登录前快照与恢复。
- **上游文件只加挂载点**：每处都带 `// loov:` 注释。合并上游时先 `grep -rn "// loov:"` 核对挂载点是否齐全。
- **前端**：默认是简洁模式，只显示 `src/components/loov/LoovPage.tsx`；点「高级模式」进入完整的 CC Switch 界面。
- **标识**：`cc.loov.agent`、深链接 `loovagent://`、配置目录 `~/.loov-agent`，可以和原版 CC Switch 同机共存。
- **去掉的部分**：上游自动更新、赞助提示和上游 CI。

## 安全模型
- 真实 new-api Key 只在进程内存里；CLI 配置里写的是本地路由令牌 `lat_…`。
- 上游代理在数据库里固定为黑洞地址 `http://127.0.0.1:1`，隧道地址只存在内存中。任何失败状态都不会退回直连。
- 首次登录前，客户端会保存 Claude Code 与 Codex 的原配置。「恢复原配置」只把受管字段写回原值。

## 开发
```bash
pnpm install
bash scripts/fetch-xray.sh                       # 下载并校验本机平台的 xray（以 src-tauri/xray.lock.json 为准）
cd src-tauri && cargo test --features test-hooks --lib loov::
pnpm vitest run tests/components/LoovPage.test.tsx
pnpm tauri dev
```
真实链路端到端测试见 `src-tauri/tests/loov_e2e.rs`，需要设置 `LOOV_E2E=1` 和测试账号。
在本机跑界面验收时，务必用隔离的 HOME：`HOME=/tmp/gui-home pnpm tauri dev`。否则客户端会改写你自己的 `~/.claude` 和 `~/.codex`。

## 打包与发布
- `.github/workflows/loov-build.yml` 产出四种安装包：Windows NSIS、macOS arm64 与 x64 的 dmg 和 app.zip、Linux 的 deb 与 AppImage。手动运行只上传 Actions 产物；推送 `loov-v*` 标签会另外创建 GitHub Release。
- 产物都没有做代码签名。macOS 是 ad-hoc 签名，首次打开要右键选「打开」；Windows 会出现 SmartScreen 提示。
- 升级 xray：把 `src-tauri/xray.lock.json` 改成新版本，四个平台的 zip 与可执行文件 sha256 都要更新，用官方 `.dgst` 核对。

## 服务端
hub 在单独的仓库 `loov-hub`，部署在 `https://me.loov.cc/hub/`，负责账号、设备、隧道参数下发、余额与兑换。
