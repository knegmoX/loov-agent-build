//! 模型故障时在同一网关内换模型重试：员工选的模型上游出错（5xx / 429）时，按 hub 下发的回退链
//! （`modelFallbacks`，hub 的 model-kinds.json 配置）依次改用别的模型，仍经 new-api 计费。
//!
//! 上游 CC Switch 的故障转移只能跨供应商，而且聚合（Stack）模式下整体关闭；Loov 的应用都在聚合
//! 模式，所以在 forwarder 里对 Loov 供应商单独做这一层。只在还没向客户端写任何数据时重试。
//! 已知故障（hub 健康度为 down）的模型跳过；换了模型会通知界面（价格可能不同，不能静默换）。

use crate::proxy::ProxyError;

/// 回退时发给界面的事件。
pub const FALLBACK_EVENT: &str = "loov-model-fallback";

/// 这个错误值不值得换模型重试：只有上游明确报错（5xx、429）才是模型 / 渠道的问题；
/// 网络、超时多半是安全通道的事，换模型没用。
pub fn should_fallback(err: &ProxyError) -> bool {
    matches!(err, ProxyError::UpstreamError { status, .. } if *status >= 500 || *status == 429)
}

/// `model` 出错时依次改用的模型：hub 下发的回退链里跳过已知故障的；Gemini 子供应商（agy 的原生
/// Gemini 接口）只能用 Gemini 模型。
pub fn next_models(provider_id: &str, model: &str) -> Vec<String> {
    let Some(b) = super::credentials().and_then(|c| c.bootstrap) else { return Vec::new() };
    chain(&b, provider_id, model)
}

fn chain(b: &super::hub_client::Bootstrap, provider_id: &str, model: &str) -> Vec<String> {
    let down = |m: &str| b.model_catalog.iter().any(|e| e.id == m && e.health.as_deref() == Some("down"));
    let gemini_only = provider_id == super::provision::gemini_child_id();
    b.model_fallbacks
        .get(model)
        .into_iter()
        .flatten()
        .filter(|m| m.as_str() != model && !down(m) && (!gemini_only || m.starts_with("gemini-")))
        .take(2)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loov::hub_client::Bootstrap;

    fn boot() -> Bootstrap {
        serde_json::from_value(serde_json::json!({
            "user": {"username": "u"}, "device": {"id": "d", "name": "n"},
            "api": {"baseUrl": "https://me.loov.cc", "key": "sk"},
            "tunnel": {"version": 1, "nodes": []},
            "models": [],
            "modelCatalog": [
                {"id": "gpt-6-sol", "kind": "openai", "health": "down"},
                {"id": "claude-opus-5-5", "kind": "anthropic", "health": "down"},
                {"id": "claude-sonnet-5", "kind": "anthropic"},
                {"id": "gemini-3.8-flash", "kind": "gemini"}
            ],
            "modelFallbacks": {
                "gpt-6-sol": ["claude-opus-5-5", "claude-sonnet-5", "gpt-6-sol"],
                "gemini-4-argon": ["claude-sonnet-5", "gemini-3.8-flash"]
            }
        }))
        .unwrap()
    }

    #[test]
    fn skips_down_models_and_self() {
        let b = boot();
        assert_eq!(chain(&b, "loov-claude-gpt", "gpt-6-sol"), ["claude-sonnet-5"]);
        assert!(chain(&b, "loov-claude-gpt", "claude-sonnet-5").is_empty(), "没配回退链就不回退");
    }

    #[test]
    fn gemini_native_route_only_falls_back_to_gemini() {
        let b = boot();
        let gem = crate::loov::provision::gemini_child_id();
        assert_eq!(chain(&b, &gem, "gemini-4-argon"), ["gemini-3.8-flash"]);
        assert_eq!(chain(&b, "loov-codex-gemini", "gemini-4-argon"), ["claude-sonnet-5", "gemini-3.8-flash"]);
    }

    #[test]
    fn only_upstream_5xx_and_429_trigger() {
        assert!(should_fallback(&ProxyError::UpstreamError { status: 500, body: None }));
        assert!(should_fallback(&ProxyError::UpstreamError { status: 429, body: None }));
        assert!(!should_fallback(&ProxyError::UpstreamError { status: 400, body: None }));
        assert!(!should_fallback(&ProxyError::Timeout("x".into())));
    }
}
