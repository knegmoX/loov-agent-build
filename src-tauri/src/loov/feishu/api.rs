//! 飞书开放平台接口：扫码创建应用（设备码流程）、tenant_access_token、发 / 回复消息、CardKit 流式卡片、
//! 长连接地址。飞书在国内直连可达，不走 Loov 的安全通道；只用应用自己的凭据，不碰 Loov 的 Key。

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::Mutex;

const ACCOUNTS: &str = "https://accounts.feishu.cn";
const OPEN: &str = "https://open.feishu.cn";

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("LoovAgent/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

// ───────────────────────────── 扫码创建应用 ─────────────────────────────

/// 扫码创建「个人助手」应用的开始结果：员工用飞书打开 `url`（或扫它的二维码）确认。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub device_code: String,
    pub url: String,
    pub interval_secs: u64,
    pub expires_in_secs: u64,
}

/// 扫码确认后拿到的应用凭据。
#[derive(Debug, Clone)]
pub struct AppCreds {
    pub app_id: String,
    pub app_secret: String,
    pub owner_open_id: Option<String>,
    /// feishu / lark
    pub brand: String,
}

pub enum PollResult {
    Pending,
    /// 轮询太快，把间隔加长
    SlowDown,
    Done(AppCreds),
}

async fn post_form(url: &str, form: &[(&str, &str)]) -> Result<Value, String> {
    let resp = http().post(url).form(form).send().await.map_err(|e| format!("连接飞书失败：{e}"))?;
    // 设备码流程等待确认期间是 HTTP 400 + {"error":"authorization_pending"}：只看响应体
    resp.json::<Value>().await.map_err(|e| format!("飞书返回的内容无法解析：{e}"))
}

/// 开始扫码创建（官方 lark-cli 同一流程：archetype=PersonalAgent，自带机器人、长连接事件、消息与卡片权限，
/// 不需要管理员审批）。
pub async fn begin_registration() -> Result<Registration, String> {
    let url = format!("{ACCOUNTS}/oauth/v1/app/registration");
    post_form(&url, &[("action", "init")]).await?;
    let b = post_form(
        &url,
        &[
            ("action", "begin"),
            ("archetype", "PersonalAgent"),
            ("auth_method", "client_secret"),
            ("request_user_info", "open_id tenant_brand"),
        ],
    )
    .await?;
    let device_code = b["device_code"].as_str().ok_or_else(|| format!("飞书没有返回设备码：{b}"))?.to_string();
    let user_code = b["user_code"].as_str().unwrap_or_default();
    Ok(Registration {
        device_code,
        url: format!("{OPEN}/page/cli?user_code={user_code}"),
        interval_secs: b["interval"].as_u64().unwrap_or(5).max(3),
        expires_in_secs: b["expire_in"].as_u64().unwrap_or(600),
    })
}

pub async fn poll_registration(device_code: &str) -> Result<PollResult, String> {
    let p = post_form(&format!("{ACCOUNTS}/oauth/v1/app/registration"), &[("action", "poll"), ("device_code", device_code)]).await?;
    match p["error"].as_str() {
        Some("authorization_pending") => return Ok(PollResult::Pending),
        Some("slow_down") => return Ok(PollResult::SlowDown),
        Some("access_denied") => return Err("已在飞书里取消创建".into()),
        Some("expired_token") => return Err("二维码已过期，请重新开始".into()),
        Some(other) => return Err(format!("飞书创建应用失败：{other} {}", p["error_description"].as_str().unwrap_or(""))),
        None => {}
    }
    let (Some(id), Some(secret)) = (p["client_id"].as_str(), p["client_secret"].as_str()) else {
        return Ok(PollResult::Pending);
    };
    Ok(PollResult::Done(AppCreds {
        app_id: id.to_string(),
        app_secret: secret.to_string(),
        owner_open_id: p["user_info"]["open_id"].as_str().map(str::to_string),
        brand: p["user_info"]["tenant_brand"].as_str().unwrap_or("feishu").to_string(),
    }))
}

// ───────────────────────────── 应用接口 ─────────────────────────────

/// 一个应用的接口客户端：tenant_access_token 缓存到过期前 5 分钟。
pub struct Client {
    app_id: String,
    app_secret: String,
    http: reqwest::Client,
    token: Mutex<Option<(String, Instant)>>,
}

