//! 飞书渠道：在飞书里私聊机器人，就像在终端里用某个 agent，费用照样经 Loov 计费。
//!
//! 配置极简：Loov 页「连接飞书」→ 用飞书扫码 → 自动创建员工自己的「个人助手」应用（不需要管理员审批）
//! → 选一个 agent 和工作目录。之后飞书私聊这个机器人，Loov（桌面版或服务器模式）在本机用这个 agent
//! 处理，结果以流式卡片回到飞书。
//!
//! - 传输：飞书长连接（[`link`]），不需要公网地址；同一个应用只连一台设备（飞书把事件随机投给多条连接中的一条）。
//! - 执行：各家官方的无头接口（[`runner`]），会话按飞书会话延续。
//! - 安全：只认创建者本人（扫码时拿到的 open_id）；群聊默认关闭，开启后也必须 @ 机器人；权限按 agent 权限档位。
//! - 凭据：App Secret 用 Loov 凭据同一套机器密钥加密后存本机（`<loov>/feishu.json`），不上传。

pub mod api;
pub mod frame;
pub mod link;
pub mod runner;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch, Mutex};

use crate::loov::credentials::{loov_dir, write_private};
use crate::store::AppState;

/// 本机保存的飞书渠道配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub app_id: String,
    /// App Secret（机器密钥加密 + base64）
    pub secret_enc: String,
    pub owner_open_id: String,
    pub brand: String,
    /// 用哪个 agent 处理（claude / codex / agy / grok）
    pub agent: String,
    /// 工作目录
    pub cwd: String,
    /// 允许在群里 @ 机器人使用
    pub allow_group: bool,
    pub enabled: bool,
}

/// 给界面 / 命令行看的状态（不含 Secret）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub configured: bool,
    pub enabled: bool,
    pub app_id: String,
    pub agent: String,
    pub cwd: String,
    pub allow_group: bool,
    pub link: link::LinkState,
}

fn config_path() -> PathBuf {
    loov_dir().join("feishu.json")
}

fn sessions_path() -> PathBuf {
    loov_dir().join("feishu-sessions.json")
}

pub fn load_config() -> Option<Config> {
    let c: Config = serde_json::from_slice(&std::fs::read(config_path()).ok()?).ok()?;
    (!c.app_id.is_empty()).then_some(c)
}

fn save_config(c: &Config) -> Result<(), String> {
    let _ = std::fs::create_dir_all(loov_dir());
    write_private(&config_path(), serde_json::to_string_pretty(c).unwrap_or_default().as_bytes())
}

fn secret_of(c: &Config) -> Result<String, String> {
    let blob = base64::engine::general_purpose::STANDARD.decode(&c.secret_enc).map_err(|e| e.to_string())?;
    let plain = crate::loov::credentials::open(&blob)?;
    String::from_utf8(plain).map_err(|e| e.to_string())
}

// ───────────────────────────── 运行中的渠道 ─────────────────────────────

struct Running {
    app_id: String,
    stop: watch::Sender<bool>,
    state: watch::Receiver<link::LinkState>,
}

fn running() -> &'static Mutex<Option<Running>> {
    static R: OnceLock<Mutex<Option<Running>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(None))
}

pub async fn status() -> Status {
    let c = load_config().unwrap_or_default();
    let link = running().lock().await.as_ref().map(|r| r.state.borrow().clone()).unwrap_or(link::LinkState::Stopped);
    Status {
        configured: !c.app_id.is_empty(),
        enabled: c.enabled,
        app_id: c.app_id,
        agent: c.agent,
        cwd: c.cwd,
        allow_group: c.allow_group,
        link,
    }
}

/// 让渠道的运行状态和配置、安全通道一致：已启用且安全通道就绪就连上，否则断开。由 Loov 的看门狗定期调用，
/// 改配置后也立即调用一次。
pub async fn ensure(state: &AppState) {
    import_dev_creds();
    let want = load_config().filter(|c| c.enabled && matches!(crate::loov::status(), crate::loov::Status::Ready { .. }));
    let mut g = running().lock().await;
    match (&*g, want) {
        (Some(r), Some(c)) if r.app_id == c.app_id => {}
        (_, Some(c)) => {
            if let Some(r) = g.take() {
                let _ = r.stop.send(true);
            }
            match start(state, &c) {
                Ok(r) => *g = Some(r),
                Err(e) => log::error!("[loov] 启动飞书渠道失败：{e}"),
            }
        }
        (Some(_), None) => {
            if let Some(r) = g.take() {
                let _ = r.stop.send(true);
            }
        }
        (None, None) => {}
    }
}

