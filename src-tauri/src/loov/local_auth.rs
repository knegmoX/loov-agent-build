//! 本地路由（127.0.0.1:15721）的 bearer 鉴权。
//!
//! 上游把「代理接管」写进 CLI 配置时用的是常量占位 `PROXY_MANAGED`，本地路由对任何来源都放行。
//! Loov 激活后改为每台设备一枚随机 `lat_…` 令牌：写进 CLI 配置的是它，本地路由只认它；
//! 真实 Key 只在进程内存。未激活（没登录过）时一切行为与上游一致。

use std::sync::{OnceLock, RwLock};

use axum::{extract::Request, middleware::Next, response::IntoResponse, response::Response};
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::database::Database;
use crate::error::AppError;
use crate::live::project::claude::PROXY_TOKEN_PLACEHOLDER;

const SETTING_KEY: &str = "loov_local_token";
const PREFIX: &str = "lat_";

static LIVE: OnceLock<RwLock<Option<String>>> = OnceLock::new();

fn live() -> &'static RwLock<Option<String>> {
    LIVE.get_or_init(|| RwLock::new(None))
}

fn random_token() -> String {
    use aes_gcm::aead::rand_core::RngCore;
    let mut bytes = [0u8; 32];
    aes_gcm::aead::OsRng.fill_bytes(&mut bytes);
    format!(
        "{PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// 库里的令牌，没有或格式不对就生成一枚存进去（不碰内存里的全局状态）。
fn load_or_create(db: &Database) -> Result<String, AppError> {
    match db.get_setting(SETTING_KEY)? {
        Some(t) if t.starts_with(PREFIX) && t.len() > 20 => Ok(t),
        _ => store_new(db),
    }
}

fn store_new(db: &Database) -> Result<String, AppError> {
    let t = random_token();
    db.set_setting(SETTING_KEY, &t)?;
    Ok(t)
}

fn set_live(token: &str) {
    if let Ok(mut g) = live().write() {
        *g = Some(token.to_string());
    }
}

/// 读出（没有就生成）本地令牌并载入内存。只在 Loov 有凭证时调用：调用之后本地路由开始要求鉴权。
pub fn init(db: &Database) -> Result<String, AppError> {
    let token = load_or_create(db)?;
    set_live(&token);
    Ok(token)
}

/// 换一枚新令牌（登录 / 登出时调用）。调用方之后要重新投影 CLI 配置。
pub fn rotate(db: &Database) -> Result<String, AppError> {
    let t = store_new(db)?;
    set_live(&t);
    Ok(t)
}

/// 退出 Loov 模式：本地路由恢复为不鉴权。
pub fn disable() {
    if let Ok(mut g) = live().write() {
        *g = None;
    }
}

pub fn enabled() -> bool {
    live().read().map(|g| g.is_some()).unwrap_or(false)
}

/// 写进 CLI 配置的「代理凭据」：Loov 激活时是本地令牌，否则是上游的占位常量。
pub fn live_token() -> String {
    live()
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| PROXY_TOKEN_PLACEHOLDER.to_string())
}

/// 上游用「值等于占位常量」识别接管态；Loov 的令牌也要被识别，否则会被当成真实 Key 导入。
pub fn is_proxy_token(value: &str) -> bool {
    value == PROXY_TOKEN_PLACEHOLDER || value.starts_with(PREFIX)
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let ha = Sha256::digest(a.as_bytes());
    let hb = Sha256::digest(b.as_bytes());
    ha.iter().zip(hb.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0 && a.len() == b.len()
}

fn exempt(path: &str, _method: &axum::http::Method) -> bool {
    path == "/health"
        || path == "/status"
        // Claude Desktop 网关自带 token 校验（handlers.rs validate_claude_desktop_gateway_auth）
        || path.starts_with("/claude-desktop/")
    // 审计 S1-05：不再豁免 GET */models。CLI 配置里已带 lat_，正常探测请求会通过鉴权；
    // 免鉴权的 /models 会进入转发链、对 Loov 供应商触发凭据注入，故取消豁免（纵深防御，主防线在
    // forwarder 的 may_inject_key 守卫）。
}

fn bearer_of(req: &Request) -> Option<String> {
    // x-goog-api-key：Loov 页启动的 agy（Gemini SDK）把 GEMINI_API_KEY 放在这个头里
    for name in [
        axum::http::header::AUTHORIZATION,
        axum::http::HeaderName::from_static("x-api-key"),
        axum::http::HeaderName::from_static("x-goog-api-key"),
    ] {
        if let Some(v) = req.headers().get(&name).and_then(|v| v.to_str().ok()) {
            let token = v
                .strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
                .unwrap_or(v)
                .trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// 鉴权决策（审计 S8-01：与全局状态解耦，便于单测）。`expected` 为 `None` 表示 Loov 未激活，一律放行。
fn authorized(expected: Option<&str>, req: &Request) -> bool {
    let Some(expected) = expected else { return true };
    if exempt(req.uri().path(), req.method()) {
        return true;
    }
    matches!(bearer_of(req), Some(t) if constant_time_eq(&t, expected))
}

async fn gate(expected: Option<String>, req: Request, next: Next) -> Response {
    if authorized(expected.as_deref(), &req) {
        next.run(req).await
    } else {
        crate::proxy::ProxyError::AuthError(
            "本地路由需要 Loov Agent 颁发的令牌；请在 Loov Agent 里重新登录以刷新配置".to_string(),
        )
        .into_response()
    }
}

/// axum 中间件：Loov 未激活放行；激活后除豁免路径外都要带本地令牌。
pub async fn require_bearer(req: Request, next: Next) -> Response {
    let expected = live().read().ok().and_then(|g| g.clone());
    gate(expected, req, next).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_and_prefix_recognised() {
        assert!(is_proxy_token(PROXY_TOKEN_PLACEHOLDER));
        assert!(is_proxy_token("lat_abc"));
        assert!(!is_proxy_token("sk-real"));
        assert!(random_token().starts_with("lat_"));
        assert_ne!(random_token(), random_token());
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq("lat_a", "lat_a"));
        assert!(!constant_time_eq("lat_a", "lat_b"));
        assert!(!constant_time_eq("lat_a", "lat_aa"));
    }

    #[test]
    fn exemptions() {
        use axum::http::Method;
        assert!(exempt("/health", &Method::GET));
        // 审计 S1-05：GET /models 不再豁免
        assert!(!exempt("/v1/models", &Method::GET));
        assert!(!exempt("/v1/models", &Method::POST));
        assert!(!exempt("/v1/messages", &Method::POST));
        assert!(exempt("/claude-desktop/x", &Method::POST));
    }

    fn req(path: &str, headers: &[(&str, &str)]) -> Request {
        let mut b = axum::http::Request::builder().method("POST").uri(path);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(axum::body::Body::empty()).unwrap()
    }

    #[test]
    fn bearer_of_parses_both_headers() {
        let t = |h: &[(&str, &str)]| bearer_of(&req("/v1/messages", h));
        assert_eq!(t(&[("authorization", "Bearer lat_x")]).as_deref(), Some("lat_x"));
        assert_eq!(t(&[("authorization", "bearer lat_x")]).as_deref(), Some("lat_x"));
        assert_eq!(t(&[("authorization", "lat_x")]).as_deref(), Some("lat_x"));
        assert_eq!(t(&[("authorization", "Bearer   lat_x  ")]).as_deref(), Some("lat_x"));
        assert_eq!(t(&[("x-api-key", "lat_y")]).as_deref(), Some("lat_y"));
        assert_eq!(t(&[("x-goog-api-key", "lat_g")]).as_deref(), Some("lat_g"));
        // Authorization 为空时回落到 x-api-key
        assert_eq!(t(&[("authorization", "Bearer "), ("x-api-key", "lat_y")]).as_deref(), Some("lat_y"));
        assert_eq!(t(&[("authorization", "Bearer ")]), None);
        assert_eq!(t(&[]), None);
    }

    #[test]
    fn authorized_requires_exact_token_when_active() {
        let good = "lat_good_token_value_123";
        // 未激活：上游行为，一律放行
        assert!(authorized(None, &req("/v1/messages", &[])));
        // 激活：豁免路径放行，其余必须带对的令牌
        assert!(authorized(Some(good), &req("/health", &[])));
        assert!(!authorized(Some(good), &req("/v1/messages", &[])));
        assert!(!authorized(Some(good), &req("/v1/messages", &[("authorization", "Bearer lat_wrong")])));
        assert!(!authorized(Some(good), &req("/v1/messages", &[("authorization", "Bearer PROXY_MANAGED")])));
        assert!(authorized(Some(good), &req("/v1/messages", &[("authorization", &format!("Bearer {good}"))])));
        assert!(authorized(Some(good), &req("/v1/responses", &[("x-api-key", good)])));
    }

    #[tokio::test]
    async fn middleware_wiring_rejects_with_401() {
        use tower::ServiceExt;
        let app = |expected: Option<&'static str>| {
            axum::Router::new()
                .route("/v1/messages", axum::routing::post(|| async { "ok" }))
                .route("/health", axum::routing::post(|| async { "ok" }))
                .layer(axum::middleware::from_fn(move |r: Request, n: Next| gate(expected.map(str::to_string), r, n)))
        };
        let status = |expected: Option<&'static str>, r: Request| async move {
            let resp: Response = app(expected).oneshot(r).await.unwrap();
            resp.status()
        };
        let good = "lat_good_token_value_123";
        assert_eq!(status(Some(good), req("/v1/messages", &[])).await, 401);
        assert_eq!(status(Some(good), req("/v1/messages", &[("authorization", "Bearer lat_old")])).await, 401);
        assert_eq!(status(Some(good), req("/v1/messages", &[("authorization", &format!("Bearer {good}"))])).await, 200);
        assert_eq!(status(Some(good), req("/health", &[])).await, 200);
        assert_eq!(status(None, req("/v1/messages", &[])).await, 200);
    }

    #[test]
    fn token_persists_and_rotation_invalidates_old() {
        let db = Database::memory().unwrap();
        let first = load_or_create(&db).unwrap();
        assert!(first.starts_with(PREFIX));
        assert_eq!(load_or_create(&db).unwrap(), first, "重启沿用同一枚令牌");
        let second = store_new(&db).unwrap();
        assert_ne!(second, first);
        assert_eq!(load_or_create(&db).unwrap(), second);
        // 轮换后旧令牌不再通过
        assert!(!authorized(Some(&second), &req("/v1/messages", &[("authorization", &format!("Bearer {first}"))])));
        // 库里是坏值时重新生成
        db.set_setting(SETTING_KEY, "garbage").unwrap();
        assert!(load_or_create(&db).unwrap().starts_with(PREFIX));
    }
}
