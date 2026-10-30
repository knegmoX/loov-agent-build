//! 渠道里跑 agent：只用各家官方的无头接口，不包装、不改它们的行为。
//!
//! | agent | 调用 | 会话延续 |
//! |---|---|---|
//! | Claude Code | `claude -p <提示> --output-format stream-json --verbose --permission-mode <档位>` | `--resume <session_id>` |
//! | Codex | `codex exec --json --skip-git-repo-check -C <目录> <提示>` | `codex exec resume <thread_id> <提示>` |
//! | Antigravity | `agy <私有 profile 参数> -p <提示>` | 无 |
//! | Grok Build | `grok -p <提示>` | 无 |
//!
//! 命令、环境（隧道代理、私有 profile）取自 `apps::cli_spec`，和启动台、服务器模式同一份；请求照样经本地
//! 路由、new-api 计费。Claude Code 的请求带 `x-loov-app: feishu`，计费记在「飞书渠道」名下。
//! 权限按 Loov 的 agent 权限档位（无头模式下要问人的操作一律拒绝，「完全」档才放行）。

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;

use crate::loov::safety::SafetyLevel;
use crate::store::AppState;

/// 渠道能用的 agent（应用 id 与启动台一致）。
pub const AGENTS: &[&str] = &["claude", "codex", "agy", "grok"];

pub struct RunRequest<'a> {
    pub agent: &'a str,
    pub prompt: &'a str,
    pub cwd: &'a str,
    /// 接着上次的会话（Claude 的 session_id / Codex 的 thread_id）
    pub session: Option<&'a str>,
    pub safety: SafetyLevel,
    /// Codex 当前用的 model_provider（来自 ~/.codex/config.toml），用来给它的请求加来源头
    pub codex_provider: Option<String>,
}

pub struct RunOutcome {
    pub text: String,
    pub session: Option<String>,
}

/// 单次运行的上限：超过就结束进程，把已有的输出交回去。
const MAX_RUN: Duration = Duration::from_secs(30 * 60);

/// 跑一轮。`on_text` 收到目前为止的完整回复（流式）；`stop` 变为 true 时结束进程。
pub async fn run(
    state: &AppState,
    req: RunRequest<'_>,
    mut on_text: impl FnMut(&str),
    mut stop: watch::Receiver<bool>,
) -> Result<RunOutcome, String> {
    if !AGENTS.contains(&req.agent) {
        return Err(format!("不支持的 agent：{}", req.agent));
    }
    let spec = crate::loov::apps::cli_spec(state, req.agent).await.map_err(|e| e.to_string())?;
    let mut req = req;
    if req.agent == "codex" && req.codex_provider.is_none() {
        req.codex_provider = codex_provider();
    }
    let args = args_for(&req, spec.args.clone());
    let mut cmd = tokio::process::Command::new(&spec.program);
    cmd.args(&args)
        .current_dir(req.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in &spec.env {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    if req.agent == "claude" {
        cmd.env("ANTHROPIC_CUSTOM_HEADERS", format!("{}: feishu", crate::loov::attribution::APP_HEADER));
    }
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("启动 {} 失败：{e}（没装的话先在 Loov 里安装）", spec.program))?;
    let stdout = child.stdout.take().ok_or("拿不到输出")?;
    let stderr = child.stderr.take().ok_or("拿不到错误输出")?;
    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tail: Vec<String> = Vec::new();
        while let Ok(Some(l)) = lines.next_line().await {
            tail.push(l);
            if tail.len() > 20 {
                tail.remove(0);
            }
        }
        tail.join("\n")
    });

    let mut parser = Parser::new(req.agent);
    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::sleep(MAX_RUN);
    tokio::pin!(deadline);
    let mut stopped = false;
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    if parser.feed(&l) {
                        on_text(&parser.text);
                    }
                }
                _ => break,
            },
            _ = stop.changed() => { stopped = true; break; }
            _ = &mut deadline => { stopped = true; break; }
        }
    }
    if stopped {
        let _ = child.start_kill();
    }
    let status = child.wait().await.ok();
    let err_tail = err_task.await.unwrap_or_default();
    if let Some(e) = parser.error.take() {
        return Err(e);
    }
    if !stopped && status.is_some_and(|s| !s.success()) && parser.text.trim().is_empty() {
        return Err(if err_tail.trim().is_empty() { format!("{} 异常退出", spec.program) } else { err_tail });
    }
    Ok(RunOutcome { text: parser.text, session: parser.session })
}

