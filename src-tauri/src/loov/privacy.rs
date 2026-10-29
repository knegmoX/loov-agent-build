//! 隐私整流器：改写 CLI 发往公司 new-api 的请求里能识别员工机器或代码仓库的字段
//! （docs/loov/dev-03-privacy-rectifier.md）。
//!
//! 只作用于 Loov 托管的供应商（`universal-claude-loov` / `universal-codex-loov`）。new-api 默认的
//! 渠道亲和规则会把 `X-Stainless-*`、`User-Agent`、`X-Codex-Turn-Metadata` 等头继续转给最终模型方，
//! 所以必须在客户端改。伪名 = HMAC-SHA256(本机随机密钥, 原值)，同一台机器确定且稳定、不可逆，
//! 保留会话粘性；`metadata.user_id` 里的 `session_id` 原样保留。
//!
//! 日志只记被改写的字段名，不记原值、也不记伪名。

use std::sync::{OnceLock, RwLock};

use hmac::{Hmac, Mac};
use http::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;

use super::credentials::{loov_dir, write_private};

const SETTINGS_KEY: &str = "privacy_config";

/// 固定的「通用」平台指纹：看起来像真实客户端，避免上游的客户端校验失败。
const FIXED_OS: &str = "MacOS";
const FIXED_ARCH: &str = "arm64";
const FIXED_RUNTIME: &str = "node";
const FIXED_RUNTIME_VERSION: &str = "v22.12.0";
/// UA 无法识别前缀时回退的固定伪装值（审计 S6-03：fail-closed，不透传未知 UA）。
const FIXED_UA: &str = "claude-cli/2.1.0 (external, cli)";

/// 遥测头前缀：这些前缀下、不在 SAFE_TELEMETRY_HEADERS 白名单里的头一律剥离（审计 S6-03 fail-closed）。
const TELEMETRY_HEADER_PREFIXES: &[&str] = &["x-stainless-", "x-codex-", "x-oai-", "x-openai-"];
/// 已知安全、需保留的 SDK 头（非机器指纹，剥离会影响兼容）。
const SAFE_TELEMETRY_HEADERS: &[&str] = &[
    "x-stainless-lang",
    "x-stainless-package-version",
    "x-stainless-retry-count",
    "x-stainless-timeout",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyConfig {
    #[serde(default = "yes")]
    pub headers: bool,
    #[serde(default = "yes")]
    pub metadata: bool,
    /// 清洗请求体 system/instructions 里的本机指纹（用户名/家目录段）。默认开。
    /// 注意：路径脱敏可能让模型复述的路径与真实路径不一致，极少数场景影响工具调用，可关闭。
    #[serde(default = "yes")]
    pub scrub_prompt: bool,
}

fn yes() -> bool {
    true
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self { headers: true, metadata: true, scrub_prompt: true }
    }
}

fn config_cell() -> &'static RwLock<PrivacyConfig> {
    static CELL: OnceLock<RwLock<PrivacyConfig>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(PrivacyConfig::default()))
}

pub fn config() -> PrivacyConfig {
    config_cell().read().map(|c| c.clone()).unwrap_or_default()
}

/// 从 settings 表读开关（缺失或损坏时用默认值：全开）。启动与登录时调用。
pub fn load_config(db: &crate::database::Database) {
    let cfg = db
        .get_setting(SETTINGS_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<PrivacyConfig>(&s).ok())
        .unwrap_or_default();
    if let Ok(mut g) = config_cell().write() {
        *g = cfg;
    }
}

pub use super::provision::is_loov_provider;

pub fn headers_apply(provider_id: &str) -> bool {
    config().headers && is_loov_provider(provider_id)
}

pub fn body_applies(provider_id: &str) -> bool {
    config().metadata && is_loov_provider(provider_id)
}

// ---- 伪名 ----

fn key_cell() -> &'static RwLock<Option<[u8; 32]>> {
    static CELL: OnceLock<RwLock<Option<[u8; 32]>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(None))
}

