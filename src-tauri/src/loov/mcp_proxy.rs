//! 云端搜索 MCP 的本地透传：`/loov/mcp` → `{hub}/v1/mcp`（见 docs/loov/dev-02-cloud-search.md）。
//!
//! CLI 的 MCP 配置里只有本地路由令牌 `lat_`（由 `require_bearer` 中间件校验）。这里把它换成内存里的
//! 设备令牌 `ldt_`，经全局 HTTP 客户端（已指向 xray 隧道）发往 hub。设备令牌不落到任何 CLI 配置。

use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;

use super::Status;

/// 透传给 hub 的请求头（其余一律丢弃，尤其是 authorization / x-api-key / user-agent）。
const FORWARD_REQUEST_HEADERS: &[&str] = &[
    "content-type",
    "accept",
    "mcp-session-id",
    "mcp-protocol-version",
    "last-event-id",
];

/// 回传给 CLI 的响应头。
const FORWARD_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "content-encoding",
    "mcp-session-id",
    "allow",
];

const MAX_REQUEST_BODY: usize = 4 * 1024 * 1024;
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(30);

fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        serde_json::json!({ "ok": false, "code": code, "message": message }).to_string(),
    )
        .into_response()
}

pub fn hub_mcp_url() -> String {
    format!("{}/v1/mcp", super::hub_base())
}

