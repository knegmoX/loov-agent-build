//! loov-hub 的 HTTP 客户端。登录类请求**直连** hub（不经隧道、不跟随系统代理）。
//!
//! 契约见 /mnt/data/workspace/loov-hub/src/app.ts：成功 `{ok:true,...}`，失败
//! `{ok:false, code, message}`；设备令牌 `Authorization: Bearer ldt_…`。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HubUser {
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HubDevice {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiInfo {
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reality {
    #[serde(rename = "serverName")]
    pub server_name: String,
    #[serde(default = "default_fingerprint")]
    pub fingerprint: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    #[serde(rename = "shortId", default)]
    pub short_id: String,
    #[serde(rename = "mldsa65Verify", default)]
    pub mldsa65_verify: String,
}

fn default_fingerprint() -> String {
    "chrome".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TunnelNode {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub region: String,
    pub address: String,
    pub port: u16,
    #[serde(default)]
    pub flow: String,
    #[serde(default)]
    pub encryption: String,
    pub reality: Reality,
    pub uuid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TunnelProfile {
    #[serde(default)]
    pub version: u32,
    pub nodes: Vec<TunnelNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bootstrap {
    pub user: HubUser,
    pub device: HubDevice,
    pub api: ApiInfo,
    pub tunnel: TunnelProfile,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(rename = "modelCatalog", default)]
    pub model_catalog: Vec<ModelEntry>,
    #[serde(rename = "topupUrl", default)]
    pub topup_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub username: String,
    #[serde(default)]
    pub group: String,
    #[serde(rename = "balanceUsd")]
    pub balance_usd: f64,
    #[serde(rename = "usedUsd", default)]
    pub used_usd: f64,
    #[serde(rename = "topupUrl", default)]
    pub topup_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub platform: String,
    #[serde(rename = "createdAt", default)]
    pub created_at: i64,
    #[serde(rename = "lastSeenAt", default)]
    pub last_seen_at: Option<i64>,
    #[serde(default)]
    pub current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Meta {
    #[serde(rename = "hubVersion", default)]
    pub hub_version: String,
    #[serde(default)]
    pub announcement: String,
    #[serde(default)]
    pub client: Value,
    #[serde(rename = "maxDevices", default)]
    pub max_devices: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResult {
    #[serde(rename = "deviceToken")]
    pub device_token: String,
    pub bootstrap: Bootstrap,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInput<'a> {
    pub name: &'a str,
    pub platform: &'a str,
}

#[derive(Debug, Clone, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HubError {
    /// 401：令牌无效 / 过期 / 需重登，`code` 为 hub 的错误码。
    #[error("{message}")]
    Unauthorized { code: String, message: String },
    /// 409 DEVICE_LIMIT：附带设备列表（最旧在前）。
    #[error("{message}")]
    DeviceLimit { message: String, devices: Vec<DeviceEntry> },
    /// 503 REGISTERED_RETRY_LOGIN：账号已建成，直接登录即可。
    #[error("{message}")]
    RegisteredRetryLogin { message: String },
    #[error("{message}")]
    RateLimited { message: String },
    /// 其它业务错误（INVALID_CREDENTIALS / INVITE_INVALID / REDEEM_FAILED …）。
    #[error("{message}")]
    Http { status: u16, code: String, message: String },
    #[error("无法连接服务器：{message}")]
    Network { message: String },
}

impl HubError {
    /// 这些错误意味着本地凭证已无效，必须清掉并回到登录页。
    pub fn is_relogin(&self) -> bool {
        matches!(self, HubError::Unauthorized { .. })
    }

    pub fn code(&self) -> &str {
        match self {
            HubError::Unauthorized { code, .. } | HubError::Http { code, .. } => code,
            HubError::DeviceLimit { .. } => "DEVICE_LIMIT",
            HubError::RegisteredRetryLogin { .. } => "REGISTERED_RETRY_LOGIN",
            HubError::RateLimited { .. } => "RATE_LIMITED",
            HubError::Network { .. } => "NETWORK",
        }
    }
}

#[derive(Clone)]
pub struct HubClient {
    http: reqwest::Client,
    base: String,
}

impl HubClient {
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(8))
            // 登录类请求直连 hub：不跟随系统代理，也不走本进程的全局代理
            .no_proxy()
            .user_agent(format!("LoovAgent/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
        }
    }

    pub fn default_client() -> Self {
        Self::new(&super::hub_base())
    }

    /// hub 的 origin（如 `https://me.loov.cc`），隧道自检要打 `/hub/healthz`。
    pub fn origin(&self) -> String {
        match url::Url::parse(&self.base) {
            Ok(u) => format!("{}://{}{}", u.scheme(), u.host_str().unwrap_or(""), u.port().map(|p| format!(":{p}")).unwrap_or_default()),
            Err(_) => self.base.clone(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v1{}", self.base, path)
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<T, HubError> {
        let resp = req.send().await.map_err(|e| HubError::Network {
            message: redact(&e.to_string()),
        })?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| HubError::Network {
            message: redact(&e.to_string()),
        })?;
        let body: Value = serde_json::from_str(&text).map_err(|_| HubError::Http {
            status,
            code: "BAD_RESPONSE".into(),
            message: format!("服务器返回了无法解析的内容（HTTP {status}）"),
        })?;
        if body.get("ok").and_then(Value::as_bool) == Some(true) {
            return serde_json::from_value(body).map_err(|e| HubError::Http {
                status,
                code: "BAD_RESPONSE".into(),
                message: format!("服务器响应缺少字段：{e}"),
            });
        }
        let code = body
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_string();
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("请求失败")
            .to_string();
        Err(match (status, code.as_str()) {
            (401, _) => HubError::Unauthorized { code, message },
            (409, "DEVICE_LIMIT") => HubError::DeviceLimit {
                message,
                devices: body
                    .get("devices")
                    .cloned()
                    .and_then(|d| serde_json::from_value(d).ok())
                    .unwrap_or_default(),
            },
            (503, "REGISTERED_RETRY_LOGIN") => HubError::RegisteredRetryLogin { message },
            (429, _) => HubError::RateLimited { message },
            _ => HubError::Http { status, code, message },
        })
    }

    fn authed(&self, req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        req.bearer_auth(token)
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
        device: DeviceInput<'_>,
        evict_oldest: bool,
    ) -> Result<AuthResult, HubError> {
        let mut body = serde_json::json!({ "username": username, "password": password, "device": device });
        if evict_oldest {
            body["evictOldest"] = Value::Bool(true);
        }
        self.send(self.http.post(self.url("/auth/login")).json(&body)).await
    }

    pub async fn register(
        &self,
        invite: &str,
        username: &str,
        password: &str,
        device: DeviceInput<'_>,
        evict_oldest: bool,
    ) -> Result<AuthResult, HubError> {
        let mut body = serde_json::json!({ "invite": invite, "username": username, "password": password, "device": device });
        if evict_oldest {
            body["evictOldest"] = Value::Bool(true);
        }
        self.send(self.http.post(self.url("/auth/register")).json(&body))
            .await
    }

    pub async fn bootstrap(&self, token: &str) -> Result<Bootstrap, HubError> {
        #[derive(Deserialize)]
        struct R {
            bootstrap: Bootstrap,
        }
        let r: R = self
            .send(self.authed(self.http.get(self.url("/bootstrap")), token))
            .await?;
        Ok(r.bootstrap)
    }

    pub async fn account(&self, token: &str) -> Result<Account, HubError> {
        #[derive(Deserialize)]
        struct R {
            account: Account,
        }
        let r: R = self
            .send(self.authed(self.http.get(self.url("/account")), token))
            .await?;
        Ok(r.account)
    }

    pub async fn devices(&self, token: &str) -> Result<Vec<DeviceEntry>, HubError> {
        #[derive(Deserialize)]
        struct R {
            devices: Vec<DeviceEntry>,
        }
        let r: R = self
            .send(self.authed(self.http.get(self.url("/devices")), token))
            .await?;
        Ok(r.devices)
    }

    pub async fn revoke_device(&self, token: &str, id: &str) -> Result<(), HubError> {
        let _: Value = self
            .send(self.authed(self.http.delete(self.url(&format!("/devices/{id}"))), token))
            .await?;
        Ok(())
    }

    pub async fn logout(&self, token: &str) -> Result<(), HubError> {
        let _: Value = self
            .send(
                self.authed(self.http.post(self.url("/auth/logout")), token)
                    .json(&serde_json::json!({})),
            )
            .await?;
        Ok(())
    }

    /// 兑换码充值，返回增加的美元数。
    pub async fn redeem(&self, token: &str, code: &str) -> Result<f64, HubError> {
        #[derive(Deserialize)]
        struct R {
            #[serde(rename = "addedUsd")]
            added_usd: f64,
        }
        let r: R = self
            .send(
                self.authed(self.http.post(self.url("/billing/redeem")), token)
                    .json(&serde_json::json!({ "code": code })),
            )
            .await?;
        Ok(r.added_usd)
    }

    pub async fn meta(&self) -> Result<Meta, HubError> {
        self.send(self.http.get(self.url("/meta"))).await
    }
}

/// 错误文案里不带 URL 查询串等可能含敏感信息的部分。
fn redact(s: &str) -> String {
    s.split('?').next().unwrap_or(s).chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_parses_hub_shape() {
        let v = serde_json::json!({
            "user": {"username": "alice"},
            "device": {"id": "0123456789abcdef", "name": "pc"},
            "api": {"baseUrl": "https://me.loov.cc", "key": "sk-x"},
            "tunnel": {"version": 1, "nodes": [{
                "id": "jp1", "name": "东京", "region": "JP", "address": "1.2.3.4", "port": 9443,
                "flow": "xtls-rprx-vision", "encryption": "mlkem768x25519plus.native.0rtt.AAA",
                "reality": {"serverName": "www.amazon.co.jp", "fingerprint": "chrome", "publicKey": "pk", "shortId": "0a", "mldsa65Verify": "vk"},
                "uuid": "u"
            }]},
            "models": ["claude-sonnet-5"],
            "topupUrl": "https://me.loov.cc/console/topup"
        });
        let b: Bootstrap = serde_json::from_value(v).unwrap();
        assert_eq!(b.tunnel.nodes[0].reality.server_name, "www.amazon.co.jp");
        assert!(b.model_catalog.is_empty());
    }

    #[tokio::test]
    async fn maps_error_codes() {
        use axum::{routing::post, Json, Router};
        let app = Router::new()
            .route("/hub/v1/auth/login", post(|| async {
                (axum::http::StatusCode::CONFLICT, Json(serde_json::json!({
                    "ok": false, "code": "DEVICE_LIMIT", "message": "满了",
                    "devices": [{"id": "a", "name": "old", "platform": "linux", "createdAt": 1, "lastSeenAt": null, "current": false}]
                })))
            }))
            .route("/hub/v1/auth/register", post(|| async {
                (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
                    "ok": false, "code": "REGISTERED_RETRY_LOGIN", "message": "请登录"
                })))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let c = HubClient::new(&format!("http://{addr}/hub"));
        let dev = DeviceInput { name: "t", platform: "linux" };
        match c.login("u", "password1", dev.clone(), false).await {
            Err(HubError::DeviceLimit { devices, .. }) => assert_eq!(devices[0].id, "a"),
            other => panic!("{other:?}"),
        }
        match c.register("LOOV-X", "u", "password1", dev, false).await {
            Err(HubError::RegisteredRetryLogin { .. }) => {}
            other => panic!("{other:?}"),
        }
    }
}