/// 本机随机密钥：`<loov_dir>/privacy.key`（0600），首次使用时生成。读写失败时用进程内随机密钥（只影响跨重启稳定性）。
fn key() -> [u8; 32] {
    if let Some(k) = key_cell().read().ok().and_then(|g| *g) {
        return k;
    }
    let path = loov_dir().join("privacy.key");
    let k = match std::fs::read(&path) {
        Ok(b) if b.len() == 32 => {
            let mut k = [0u8; 32];
            k.copy_from_slice(&b);
            k
        }
        _ => {
            use aes_gcm::aead::rand_core::RngCore;
            let mut k = [0u8; 32];
            aes_gcm::aead::OsRng.fill_bytes(&mut k);
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
                super::credentials::restrict_dir(dir);
            }
            if let Err(e) = write_private(&path, &k) {
                log::warn!("[loov] 隐私密钥写入失败（本次运行内仍稳定）: {e}");
            }
            k
        }
    };
    if let Ok(mut g) = key_cell().write() {
        *g = Some(k);
    }
    k
}

#[cfg(test)]
pub(crate) fn set_key_for_test(k: [u8; 32]) {
    if let Ok(mut g) = key_cell().write() {
        *g = Some(k);
    }
}

fn mac(input: &str, salt: &[u8]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&key()).expect("hmac key");
    m.update(salt);
    m.update(input.as_bytes());
    m.finalize().into_bytes().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(n, p)| p.len() == *n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// 保持原格式的伪名：UUID → UUID（v4 形态）；hex 串 → 同长度 hex；其它 → 同长度（16–64）hex。
pub fn pseudonym(original: &str) -> String {
    let d = mac(original, b"loov-privacy-v1:");
    if is_uuid(original) {
        let mut b = [0u8; 16];
        b.copy_from_slice(&d[..16]);
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let h = hex(&b);
        return format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]);
    }
    let mut h = hex(&d);
    let want = original.len().clamp(16, 128);
    while h.len() < want {
        h.push_str(&hex(&mac(&h, b"loov-privacy-v1-ext:")));
    }
    h.truncate(want);
    h
}

// ---- 请求头 ----

fn rewrite_user_agent(ua: &str) -> Option<String> {
    let first = ua.split_whitespace().next().unwrap_or("");
    if first.starts_with("claude-cli/") {
        let new = format!("{first} (external, cli)");
        return (new != ua).then_some(new);
    }
    if first.starts_with("codex_") && first.contains('/') {
        return (first != ua).then(|| first.to_string());
    }
    // 审计 S6-03：无法识别前缀（CLI 改版 / 新客户端）时回退固定伪装值而非透传，fail-closed。
    (ua != FIXED_UA).then(|| FIXED_UA.to_string())
}

/// 改写出站头，返回被改写的头名。
pub fn rewrite_headers(h: &mut HeaderMap) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if let Some(ua) = h.get(http::header::USER_AGENT).and_then(|v| v.to_str().ok()) {
        if let Some(new) = rewrite_user_agent(ua) {
            if let Ok(v) = HeaderValue::from_str(&new) {
                h.insert(http::header::USER_AGENT, v);
                changed.push("user-agent");
            }
        }
    }
    for (name, fixed) in [
        ("x-stainless-os", FIXED_OS),
        ("x-stainless-arch", FIXED_ARCH),
        ("x-stainless-runtime", FIXED_RUNTIME),
        ("x-stainless-runtime-version", FIXED_RUNTIME_VERSION),
        ("anthropic-client-os", FIXED_OS),
    ] {
        if let Some(cur) = h.get(name) {
            if cur.as_bytes() != fixed.as_bytes() {
                h.insert(name, HeaderValue::from_static(fixed));
                changed.push(name);
            }
        }
    }
    if let Some(id) = h.get("x-codex-installation-id").and_then(|v| v.to_str().ok()).map(str::to_string) {
        if let Ok(v) = HeaderValue::from_str(&pseudonym(&id)) {
            h.insert("x-codex-installation-id", v);
            changed.push("x-codex-installation-id");
        }
    }
    for name in ["x-codex-turn-metadata", "x-oai-attestation", "x-forwarded-for", "x-real-ip"] {
        if h.remove(name).is_some() {
            changed.push(name);
        }
    }
    // 审计 S6-03：对遥测头前缀做白名单放行——已处理/已知安全的保留，其余（含 CLI 升级新增的未知遥测头）
    // 一律剥离，fail-closed 防止指纹泄漏。
    let handled: &[&str] = &[
        "x-stainless-os",
        "x-stainless-arch",
        "x-stainless-runtime",
        "x-stainless-runtime-version",
        "x-codex-installation-id",
    ];
    let to_strip: Vec<String> = h
        .keys()
        .map(|k| k.as_str().to_ascii_lowercase())
        .filter(|k| {
            TELEMETRY_HEADER_PREFIXES.iter().any(|p| k.starts_with(p))
                && !SAFE_TELEMETRY_HEADERS.contains(&k.as_str())
                && !handled.contains(&k.as_str())
        })
        .collect();
    for k in to_strip {
        if let Ok(name) = http::header::HeaderName::from_bytes(k.as_bytes()) {
            h.remove(&name);
            changed.push("x-telemetry-unknown");
        }
    }
    changed
}

