//! Loov 页启动台：Claude Code / Codex / Antigravity / Grok Build / Claude Desktop / Codex 桌面版的检测、安装、启动。
//!
//! 原则：从这里启动的应用一律经 Loov 隧道走 new-api 计费，员工不做任何配置。
//! - Claude Code / Codex：登录时已进路由模式（provision::attach），直接在终端里跑。
//! - Antigravity（agy）：私有 `--gemini_dir`（不碰员工自己的 `~/.gemini/antigravity-cli`），
//!   `GOOGLE_GEMINI_BASE_URL` 指向本地路由的 `/loov/gemini`，Key 是本地令牌 `lat_`（见 gemini_route）。
//! - Grok Build：启动前进 Grok 的路由模式（`~/.grok/config.toml` 指向本地路由 `/grokbuild/v1`），
//!   模型可选 new-api 上的 Claude / GPT / Gemini（Gemini 由本地路由转成 Chat Completions）。
//! - Claude Desktop：启动前进路由模式，Desktop 的 3p 网关配置指向本地路由 `/claude-desktop`。
//! - Codex 桌面版（ChatGPT 桌面 App）：和 Codex 命令行共用 `~/.codex/config.toml`，登录时已接好；
//!   没登录官方 ChatGPT 账号时它只能选官方默认模型，forwarder 把 new-api 上没有的模型换成员工选的 Codex 模型。
//!
//! 安装经隧道代理下载：国内直连 npm、Google、Anthropic 的下载站不稳定或不通。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use super::flows::LoovError;
use super::Status;
use crate::store::AppState;

/// 安装进度事件（前端 AppLauncher 订阅）。
pub const INSTALL_EVENT: &str = "loov-app-install";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum App {
    Claude,
    Codex,
    Agy,
    Grok,
    Desktop,
    CodexDesktop,
}

impl App {
    const ALL: [App; 6] = [App::Claude, App::Codex, App::Agy, App::Grok, App::Desktop, App::CodexDesktop];

    fn parse(id: &str) -> Result<Self, LoovError> {
        Self::ALL
            .into_iter()
            .find(|a| a.id() == id)
            .ok_or_else(|| LoovError::Internal { message: format!("未知应用：{id}") })
    }

