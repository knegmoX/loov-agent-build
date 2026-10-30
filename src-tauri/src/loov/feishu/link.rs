//! 飞书长连接：取地址 → WebSocket → 定时 ping → 收数据帧（3 秒内 ACK）→ 拼分片 → 按 message_id 去重 →
//! 把收到的消息交给渠道。断线后退避重连（2 秒起，最长 60 秒）；重连后飞书可能重放旧事件，所以去重
//! 要跨连接保留（12 小时 / 5000 条）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

use super::api::Client;
use super::frame::{Frame, METHOD_DATA};

/// 收到的一条消息（只处理 im.message.receive_v1）。
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    pub message_id: String,
    pub chat_id: String,
    /// p2p / group
    pub chat_type: String,
    pub sender_open_id: String,
    pub text: String,
    /// 群聊里有没有 @ 机器人
    pub mentioned: bool,
}

/// 长连接状态（给界面看）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum LinkState {
    Connecting,
    Connected,
    Retrying { reason: String },
    Stopped,
}

/// 跨连接的去重：12 小时内见过的 message_id 不再处理。
pub struct Dedup {
    seen: HashSet<String>,
    order: VecDeque<(Instant, String)>,
}

impl Dedup {
    const CAP: usize = 5000;
    const TTL: Duration = Duration::from_secs(12 * 3600);

    pub fn new() -> Self {
        Dedup { seen: HashSet::new(), order: VecDeque::new() }
    }

    /// 第一次见到返回 true。
    pub fn first(&mut self, id: &str) -> bool {
        let now = Instant::now();
        while let Some((t, old)) = self.order.front() {
            if now.duration_since(*t) > Self::TTL || self.order.len() >= Self::CAP {
                self.seen.remove(old);
                self.order.pop_front();
            } else {
                break;
            }
        }
        if !self.seen.insert(id.to_string()) {
            return false;
        }
        self.order.push_back((now, id.to_string()));
        true
    }
}

impl Default for Dedup {
    fn default() -> Self {
        Self::new()
    }
}