/// Codex 配置里当前的 model_provider（Loov 写的路由供应商）。
fn codex_provider() -> Option<String> {
    let text = std::fs::read_to_string(crate::codex_config::get_codex_config_path()).ok()?;
    let doc: toml::Value = text.parse().ok()?;
    doc.get("model_provider")?.as_str().map(str::to_string)
}

/// 各家的命令行参数（`base` 是 cli_spec 给的前置参数，如 agy 的私有 profile）。
pub fn args_for(req: &RunRequest<'_>, base: Vec<String>) -> Vec<String> {
    let mut a = base;
    match req.agent {
        "claude" => {
            let mode = match req.safety {
                SafetyLevel::Careful => "default",
                SafetyLevel::Standard => "acceptEdits",
                SafetyLevel::Full => "bypassPermissions",
            };
            a.extend(["-p".into(), req.prompt.into(), "--output-format".into(), "stream-json".into(), "--verbose".into()]);
            a.extend(["--permission-mode".into(), mode.into()]);
            if let Some(s) = req.session {
                a.extend(["--resume".into(), s.into()]);
            }
        }
        "codex" => {
            // `codex exec [resume] [选项] [会话 id] [提示]`；resume 不认 --sandbox，统一用 -c 覆盖 sandbox_mode
            a.push("exec".into());
            if req.session.is_some() {
                a.push("resume".into());
            }
            a.extend(["--json".into(), "--skip-git-repo-check".into()]);
            // 请求带上来源，计费记在「飞书渠道」名下（loov::attribution）
            if let Some(p) = &req.codex_provider {
                a.extend(["-c".into(), format!("model_providers.{p}.http_headers={{ \"{}\" = \"feishu\" }}", crate::loov::attribution::APP_HEADER)]);
            }
            match req.safety {
                SafetyLevel::Careful => a.extend(["-c".into(), "sandbox_mode=\"read-only\"".into()]),
                SafetyLevel::Standard => a.extend(["-c".into(), "sandbox_mode=\"workspace-write\"".into()]),
                SafetyLevel::Full => a.push("--dangerously-bypass-approvals-and-sandbox".into()),
            }
            if let Some(s) = req.session {
                a.push(s.into());
            }
            a.push(req.prompt.into());
        }
        _ => {
            // agy / grok：打印模式，输出就是回复
            a.extend(["-p".into(), req.prompt.into()]);
        }
    }
    a
}

/// 解析各家的输出，攒出回复全文与会话 id。
pub struct Parser {
    agent: String,
    pub text: String,
    pub session: Option<String>,
    pub error: Option<String>,
    /// Claude 每条 assistant 消息独立，回复是它们拼起来的
    claude_parts: Vec<String>,
}

impl Parser {
    pub fn new(agent: &str) -> Self {
        Parser { agent: agent.into(), text: String::new(), session: None, error: None, claude_parts: Vec::new() }
    }

    /// 喂一行输出；回复变了返回 true。
    pub fn feed(&mut self, line: &str) -> bool {
        match self.agent.as_str() {
            "claude" => self.feed_claude(line),
            "codex" => self.feed_codex(line),
            _ => {
                if !self.text.is_empty() {
                    self.text.push('\n');
                }
                self.text.push_str(line);
                true
            }
        }
    }

