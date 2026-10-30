//! 登录前快照与「恢复原配置」。
//!
//! 背景（2026-10-23 真机验收发现）：登录时 CC Switch 的路由模式会改写 Claude Code 的
//! `settings.json` 与 Codex 的 `config.toml`；CC Switch 首次启动导入的「直连供应商」只记下了
//! 少数字段（Claude 只有 `model`，Vertex 的 `CLAUDE_CODE_USE_VERTEX` 等都没记），所以上游的
//! 「退回直连」恢复不出用户原来的配置。Loov 自己在**首次接管前**把这几个文件整份存下来，
//! 恢复时只把 CC Switch / Loov 管理的字段（关键字段、独有字段、加固键）写回原值，用户在
//! 这期间改过的其它字段保持不变。

use std::path::{Path, PathBuf};

use serde_json::Value;
use toml_edit::{DocumentMut, Item};

use crate::app_config::AppType;
use crate::live::engine::LiveFile;
use crate::live::floor;
use crate::live::patch::json::JsonPatch;
use crate::live::patch::toml::TomlPatch;
use crate::live::patch::{KeyPath, WholeFile};
use crate::mode::operation::{AppWrite, FileChange};
use crate::mode::state::PendingTarget;
use crate::services::provider::{claude_direct, ProviderService};
use crate::store::AppState;

use super::credentials::{loov_dir, restrict_dir, write_private};
use super::{local_auth, BLACKHOLE_PROXY, KEY_PLACEHOLDER, PROVIDER_ID};

/// Loov 写进 Claude `settings.json` 的加固键（见 provision::claude_harden_patch）。
pub const CLAUDE_HARDEN_TOP: &[&str] = &["skipWebFetchPreflight"];
pub const CLAUDE_HARDEN_ENV: &[&str] = &[
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "DISABLE_TELEMETRY",
    "DISABLE_AUTOUPDATER",
    "CLAUDE_CODE_AUTO_MODE_SERVER",
];
/// Loov 写进 Codex `config.toml` 的加固键。
pub const CODEX_HARDEN: &[&[&str]] = &[&["features", "plugins"], &["analytics", "enabled"]];
/// Loov 写进 Codex `config.toml` 顶层的加固键（不在任何表里）。
pub const CODEX_HARDEN_ROOT: &[&str] = &["web_search"];

const CLAUDE_FILE: &str = "claude-settings.json";
const CODEX_CONFIG_FILE: &str = "codex-config.toml";
const CODEX_AUTH_FILE: &str = "codex-auth.json";
const ABSENT_SUFFIX: &str = ".absent";

pub fn snapshot_dir() -> PathBuf {
    loov_dir().join("original")
}

pub fn snapshot_exists() -> bool {
    snapshot_dir().join("meta.json").is_file()
}

/// Loov 是否接管过本机的 CLI 配置（有快照，或者已登录）。接管过就不能让上游在退出时
/// 「指回直连」：那会按不完整的直连快照重写配置。
pub fn managed() -> bool {
    snapshot_exists() || super::active()
}

/// 官方直连 / 订阅类供应商：会让 CLI 绕开公司 API。Loov 接管期间不预置、不在托盘列出
/// （docs/loov/dev-01-preset-cleanup.md 第四节）。
pub fn blocked_provider(p: &crate::provider::Provider) -> bool {
    p.category.as_deref() == Some("official") || p.is_codex_oauth() || p.is_xai_oauth() || p.is_github_copilot()
}

/// Loov 接管期间是否隐藏该供应商。
pub fn hide_provider(p: &crate::provider::Provider) -> bool {
    managed() && blocked_provider(p)
}

fn claude_path() -> PathBuf {
    crate::config::get_claude_settings_path()
}

fn codex_config_path() -> PathBuf {
    crate::codex_config::get_codex_config_path()
}

fn codex_auth_path() -> PathBuf {
    crate::codex_config::get_codex_auth_path()
}

/// 这份内容是不是已经被 Loov 接管过（不能当作「原配置」存下来）。
fn looks_loov_managed(text: &str) -> bool {
    text.contains("\"lat_") || text.contains("= \"lat_") || text.contains(KEY_PLACEHOLDER)
}