fn start(state: &AppState, c: &Config) -> Result<Running, String> {
    let client = Arc::new(api::Client::new(&c.app_id, &secret_of(c)?));
    let (stop_tx, stop_rx) = watch::channel(false);
    let (state_tx, state_rx) = watch::channel(link::LinkState::Connecting);
    let (msg_tx, msg_rx) = mpsc::channel::<link::Incoming>(64);
    tokio::spawn(link::run(client.clone(), msg_tx, stop_rx.clone(), state_tx));
    tokio::spawn(dispatch(state.clone(), client, msg_rx, stop_rx));
    Ok(Running { app_id: c.app_id.clone(), stop: stop_tx, state: state_rx })
}

/// 每个飞书会话一个串行队列：同一会话一次只跑一轮，别的会话不受影响。
async fn dispatch(state: AppState, client: Arc<api::Client>, mut rx: mpsc::Receiver<link::Incoming>, stop: watch::Receiver<bool>) {
    let mut queues: HashMap<String, mpsc::Sender<link::Incoming>> = HashMap::new();
    let stops: Arc<Mutex<HashMap<String, watch::Sender<bool>>>> = Arc::default();
    while let Some(m) = rx.recv().await {
        let Some(cfg) = load_config() else { continue };
        if m.sender_open_id != cfg.owner_open_id {
            let _ = client.reply_text(&m.message_id, "这个机器人只给创建它的人用。").await;
            continue;
        }
        if m.chat_type != "p2p" && !(cfg.allow_group && m.mentioned) {
            continue;
        }
        // /stop 不排队：直接结束这个会话正在跑的那一轮
        if m.text.trim() == "/stop" {
            let stopped = stops.lock().await.get(&m.chat_id).map(|s| s.send(true).is_ok()).unwrap_or(false);
            let _ = client.reply_text(&m.message_id, if stopped { "已停止当前任务。" } else { "现在没有在跑的任务。" }).await;
            continue;
        }
        let tx = queues.entry(m.chat_id.clone()).or_insert_with(|| {
            let (tx, mut q) = mpsc::channel::<link::Incoming>(16);
            let (state, client, stops, stop) = (state.clone(), client.clone(), stops.clone(), stop.clone());
            tokio::spawn(async move {
                while let Some(m) = q.recv().await {
                    if *stop.borrow() {
                        break;
                    }
                    handle(&state, &client, &stops, m).await;
                }
            });
            tx
        });
        let _ = tx.send(m).await;
    }
}

/// 一个飞书会话的状态：用哪个 agent、在哪个目录、接着哪个会话。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Session {
    agent: String,
    cwd: String,
    session: Option<String>,
}

