//! agent 权限档位（基础配置）：Loov 不做自己的沙箱，只把一个全局档位写成各家 agent 自己的原生设置。
//!
//! | 档位 | Claude Code `permissions.defaultMode` | Codex `approval_policy` / `sandbox_mode` | Grok `[ui] permission_mode` | agy 启动参数 |
//! |---|---|---|---|---|
//! | 谨慎 | `default` | `on-request` / `read-only` | `ask` | （默认：逐项询问） |
//! | 标准 | `acceptEdits` | `on-request` / `workspace-write` | `ask` | `--mode accept-edits` |
//! | 完全 | `bypassPermissions` | `never` / `danger-full-access` | `always-approve` | `--dangerously-skip-permissions` |
//!
//! 员工自己在 agent 里改过的值不覆盖：每次只在「文件里没有这个键，或者还是 Loov 上次写的值」时才写，
//! Loov 写过的值记在 prefs（`safety_applied`），恢复原配置时据此清除。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SafetyLevel {
    Careful,
    #[default]
    Standard,
    Full,
}

pub fn claude_mode(l: SafetyLevel) -> &'static str {
    match l {
        SafetyLevel::Careful => "default",
        SafetyLevel::Standard => "acceptEdits",
        SafetyLevel::Full => "bypassPermissions",
    }
}

/// (approval_policy, sandbox_mode)
pub fn codex_policy(l: SafetyLevel) -> (&'static str, &'static str) {
    match l {
        SafetyLevel::Careful => ("on-request", "read-only"),
        SafetyLevel::Standard => ("on-request", "workspace-write"),
        SafetyLevel::Full => ("never", "danger-full-access"),
    }
}

pub fn grok_mode(l: SafetyLevel) -> &'static str {
    match l {
        SafetyLevel::Full => "always-approve",
        _ => "ask",
    }
}

pub fn agy_args(l: SafetyLevel) -> Vec<String> {
    match l {
        SafetyLevel::Careful => Vec::new(),
        SafetyLevel::Standard => vec!["--mode".into(), "accept-edits".into()],
        SafetyLevel::Full => vec!["--dangerously-skip-permissions".into()],
    }
}

/// 写入目标在 prefs `safety_applied` 里的键。
pub const CLAUDE_KEY: &str = "claude.permissions.defaultMode";
pub const CODEX_APPROVAL_KEY: &str = "codex.approval_policy";
pub const CODEX_SANDBOX_KEY: &str = "codex.sandbox_mode";
pub const GROK_KEY: &str = "grok.ui.permission_mode";

/// 这次该不该写 `want`：文件里没有，或者还是 Loov 上次写的值（员工自己改过就不动）。
pub fn should_write(current: Option<&str>, last_applied: Option<&str>, want: &str) -> bool {
    match current {
        None => true,
        Some(c) if c == want => false,
        Some(c) => last_applied == Some(c),
    }
}

/// 读 JSON 里某个路径的字符串值。
pub fn json_str<'a>(doc: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = doc;
    for seg in path {
        cur = cur.get(seg)?;
    }
    cur.as_str()
}

/// 读 TOML 里某个路径的字符串值。
pub fn toml_str(doc: &toml_edit::DocumentMut, path: &[&str]) -> Option<String> {
    let mut item = doc.as_item();
    for seg in path {
        item = item.as_table_like()?.get(seg)?;
    }
    item.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_only_when_missing_or_still_loovs_value() {
        assert!(should_write(None, None, "acceptEdits"));
        assert!(!should_write(Some("acceptEdits"), Some("acceptEdits"), "acceptEdits"), "已经是目标值");
        assert!(should_write(Some("default"), Some("default"), "acceptEdits"), "还是 Loov 上次写的：换档");
        assert!(!should_write(Some("plan"), Some("default"), "acceptEdits"), "员工自己改成了 plan：不动");
        assert!(!should_write(Some("plan"), None, "acceptEdits"), "员工原本就有值：不动");
    }

    #[test]
    fn levels_map_to_native_settings() {
        assert_eq!(claude_mode(SafetyLevel::Standard), "acceptEdits");
        assert_eq!(codex_policy(SafetyLevel::Careful), ("on-request", "read-only"));
        assert_eq!(codex_policy(SafetyLevel::Full), ("never", "danger-full-access"));
        assert_eq!(grok_mode(SafetyLevel::Standard), "ask");
        assert_eq!(agy_args(SafetyLevel::Full), ["--dangerously-skip-permissions"]);
        assert_eq!(serde_json::to_string(&SafetyLevel::Careful).unwrap(), "\"careful\"");
    }
}
