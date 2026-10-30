//! 按应用计费：判定一个请求来自哪个应用，注入 hub 为这个应用单独建的 Key（`loov-<设备>-<应用>`），
//! new-api 日志的 token_name 就能分出是哪个应用花的钱。
//!
//! 依据依次是：请求头 `x-loov-app`（Loov 自己写进配置的，如 Grok 的额外模型表、飞书渠道）→
//! 供应商行（每个应用有自己的 Loov 供应商 / 聚合成员）→ Codex 的 `originator` 头区分命令行与桌面版。
//! 这个头只在本机用，转发前由 forwarder 删掉。

use axum::http::HeaderMap;

/// Loov 写进各家配置、用来声明来源应用的请求头。
pub const APP_HEADER: &str = "x-loov-app";

/// hub 下发的应用 Key 名（与 loov-hub `APP_KEYS` 一致）。
pub const APPS: &[&str] = &["claude", "codex", "codexdesk", "agy", "grok", "desktop", "feishu"];

/// 这个请求该记在哪个应用上；认不出时为 None（用通用 Key）。
pub fn app_of(provider_id: &str, headers: &HeaderMap) -> Option<&'static str> {
    if let Some(declared) = headers.get(APP_HEADER).and_then(|v| v.to_str().ok()) {
        let declared = declared.trim().to_ascii_lowercase();
        if let Some(app) = APPS.iter().find(|a| **a == declared) {
            return Some(app);
        }
    }
    use super::provision::{claude_child_id, codex_child_id, gemini_child_id, DESKTOP_PROVIDER_ID, GROK_PROVIDER_ID};
    let member = super::provision::MEMBERS.iter().find(|m| m.id == provider_id).map(|m| m.app.clone());
    let app = if provider_id == claude_child_id() || member == Some(crate::app_config::AppType::Claude) {
        "claude"
    } else if provider_id == codex_child_id() || member == Some(crate::app_config::AppType::Codex) {
        if is_codex_desktop(headers) {
            "codexdesk"
        } else {
            "codex"
        }
    } else if provider_id == gemini_child_id() {
        "agy"
    } else if provider_id == GROK_PROVIDER_ID {
        "grok"
    } else if provider_id == DESKTOP_PROVIDER_ID || member == Some(crate::app_config::AppType::ClaudeDesktop) {
        "desktop"
    } else {
        return None;
    };
    Some(app)
}

/// 本地用量日志里记的应用：Grok 的额外模型表经 Codex 入口转发，按声明记成 `grokbuild`，
/// 其余照旧。
pub fn log_app_label(current: &'static str, headers: &HeaderMap) -> &'static str {
    match headers.get(APP_HEADER).and_then(|v| v.to_str().ok()) {
        Some(v) if v.eq_ignore_ascii_case("grok") => "grokbuild",
        _ => current,
    }
}

/// Codex 桌面版（ChatGPT 桌面 App）与命令行共用 config.toml，靠 `originator` 头区分：命令行是
/// `codex_cli_rs` / `codex_exec`，桌面版的值里带 desktop / chatgpt。
fn is_codex_desktop(headers: &HeaderMap) -> bool {
    headers
        .get("originator")
        .and_then(|v| v.to_str().ok())
        .map(|o| {
            let o = o.to_ascii_lowercase();
            o.contains("desktop") || o.contains("chatgpt")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, v.parse().unwrap());
        }
        m
    }

    #[test]
    fn provider_rows_map_to_apps() {
        use super::super::provision::*;
        let none = HeaderMap::new();
        assert_eq!(app_of(&claude_child_id(), &none), Some("claude"));
        assert_eq!(app_of("loov-claude-gemini", &none), Some("claude"));
        assert_eq!(app_of(&codex_child_id(), &none), Some("codex"));
        assert_eq!(app_of("loov-codex-gemini", &none), Some("codex"));
        assert_eq!(app_of(&gemini_child_id(), &none), Some("agy"));
        assert_eq!(app_of(GROK_PROVIDER_ID, &none), Some("grok"));
        assert_eq!(app_of(DESKTOP_PROVIDER_ID, &none), Some("desktop"));
        assert_eq!(app_of("loov-desktop-gpt", &none), Some("desktop"));
        assert_eq!(app_of("someone-elses-provider", &none), None);
    }

    #[test]
    fn codex_desktop_is_told_apart_by_originator() {
        let codex = super::super::provision::codex_child_id();
        assert_eq!(app_of(&codex, &h(&[("originator", "codex_cli_rs")])), Some("codex"));
        assert_eq!(app_of(&codex, &h(&[("originator", "Codex Desktop")])), Some("codexdesk"));
    }

    #[test]
    fn declared_header_wins_but_only_known_apps() {
        let codex = super::super::provision::codex_child_id();
        assert_eq!(app_of(&codex, &h(&[(APP_HEADER, "grok")])), Some("grok"));
        assert_eq!(app_of(&codex, &h(&[(APP_HEADER, "Feishu")])), Some("feishu"));
        assert_eq!(app_of(&codex, &h(&[(APP_HEADER, "evil")])), Some("codex"));
    }
}