fn load_sessions() -> HashMap<String, Session> {
    std::fs::read(sessions_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_session(chat_id: &str, s: &Session) {
    let mut all = load_sessions();
    all.insert(chat_id.to_string(), s.clone());
    let _ = write_private(&sessions_path(), serde_json::to_string_pretty(&all).unwrap_or_default().as_bytes());
}

const HELP: &str = "直接发消息就是在用 agent。可用命令：\n/new 开新会话\n/agent claude|codex|agy|grok 换 agent\n/cwd <路径> 换工作目录\n/stop 停止当前任务\n/status 当前设置";

fn agent_name(id: &str) -> &'static str {
    match id {
        "codex" => "Codex",
        "agy" => "Antigravity",
        "grok" => "Grok Build",
        _ => "Claude Code",
    }
}

async fn handle(state: &AppState, client: &api::Client, stops: &Arc<Mutex<HashMap<String, watch::Sender<bool>>>>, m: link::Incoming) {
    let cfg = load_config().unwrap_or_default();
    let mut s = load_sessions().remove(&m.chat_id).unwrap_or_default();
    if s.agent.is_empty() {
        s.agent = if cfg.agent.is_empty() { "claude".into() } else { cfg.agent.clone() };
    }
    if s.cwd.is_empty() {
        s.cwd = if cfg.cwd.is_empty() { crate::config::get_home_dir().to_string_lossy().into_owned() } else { cfg.cwd.clone() };
    }
    let text = m.text.trim();
    let message_id = m.message_id.clone();
    let reply = |t: String| {
        let message_id = message_id.clone();
        async move {
            let _ = client.reply_text(&message_id, &t).await;
        }
    };
    // 会话命令
    if let Some(rest) = text.strip_prefix('/') {
        let (cmd, arg) = rest.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
        match cmd {
            "new" => {
                s.session = None;
                save_session(&m.chat_id, &s);
                return reply(format!("已开新会话（{}，目录 {}）。", agent_name(&s.agent), s.cwd)).await;
            }
            "agent" if runner::AGENTS.contains(&arg) => {
                s.agent = arg.to_string();
                s.session = None;
                save_session(&m.chat_id, &s);
                return reply(format!("已换成 {}，开了新会话。", agent_name(arg))).await;
            }
            "cwd" if !arg.is_empty() => {
                let path = PathBuf::from(arg);
                if !path.is_dir() {
                    return reply(format!("目录不存在：{arg}")).await;
                }
                s.cwd = arg.to_string();
                s.session = None;
                save_session(&m.chat_id, &s);
                return reply(format!("工作目录换成 {arg}，开了新会话。")).await;
            }
            "status" => {
                return reply(format!("agent：{}\n目录：{}\n会话：{}", agent_name(&s.agent), s.cwd, if s.session.is_some() { "接着上次" } else { "新会话" })).await;
            }
            "help" => return reply(HELP.to_string()).await,
            _ => {}
        }
    }
    if text.is_empty() {
        return reply("暂时只支持文字消息。".to_string()).await;
    }

    let title = format!("{} · 处理中…", agent_name(&s.agent));
    let card = match client.create_stream_card(&title, "…").await {
        Ok(id) if client.reply_card(&m.message_id, &id).await.is_ok() => Some(id),
        _ => None,
    };
    let (stop_tx, stop_rx) = watch::channel(false);
    stops.lock().await.insert(m.chat_id.clone(), stop_tx);
    let mut streamer = Streamer::new(card.clone());
    let safety = crate::loov::netmode::load_prefs().safety;
    let req = runner::RunRequest { agent: &s.agent, prompt: text, cwd: &s.cwd, session: s.session.as_deref(), safety, codex_provider: None };
    // 回调里只记下最新全文，由定时器节流发往飞书（卡片每秒最多 10 次操作，这里 400ms 一次）
    let pending: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
    let pending_cb = pending.clone();
    let result = {
        let run = runner::run(state, req, move |t| *pending_cb.lock().unwrap() = Some(t.to_string()), stop_rx);
        tokio::pin!(run);
        let mut tick = tokio::time::interval(Duration::from_millis(400));
        loop {
            tokio::select! {
                r = &mut run => break r,
                _ = tick.tick() => {
                    let latest = pending.lock().unwrap().take();
                    if let Some(t) = latest {
                        streamer.push(client, &t).await;
                    }
                }
            }
        }
    };
    stops.lock().await.remove(&m.chat_id);
    let final_text = match result {
        Ok(out) => {
            if out.session.is_some() {
                s.session = out.session;
            }
            save_session(&m.chat_id, &s);
            if out.text.trim().is_empty() { "（没有输出）".to_string() } else { out.text }
        }
        Err(e) => format!("出错了：{e}"),
    };
    if streamer.finish(client, &final_text).await.is_err() || card.is_none() {
        let _ = client.reply_text(&m.message_id, &truncate_tail(&final_text, 20_000)).await;
    }
}

/// 卡片正文最多约 30KB：超出时保留结尾（最新的输出），前面标省略。
fn truncate_tail(text: &str, max_chars: usize) -> String {
    let n = text.chars().count();
    if n <= max_chars {
        return text.to_string();
    }
    let tail: String = text.chars().skip(n - max_chars).collect();
    format!("…（前面省略 {} 字）\n{tail}", n - max_chars)
}

/// 把回复流式写进卡片：sequence 严格递增；流式模式 10 分钟后会自动关，9 分钟时续开。
struct Streamer {
    card: Option<String>,
    seq: u64,
    opened: Instant,
    last: String,
}

impl Streamer {
    fn new(card: Option<String>) -> Self {
        Streamer { card, seq: 0, opened: Instant::now(), last: String::new() }
    }

    async fn push(&mut self, client: &api::Client, text: &str) {
        let Some(card) = self.card.clone() else { return };
        let body = truncate_tail(text, 9_000);
        if body == self.last {
            return;
        }
        if self.opened.elapsed() > Duration::from_secs(9 * 60) {
            self.seq += 1;
            let _ = client.set_streaming(&card, true, self.seq).await;
            self.opened = Instant::now();
        }
        self.seq += 1;
        if client.stream_text(&card, &body, self.seq).await.is_ok() {
            self.last = body;
        }
    }

    async fn finish(&mut self, client: &api::Client, text: &str) -> Result<(), String> {
        let Some(card) = self.card.clone() else { return Err("没有卡片".into()) };
        self.seq += 1;
        client.stream_text(&card, &truncate_tail(text, 9_000), self.seq).await?;
        self.seq += 1;
        client.set_streaming(&card, false, self.seq).await
    }
}

/// 仅调试 / 测试构建：`LOOV_FEISHU_IMPORT` 指向一个 {app_id, app_secret, owner_open_id} 文件时，
/// 本机还没有配置就直接导入（联调用，免得每次扫码）。发布构建里 `dev_env_override` 恒为 None。
fn import_dev_creds() {
    let Some(path) = crate::loov::dev_env_override("LOOV_FEISHU_IMPORT") else { return };
    if load_config().is_some() {
        return;
    }
    let Ok(v) = std::fs::read(&path).map_err(|_| ()).and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).map_err(|_| ())) else {
        return;
    };
    let (Some(id), Some(secret)) = (v["app_id"].as_str(), v["app_secret"].as_str()) else { return };
    let c = Config {
        app_id: id.into(),
        secret_enc: base64::engine::general_purpose::STANDARD.encode(crate::loov::credentials::seal(secret.as_bytes())),
        owner_open_id: v["owner_open_id"].as_str().unwrap_or_default().into(),
        brand: "feishu".into(),
        agent: "claude".into(),
        enabled: true,
        ..Default::default()
    };
    let _ = save_config(&c);
}