    fn id(self) -> &'static str {
        match self {
            App::Claude => "claude",
            App::Codex => "codex",
            App::Agy => "agy",
            App::Grok => "grok",
            App::Desktop => "claude-desktop",
            App::CodexDesktop => "codex-desktop",
        }
    }

    /// 上游工具管理里的名字（CLI 才有）。
    fn tool(self) -> Option<&'static str> {
        match self {
            App::Claude => Some("claude"),
            App::Codex => Some("codex"),
            App::Agy => Some("agy"),
            App::Grok => Some("grok"),
            App::Desktop | App::CodexDesktop => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoovAppStatus {
    pub id: &'static str,
    /// 本平台能否使用（Claude Desktop 没有 Linux 版）。
    pub supported: bool,
    pub installed: bool,
    pub version: Option<String>,
    /// 当前模型（Grok Build 的模型存在它自己的供应商里，前端从这里读）
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallProgress {
    app: &'static str,
    /// download / install / configure / done
    stage: &'static str,
    /// 0..=100；不确定进度（npm、安装脚本）时为空
    percent: Option<f64>,
}

fn emit(handle: &AppHandle, app: App, stage: &'static str, percent: Option<f64>) {
    let _ = handle.emit(INSTALL_EVENT, InstallProgress { app: app.id(), stage, percent });
}

fn app_error(code: &str, message: impl Into<String>) -> LoovError {
    LoovError::App { code: code.to_string(), message: message.into() }
}

/// 隧道代理（`http://user:pass@127.0.0.1:port`）；隧道未就绪时为空。
fn tunnel_proxy() -> Option<String> {
    match super::status() {
        Status::Ready { proxy_url, .. } => Some(proxy_url),
        _ => None,
    }
}

/// 隧道就绪时给安装 / 升级进程的代理环境变量；未就绪时为空（照常直连）。
pub fn tunnel_proxy_env() -> Vec<(String, String)> {
    tunnel_proxy().as_deref().map(proxy_env).unwrap_or_default()
}

fn proxy_env(proxy: &str) -> Vec<(String, String)> {
    ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"]
        .into_iter()
        .map(|k| (k.to_string(), proxy.to_string()))
        .chain([("NO_PROXY".to_string(), "127.0.0.1,localhost".to_string())])
        .collect()
}

// ───────────────────────────── 检测 ─────────────────────────────

pub async fn status(state: &AppState) -> Vec<LoovAppStatus> {
    let grok_model = super::provision::grok_model(state);
    tokio::task::spawn_blocking(move || {
        App::ALL
            .into_iter()
            .map(|app| match app.tool() {
                Some(tool) => {
                    let (installed, version) = crate::commands::loov_probe_tool(tool);
                    let model = (app == App::Grok).then(|| grok_model.clone()).flatten();
                    LoovAppStatus { id: app.id(), supported: true, installed, version, model }
                }
                None => {
                    let spec = desktop::spec(app);
                    let found = desktop::detect(spec);
                    LoovAppStatus {
                        id: app.id(),
                        supported: desktop::supported(spec),
                        installed: found.is_some(),
                        version: found.and_then(|f| f.version),
                        model: None,
                    }
                }
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

// ───────────────────────────── 安装 ─────────────────────────────

pub async fn install(handle: AppHandle, id: &str) -> Result<(), LoovError> {
    let app = App::parse(id)?;
    let proxy = tunnel_proxy();
    match app.tool() {
        Some(tool) => {
            emit(&handle, app, "install", None);
            let env = tunnel_proxy_env();
            crate::commands::run_tool_install_with_env(vec![tool.to_string()], env)
                .await
                .map_err(|e| install_error(app, e))?;
        }
        None => {
            let spec = desktop::spec(app);
            if !desktop::supported(spec) {
                return Err(app_error("UNSUPPORTED", format!("{} 暂不支持本机", spec.name)));
            }
            desktop::install(spec, &handle, proxy.as_deref()).await?;
            // 安装器说装好了不算数，要能检测到（检测不到时启动也会失败）
            if tokio::task::spawn_blocking(move || desktop::detect(spec).is_none()).await.unwrap_or(true) {
                return Err(app_error("INSTALL_FAILED", format!("{} 安装后没有检测到，请重试或手动安装", spec.name)));
            }
        }
    }
    emit(&handle, app, "done", Some(100.0));
    Ok(())
}

/// Codex 只能经 npm 安装：没有 Node.js 时给出明确提示（前端据 code 显示下载 Node.js 的入口）。
fn install_error(app: App, raw: String) -> LoovError {
    let lower = raw.to_lowercase();
    let npm_missing = lower.contains("npm: command not found")
        || lower.contains("npm: not found")
        || lower.contains("'npm' is not recognized")
        || lower.contains("npm 不是内部或外部命令");
    if app == App::Codex && npm_missing {
        return app_error("NODE_REQUIRED", "安装 Codex 需要 Node.js，请先安装 Node.js（LTS）后重试");
    }
    app_error("INSTALL_FAILED", format!("安装失败：{raw}"))
}

// ───────────────────────────── 启动 ─────────────────────────────

/// 启动应用。`restart`：Claude Desktop 正在运行时是否先退出再启动（新配置要重启才生效）。
pub async fn launch(state: &AppState, id: &str, restart: bool) -> Result<(), LoovError> {
    let app = App::parse(id)?;
    let Some(proxy) = tunnel_proxy() else {
        return Err(app_error("TUNNEL_DOWN", "安全通道未连接，连上后才能启动（应用的请求都要经 Loov 计费）"));
    };
    let home = crate::config::get_home_dir();
    match app {
        App::Claude => crate::commands::launch_loov_cli("claude", "Claude Code · Loov", Vec::new(), Vec::new(), Some(&home))?,
        App::Codex => crate::commands::launch_loov_cli("codex", "Codex · Loov", Vec::new(), Vec::new(), Some(&home))?,
        App::Agy => launch_agy(state, &proxy, &home)?,
        App::Grok => launch_grok(state, &proxy, &home).await?,
        App::Desktop | App::CodexDesktop => desktop::launch(desktop::spec(app), state, restart).await?,
    }
    Ok(())
}

/// Grok Build：进路由模式（写 ~/.grok/config.toml）后在终端里跑 `grok`。
async fn launch_grok(state: &AppState, proxy: &str, home: &Path) -> Result<(), LoovError> {
    super::provision::attach_grok(state).await?;
    // 选择器里列出 Loov 的其余模型；写不进去不拦着启动（员工仍能用 Loov 页选的那个）
    if let Err(e) = super::provision::sync_grok_models(state) {
        log::error!("[loov] 写 Grok 的模型表失败：{e}");
    }
    let env = vec![
        // 模型以外的外连也走隧道；本地路由不经代理
        ("HTTPS_PROXY", Some(proxy.to_string())),
        ("HTTP_PROXY", Some(proxy.to_string())),
        ("NO_PROXY", Some("127.0.0.1,localhost".to_string())),
        // 员工 shell 里的 xAI Key 不让 grok 拿去直连
        ("XAI_API_KEY", None),
    ];
    crate::commands::launch_loov_cli("grok", "Grok Build · Loov", Vec::new(), env, Some(home))?;
    Ok(())
}

/// 改某个应用的默认模型（目前只有 Grok Build 不在统一供应商里，单独存）。
pub async fn set_model(state: &AppState, id: &str, model: &str) -> Result<(), LoovError> {
    match App::parse(id)? {
        App::Grok => Ok(super::provision::set_grok_model(state, model).await?),
        _ => Err(app_error("UNSUPPORTED", "这个应用的模型在统一供应商里设置")),
    }
}

/// agy 用 Loov 的私有 profile 启动。
fn launch_agy(state: &AppState, proxy: &str, home: &Path) -> Result<(), LoovError> {
    let (args, env) = agy_launch_spec(state, proxy)?;
    crate::commands::launch_loov_cli("agy", "Antigravity · Loov", args, env, Some(home))?;
    Ok(())
}

/// agy 的启动参数与环境（E2E 用同一份直接跑 `agy -p`）。模型取员工在 Loov 页选的 Gemini
/// （`--model` 只对本次会话生效，不写进 profile，免得和 agy 自己的 `/model` 打架）。
pub fn agy_launch_spec(
    state: &AppState,
    proxy: &str,
) -> Result<(Vec<String>, Vec<(&'static str, Option<String>)>), LoovError> {
    let provider = state
        .db
        .get_provider_by_id(&super::provision::gemini_child_id(), "gemini")
        .ok()
        .flatten()
        .ok_or_else(|| app_error("NO_GEMINI", "Loov 账号下没有可用的 Gemini 模型"))?;
    let model = provider
        .settings_config
        .pointer("/env/GEMINI_MODEL")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let (host, port) = state.db.get_proxy_client_endpoint_sync();
    let base_url = format!(
        "{}{}",
        crate::services::proxy::proxy_origin(&host, port),
        super::gemini_route::PREFIX
    );
    let settings = serde_json::json!({ "env": {
        "GEMINI_API_KEY": super::local_auth::live_token(),
        "GOOGLE_GEMINI_BASE_URL": base_url,
    }});
    let prepared = crate::agy_config::prepare_launch(super::PROVIDER_ID, &settings)?;
    let dir = prepared
        .gemini_dir
        .ok_or_else(|| app_error("INTERNAL", "Antigravity 私有目录创建失败"))?;
    disable_agy_telemetry(&dir)?;
    let mut args = vec![format!("--gemini_dir={}", dir.to_string_lossy())];
    // 权限档位（loov::safety）：agy 的执行模式只能在启动参数里给
    args.extend(super::safety::agy_args(super::netmode::load_prefs().safety));
    if !model.is_empty() {
        args.push(format!("--model={model}"));
    }
    let mut env = prepared.env;
    env.extend([
        // 模型以外的外连（更新检查等）也走隧道；本地路由不经代理
        ("HTTPS_PROXY", Some(proxy.to_string())),
        ("HTTP_PROXY", Some(proxy.to_string())),
        ("NO_PROXY", Some("127.0.0.1,localhost".to_string())),
        // 员工 shell 里可能 export 了这些（本机 ~/.bashrc 就有 AGY_ADC_AUTH=1），会让 agy 改走自己的账号
        ("AGY_ADC_AUTH", None),
        ("GOOGLE_GENAI_USE_VERTEXAI", None),
        ("GOOGLE_API_KEY", None),
    ]);
    Ok((args, env))
}

/// 在 Loov 的 agy 私有 profile 里写 `telemetryEnabled: false`（员工在 agy 里改过就不动）。
/// agy 1.4.7 在 API Key 模式下还不认这个键（引导页的「分享交互数据」照样默认勾选、照样上报），
/// 真正拦住上报的是隧道里的 `tunnel::TELEMETRY_BLOCK`；这里是为以后的版本兜底。
fn disable_agy_telemetry(gemini_dir: &Path) -> Result<(), LoovError> {
    let path = gemini_dir.join("antigravity-cli").join("settings.json");
    let mut doc: serde_json::Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let Some(obj) = doc.as_object_mut() else { return Ok(()) };
    if obj.contains_key("telemetryEnabled") {
        return Ok(());
    }
    obj.insert("telemetryEnabled".to_string(), serde_json::Value::Bool(false));
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| app_error("INTERNAL", e.to_string()))?;
    crate::config::atomic_write_private(&path, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agy_profile_gets_telemetry_off_but_keeps_user_choice() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("antigravity-cli").join("settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, r#"{"modelProvider":"gemini"}"#).unwrap();
        disable_agy_telemetry(dir.path()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(v["telemetryEnabled"], false);
        assert_eq!(v["modelProvider"], "gemini");

        std::fs::write(&settings, r#"{"modelProvider":"gemini","telemetryEnabled":true}"#).unwrap();
        disable_agy_telemetry(dir.path()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(v["telemetryEnabled"], true, "员工自己改过的不覆盖");
    }
}

// ───────────────────────────── 桌面应用（Claude Desktop / Codex 桌面版） ─────────────────────────────

mod desktop {
    use super::*;

    pub const SUPPORTED: bool = cfg!(any(target_os = "macos", windows));

    /// 一个桌面应用在各平台上的样子。
    pub struct Spec {
        pub app: App,
        pub name: &'static str,
        /// macOS：/Applications 下的 .app 名（按顺序找）
        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        mac_apps: &'static [&'static str],
        /// macOS：认这个 bundle id。.app 同名不同应用（旧版 ChatGPT 也叫 ChatGPT.app）时靠它区分
        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        mac_bundle_id: &'static str,
        /// macOS：官方只发 Apple 芯片版本
        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        mac_arm_only: bool,
        /// Windows：MSIX 包名（可带通配）与发布者的通配（None 表示包名已足够唯一）
        #[cfg_attr(not(windows), allow(dead_code))]
        win_name: &'static str,
        #[cfg_attr(not(windows), allow(dead_code))]
        win_publisher: Option<&'static str>,
        /// Windows：官方 MSIX 下载地址，`{arch}` 换成 x64 / arm64
        #[cfg_attr(not(windows), allow(dead_code))]
        win_msix: &'static str,
        /// Windows：微软商店的产品 ID（有就先用 winget 从商店装）
        #[cfg_attr(not(windows), allow(dead_code))]
        win_store_id: Option<&'static str>,
        /// Windows 旧版非 MSIX 安装：(%LOCALAPPDATA% 下的目录, exe)
        #[cfg_attr(not(windows), allow(dead_code))]
        win_legacy: Option<(&'static str, &'static str)>,
    }

    pub const CLAUDE: Spec = Spec {
        app: App::Desktop,
        name: "Claude Desktop",
        mac_apps: &["Claude.app"],
        mac_bundle_id: "com.anthropic.claudefordesktop",
        mac_arm_only: false,
        win_name: "*Claude*",
        win_publisher: Some("*Anthropic*"),
        win_msix: "https://claude.ai/api/desktop/win32/{arch}/msix/latest/redirect",
        win_store_id: None,
        win_legacy: Some(("AnthropicClaude", "claude.exe")),
    };

    /// Codex 桌面版：OpenAI 现在的桌面 App（名字叫 ChatGPT，内含 Codex 模式），和 Codex 命令行共用
    /// `~/.codex/config.toml`，登录时 Loov 写好的路由对它同样生效。
    /// - Windows：商店里上架名是 ChatGPT（9PLM9XGG6VKS），装下来的 MSIX 包名是 `OpenAI.Codex`。
    /// - macOS：`ChatGPT.app`，bundle id `com.openai.codex`；旧版 ChatGPT（`com.openai.chat`）不读
    ///   config.toml，不算。官方只有 Apple 芯片版本。
    pub const CODEX: Spec = Spec {
        app: App::CodexDesktop,
        name: "Codex 桌面版",
        mac_apps: &["ChatGPT.app", "Codex.app"],
        mac_bundle_id: "com.openai.codex",
        mac_arm_only: true,
        win_name: "OpenAI.Codex",
        win_publisher: None,
        win_msix: "https://persistent.oaistatic.com/codex-app-prod/ChatGPT-{arch}.msix",
        win_store_id: Some("9PLM9XGG6VKS"),
        win_legacy: None,
    };

    /// 本机能不能装这个应用：Linux 都不行；Intel Mac 装不了只有 Apple 芯片版本的应用。
    pub fn supported(spec: &Spec) -> bool {
        if !SUPPORTED {
            return false;
        }
        #[cfg(target_os = "macos")]
        if spec.mac_arm_only && !is_apple_silicon() {
            return false;
        }
        let _ = spec;
        true
    }

    /// 按硬件判断（x86_64 版 Loov 在 Rosetta 下运行时也能认出 Apple 芯片）。
    #[cfg(target_os = "macos")]
    fn is_apple_silicon() -> bool {
        cfg!(target_arch = "aarch64")
            || std::process::Command::new("sysctl")
                .args(["-n", "hw.optional.arm64"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
                .unwrap_or(false)
    }

    pub fn spec(app: App) -> &'static Spec {
        match app {
            App::CodexDesktop => &CODEX,
            _ => &CLAUDE,
        }
    }

    pub struct Found {
        #[cfg_attr(not(windows), allow(dead_code))]
        pub kind: Kind,
        pub version: Option<String>,
    }

    #[allow(dead_code)]
    pub enum Kind {
        /// macOS 的 .app
        MacApp(PathBuf),
        /// Windows 旧版非 MSIX 安装
        WinExe(PathBuf),
        /// Windows MSIX 包：AUMID（PackageFamilyName!AppId）与安装目录
        WinPackage { aumid: String, location: String },
    }

    // ---- macOS ----

    #[cfg(target_os = "macos")]
    pub fn detect(spec: &Spec) -> Option<Found> {
        let home_apps = crate::config::get_home_dir().join("Applications");
        let app = spec
            .mac_apps
            .iter()
            .flat_map(|name| [PathBuf::from("/Applications").join(name), home_apps.join(name)])
            .find(|p| p.join("Contents").is_dir() && plist_value(p, "CFBundleIdentifier").as_deref() == Some(spec.mac_bundle_id))?;
        let version = plist_value(&app, "CFBundleShortVersionString");
        Some(Found { kind: Kind::MacApp(app), version })
    }

    #[cfg(target_os = "macos")]
    fn plist_value(app: &Path, key: &str) -> Option<String> {
        std::process::Command::new("defaults")
            .arg("read")
            .arg(app.join("Contents").join("Info"))
            .arg(key)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    }

    #[cfg(target_os = "macos")]
    pub async fn install(spec: &'static Spec, handle: &AppHandle, proxy: Option<&str>) -> Result<(), LoovError> {
        let client = http_client(spec, proxy)?;
        let tmp = tempfile::Builder::new().prefix("loov-desktop-").tempdir().map_err(io_error)?;
        if spec.app == App::CodexDesktop {
            // 官方 dmg（约 800MB，只有 Apple 芯片版本）：挂载后把 ChatGPT.app 拷进应用程序目录
            let dmg = tmp.path().join("ChatGPT.dmg");
            download(spec, handle, &client, "https://persistent.oaistatic.com/codex-app-prod/ChatGPT.dmg", &dmg).await?;
            super::emit(handle, spec.app, "install", None);
            let mnt = tmp.path().join("mnt");
            let out = tokio::process::Command::new("hdiutil")
                .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
                .arg(&mnt)
                .arg(&dmg)
                .output()
                .await
                .map_err(io_error)?;
            if !out.status.success() {
                return Err(app_error("INSTALL_FAILED", format!("挂载 ChatGPT.dmg 失败：{}", String::from_utf8_lossy(&out.stderr).trim())));
            }
            let result = copy_app(&mnt.join("ChatGPT.app"), "ChatGPT.app").await;
            let _ = tokio::process::Command::new("hdiutil").args(["detach", "-quiet"]).arg(&mnt).output().await;
            return result;
        }
        // Claude：官方 RELEASES.json 给的是 Claude.app 的 zip（Squirrel.Mac 的更新包），解开即可用
        let releases: serde_json::Value = client
            .get("https://downloads.claude.ai/releases/darwin/universal/RELEASES.json")
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| net_error(spec, e))?
            .json()
            .await
            .map_err(|e| net_error(spec, e))?;
        let current = releases.get("currentRelease").and_then(|v| v.as_str()).unwrap_or_default();
        let url = releases
            .get("releases")
            .and_then(|v| v.as_array())
            .and_then(|list| {
                list.iter()
                    .find(|r| r.get("version").and_then(|v| v.as_str()) == Some(current))
                    .or_else(|| list.first())
            })
            .and_then(|r| r.pointer("/updateTo/url"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| app_error("INSTALL_FAILED", "没有找到 Claude Desktop 的下载地址"))?
            .to_string();
        let zip = tmp.path().join("Claude.zip");
        download(spec, handle, &client, &url, &zip).await?;
        super::emit(handle, spec.app, "install", None);
        let unpacked = tmp.path().join("unpacked");
        let out = tokio::process::Command::new("ditto")
            .args(["-x", "-k"])
            .arg(&zip)
            .arg(&unpacked)
            .output()
            .await
            .map_err(io_error)?;
        if !out.status.success() {
            return Err(app_error("INSTALL_FAILED", format!("解压 Claude.app 失败：{}", String::from_utf8_lossy(&out.stderr).trim())));
        }
        copy_app(&unpacked.join("Claude.app"), "Claude.app").await
    }

    /// 用 ditto 拷到 /Applications；不可写（标准用户）时拷到 ~/Applications。
    #[cfg(target_os = "macos")]
    async fn copy_app(src: &Path, name: &str) -> Result<(), LoovError> {
        let mut last_err = String::new();
        for dir in [PathBuf::from("/Applications"), crate::config::get_home_dir().join("Applications")] {
            let _ = std::fs::create_dir_all(&dir);
            let out = tokio::process::Command::new("ditto").arg(src).arg(dir.join(name)).output().await.map_err(io_error)?;
            if out.status.success() && dir.join(name).join("Contents").is_dir() {
                return Ok(());
            }
            last_err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        }
        Err(app_error("INSTALL_FAILED", format!("安装 {name} 失败：{last_err}")))
    }

    #[cfg(target_os = "macos")]
    fn is_running(spec: &Spec) -> bool {
        // 按 bundle id 问系统，不依赖进程名
        std::process::Command::new("osascript")
            .args(["-e", &format!("application id \"{}\" is running", spec.mac_bundle_id)])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false)
    }

    #[cfg(target_os = "macos")]
    fn quit(spec: &Spec) {
        let _ = std::process::Command::new("osascript")
            .args(["-e", &format!("tell application id \"{}\" to quit", spec.mac_bundle_id)])
            .output();
    }

    #[cfg(target_os = "macos")]
    fn open(found: &Found) -> Result<(), LoovError> {
        let Kind::MacApp(app) = &found.kind else { unreachable!() };
        std::process::Command::new("open").arg(app).spawn().map_err(io_error)?;
        Ok(())
    }

    // ---- Windows ----

    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    #[cfg(windows)]
    fn powershell(script: &str) -> std::io::Result<std::process::Output> {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    }

    /// 找 MSIX 包：`$p`（找不到为 $null）。
    #[cfg(windows)]
    fn find_package_ps(spec: &Spec) -> String {
        let publisher = spec
            .win_publisher
            .map(|p| format!(" | Where-Object {{ $_.Publisher -like '{p}' }}"))
            .unwrap_or_default();
        format!("$p = Get-AppxPackage -Name '{}'{publisher} | Select-Object -First 1; ", spec.win_name)
    }

    #[cfg(windows)]
    pub fn detect(spec: &Spec) -> Option<Found> {
        let script = find_package_ps(spec)
            + "if ($p) { $id = (Get-AppxPackageManifest $p).Package.Applications.Application | Select-Object -First 1 -ExpandProperty Id; \
               Write-Output ($p.PackageFamilyName + '!' + $id + '|' + $p.Version + '|' + $p.InstallLocation) }";
        if let Ok(out) = powershell(&script) {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let parts: Vec<&str> = text.splitn(3, '|').collect();
            if parts.len() == 3 && parts[0].contains('!') {
                return Some(Found {
                    kind: Kind::WinPackage { aumid: parts[0].to_string(), location: parts[2].to_string() },
                    version: Some(parts[1].to_string()),
                });
            }
        }
        let (dir, exe) = spec.win_legacy?;
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
        let exe = local.join(dir).join(exe);
        exe.is_file().then(|| Found { kind: Kind::WinExe(exe), version: None })
    }

    #[cfg(windows)]
    pub async fn install(spec: &'static Spec, handle: &AppHandle, proxy: Option<&str>) -> Result<(), LoovError> {
        if let Some(store_id) = spec.win_store_id {
            if install_from_store(spec, handle, store_id).await {
                return Ok(());
            }
            log::info!("[loov] {} 经微软商店没装上，改为经安全通道下载官方 MSIX", spec.name);
        }
        install_msix(spec, handle, proxy).await
    }

    /// 官方 MSIX，下载后 Add-AppxPackage（按用户安装，不需要管理员）。
    #[cfg(windows)]
    async fn install_msix(spec: &'static Spec, handle: &AppHandle, proxy: Option<&str>) -> Result<(), LoovError> {
        let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
        let client = http_client(spec, proxy)?;
        let url = spec.win_msix.replace("{arch}", arch);
        let tmp = tempfile::Builder::new().prefix("loov-desktop-").tempdir().map_err(io_error)?;
        let msix = tmp.path().join("app.msix");
        download(spec, handle, &client, &url, &msix).await?;
        super::emit(handle, spec.app, "install", None);
        let path = msix.to_string_lossy().replace('\'', "''");
        let out = tokio::task::spawn_blocking(move || {
            powershell(&format!("$ErrorActionPreference = 'Stop'; Add-AppxPackage -Path '{path}'"))
        })
        .await
        .map_err(|e| app_error("INSTALL_FAILED", e.to_string()))?
        .map_err(io_error)?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).take(3).collect();
            return Err(app_error("INSTALL_FAILED", format!("安装 {} 失败：{}", spec.name, tail.join(" "))));
        }
        Ok(())
    }

    /// 先用 winget 从微软商店静默装（走微软 CDN，国内一般比经节点快）。成败以「检测到包」为准，
    /// 不看 winget 的退出码；每 3 秒查一次，装好就结束。没有 winget、失败或超过 20 分钟返回 false，
    /// 由调用方改走 MSIX。
    #[cfg(windows)]
    async fn install_from_store(spec: &'static Spec, handle: &AppHandle, store_id: &str) -> bool {
        const LIMIT: Duration = Duration::from_secs(20 * 60);
        super::emit(handle, spec.app, "install", None);
        let mut child = match tokio::process::Command::new("winget")
            .args(["install", "--id", store_id, "-s", "msstore", "--silent",
                   "--accept-package-agreements", "--accept-source-agreements"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                log::info!("[loov] 没有可用的 winget：{e}");
                return false;
            }
        };
        let installed = || async { tokio::task::spawn_blocking(move || detect(spec).is_some()).await.unwrap_or(false) };
        let started = std::time::Instant::now();
        loop {
            match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
                // winget 结束了：最后再认一次包（退出码 0 也可能没装上）
                Ok(status) => {
                    let ok = installed().await;
                    if !ok {
                        log::warn!("[loov] winget 结束（{status:?}）但没有检测到 {}", spec.name);
                    }
                    return ok;
                }
                Err(_) => {
                    if installed().await {
                        // 包已注册，winget 可能还在收尾，不必等它
                        return true;
                    }
                    if started.elapsed() > LIMIT {
                        log::warn!("[loov] winget 安装 {} 超过 20 分钟，放弃", spec.name);
                        let _ = child.kill().await;
                        return false;
                    }
                }
            }
        }
    }

    /// 按安装目录找进程（MSIX 的 exe 名各版本可能不同）；旧版按 exe 名。
    #[cfg(windows)]
    fn process_filter(spec: &Spec) -> String {
        let legacy = spec.win_legacy.map(|(_, exe)| exe.trim_end_matches(".exe")).unwrap_or("__none__");
        find_package_ps(spec)
            + &format!(
                "$procs = if ($p) {{ Get-Process | Where-Object {{ $_.Path -and $_.Path.StartsWith($p.InstallLocation) }} }} \
                 else {{ Get-Process -Name '{legacy}' -ErrorAction SilentlyContinue }}; "
            )
    }

    #[cfg(windows)]
    fn is_running(spec: &Spec) -> bool {
        powershell(&(process_filter(spec) + "if ($procs) { 'yes' }"))
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes")
            .unwrap_or(false)
    }

    #[cfg(windows)]
    fn quit(spec: &Spec) {
        // 先请主窗口关闭，2 秒后还在的再结束
        let _ = powershell(&(process_filter(spec)
            + "$procs | ForEach-Object { $_.CloseMainWindow() | Out-Null }; Start-Sleep -Seconds 2; \
               $procs | Where-Object { -not $_.HasExited } | Stop-Process -ErrorAction SilentlyContinue"));
    }

    #[cfg(windows)]
    fn open(found: &Found) -> Result<(), LoovError> {
        use std::os::windows::process::CommandExt;
        match &found.kind {
            Kind::WinPackage { aumid, .. } => {
                std::process::Command::new("explorer.exe")
                    .arg(format!("shell:AppsFolder\\{aumid}"))
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .map_err(io_error)?;
            }
            Kind::WinExe(exe) => {
                std::process::Command::new(exe).spawn().map_err(io_error)?;
            }
            Kind::MacApp(_) => unreachable!(),
        }
        Ok(())
    }

    // ---- 其他平台 ----

    #[cfg(not(any(target_os = "macos", windows)))]
    pub fn detect(_spec: &Spec) -> Option<Found> {
        None
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    pub async fn install(spec: &'static Spec, _handle: &AppHandle, _proxy: Option<&str>) -> Result<(), LoovError> {
        Err(app_error("UNSUPPORTED", format!("{} 暂不支持本平台", spec.name)))
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn is_running(_spec: &Spec) -> bool {
        false
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn quit(_spec: &Spec) {}

    #[cfg(not(any(target_os = "macos", windows)))]
    fn open(_found: &Found) -> Result<(), LoovError> {
        Err(app_error("UNSUPPORTED", "暂不支持本平台"))
    }

    // ---- 共用 ----

    /// 接入 Loov 后启动。配置是这次才写的而应用正开着，要重启才生效：没让重启就返回
    /// `DESKTOP_RESTART_REQUIRED`，由前端问过员工再带 `restart` 重来。
    /// - Claude Desktop：这里进路由模式（写 3p 网关配置）。
    /// - Codex 桌面版：路由在登录时随 Codex 命令行一起接好（共用 config.toml），这里只确认还接着。
    pub async fn launch(spec: &'static Spec, state: &AppState, restart: bool) -> Result<(), LoovError> {
        let found = tokio::task::spawn_blocking(move || detect(spec))
            .await
            .ok()
            .flatten()
            .ok_or_else(|| app_error("NOT_INSTALLED", format!("没有找到 {}", spec.name)))?;
        let already = attached(spec, state);
        if !already {
            match spec.app {
                App::CodexDesktop => super::super::provision::attach(state).await?,
                _ => super::super::provision::attach_desktop(state).await?,
            }
        }
        if tokio::task::spawn_blocking(move || is_running(spec)).await.unwrap_or(false) {
            if already && !restart {
                // 已经走 Loov，应用开着：拉到前台即可
                return open(&found);
            }
            if !restart {
                return Err(app_error(
                    "DESKTOP_RESTART_REQUIRED",
                    format!("{} 正在运行，需要重启才能改走 Loov 计费", spec.name),
                ));
            }
            tokio::task::spawn_blocking(move || quit(spec)).await.ok();
            let mut exited = false;
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(300)).await;
                if !tokio::task::spawn_blocking(move || is_running(spec)).await.unwrap_or(false) {
                    exited = true;
                    break;
                }
            }
            // 没退掉（比如弹了确认框）就别再 open：那只会聚焦旧窗口，员工会以为已经走 Loov 了
            if !exited {
                return Err(app_error(
                    "DESKTOP_QUIT_TIMEOUT",
                    format!("{} 没能自动退出，请手动关闭它后再点一次启动", spec.name),
                ));
            }
        }
        open(&found)
    }

    /// 是否已在路由模式且路由到 Loov。
    fn attached(spec: &Spec, state: &AppState) -> bool {
        use crate::app_config::AppType;
        let (app, id) = match spec.app {
            App::CodexDesktop => (AppType::Codex, super::super::provision::codex_child_id()),
            _ => (AppType::ClaudeDesktop, super::super::provision::DESKTOP_PROVIDER_ID.to_string()),
        };
        crate::mode::current::is_proxy(&app)
            && crate::mode::current::provider_in_use(&state.db, &app)
                .ok()
                .flatten()
                .is_some_and(|p| p.id == id)
    }

    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    fn http_client(spec: &Spec, proxy: Option<&str>) -> Result<reqwest::Client, LoovError> {
        let mut b = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) LoovAgent");
        if let Some(p) = proxy {
            b = b.proxy(reqwest::Proxy::all(p).map_err(|e| net_error(spec, e))?);
        }
        b.build().map_err(|e| net_error(spec, e))
    }

    /// 流式下载并按百分比发进度事件（每 1% 一次）。
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    async fn download(spec: &Spec, handle: &AppHandle, client: &reqwest::Client, url: &str, dest: &Path) -> Result<(), LoovError> {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;
        super::emit(handle, spec.app, "download", Some(0.0));
        let resp = client.get(url).send().await.and_then(|r| r.error_for_status()).map_err(|e| net_error(spec, e))?;
        let total = resp.content_length().filter(|n| *n > 0);
        let mut file = tokio::fs::File::create(dest).await.map_err(io_error)?;
        let mut stream = resp.bytes_stream();
        let (mut got, mut last) = (0u64, -1i64);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| net_error(spec, e))?;
            file.write_all(&chunk).await.map_err(io_error)?;
            got += chunk.len() as u64;
            if let Some(total) = total {
                let pct = (got * 100 / total) as i64;
                if pct != last {
                    last = pct;
                    super::emit(handle, spec.app, "download", Some(pct as f64));
                }
            }
        }
        file.flush().await.map_err(io_error)?;
        Ok(())
    }

    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    fn net_error(spec: &Spec, e: reqwest::Error) -> LoovError {
        app_error("DOWNLOAD_FAILED", format!("下载 {} 失败：{e}", spec.name))
    }

    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    fn io_error(e: std::io::Error) -> LoovError {
        app_error("INSTALL_FAILED", e.to_string())
    }
}