fn save_one(dir: &Path, name: &str, src: &Path) -> Result<(), String> {
    match std::fs::read(src) {
        Ok(bytes) => write_private(&dir.join(name), &bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_private(&dir.join(format!("{name}{ABSENT_SUFFIX}")), b"")
        }
        Err(e) => Err(format!("读取 {} 失败：{e}", src.display())),
    }
}

/// 首次接管前存一份原配置；已有快照就不动（多次登录 / 登出不会覆盖最初的原配置）。
/// 如果当前文件看起来已经被 Loov 接管过（旧版本留下的），不存，免得把接管后的配置当成原配置。
pub fn snapshot_if_absent() -> Result<bool, String> {
    if snapshot_exists() {
        return Ok(false);
    }
    for p in [claude_path(), codex_config_path(), codex_auth_path()] {
        if let Ok(text) = std::fs::read_to_string(&p) {
            if looks_loov_managed(&text) {
                log::warn!("[loov] {} 已是接管后的状态，不作为原配置保存", p.display());
                return Ok(false);
            }
        }
    }
    let dir = snapshot_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建快照目录失败：{e}"))?;
    restrict_dir(&dir);
    save_one(&dir, CLAUDE_FILE, &claude_path())?;
    save_one(&dir, CODEX_CONFIG_FILE, &codex_config_path())?;
    save_one(&dir, CODEX_AUTH_FILE, &codex_auth_path())?;
    let meta = serde_json::json!({
        "version": 1,
        "createdAt": chrono::Utc::now().timestamp(),
        "claude": claude_path(),
        "codexConfig": codex_config_path(),
        "codexAuth": codex_auth_path(),
    });
    write_private(&dir.join("meta.json"), serde_json::to_string_pretty(&meta).unwrap().as_bytes())?;
    log::info!("[loov] 已保存登录前的 CLI 配置快照：{}", dir.display());
    Ok(true)
}

/// 撤掉刚拍、还没用上的快照（登录 / 注册被拒时）。
pub fn discard_snapshot() {
    let dir = snapshot_dir();
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => log::info!("[loov] 登录未成功，已撤掉本次快照：{}", dir.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("[loov] 撤掉快照失败: {e}"),
    }
}

/// 快照里的一份文件：`Ok(None)` 表示当时文件不存在。
fn read_snapshot(name: &str) -> Result<Option<Vec<u8>>, String> {
    let dir = snapshot_dir();
    if dir.join(format!("{name}{ABSENT_SUFFIX}")).is_file() {
        return Ok(None);
    }
    std::fs::read(dir.join(name))
        .map(Some)
        .map_err(|e| format!("快照缺少 {name}：{e}"))
}

fn claude_managed_env(key: &str) -> bool {
    floor::claude_floor_env(key) || floor::claude_exclusive_env(key) || CLAUDE_HARDEN_ENV.contains(&key)
}

fn claude_managed_top(key: &str) -> bool {
    floor::claude_floor_top(key) || CLAUDE_HARDEN_TOP.contains(&key)
}

/// 把 `current` 里受管理的字段改回 `original` 的值（original 为 None 表示原来没有这个文件：
/// 受管理字段全部删除）。其余字段不动。
pub fn claude_restore_patch(original: Option<&Value>, current: &Value) -> JsonPatch {
    let mut patch = JsonPatch::default();
    let empty = serde_json::Map::new();
    let orig_top = original.and_then(Value::as_object).unwrap_or(&empty);
    let cur_top = current.as_object().unwrap_or(&empty);
    let orig_env = orig_top.get("env").and_then(Value::as_object).unwrap_or(&empty);
    let cur_env = cur_top.get("env").and_then(Value::as_object).unwrap_or(&empty);

    for (k, v) in cur_env {
        if !claude_managed_env(k) {
            continue;
        }
        match orig_env.get(k) {
            Some(o) if o == v => {}
            Some(o) => patch.set.push((KeyPath::new(&["env", k]), o.clone())),
            None => patch.remove.push(KeyPath::new(&["env", k])),
        }
    }
    for (k, o) in orig_env {
        if claude_managed_env(k) && !cur_env.contains_key(k) {
            patch.set.push((KeyPath::new(&["env", k]), o.clone()));
        }
    }
    for (k, v) in cur_top {
        if !claude_managed_top(k) {
            continue;
        }
        match orig_top.get(k) {
            Some(o) if o == v => {}
            Some(o) => patch.set.push((KeyPath::new(&[k]), o.clone())),
            None => patch.remove.push(KeyPath::new(&[k])),
        }
    }
    for (k, o) in orig_top {
        if claude_managed_top(k) && !cur_top.contains_key(k) {
            patch.set.push((KeyPath::new(&[k]), o.clone()));
        }
    }
    // 权限档位写的 permissions.defaultMode（loov::safety）：改回原值；原来没有就删，整个 permissions
    // 只剩它的连表删
    let mode_of = |top: &serde_json::Map<String, Value>| top.get("permissions").and_then(|p| p.get("defaultMode")).cloned();
    match (mode_of(cur_top), mode_of(orig_top)) {
        (Some(c), Some(o)) if c == o => {}
        (_, Some(o)) => patch.set.push((KeyPath::new(&["permissions", "defaultMode"]), o)),
        (Some(_), None) => {
            let only_mode = cur_top
                .get("permissions")
                .and_then(Value::as_object)
                .is_some_and(|p| p.len() == 1);
            if only_mode && !orig_top.contains_key("permissions") {
                patch.remove.push(KeyPath::new(&["permissions"]));
            } else {
                patch.remove.push(KeyPath::new(&["permissions", "defaultMode"]));
            }
        }
        (None, None) => {}
    }
    patch
}

