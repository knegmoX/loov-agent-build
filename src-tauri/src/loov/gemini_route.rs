//! Antigravity（agy）的 Loov 入口：`/loov/gemini/v1beta/...` 固定发往 Loov 的 Gemini 子供应商。
//!
//! 不进 Gemini 的路由模式：那会改写员工自己 Gemini CLI 的 `~/.gemini/.env`。agy 由 Loov 页启动，
//! 环境变量里的 `GOOGLE_GEMINI_BASE_URL` 指向这里、`GEMINI_API_KEY` 是本地令牌 `lat_`
//! （agy 放在 `x-goog-api-key` 头里，由 `require_bearer` 校验），真实 Key 由 forwarder 注入。
//!
//! agy 的模型表是它内置的（含 3.7 Flash、3.1 Pro 等），标题生成还会悄悄用
//! `gemini-3.1-flash-lite-preview`。agy 的 `/model` 也只能列出这张内置表，所以 new-api 上没有的模型
//! 按档位换成 Loov 的：名字带 pro 的换成 Loov 的 Pro 档（非 Flash 的对话模型，如 gemini-4-argon），
//! 带 flash 的换成 Loov 的 Flash；员工在 agy 里切 Pro / Flash 就真的换了模型。都认不出时用员工在
//! Loov 页选的 Gemini，免得 404。

use axum::http::Uri;

use crate::database::Database;
use crate::mode::stack::StackTarget;
use crate::proxy::ProxyError;

/// 本地路由上 agy 的入口前缀（`GOOGLE_GEMINI_BASE_URL` = 路由地址 + 这个前缀）。
pub const PREFIX: &str = "/loov/gemini";

/// 去掉 [`PREFIX`]，并把模型换成 new-api 上有的。返回新 URI 与固定发往的供应商。
pub fn prepare(db: &Database, uri: &Uri) -> Result<(Uri, StackTarget), ProxyError> {
    let provider = db
        .get_provider_by_id(&super::provision::gemini_child_id(), "gemini")
        .ok()
        .flatten()
        .ok_or(ProxyError::NoProvidersConfigured)?;
    let fallback = provider
        .settings_config
        .pointer("/env/GEMINI_MODEL")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let available: Vec<String> = super::credentials()
        .and_then(|c| c.bootstrap)
        .map(|b| b.models)
        .unwrap_or_default();
    let pq = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    // 路由只把 `/loov/gemini/*path` 交到这里，前缀一定在
    let stripped = pq
        .strip_prefix(PREFIX)
        .ok_or_else(|| ProxyError::Internal(format!("unexpected gemini path: {pq}")))?;
    let (rewritten, original, upstream) = rewrite_model(stripped, &available, fallback.as_deref());
    let uri: Uri = rewritten
        .parse()
        .map_err(|e| ProxyError::Internal(format!("invalid gemini path: {e}")))?;
    Ok((uri, StackTarget { provider, upstream_model: upstream, original_model: original }))
}

/// `/v1beta/models/<model>:<method>?...` 里的模型不在 `available` 时换成 `fallback`。
/// 返回（新路径，原模型，上游模型）；路径里没有模型（如 GET /v1beta/models）时原样返回。
fn rewrite_model(path: &str, available: &[String], fallback: Option<&str>) -> (String, String, String) {
    const MARKER: &str = "/models/";
    let Some(start) = path.find(MARKER).map(|i| i + MARKER.len()) else {
        return (path.to_string(), String::new(), String::new());
    };
    let rest = &path[start..];
    let end = rest.find([':', '?', '/']).unwrap_or(rest.len());
    let model = &rest[..end];
    let keep = model.is_empty() || available.is_empty() || available.iter().any(|m| m == model);
    let target = if keep { None } else { same_tier(model, available).or(fallback) };
    match target {
        Some(fb) => {
            log::info!("[loov] agy 请求的模型 {model} 不在 Loov 目录，改用 {fb}");
            (format!("{}{}{}", &path[..start], fb, &rest[end..]), model.to_string(), fb.to_string())
        }
        None => (path.to_string(), model.to_string(), model.to_string()),
    }
}

/// agy 内置模型对应的 Loov 档位：带 pro 的 → 第一个非 Flash 的对话 Gemini；带 flash 的 → 名字最短的
/// Flash（gemini-3.8-flash，而不是 -cyber / -search 变体）。
fn same_tier<'a>(model: &str, available: &'a [String]) -> Option<&'a str> {
    let agent = available.iter().filter(|m| super::provision::is_agent_gemini(m));
    if model.contains("pro") {
        agent.clone().find(|m| !m.contains("flash")).map(String::as_str)
    } else if model.contains("flash") {
        agent.filter(|m| m.contains("flash")).min_by_key(|m| m.len()).map(String::as_str)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avail() -> Vec<String> {
        vec!["gemini-3.8-flash-cyber".into(), "gemini-3.8-flash".into(), "gemini-4-argon".into(), "gemini-embedding-001".into()]
    }

    #[test]
    fn keeps_known_model() {
        let (p, o, u) = rewrite_model("/v1beta/models/gemini-4-argon:streamGenerateContent?alt=sse", &avail(), Some("gemini-3.8-flash"));
        assert_eq!(p, "/v1beta/models/gemini-4-argon:streamGenerateContent?alt=sse");
        assert_eq!((o.as_str(), u.as_str()), ("gemini-4-argon", "gemini-4-argon"));
    }

    #[test]
    fn rewrites_unknown_model_by_tier() {
        // agy 内置的 Flash（含标题用的 flash-lite）→ Loov 的 Flash，即使员工选的是 argon
        let (p, o, u) = rewrite_model(
            "/v1beta/models/gemini-3.1-flash-lite-preview:streamGenerateContent?alt=sse",
            &avail(),
            Some("gemini-4-argon"),
        );
        assert_eq!(p, "/v1beta/models/gemini-3.8-flash:streamGenerateContent?alt=sse");
        assert_eq!(o, "gemini-3.1-flash-lite-preview");
        assert_eq!(u, "gemini-3.8-flash");
        // 内置的 Pro → Loov 的 Pro 档
        let (_, _, u) = rewrite_model("/v1beta/models/gemini-3.1-pro:generateContent", &avail(), Some("gemini-3.8-flash"));
        assert_eq!(u, "gemini-4-argon");
        // 认不出档位 → 员工选的
        let (_, _, u) = rewrite_model("/v1beta/models/gemini-ultra-x:generateContent", &avail(), Some("gemini-3.8-flash"));
        assert_eq!(u, "gemini-3.8-flash");
    }

    #[test]
    fn leaves_paths_without_model() {
        let (p, o, _) = rewrite_model("/v1beta/models", &avail(), Some("gemini-3.8-flash"));
        assert_eq!(p, "/v1beta/models");
        assert!(o.is_empty());
        let (p, _, _) = rewrite_model("/v1beta/models/x:countTokens", &[], Some("gemini-3.8-flash"));
        assert_eq!(p, "/v1beta/models/x:countTokens", "目录未知时不改写");
    }
}