impl Client {
    pub fn new(app_id: &str, app_secret: &str) -> Self {
        Client { app_id: app_id.into(), app_secret: app_secret.into(), http: http(), token: Mutex::new(None) }
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    async fn tenant_token(&self) -> Result<String, String> {
        let mut g = self.token.lock().await;
        if let Some((t, until)) = g.as_ref() {
            if Instant::now() < *until {
                return Ok(t.clone());
            }
        }
        let v: Value = self
            .http
            .post(format!("{OPEN}/open-apis/auth/v3/tenant_access_token/internal"))
            .json(&json!({ "app_id": self.app_id, "app_secret": self.app_secret }))
            .send()
            .await
            .map_err(|e| format!("连接飞书失败：{e}"))?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let token = v["tenant_access_token"].as_str().ok_or_else(|| format!("飞书应用凭据无效：{}", v["msg"]))?.to_string();
        let ttl = v["expire"].as_u64().unwrap_or(7200).saturating_sub(300);
        *g = Some((token.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(token)
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Value) -> Result<Value, String> {
        let token = self.tenant_token().await?;
        let v: Value = self
            .http
            .request(method, format!("{OPEN}{path}"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("连接飞书失败：{e}"))?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        if v["code"].as_i64() != Some(0) {
            return Err(format!("飞书接口 {path} 返回 {}：{}", v["code"], v["msg"].as_str().unwrap_or("")));
        }
        Ok(v["data"].clone())
    }

    /// 长连接地址与心跳间隔（秒）。
    pub async fn ws_endpoint(&self) -> Result<(String, u64), String> {
        let v: Value = self
            .http
            .post(format!("{OPEN}/callback/ws/endpoint"))
            .json(&json!({ "AppID": self.app_id, "AppSecret": self.app_secret }))
            .send()
            .await
            .map_err(|e| format!("连接飞书失败：{e}"))?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        if v["code"].as_i64() != Some(0) {
            return Err(format!("获取飞书长连接地址失败：{} {}", v["code"], v["msg"]));
        }
        let url = v["data"]["URL"].as_str().ok_or("飞书没有返回长连接地址")?.to_string();
        let ping = v["data"]["ClientConfig"]["PingInterval"].as_u64().unwrap_or(120);
        Ok((url, ping))
    }

    /// 回复一条消息（文本）。
    pub async fn reply_text(&self, message_id: &str, text: &str) -> Result<(), String> {
        self.call(
            reqwest::Method::POST,
            &format!("/open-apis/im/v1/messages/{message_id}/reply"),
            json!({ "msg_type": "text", "content": json!({ "text": text }).to_string() }),
        )
        .await
        .map(|_| ())
    }

    /// 建一张流式卡片（CardKit），返回 card_id。正文在 element_id 为 `body` 的 markdown 元素里。
    pub async fn create_stream_card(&self, title: &str, initial: &str) -> Result<String, String> {
        let card = json!({
            "schema": "2.0",
            "config": { "streaming_mode": true, "update_multi": true, "summary": { "content": title } },
            "header": { "title": { "tag": "plain_text", "content": title } },
            "body": { "elements": [ { "tag": "markdown", "content": initial, "element_id": "body" } ] }
        });
        let d = self
            .call(reqwest::Method::POST, "/open-apis/cardkit/v1/cards", json!({ "type": "card_json", "data": card.to_string() }))
            .await?;
        d["card_id"].as_str().map(str::to_string).ok_or_else(|| "飞书没有返回 card_id".into())
    }

    /// 用卡片回复一条消息。
    pub async fn reply_card(&self, message_id: &str, card_id: &str) -> Result<(), String> {
        self.call(
            reqwest::Method::POST,
            &format!("/open-apis/im/v1/messages/{message_id}/reply"),
            json!({ "msg_type": "interactive", "content": json!({ "type": "card", "data": { "card_id": card_id } }).to_string() }),
        )
        .await
        .map(|_| ())
    }

    /// 流式更新卡片正文：每次传全文，`sequence` 对同一张卡严格递增。
    pub async fn stream_text(&self, card_id: &str, content: &str, sequence: u64) -> Result<(), String> {
        self.call(
            reqwest::Method::PUT,
            &format!("/open-apis/cardkit/v1/cards/{card_id}/elements/body/content"),
            json!({ "content": content, "sequence": sequence }),
        )
        .await
        .map(|_| ())
    }

    /// 关掉卡片的流式模式（结束时；也是 10 分钟后自动关闭前续期用的开关）。
    pub async fn set_streaming(&self, card_id: &str, on: bool, sequence: u64) -> Result<(), String> {
        self.call(
            reqwest::Method::PATCH,
            &format!("/open-apis/cardkit/v1/cards/{card_id}/settings"),
            json!({ "settings": json!({ "config": { "streaming_mode": on } }).to_string(), "sequence": sequence }),
        )
        .await
        .map(|_| ())
    }
}