fn get_path<'a>(doc: &'a DocumentMut, path: &[&str]) -> Option<&'a Item> {
    let mut item = doc.as_item();
    for seg in path {
        item = item.as_table_like()?.get(seg)?;
    }
    Some(item)
}

/// 语义比较两个条目。不能直接比 `Item::to_string()`：隐式父表（如只有 `[model_providers.x]`
/// 子表的 `model_providers`）自身输出为空串，两张完全不同的表会被判成相同。
fn items_equal(a: &Item, b: &Item) -> bool {
    fn canonical(item: &Item) -> Option<toml::Value> {
        let mut doc = DocumentMut::new();
        doc.insert("x", item.clone());
        toml::from_str::<toml::Value>(&doc.to_string()).ok()?.get("x").cloned()
    }
    match (canonical(a), canonical(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Codex：顶层关键 / 独有字段、`model_providers` 整张表、嵌套关键字段、加固键改回原值。
pub fn codex_restore_patch(original: Option<&DocumentMut>, current: &DocumentMut) -> TomlPatch {
    let mut patch = TomlPatch::default();
    let blank = DocumentMut::new();
    let orig = original.unwrap_or(&blank);
    let managed_top = |k: &str| floor::CODEX_FLOOR_TOP.contains(&k) || floor::CODEX_EXCLUSIVE_TOP.contains(&k);

    fn fix(patch: &mut TomlPatch, current: &DocumentMut, orig: &DocumentMut, path: &[&str]) {
        match (get_path(current, path), get_path(orig, path)) {
            (Some(c), Some(o)) if items_equal(c, o) => {}
            (_, Some(o)) => patch.set.push((KeyPath::new(path), o.clone())),
            (Some(_), None) => patch.remove.push(KeyPath::new(path)),
            (None, None) => {}
        }
    }

    let mut tops: Vec<String> = current
        .as_table()
        .iter()
        .map(|(k, _)| k.to_string())
        .chain(orig.as_table().iter().map(|(k, _)| k.to_string()))
        .filter(|k| managed_top(k))
        .collect();
    tops.sort();
    tops.dedup();
    for k in &tops {
        fix(&mut patch, current, orig, &[k.as_str()]);
    }
    fix(&mut patch, current, orig, &["model_providers"]);
    // 下发 / 删除 loov-search 可能留下空的 [mcp_servers] 表：原配置里没有就一并删掉
    if get_path(orig, &["mcp_servers"]).is_none()
        && get_path(current, &["mcp_servers"])
            .and_then(Item::as_table_like)
            .is_some_and(|t| t.is_empty())
    {
        patch.remove.push(KeyPath::new(&["mcp_servers"]));
    }
    for nested in floor::CODEX_FLOOR_NESTED {
        fix(&mut patch, current, orig, nested);
    }
    for key in CODEX_HARDEN_ROOT.iter().chain(&["approval_policy", "sandbox_mode"]) {
        fix(&mut patch, current, orig, &[key]);
    }
    for harden in CODEX_HARDEN {
        let parent = harden[0];
        let orig_has_parent = get_path(orig, &[parent]).is_some();
        let cur_parent_only_harden = get_path(current, &[parent])
            .and_then(Item::as_table_like)
            .map(|t| t.iter().all(|(k, _)| k == harden[1]))
            .unwrap_or(false);
        if !orig_has_parent && cur_parent_only_harden {
            // 整张表都是 Loov 加的：连表一起删，不留空表
            patch.remove.push(KeyPath::new(&[parent]));
        } else {
            fix(&mut patch, current, orig, harden);
        }
    }
    patch
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RestoreReport {
    /// 是否找到了登录前的快照（找不到时只退出路由模式并删掉加固键）。
    pub snapshot: bool,
    pub claude: bool,
    pub codex: bool,
}

fn restore_claude(state: &AppState, have_snapshot: bool) -> Result<bool, String> {
    let path = claude_path();
    let current: Value = match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| format!("settings.json 无法解析：{e}"))?,
        Err(_) => Value::Object(Default::default()),
    };
    let mut patch = if have_snapshot {
        let original = read_snapshot(CLAUDE_FILE)?
            .map(|b| serde_json::from_slice::<Value>(&b))
            .transpose()
            .map_err(|e| format!("快照里的 settings.json 无法解析：{e}"))?;
        claude_restore_patch(original.as_ref(), &current)
    } else {
        // 没有快照：只删加固键（它们一定是 Loov 加的），关键字段交给上游的直连供应商
        JsonPatch {
            remove: CLAUDE_HARDEN_TOP
                .iter()
                .map(|k| KeyPath::new(&[k]))
                .chain(CLAUDE_HARDEN_ENV.iter().map(|k| KeyPath::new(&["env", k])))
                .collect(),
            ..Default::default()
        }
    };
    if !have_snapshot {
        // 没有快照：权限档位只在还是 Loov 写的值时删
        let applied = super::netmode::load_prefs().safety_applied.get(super::safety::CLAUDE_KEY).cloned();
        if applied.is_some() && super::safety::json_str(&current, &["permissions", "defaultMode"]) == applied.as_deref() {
            patch.remove.push(KeyPath::new(&["permissions", "defaultMode"]));
        }
    }
    if patch.set.is_empty() && patch.remove.is_empty() {
        return Ok(false);
    }
    claude_direct::run(&state.db, "loov.restore", Some(&patch), PendingTarget::default())
        .map_err(|e| e.to_string())?;
    Ok(true)
}

fn restore_codex(state: &AppState, have_snapshot: bool) -> Result<bool, String> {
    let path = codex_config_path();
    let current: DocumentMut = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .parse()
        .map_err(|e| format!("config.toml 无法解析：{e}"))?;
    let patch = if have_snapshot {
        let original = read_snapshot(CODEX_CONFIG_FILE)?
            .map(|b| String::from_utf8_lossy(&b).parse::<DocumentMut>())
            .transpose()
            .map_err(|e| format!("快照里的 config.toml 无法解析：{e}"))?;
        codex_restore_patch(original.as_ref(), &current)
    } else {
        codex_harden_only_patch(&current)
    };
    let auth_change = if have_snapshot {
        match read_snapshot(CODEX_AUTH_FILE)? {
            Some(bytes) => Some(WholeFile::Write(bytes)),
            None if codex_auth_path().exists() => Some(WholeFile::Delete),
            None => None,
        }
    } else {
        None
    };
    if patch.set.is_empty() && patch.remove.is_empty() && auth_change.is_none() {
        return Ok(false);
    }
    let write = AppWrite::begin(&state.db, AppType::Codex.as_str()).map_err(|e| e.to_string())?;
    let mut changes: Vec<FileChange<'_>> = vec![FileChange { file: LiveFile::private(path), patch: &patch }];
    if let Some(a) = auth_change.as_ref() {
        changes.push(FileChange { file: LiveFile::private(codex_auth_path()), patch: a });
    }
    write
        .run("loov.restore", &changes, PendingTarget::default())
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// 没有快照时只删 Loov 自己加的加固键（整张表都是加固键时连表删）。
pub fn codex_harden_only_patch(current: &DocumentMut) -> TomlPatch {
    let mut p = TomlPatch::default();
    for key in CODEX_HARDEN_ROOT {
        if get_path(current, &[key]).is_some() {
            p.remove.push(KeyPath::new(&[key]));
        }
    }
    // 权限档位只在还是 Loov 写的值时删
    let prefs = super::netmode::load_prefs();
    for (key, toml_key) in [(super::safety::CODEX_APPROVAL_KEY, "approval_policy"), (super::safety::CODEX_SANDBOX_KEY, "sandbox_mode")] {
        if let Some(applied) = prefs.safety_applied.get(key) {
            if super::safety::toml_str(current, &[toml_key]).as_deref() == Some(applied.as_str()) {
                p.remove.push(KeyPath::new(&[toml_key]));
            }
        }
    }
    for harden in CODEX_HARDEN {
        let only = get_path(current, &[harden[0]])
            .and_then(Item::as_table_like)
            .map(|t| t.iter().all(|(k, _)| k == harden[1]))
            .unwrap_or(false);
        if only {
            p.remove.push(KeyPath::new(&[harden[0]]));
        } else if get_path(current, harden).is_some() {
            p.remove.push(KeyPath::new(harden));
        }
    }
    p
}

/// 停用 Loov：登出（服务端吊销设备）→ 退出 Claude / Codex 的路由模式 → 按快照写回原配置 →
/// 删掉 Loov 的供应商行、本地令牌与黑洞代理。快照改名留档，下次登录重新拍。
pub async fn restore_original(state: &AppState) -> Result<RestoreReport, String> {
    if super::active() {
        if let Err(e) = super::flows::logout(state).await {
            log::warn!("[loov] 恢复前登出失败（继续恢复）: {e}");
        }
    } else {
        super::flows::shutdown().await;
    }
    // Claude Desktop、Grok Build 由 Loov 页启动时接入路由；退出代理模式（路由 / 聚合）即回到原来的模式
    for app in [AppType::Claude, AppType::Codex, AppType::ClaudeDesktop, AppType::GrokBuild] {
        if crate::mode::current::is_proxy(&app) {
            if let Err(e) = crate::mode::controller::exit(state, &app).await {
                log::warn!("[loov] 退出 {} 路由模式失败（继续恢复）: {e}", app.as_str());
            }
        }
    }
    // 先删云端搜索 MCP，再写回原配置：否则删条目会在 config.toml 里留下空的 [mcp_servers] 表
    if let Err(e) = super::provision::remove_mcp(state).await {
        log::warn!("[loov] 删除云端搜索 MCP 失败: {e}");
    }
    let have_snapshot = snapshot_exists();
    let claude = restore_claude(state, have_snapshot)?;
    let codex = restore_codex(state, have_snapshot)?;

    if let Err(e) = ProviderService::delete_universal(state, PROVIDER_ID) {
        log::warn!("[loov] 删除 Loov 供应商失败: {e}");
    }
    if let Err(e) = state.db.delete_provider(AppType::ClaudeDesktop.as_str(), super::provision::DESKTOP_PROVIDER_ID) {
        log::warn!("[loov] 删除 Claude Desktop 的 Loov 供应商失败: {e}");
    }
    if let Err(e) = state.db.delete_provider(AppType::GrokBuild.as_str(), super::provision::GROK_PROVIDER_ID) {
        log::warn!("[loov] 删除 Grok Build 的 Loov 供应商失败: {e}");
    }
    if let Err(e) = super::provision::remove_grok_models(&state.db) {
        log::warn!("[loov] 删除 Grok 的 Loov 模型表失败: {e}");
    }
    // 聚合成员：先移出名单（已退出代理模式，只改登记）再删行，员工以后自己用聚合时不会带上空成员
    for m in &super::provision::MEMBERS {
        let _ = crate::mode::controller::set_stack_member(state, &m.app, m.id, false).await;
        if let Err(e) = state.db.delete_provider(m.app.as_str(), m.id) {
            log::warn!("[loov] 删除聚合成员 {} 失败: {e}", m.id);
        }
    }
    local_auth::disable();
    // 权限档位的写入记录随恢复一起清掉（档位选择本身保留）
    let mut prefs = super::netmode::load_prefs();
    if !prefs.safety_applied.is_empty() {
        prefs.safety_applied.clear();
        let _ = super::netmode::save_prefs(&prefs);
    }
    if state.db.get_global_proxy_url().ok().flatten().as_deref() == Some(BLACKHOLE_PROXY) {
        let _ = state.db.set_global_proxy_url(None);
        let _ = crate::proxy::http_client::apply_proxy(None);
    }
    if have_snapshot {
        let archived = loov_dir().join(format!("original.restored-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S")));
        if let Err(e) = std::fs::rename(snapshot_dir(), &archived) {
            log::warn!("[loov] 快照归档失败: {e}");
        }
    }
    super::set_status(super::Status::NoCredentials);
    log::info!("[loov] 已恢复原配置（快照 {have_snapshot}，claude {claude}，codex {codex}）");
    Ok(RestoreReport { snapshot: have_snapshot, claude, codex })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn apply_json(patch: &JsonPatch, mut doc: Value) -> Value {
        patch.apply_to(Path::new("settings.json"), &mut doc).unwrap();
        doc
    }

    fn apply_toml(patch: &TomlPatch, text: &str) -> String {
        let mut doc: DocumentMut = text.parse().unwrap();
        patch.apply_to(Path::new("config.toml"), &mut doc).unwrap();
        doc.to_string()
    }

    #[test]
    fn claude_restores_vertex_env_and_drops_loov_keys() {
        let original = json!({
            "env": {"CLAUDE_CODE_USE_VERTEX": "1", "ANTHROPIC_VERTEX_PROJECT_ID": "p", "CLOUD_ML_REGION": "global", "MY_OWN": "x"},
            "model": "claude-fable-5-1",
            "permissions": {"allow": []}
        });
        let current = json!({
            "env": {
                "ANTHROPIC_BASE_URL": "http://127.0.0.1:15721", "ANTHROPIC_AUTH_TOKEN": "lat_abc",
                "HTTPS_PROXY": "http://127.0.0.1:1", "NO_PROXY": "127.0.0.1,localhost",
                "DISABLE_TELEMETRY": "1", "MY_OWN": "x", "ADDED_LATER": "keep"
            },
            "model": "claude-sonnet-5",
            "skipWebFetchPreflight": true,
            "permissions": {"allow": ["Bash"]}
        });
        let out = apply_json(&claude_restore_patch(Some(&original), &current), current);
        let env = out["env"].as_object().unwrap();
        assert_eq!(env.get("CLAUDE_CODE_USE_VERTEX").unwrap(), "1");
        assert_eq!(env.get("ANTHROPIC_VERTEX_PROJECT_ID").unwrap(), "p");
        assert_eq!(env.get("CLOUD_ML_REGION").unwrap(), "global");
        for k in ["ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN", "HTTPS_PROXY", "NO_PROXY", "DISABLE_TELEMETRY"] {
            assert!(!env.contains_key(k), "{k} 应删除");
        }
        assert_eq!(env.get("MY_OWN").unwrap(), "x");
        assert_eq!(env.get("ADDED_LATER").unwrap(), "keep", "非受管字段保留");
        assert_eq!(out["model"], "claude-fable-5-1");
        assert!(out.get("skipWebFetchPreflight").is_none());
        assert_eq!(out["permissions"]["allow"][0], "Bash", "非受管字段保留");
    }

    #[test]
    fn claude_without_original_file_removes_managed_keys() {
        let current = json!({"env": {"ANTHROPIC_BASE_URL": "x", "HTTPS_PROXY": "y", "KEEP": "1"}, "skipWebFetchPreflight": true});
        let out = apply_json(&claude_restore_patch(None, &current), current);
        assert_eq!(out, json!({"env": {"KEEP": "1"}}));
    }

    #[test]
    fn codex_restores_provider_and_drops_harden_tables() {
        let original = r#"# my comment
model_provider = "newapi"
model = "claude-opus-5-5"

[model_providers.newapi]
name = "new-api"
base_url = "http://127.0.0.1:3001/v1"
wire_api = "responses"
env_key = "NEW_API_KEY"

[projects."/tmp/x"]
trust_level = "trusted"
"#;
        let current = r#"# my comment
model_provider = "custom"
model = "gpt-6-sol"
model_reasoning_effort = "medium"
disable_response_storage = true

[model_providers.custom]
name = "custom"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
experimental_bearer_token = "lat_x"

[projects."/tmp/x"]
trust_level = "trusted"

[projects."/tmp/later"]
trust_level = "trusted"

[features]
plugins = false

[analytics]
enabled = false
"#;
        let orig_doc: DocumentMut = original.parse().unwrap();
        let cur_doc: DocumentMut = current.parse().unwrap();
        let out = apply_toml(&codex_restore_patch(Some(&orig_doc), &cur_doc), current);
        let parsed: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(parsed["model_provider"].as_str(), Some("newapi"), "{out}");
        assert_eq!(parsed["model"].as_str(), Some("claude-opus-5-5"));
        assert!(parsed.get("model_reasoning_effort").is_none(), "{out}");
        assert!(parsed.get("disable_response_storage").is_none(), "{out}");
        let providers = parsed["model_providers"].as_table().unwrap();
        assert!(providers.contains_key("newapi") && !providers.contains_key("custom"), "{out}");
        assert_eq!(providers["newapi"]["env_key"].as_str(), Some("NEW_API_KEY"));
        assert!(parsed.get("features").is_none() && parsed.get("analytics").is_none(), "{out}");
        assert!(parsed["projects"].get("/tmp/later").is_some(), "用户后来加的项目保留");
        assert!(!out.contains("lat_"));
        assert!(out.starts_with("# my comment"));
    }

    #[test]
    fn codex_drops_leftover_empty_mcp_servers_table() {
        let original = "model = \"a\"\n";
        let current = "model = \"a\"\n\n[mcp_servers]\n";
        let out = apply_toml(
            &codex_restore_patch(Some(&original.parse().unwrap()), &current.parse().unwrap()),
            current,
        );
        assert!(!out.contains("mcp_servers"), "{out}");
        // 用户自己有 MCP 时不动
        let current = "model = \"a\"\n\n[mcp_servers.mine]\ncommand = \"x\"\n";
        let out = apply_toml(
            &codex_restore_patch(Some(&original.parse().unwrap()), &current.parse().unwrap()),
            current,
        );
        assert!(out.contains("[mcp_servers.mine]"), "{out}");
    }

    #[test]
    fn codex_keeps_user_features_table() {
        let original = "[features]\nweb_search = true\n";
        let current = "[features]\nweb_search = true\nplugins = false\n";
        let out = apply_toml(
            &codex_restore_patch(Some(&original.parse().unwrap()), &current.parse().unwrap()),
            current,
        );
        assert!(out.contains("web_search = true") && !out.contains("plugins"), "{out}");
    }

    #[test]
    fn official_and_subscription_providers_are_blocked() {
        let mk = |category: Option<&str>, provider_type: Option<&str>, base: &str| {
            let mut p = crate::provider::Provider::with_id(
                "x".into(),
                "x".into(),
                json!({"env": {"ANTHROPIC_BASE_URL": base}}),
                None,
            );
            p.category = category.map(str::to_string);
            if let Some(t) = provider_type {
                let mut meta = crate::provider::ProviderMeta::default();
                meta.provider_type = Some(t.to_string());
                p.meta = Some(meta);
            }
            p
        };
        assert!(blocked_provider(&mk(Some("official"), None, "https://api.anthropic.com")));
        assert!(blocked_provider(&mk(None, Some("codex_oauth"), "")));
        assert!(blocked_provider(&mk(None, Some("xai_oauth"), "")));
        assert!(blocked_provider(&mk(None, Some("github_copilot"), "")));
        assert!(blocked_provider(&mk(Some("third_party"), None, "https://api.githubcopilot.com")));
        assert!(!blocked_provider(&mk(Some("aggregator"), None, "https://me.loov.cc")));
        assert!(!blocked_provider(&mk(Some("third_party"), None, "https://api.example.com")));
    }

    #[test]
    fn loov_managed_content_is_not_snapshotted() {
        assert!(looks_loov_managed(r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"lat_abc"}}"#));
        assert!(looks_loov_managed("experimental_bearer_token = \"lat_abc\""));
        assert!(looks_loov_managed(r#"{"OPENAI_API_KEY":"LOOV_MANAGED"}"#));
        assert!(!looks_loov_managed(r#"{"env":{"CLAUDE_CODE_USE_VERTEX":"1"}}"#));
    }
}