// ───────────────────────────── 配置流程 ─────────────────────────────

/// 开始扫码：返回链接与二维码（SVG）。
pub async fn begin() -> Result<(api::Registration, String), String> {
    let reg = api::begin_registration().await?;
    Ok((reg.clone(), qr_svg(&reg.url)))
}

pub fn qr_svg(url: &str) -> String {
    qrcode::QrCode::new(url.as_bytes())
        .map(|c| c.render::<qrcode::render::svg::Color>().min_dimensions(200, 200).quiet_zone(true).build())
        .unwrap_or_default()
}

/// 终端里显示的二维码（服务器模式）。
pub fn qr_text(url: &str) -> String {
    qrcode::QrCode::new(url.as_bytes())
        .map(|c| c.render::<char>().quiet_zone(true).module_dimensions(2, 1).light_color(' ').dark_color('█').build())
        .unwrap_or_default()
}

/// 轮询一次扫码结果：Some(true) 已完成并保存，Some(false) 还在等，Err 失败。
pub async fn poll(state: &AppState, device_code: &str) -> Result<bool, String> {
    match api::poll_registration(device_code).await? {
        api::PollResult::Pending | api::PollResult::SlowDown => Ok(false),
        api::PollResult::Done(creds) => {
            let old = load_config().unwrap_or_default();
            let secret_enc = base64::engine::general_purpose::STANDARD.encode(crate::loov::credentials::seal(creds.app_secret.as_bytes()));
            let c = Config {
                app_id: creds.app_id,
                secret_enc,
                owner_open_id: creds.owner_open_id.unwrap_or_default(),
                brand: creds.brand,
                agent: if old.agent.is_empty() { "claude".into() } else { old.agent },
                cwd: old.cwd,
                allow_group: false,
                enabled: true,
            };
            save_config(&c)?;
            ensure(state).await;
            Ok(true)
        }
    }
}

/// 改设置（agent、目录、群聊、启用）。
pub async fn configure(state: &AppState, agent: Option<String>, cwd: Option<String>, allow_group: Option<bool>, enabled: Option<bool>) -> Result<Status, String> {
    let mut c = load_config().ok_or("还没有连接飞书")?;
    if let Some(a) = agent {
        if !runner::AGENTS.contains(&a.as_str()) {
            return Err(format!("不支持的 agent：{a}"));
        }
        c.agent = a;
    }
    if let Some(d) = cwd {
        if !d.is_empty() && !std::path::Path::new(&d).is_dir() {
            return Err(format!("目录不存在：{d}"));
        }
        c.cwd = d;
    }
    if let Some(g) = allow_group {
        c.allow_group = g;
    }
    if let Some(e) = enabled {
        c.enabled = e;
    }
    save_config(&c)?;
    ensure(state).await;
    Ok(status().await)
}

/// 断开并删除本机配置（飞书里的应用保留，员工可在飞书开放平台「我的应用」里删除）。
pub async fn disconnect(state: &AppState) -> Result<(), String> {
    let _ = std::fs::remove_file(config_path());
    let _ = std::fs::remove_file(sessions_path());
    ensure(state).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_tail_when_too_long() {
        assert_eq!(truncate_tail("abc", 5), "abc");
        let t = truncate_tail("0123456789", 4);
        assert!(t.ends_with("6789") && t.contains("省略 6 字"), "{t}");
    }

    #[test]
    fn renders_qr_codes() {
        assert!(qr_svg("https://open.feishu.cn/page/cli?user_code=ABCD-1234").starts_with("<?xml"));
        assert!(qr_text("https://open.feishu.cn/page/cli?user_code=ABCD-1234").contains('█'));
    }
}