    fn feed_claude(&mut self, line: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return false };
        if let Some(id) = v["session_id"].as_str() {
            self.session = Some(id.to_string());
        }
        match v["type"].as_str() {
            Some("assistant") => {
                let text: String = v["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["type"] == "text")
                    .filter_map(|c| c["text"].as_str())
                    .collect();
                if text.is_empty() {
                    return false;
                }
                self.claude_parts.push(text);
                self.text = self.claude_parts.join("\n\n");
                true
            }
            Some("result") => {
                if v["is_error"].as_bool() == Some(true) {
                    self.error = Some(v["result"].as_str().unwrap_or("Claude Code 出错").to_string());
                    return false;
                }
                match v["result"].as_str() {
                    Some(r) if !r.is_empty() && self.text.is_empty() => {
                        self.text = r.to_string();
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn feed_codex(&mut self, line: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return false };
        match v["type"].as_str() {
            Some("thread.started") => {
                self.session = v["thread_id"].as_str().map(str::to_string);
                false
            }
            Some("item.completed") if v["item"]["type"] == "agent_message" => {
                let t = v["item"]["text"].as_str().unwrap_or_default();
                if t.is_empty() {
                    return false;
                }
                if !self.text.is_empty() {
                    self.text.push_str("\n\n");
                }
                self.text.push_str(t);
                true
            }
            Some("error") | Some("turn.failed") => {
                let msg = v["message"].as_str().or_else(|| v["error"]["message"].as_str()).unwrap_or("Codex 出错");
                self.error = Some(msg.to_string());
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req<'a>(agent: &'a str, session: Option<&'a str>, safety: SafetyLevel) -> RunRequest<'a> {
        RunRequest { agent, prompt: "hi", cwd: "/tmp", session, safety, codex_provider: Some("custom".into()) }
    }

    #[test]
    fn builds_official_headless_args() {
        let c = args_for(&req("claude", Some("s1"), SafetyLevel::Standard), vec![]);
        assert_eq!(c, ["-p", "hi", "--output-format", "stream-json", "--verbose", "--permission-mode", "acceptEdits", "--resume", "s1"]);
        let x = args_for(&req("codex", Some("t1"), SafetyLevel::Careful), vec![]);
        assert_eq!(
            x,
            [
                "exec", "resume", "--json", "--skip-git-repo-check",
                "-c", "model_providers.custom.http_headers={ \"x-loov-app\" = \"feishu\" }",
                "-c", "sandbox_mode=\"read-only\"", "t1", "hi"
            ]
        );
        let g = args_for(&req("agy", None, SafetyLevel::Full), vec!["--gemini_dir=/x".into()]);
        assert_eq!(g, ["--gemini_dir=/x", "-p", "hi"]);
    }

    #[test]
    fn parses_claude_stream_json() {
        let mut p = Parser::new("claude");
        assert!(!p.feed(r#"{"type":"system","subtype":"init","session_id":"abc"}"#));
        assert!(p.feed(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"你好"}]}}"#));
        assert!(!p.feed(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read"}]}}"#));
        assert!(p.feed(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"完成"}]}}"#));
        assert!(!p.feed(r#"{"type":"result","result":"完成","session_id":"abc","is_error":false}"#));
        assert_eq!((p.text.as_str(), p.session.as_deref()), ("你好\n\n完成", Some("abc")));
        let mut e = Parser::new("claude");
        e.feed(r#"{"type":"result","result":"额度不足","is_error":true}"#);
        assert_eq!(e.error.as_deref(), Some("额度不足"));
    }

    #[test]
    fn parses_codex_json_events() {
        let mut p = Parser::new("codex");
        p.feed(r#"{"type":"thread.started","thread_id":"t-9"}"#);
        assert!(p.feed(r#"{"type":"item.completed","item":{"type":"agent_message","text":"OK"}}"#));
        assert!(!p.feed(r#"{"type":"item.completed","item":{"type":"command_execution","command":"ls"}}"#));
        assert_eq!((p.text.as_str(), p.session.as_deref()), ("OK", Some("t-9")));
    }
}