/// 一直保持连接，直到 `stop` 变为 true。
pub async fn run(client: Arc<Client>, tx: mpsc::Sender<Incoming>, mut stop: watch::Receiver<bool>, state: watch::Sender<LinkState>) {
    let mut dedup = Dedup::new();
    let mut backoff = Duration::from_secs(2);
    loop {
        if *stop.borrow() {
            break;
        }
        let _ = state.send(LinkState::Connecting);
        let started = Instant::now();
        let result = connect_once(&client, &tx, &mut stop, &state, &mut dedup).await;
        if *stop.borrow() {
            break;
        }
        let reason = match result {
            Ok(()) => "连接被关闭".to_string(),
            Err(e) => e,
        };
        log::warn!("[loov] 飞书长连接断开：{reason}，{}s 后重连", backoff.as_secs());
        let _ = state.send(LinkState::Retrying { reason });
        // 连上过一阵子再断的，从头退避
        if started.elapsed() > Duration::from_secs(120) {
            backoff = Duration::from_secs(2);
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = stop.changed() => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
    let _ = state.send(LinkState::Stopped);
}

async fn connect_once(
    client: &Client,
    tx: &mpsc::Sender<Incoming>,
    stop: &mut watch::Receiver<bool>,
    state: &watch::Sender<LinkState>,
    dedup: &mut Dedup,
) -> Result<(), String> {
    let (url, ping_secs) = client.ws_endpoint().await?;
    let service: i32 = url::Url::parse(&url)
        .ok()
        .and_then(|u| u.query_pairs().find(|(k, _)| k == "service_id").and_then(|(_, v)| v.parse().ok()))
        .unwrap_or(0);
    let (ws, _) = tokio_tungstenite::connect_async(url.as_str()).await.map_err(|e| format!("连接失败：{e}"))?;
    let (mut sink, mut stream) = ws.split();
    let _ = state.send(LinkState::Connected);
    log::info!("[loov] 飞书长连接已建立（应用 {}）", client.app_id());
    let mut ping = tokio::time::interval(Duration::from_secs(ping_secs.max(10)));
    let mut chunks: HashMap<String, Vec<Option<Vec<u8>>>> = HashMap::new();
    loop {
        tokio::select! {
            _ = stop.changed() => {
                let _ = sink.send(Message::Close(None)).await;
                return Ok(());
            }
            _ = ping.tick() => {
                sink.send(Message::Binary(Frame::ping(service).encode().into())).await.map_err(|e| format!("发送心跳失败：{e}"))?;
            }
            msg = stream.next() => {
                let bytes = match msg {
                    None => return Ok(()),
                    Some(Err(e)) => return Err(format!("读取失败：{e}")),
                    Some(Ok(Message::Binary(b))) => b,
                    Some(Ok(Message::Close(_))) => return Ok(()),
                    Some(Ok(_)) => continue,
                };
                let frame = Frame::decode(&bytes)?;
                if frame.method != METHOD_DATA {
                    continue; // pong 等控制帧
                }
                // 先 ACK（3 秒内），再处理
                sink.send(Message::Binary(frame.ack().encode().into())).await.map_err(|e| format!("应答失败：{e}"))?;
                if frame.header("type") != Some("event") {
                    continue;
                }
                let Some(payload) = assemble(&mut chunks, &frame) else { continue };
                let Ok(event) = serde_json::from_slice::<Value>(&payload) else { continue };
                if let Some(incoming) = parse_message(&event) {
                    if dedup.first(&incoming.message_id) && tx.send(incoming).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// 大事件会拆成多帧（header sum / seq）：凑齐了返回完整 payload。
fn assemble(chunks: &mut HashMap<String, Vec<Option<Vec<u8>>>>, frame: &Frame) -> Option<Vec<u8>> {
    let sum: usize = frame.header("sum").and_then(|s| s.parse().ok()).unwrap_or(1);
    if sum <= 1 {
        return Some(frame.payload.clone());
    }
    let seq: usize = frame.header("seq").and_then(|s| s.parse().ok()).unwrap_or(0);
    let id = frame.header("message_id").unwrap_or_default().to_string();
    let parts = chunks.entry(id.clone()).or_insert_with(|| vec![None; sum]);
    if seq < parts.len() {
        parts[seq] = Some(frame.payload.clone());
    }
    if parts.iter().all(Option::is_some) {
        let whole = chunks.remove(&id)?.into_iter().flatten().flatten().collect();
        return Some(whole);
    }
    None
}

/// 只要 im.message.receive_v1；已读回执等其它事件忽略。
pub fn parse_message(event: &Value) -> Option<Incoming> {
    if event["header"]["event_type"].as_str() != Some("im.message.receive_v1") {
        return None;
    }
    let e = &event["event"];
    let msg = &e["message"];
    let content: Value = msg["content"].as_str().and_then(|c| serde_json::from_str(c).ok()).unwrap_or(Value::Null);
    let text = match msg["message_type"].as_str() {
        Some("text") => content["text"].as_str().unwrap_or_default().to_string(),
        Some("post") => post_text(&content),
        Some(other) => format!("[{other}]"),
        None => String::new(),
    };
    // 群聊里的 @机器人 在文本里是 @_user_1 占位：去掉，免得发给 agent
    let text = msg["mentions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["key"].as_str())
        .fold(text, |t, key| t.replace(key, ""))
        .trim()
        .to_string();
    Some(Incoming {
        message_id: msg["message_id"].as_str()?.to_string(),
        chat_id: msg["chat_id"].as_str().unwrap_or_default().to_string(),
        chat_type: msg["chat_type"].as_str().unwrap_or("p2p").to_string(),
        sender_open_id: e["sender"]["sender_id"]["open_id"].as_str().unwrap_or_default().to_string(),
        text,
        mentioned: msg["mentions"].as_array().is_some_and(|m| !m.is_empty()),
    })
}

/// 富文本消息里的纯文字。
fn post_text(content: &Value) -> String {
    let body = content.get("content").or_else(|| content.as_object().and_then(|o| o.values().next()).and_then(|v| v.get("content")));
    body.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|line| {
            line.as_array()
                .into_iter()
                .flatten()
                .filter_map(|seg| seg["text"].as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: &str, content: Value, mentions: Value) -> Value {
        json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_owner" } },
                "message": { "message_id": "om_1", "chat_id": "oc_1", "chat_type": "group", "message_type": kind,
                             "content": content.to_string(), "mentions": mentions }
            }
        })
    }

    #[test]
    fn parses_text_and_strips_mentions() {
        let m = parse_message(&event("text", json!({"text": "@_user_1 帮我看看 README"}), json!([{"key": "@_user_1"}]))).unwrap();
        assert_eq!(m.text, "帮我看看 README");
        assert!(m.mentioned);
        assert_eq!((m.message_id.as_str(), m.sender_open_id.as_str(), m.chat_type.as_str()), ("om_1", "ou_owner", "group"));
    }

    #[test]
    fn parses_post_and_ignores_other_events() {
        let post = json!({"zh_cn": {"title": "", "content": [[{"tag": "text", "text": "第一行"}], [{"tag": "text", "text": "第二行"}]]}});
        assert_eq!(parse_message(&event("post", post, json!([]))).unwrap().text, "第一行\n第二行");
        assert!(parse_message(&json!({"header": {"event_type": "im.message.message_read_v1"}})).is_none());
    }

    #[test]
    fn dedup_and_chunks() {
        let mut d = Dedup::new();
        assert!(d.first("a"));
        assert!(!d.first("a"));
        assert!(d.first("b"));
        let mut chunks = HashMap::new();
        let part = |seq: &str, data: &[u8]| Frame {
            headers: vec![("sum".into(), "2".into()), ("seq".into(), seq.into()), ("message_id".into(), "m".into())],
            payload: data.to_vec(),
            ..Default::default()
        };
        assert!(assemble(&mut chunks, &part("1", b"world")).is_none());
        assert_eq!(assemble(&mut chunks, &part("0", b"hello ")).unwrap(), b"hello world");
    }
}