pub async fn handle(req: Request) -> Response {
    let Some(device_token) = super::credentials().map(|c| c.device_token) else {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "LOOV_NOT_LOGGED_IN", "Loov Agent 尚未登录，云端搜索不可用");
    };
    if !matches!(super::status(), Status::Ready { .. }) {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "LOOV_TUNNEL_DOWN", "安全通道未连接，云端搜索暂不可用");
    }

    let method = req.method().clone();
    let in_headers = req.headers().clone();
    let body = match axum::body::to_bytes(req.into_body(), MAX_REQUEST_BODY).await {
        Ok(b) => b,
        Err(_) => return json_error(StatusCode::PAYLOAD_TOO_LARGE, "PAYLOAD_TOO_LARGE", "请求体过大"),
    };

    let Ok(upstream_method) = reqwest::Method::from_bytes(method.as_str().as_bytes()) else {
        return json_error(StatusCode::METHOD_NOT_ALLOWED, "METHOD_NOT_ALLOWED", "不支持的方法");
    };
    let mut rb = crate::proxy::http_client::get()
        .request(upstream_method, hub_mcp_url())
        // 全局客户端默认 600 秒整请求超时；首包另有 30 秒外层超时
        .timeout(Duration::from_secs(24 * 3600))
        .header(header::USER_AGENT, format!("LoovAgent-MCP/{}", env!("CARGO_PKG_VERSION")));
    for name in FORWARD_REQUEST_HEADERS {
        for value in in_headers.get_all(*name) {
            rb = rb.header(*name, value.clone());
        }
    }
    rb = rb.bearer_auth(device_token);
    if !body.is_empty() {
        rb = rb.body(body);
    }

    let resp = match tokio::time::timeout(FIRST_BYTE_TIMEOUT, rb.send()).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            log::warn!("[loov] 云端搜索请求失败: {}", if e.is_connect() { "连接失败" } else if e.is_timeout() { "超时" } else { "请求错误" });
            return json_error(StatusCode::BAD_GATEWAY, "LOOV_SEARCH_UNREACHABLE", "无法连接云端搜索服务");
        }
        Err(_) => return json_error(StatusCode::GATEWAY_TIMEOUT, "LOOV_SEARCH_TIMEOUT", "云端搜索响应超时"),
    };

    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED {
        // 不回传 401 / www-authenticate：Claude Code 收到 401 会尝试 OAuth 发现
        log::warn!("[loov] hub 拒绝设备令牌（云端搜索），需要重新登录");
        return json_error(StatusCode::BAD_GATEWAY, "LOOV_RELOGIN_REQUIRED", "设备登录已失效，请在 Loov Agent 里重新登录");
    }

    let mut out_headers = HeaderMap::new();
    for name in FORWARD_RESPONSE_HEADERS {
        if let Ok(hn) = HeaderName::from_bytes(name.as_bytes()) {
            for value in resp.headers().get_all(&hn) {
                out_headers.append(hn.clone(), value.clone());
            }
        }
    }
    let stream = resp.bytes_stream().map(|r| r.map_err(std::io::Error::other));
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = out_headers;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loov::credentials::Credentials;
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<(String, HeaderMap, String)>>>;

    fn test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    async fn fake_hub(reply_status: u16) -> (String, Seen) {
        use axum::routing::any;
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let app = axum::Router::new().route(
            "/hub/v1/mcp",
            any(move |req: Request| {
                let seen = s2.clone();
                async move {
                    let method = req.method().to_string();
                    let headers = req.headers().clone();
                    let body = axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap();
                    seen.lock().unwrap().push((method, headers, String::from_utf8_lossy(&body).into_owned()));
                    let mut r = Response::new(Body::from(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#));
                    *r.status_mut() = StatusCode::from_u16(reply_status).unwrap();
                    r.headers_mut().insert("content-type", HeaderValue::from_static("application/json"));
                    r.headers_mut().insert("mcp-session-id", HeaderValue::from_static("sess-1"));
                    r.headers_mut().insert("www-authenticate", HeaderValue::from_static("Bearer realm=x"));
                    r.headers_mut().insert("x-internal", HeaderValue::from_static("secret"));
                    r
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/hub"), seen)
    }

    fn login_state(ready: bool) {
        crate::loov::set_credentials(Some(Credentials {
            device_token: "ldt_device_secret_token_value_123456".into(),
            username: "u".into(),
            device_id: "d".into(),
            bootstrap: None,
            cached_at: 0,
        }));
        crate::loov::set_status(if ready {
            Status::Ready {
                username: "u".into(),
                device_id: "d".into(),
                proxy_url: String::new(),
                http_port: 1,
                nodes: vec![],
                route: crate::loov::tunnel::Route::Direct,
                via: None,
                net: Default::default(),
            }
        } else {
            Status::TunnelFailed { reason: "x".into() }
        });
    }

    fn post(body: &str) -> Request {
        axum::http::Request::builder()
            .method("POST")
            .uri("/loov/mcp")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-session-id", "client-sess")
            .header("authorization", "Bearer lat_local_token")
            .header("x-api-key", "lat_local_token")
            .header("user-agent", "claude-code/9.9 (Linux 6.1; x64)")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn forwards_with_device_token_and_filters_headers() {
        let _g = test_lock().lock().await;
        let (base, seen) = fake_hub(200).await;
        std::env::set_var("LOOV_HUB_BASE", &base);
        login_state(true);

        let resp = handle(post(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("mcp-session-id").unwrap(), "sess-1");
        assert!(resp.headers().get("www-authenticate").is_none());
        assert!(resp.headers().get("x-internal").is_none());
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("\"result\""));

        let (method, headers, body) = seen.lock().unwrap().pop().unwrap();
        assert_eq!(method, "POST");
        assert_eq!(headers.get("authorization").unwrap(), "Bearer ldt_device_secret_token_value_123456");
        assert!(headers.get("x-api-key").is_none(), "lat_ 不得外发");
        assert_eq!(headers.get("mcp-session-id").unwrap(), "client-sess");
        assert!(!headers.get("user-agent").unwrap().to_str().unwrap().contains("Linux"), "不透传客户端 UA");
        assert!(body.contains("\"ping\""));
        std::env::remove_var("LOOV_HUB_BASE");
    }

    #[tokio::test]
    async fn hub_401_becomes_502_without_www_authenticate() {
        let _g = test_lock().lock().await;
        let (base, _seen) = fake_hub(401).await;
        std::env::set_var("LOOV_HUB_BASE", &base);
        login_state(true);
        let resp = handle(post("{}")).await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        assert!(resp.headers().get("www-authenticate").is_none());
        std::env::remove_var("LOOV_HUB_BASE");
    }

    #[tokio::test]
    async fn refuses_when_not_ready_or_logged_out() {
        let _g = test_lock().lock().await;
        login_state(false);
        assert_eq!(handle(post("{}")).await.status(), StatusCode::SERVICE_UNAVAILABLE);
        crate::loov::set_credentials(None);
        crate::loov::set_status(Status::NoCredentials);
        assert_eq!(handle(post("{}")).await.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