// ---- 请求体 ----

fn rewrite_user_id(raw: &str) -> Option<String> {
    // 新版：JSON 字符串 {device_id, account_uuid, session_id, ...}
    if let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(raw) {
        let mut touched = false;
        if let Some(Value::String(dev)) = obj.get("device_id").cloned() {
            if !dev.is_empty() {
                obj.insert("device_id".into(), Value::String(pseudonym(&dev)));
                touched = true;
            }
        }
        if matches!(obj.get("account_uuid"), Some(Value::String(s)) if !s.is_empty()) {
            obj.insert("account_uuid".into(), Value::String(String::new()));
            touched = true;
        }
        return touched.then(|| Value::Object(obj).to_string());
    }
    // 旧版：user_<hash>_account_<uuid>_session_<uuid>
    if let Some(rest) = raw.strip_prefix("user_") {
        if let Some((hash, rest)) = rest.split_once("_account_") {
            if let Some((_account, session)) = rest.split_once("_session_") {
                if !hash.is_empty() {
                    let new = format!("user_{}_account__session_{}", pseudonym(hash), session);
                    return (new != raw).then_some(new);
                }
            }
        }
    }
    // 审计 S6-03：既不是已知 JSON 也不是已知旧格式（CLI 改版出现未知结构）→ 整体伪名化，
    // 不透传原值（fail-closed）。会牺牲会话粘性，但隐私优先。
    (!raw.is_empty()).then(|| pseudonym(raw))
}

