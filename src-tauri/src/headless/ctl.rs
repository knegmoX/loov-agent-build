//! 本机控制套接字：命令行子命令和常驻进程之间的协议。一条连接一个请求，按行 JSON：
//! 请求一行 `{"cmd": "...", ...}`；响应若干行，安装进度是 `{"progress": {...}}`，最后一行是
//! `{"ok": true, "data": ...}` 或 `{"ok": false, "error": {"code", "message"}}`。
//! 套接字文件只有属主能读写（0600），同一用户的进程才能控制 Loov。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::loov::flows::{self, LoovError};
use crate::store::AppState;

pub fn socket_path() -> PathBuf {
    crate::loov::credentials::loov_dir().join("ctl.sock")
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    Status,
    Login { username: String, password: String, evict_oldest: bool },
    Register { invite: String, username: String, password: String, evict_oldest: bool },
    Logout,
    Refresh,
    Node { id: Option<String> },
    Devices,
    Usage { range: String },
    Restore,
    Apps,
    Install { app: String },
    Spec { app: String },
    Safety { level: crate::loov::safety::SafetyLevel },
}

/// 常驻进程：监听套接字，逐个连接处理。
pub async fn serve(state: AppState) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 上次异常退出留下的套接字文件（core.lock 已保证没有别的 serve 在跑）
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    log::info!("[loov] 控制套接字：{}", path.display());
    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, state).await {
                log::warn!("[loov] 控制连接出错：{e}");
            }
        });
    }
}

async fn handle(stream: tokio::net::UnixStream, state: AppState) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let Some(line) = lines.next_line().await? else { return Ok(()) };
    let reply = match serde_json::from_str::<Request>(&line) {
        Err(e) => json!({ "ok": false, "error": { "code": "BAD_REQUEST", "message": e.to_string() } }),
        Ok(req) => {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
            let task = tokio::spawn(dispatch(req, state, tx));
            // 安装进度边跑边转发
            while let Some(p) = rx.recv().await {
                write.write_all(format!("{}\n", json!({ "progress": p })).as_bytes()).await?;
            }
            match task.await {
                Ok(Ok(data)) => json!({ "ok": true, "data": data }),
                Ok(Err(e)) => json!({ "ok": false, "error": e }),
                Err(e) => json!({ "ok": false, "error": { "code": "INTERNAL", "message": e.to_string() } }),
            }
        }
    };
    write.write_all(format!("{reply}\n").as_bytes()).await?;
    write.shutdown().await
}

fn to_value<T: Serialize>(v: T) -> Result<Value, LoovError> {
    serde_json::to_value(v).map_err(|e| LoovError::Internal { message: e.to_string() })
}

async fn dispatch(req: Request, state: AppState, progress: tokio::sync::mpsc::UnboundedSender<Value>) -> Result<Value, LoovError> {
    match req {
        Request::Status => to_value(crate::loov::status().redacted()),
        Request::Login { username, password, evict_oldest } => {
            to_value(flows::login(&state, &username, &password, evict_oldest).await?.redacted())
        }
        Request::Register { invite, username, password, evict_oldest } => {
            to_value(flows::register(&state, &invite, &username, &password, evict_oldest).await?.redacted())
        }
        Request::Logout => to_value(flows::logout(&state).await?),
        Request::Refresh => to_value(flows::refresh(&state).await?.redacted()),
        Request::Node { id } => to_value(flows::set_selection(&state, Some(id), None).await?.redacted()),
        Request::Devices => to_value(flows::devices().await?),
        Request::Usage { range } => to_value(flows::usage(&range).await?),
        Request::Restore => to_value(crate::loov::restore::restore_original(&state).await.map_err(LoovError::from)?),
        Request::Apps => to_value(crate::loov::apps::status(&state).await),
        Request::Install { app } => {
            let sink: crate::loov::apps::Progress = std::sync::Arc::new(move |p| {
                let _ = progress.send(serde_json::to_value(p).unwrap_or(Value::Null));
            });
            to_value(crate::loov::apps::install(sink, &app).await?)
        }
        Request::Spec { app } => to_value(crate::loov::apps::cli_spec(&state, &app).await?),
        Request::Safety { level } => to_value(flows::set_safety(&state, level)?),
    }
}

/// 命令行这一侧：发一个请求，逐行读回。`on_progress` 处理安装进度；返回最后一行的 data，出错返回 (code, message)。
pub fn call(req: &Request, mut on_progress: impl FnMut(&Value)) -> Result<Value, (String, String)> {
    use std::io::{BufRead, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path()).map_err(|_| {
        (
            "NOT_RUNNING".to_string(),
            "Loov 服务没有运行。先启动：loov-agent serve（或 systemctl --user start loov-agent）".to_string(),
        )
    })?;
    let line = serde_json::to_string(req).map_err(|e| ("INTERNAL".to_string(), e.to_string()))?;
    stream
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|e| ("IO".to_string(), e.to_string()))?;
    for line in std::io::BufReader::new(stream).lines() {
        let line = line.map_err(|e| ("IO".to_string(), e.to_string()))?;
        let v: Value = serde_json::from_str(&line).map_err(|e| ("BAD_RESPONSE".to_string(), e.to_string()))?;
        if let Some(p) = v.get("progress") {
            on_progress(p);
            continue;
        }
        if v["ok"].as_bool() == Some(true) {
            return Ok(v["data"].clone());
        }
        let code = v["error"]["code"].as_str().unwrap_or("ERROR").to_string();
        let message = v["error"]["message"].as_str().unwrap_or("未知错误").to_string();
        return Err((code, message));
    }
    Err(("BAD_RESPONSE".to_string(), "服务没有返回结果".to_string()))
}