/// 当前系统用户名（审计 S2-01：用于脱敏 system/instructions 里的家目录段）。
fn current_username() -> Option<String> {
    for k in ["USER", "USERNAME", "LOGNAME"] {
        if let Ok(v) = std::env::var(k) {
            let v = v.trim();
            if !v.is_empty() && v != "root" && v != "user" {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// 把文本里 `/home/<user>`、`/Users/<user>`、`\Users\<user>` 的用户名段替换为 `user`，
/// 以及裸出现的用户名（词边界）。只替换与当前用户名精确匹配的段，降低误伤。返回是否改动。
fn scrub_fingerprint(text: &str, username: &str) -> Option<String> {
    if username.is_empty() || !text.contains(username) {
        return None;
    }
    let mut out = text.to_string();
    for sep in ['/', '\\'] {
        for root in ["home", "Users"] {
            let from = format!("{sep}{root}{sep}{username}");
            let to = format!("{sep}{root}{sep}user");
            out = out.replace(&from, &to);
        }
    }
    (out != text).then_some(out)
}

fn scrub_value_text(v: &mut Value, username: &str, changed: &mut bool) {
    match v {
        Value::String(s) => {
            if let Some(n) = scrub_fingerprint(s, username) {
                *s = n;
                *changed = true;
            }
        }
        // Claude 的 system 是 content block 数组：[{type:"text", text:"..."}]
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                if let Some(t) = item.get_mut("text") {
                    scrub_value_text(t, username, changed);
                }
            }
        }
        _ => {}
    }
}

/// 改写请求体，返回被改写的字段路径。
pub fn rewrite_body(body: &mut Value) -> Vec<&'static str> {
    let mut changed = Vec::new();
    // 审计 S2-01：脱敏客户端自动注入的 system / instructions 里的本机指纹（家目录含用户名）。
    // 只处理 system / instructions（客户端生成），不碰 messages / input（用户内容，脱敏会损坏语义）。
    if config().scrub_prompt {
        if let Some(username) = current_username() {
            let mut hit = false;
            if let Some(sys) = body.get_mut("system") {
                scrub_value_text(sys, &username, &mut hit);
            }
            if let Some(ins) = body.get_mut("instructions") {
                scrub_value_text(ins, &username, &mut hit);
            }
            if hit {
                changed.push("system/instructions");
            }
        }
    }
    if let Some(Value::String(uid)) = body.pointer("/metadata/user_id").cloned() {
        if let Some(new) = rewrite_user_id(&uid) {
            if let Some(slot) = body.pointer_mut("/metadata/user_id") {
                *slot = Value::String(new);
                changed.push("metadata.user_id");
            }
        }
    }
    if let Some(Value::Object(cm)) = body.get_mut("client_metadata") {
        if let Some(Value::String(id)) = cm.get("x-codex-installation-id").cloned() {
            cm.insert("x-codex-installation-id".into(), Value::String(pseudonym(&id)));
            changed.push("client_metadata.installation_id");
        }
        let turn_keys: Vec<String> = cm.keys().filter(|k| k.contains("turn-metadata") || k.contains("turn_metadata")).cloned().collect();
        for k in turn_keys {
            match cm.get(&k).cloned() {
                Some(Value::String(s)) => {
                    if let Ok(Value::Object(mut t)) = serde_json::from_str::<Value>(&s) {
                        if t.remove("workspaces").is_some() {
                            cm.insert(k, Value::String(Value::Object(t).to_string()));
                            changed.push("client_metadata.turn_metadata");
                        }
                    }
                }
                Some(Value::Object(mut t)) => {
                    if t.remove("workspaces").is_some() {
                        cm.insert(k, Value::Object(t));
                        changed.push("client_metadata.turn_metadata");
                    }
                }
                _ => {}
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn init() {
        set_key_for_test([7u8; 32]);
    }

    #[test]
    fn pseudonym_keeps_format_and_is_stable() {
        init();
        let uuid = "123e4567-e89b-12d3-a456-426614174000";
        let p = pseudonym(uuid);
        assert!(is_uuid(&p) && p != uuid);
        assert_eq!(p, pseudonym(uuid), "确定性");
        let hex64 = "a".repeat(64);
        let ph = pseudonym(&hex64);
        assert_eq!(ph.len(), 64);
        assert!(ph.chars().all(|c| c.is_ascii_hexdigit()) && ph != hex64);
        assert_ne!(pseudonym("device-a"), pseudonym("device-b"));
    }

    #[test]
    fn rewrites_headers() {
        init();
        let mut h = HeaderMap::new();
        h.insert("user-agent", HeaderValue::from_static("claude-cli/2.1.310 (external, sdk-ts, agent-sdk/0.9)"));
        h.insert("x-stainless-os", HeaderValue::from_static("Linux"));
        h.insert("x-stainless-arch", HeaderValue::from_static("x64"));
        h.insert("x-stainless-runtime-version", HeaderValue::from_static("v24.12.0"));
        h.insert("x-stainless-lang", HeaderValue::from_static("js"));
        h.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.5"));
        h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        h.insert("x-claude-code-session-id", HeaderValue::from_static("sess-1"));
        let changed = rewrite_headers(&mut h);
        assert_eq!(h["user-agent"], "claude-cli/2.1.310 (external, cli)");
        assert_eq!(h["x-stainless-os"], FIXED_OS);
        assert_eq!(h["x-stainless-arch"], FIXED_ARCH);
        assert_eq!(h["x-stainless-runtime-version"], FIXED_RUNTIME_VERSION);
        assert_eq!(h["x-stainless-lang"], "js", "SDK 语言不动");
        assert_eq!(h["anthropic-version"], "2023-06-01");
        assert_eq!(h["x-claude-code-session-id"], "sess-1");
        assert!(h.get("x-forwarded-for").is_none());
        assert!(h.get("x-stainless-runtime").is_none(), "不存在的头不添加");
        assert!(changed.contains(&"user-agent") && changed.contains(&"x-forwarded-for"));
    }

    #[test]
    fn rewrites_codex_headers() {
        init();
        let mut h = HeaderMap::new();
        h.insert("user-agent", HeaderValue::from_static("codex_cli_rs/0.139.0 (Ubuntu 24.4.0; x86_64) xterm-256color"));
        h.insert("originator", HeaderValue::from_static("codex_cli_rs"));
        h.insert("x-codex-installation-id", HeaderValue::from_static("123e4567-e89b-12d3-a456-426614174000"));
        h.insert("x-codex-turn-metadata", HeaderValue::from_static(r#"{"workspaces":{"associated_remote_urls":["git@github.com:corp/secret.git"]}}"#));
        h.insert("session_id", HeaderValue::from_static("abc"));
        rewrite_headers(&mut h);
        assert_eq!(h["user-agent"], "codex_cli_rs/0.139.0");
        assert_eq!(h["originator"], "codex_cli_rs");
        assert_ne!(h["x-codex-installation-id"], "123e4567-e89b-12d3-a456-426614174000");
        assert!(is_uuid(h["x-codex-installation-id"].to_str().unwrap()));
        assert!(h.get("x-codex-turn-metadata").is_none());
        assert_eq!(h["session_id"], "abc");
    }

    #[test]
    fn unknown_ua_falls_back_closed() {
        init();
        // 审计 S6-03：无法识别前缀的 UA 回退固定伪装值，不透传
        let mut h = HeaderMap::new();
        h.insert("user-agent", HeaderValue::from_static("curl/8.5.0"));
        let changed = rewrite_headers(&mut h);
        assert_eq!(h["user-agent"], FIXED_UA);
        assert!(changed.contains(&"user-agent"));
    }

    #[test]
    fn strips_unknown_telemetry_headers() {
        init();
        // 审计 S6-03：CLI 升级新增的未知遥测头被剥离；已知安全 SDK 头保留
        let mut h = HeaderMap::new();
        h.insert("user-agent", HeaderValue::from_static("claude-cli/9.9.9"));
        h.insert("x-stainless-new-telemetry", HeaderValue::from_static("leak"));
        h.insert("x-codex-future-fingerprint", HeaderValue::from_static("leak"));
        h.insert("x-oai-something", HeaderValue::from_static("leak"));
        h.insert("x-stainless-lang", HeaderValue::from_static("js"));
        h.insert("x-stainless-package-version", HeaderValue::from_static("1.2.3"));
        h.insert("anthropic-beta", HeaderValue::from_static("x"));
        rewrite_headers(&mut h);
        assert!(h.get("x-stainless-new-telemetry").is_none());
        assert!(h.get("x-codex-future-fingerprint").is_none());
        assert!(h.get("x-oai-something").is_none());
        assert_eq!(h["x-stainless-lang"], "js", "已知安全 SDK 头保留");
        assert_eq!(h["x-stainless-package-version"], "1.2.3");
        assert_eq!(h["anthropic-beta"], "x");
    }

    #[test]
    fn unknown_user_id_is_pseudonymized_not_passed() {
        init();
        // 审计 S6-03：未知 user_id 结构整体伪名化，不透传原值
        let mut b = json!({"metadata": {"user_id": "plain-identifying-value"}});
        let changed = rewrite_body(&mut b);
        assert_eq!(changed, vec!["metadata.user_id"]);
        assert_ne!(b["metadata"]["user_id"], "plain-identifying-value");
    }

    #[test]
    fn scrubs_home_dir_username_in_system() {
        init();
        std::env::set_var("USER", "zhangsan");
        // Claude system 为字符串
        let mut b = json!({"system": "Working directory: /home/zhangsan/proj\nOS: Linux"});
        let changed = rewrite_body(&mut b);
        assert!(changed.contains(&"system/instructions"));
        assert_eq!(b["system"], "Working directory: /home/user/proj\nOS: Linux");
        // Claude system 为 content block 数组 + Codex instructions
        let mut b2 = json!({
            "system": [{"type": "text", "text": "cwd /home/zhangsan/x"}],
            "instructions": "run in /Users/zhangsan/y"
        });
        rewrite_body(&mut b2);
        assert_eq!(b2["system"][0]["text"], "cwd /home/user/x");
        assert_eq!(b2["instructions"], "run in /Users/user/y");
        std::env::remove_var("USER");
    }

    #[test]
    fn rewrites_json_user_id_keeping_session() {
        init();
        let raw = r#"{"device_id":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","account_uuid":"123e4567-e89b-12d3-a456-426614174000","session_id":"11111111-2222-4333-8444-555555555555"}"#;
        let mut b = json!({"metadata": {"user_id": raw}});
        let changed = rewrite_body(&mut b);
        assert_eq!(changed, vec!["metadata.user_id"]);
        let out = b["metadata"]["user_id"].as_str().unwrap().to_string();
        assert!(!out.contains("0123456789abcdef0123"), "原 device_id 不得出现");
        assert!(!out.contains("123e4567"), "account_uuid 应置空");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["session_id"], "11111111-2222-4333-8444-555555555555");
        assert_eq!(v["account_uuid"], "");
        // 本地会话识别仍然拿得到原 session
        let sid = crate::proxy::session::extract_session_id(&http::HeaderMap::new(), &b, "claude");
        assert_eq!(sid.session_id, "11111111-2222-4333-8444-555555555555");
        // 确定性：同一输入两次结果相同
        let mut b2 = json!({"metadata": {"user_id": raw}});
        rewrite_body(&mut b2);
        assert_eq!(b2["metadata"]["user_id"], b["metadata"]["user_id"]);
    }

    #[test]
    fn rewrites_legacy_user_id() {
        init();
        let raw = "user_abcdef0123456789_account_123e4567-e89b-12d3-a456-426614174000_session_11111111-2222-4333-8444-555555555555";
        let mut b = json!({"metadata": {"user_id": raw}});
        rewrite_body(&mut b);
        let out = b["metadata"]["user_id"].as_str().unwrap();
        assert!(out.starts_with("user_") && out.contains("_account__session_11111111-2222-4333-8444-555555555555"));
        assert!(!out.contains("abcdef0123456789_account") && !out.contains("123e4567"));
    }

    #[test]
    fn rewrites_codex_client_metadata() {
        init();
        let mut b = json!({"client_metadata": {
            "x-codex-installation-id": "123e4567-e89b-12d3-a456-426614174000",
            "x-codex-turn-metadata": "{\"workspaces\":{\"associated_remote_urls\":[\"git@x:y.git\"]},\"sandbox\":\"ro\"}"
        }, "prompt_cache_key": "keep-me"});
        let changed = rewrite_body(&mut b);
        assert!(changed.contains(&"client_metadata.installation_id"));
        assert!(changed.contains(&"client_metadata.turn_metadata"));
        assert!(!b.to_string().contains("git@x:y.git"));
        assert_eq!(b["prompt_cache_key"], "keep-me");
    }

    #[test]
    fn applies_only_to_loov_providers() {
        assert!(is_loov_provider("universal-claude-loov"));
        assert!(is_loov_provider("universal-codex-loov"));
        assert!(!is_loov_provider("universal-claude-other"));
        assert!(!is_loov_provider("claude-official"));
    }
}
