//! Loov Agent 端到端：真实 hub + 东京隧道 + Claude Code / Codex 一轮对话 + strace 零直连。
//!
//! 运行：
//! ```text
//! LOOV_E2E=1 LOOV_HUB_USER=… LOOV_HUB_PASS=… LOOV_XRAY_BIN=/path/xray \
//!   cargo test --features test-hooks --test loov_e2e -- --nocapture --test-threads=1
//! ```
//! 没有 `LOOV_E2E=1` 时所有用例直接返回（CI 安全）。嵌套运行 claude 必须 `env -i`，否则会继承
//! 本机的 `CLAUDE_CODE_USE_VERTEX` 去打 GCP metadata。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use cc_switch_lib::loov;
use cc_switch_lib::loov::commands::*;
use cc_switch_lib::loov::Status;

#[path = "support.rs"]
mod support;
use support::{create_test_state, ensure_test_home, reset_test_fs, test_mutex};

fn gated() -> bool {
    if std::env::var("LOOV_E2E").as_deref() != Ok("1") {
        return false;
    }
    // 应用在 Tauri setup 里装 rustls 的 ring provider（lib.rs），测试进程要自己装
    let _ = rustls::crypto::ring::default_provider().install_default();
    true
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing env {name}"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// 测试失败（panic）时也要吊销设备、停 sidecar。
struct RevokeGuard<'a> {
    state: &'a cc_switch_lib::AppState,
    done: bool,
}

impl Drop for RevokeGuard<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let state = self.state.clone();
        let handle = tokio::runtime::Handle::current();
        std::thread::scope(|s| {
            s.spawn(|| {
                handle.block_on(async {
                    let _ = loov_logout_test_hook(&state).await;
                });
            });
        });
    }
}

/// strace 输出里所有 IPv4/IPv6 的 connect 目标，排除 AF_UNIX。
fn connect_targets(trace: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in trace.lines() {
        if !line.contains("connect(") || line.contains("AF_UNIX") {
            continue;
        }
        if let Some(i) = line.find("inet_addr(\"") {
            let rest = &line[i + 11..];
            if let Some(j) = rest.find('"') {
                out.push(rest[..j].to_string());
                continue;
            }
        }
        if let Some(i) = line.find("inet_pton(AF_INET6, \"") {
            let rest = &line[i + 21..];
            if let Some(j) = rest.find('"') {
                out.push(format!("[{}]", &rest[..j]));
            }
        }
    }
    out
}

fn assert_only_loopback(trace: &str, label: &str) {
    let targets = connect_targets(trace);
    assert!(!targets.is_empty(), "{label}: strace 里没有任何 connect，命令可能没跑起来");
    let bad: Vec<&String> = targets
        .iter()
        .filter(|t| !(t.as_str() == "127.0.0.1" || t.as_str() == "[::1]" || t.starts_with("127.")))
        .collect();
    assert!(bad.is_empty(), "{label}: 发现直连目标 {bad:?}");
}

fn clean_env_cmd(home: &Path, program: &str, extra_env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new("env");
    cmd.arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg(format!("PATH={}", std::env::var("PATH").unwrap_or_default()))
        .arg("TERM=dumb")
        .arg("LANG=C.UTF-8")
        .arg("NO_COLOR=1");
    for (k, v) in extra_env {
        cmd.arg(format!("{k}={v}"));
    }
    cmd.arg(program);
    cmd
}

fn run_traced(home: &Path, trace: &Path, program: &str, args: &[&str], timeout: Duration) -> (bool, String, String) {
    run_traced_env(home, trace, program, args, timeout, &[])
}

fn run_traced_env(home: &Path, trace: &Path, program: &str, args: &[&str], timeout: Duration, extra_env: &[(&str, &str)]) -> (bool, String, String) {
    let mut cmd = clean_env_cmd(home, "strace", extra_env);
    cmd.args(["-f", "-e", "trace=connect", "-o"])
        .arg(trace)
        .arg(program)
        .args(args)
        .current_dir(home);
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
        .expect("spawn strace");
    let start = std::time::Instant::now();
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let out = child.wait_with_output().expect("wait");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn router_port(state: &cc_switch_lib::AppState) -> u16 {
    state.db.get_proxy_client_endpoint_sync().1
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_flow_login_tunnel_agents_logout() {
    if !gated() {
        eprintln!("LOOV_E2E 未设置，跳过");
        return;
    }
    let _guard = test_mutex().lock().unwrap_or_else(|e| e.into_inner());
    reset_test_fs();
    let home: PathBuf = ensure_test_home().to_path_buf();
    std::fs::write(home.join(".claude.json"), r#"{"hasCompletedOnboarding":true,"theme":"dark"}"#).unwrap();
    // 模拟一台已经在用 Vertex 版 Claude Code、Codex 直连 new-api 的机器（2026-10-23 真机验收踩到的场景）
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let original_settings = serde_json::json!({
        "env": {"CLAUDE_CODE_USE_VERTEX": "1", "ANTHROPIC_VERTEX_PROJECT_ID": "proj-x", "CLOUD_ML_REGION": "global", "MY_FLAG": "1"},
        "model": "claude-fable-5-1",
        "permissions": {"allow": ["Read"]}
    });
    std::fs::write(home.join(".claude").join("settings.json"), serde_json::to_string_pretty(&original_settings).unwrap()).unwrap();
    let original_codex = "# mine\nmodel_provider = \"newapi\"\nmodel = \"claude-opus-5-5\"\n\n[model_providers.newapi]\nname = \"new-api\"\nbase_url = \"http://127.0.0.1:3001/v1\"\nwire_api = \"responses\"\nenv_key = \"NEW_API_KEY\"\n";
    std::fs::write(home.join(".codex").join("config.toml"), original_codex).unwrap();

    let state = create_test_state().expect("state");
    let user = env("LOOV_HUB_USER");
    let pass = env("LOOV_HUB_PASS");

    // 1. 登录：建设备 → 起隧道 → 自检 → 写供应商 → 接上路由 → 加固键
    let status = loov_login_test_hook(&state, &user, &pass, true)
        .await
        .unwrap_or_else(|e| panic!("login failed: {e}"));
    let mut guard = RevokeGuard { state: &state, done: false };
    let (proxy_url, http_port) = match &status {
        Status::Ready { proxy_url, http_port, username, route, via, nodes, .. } => {
            assert_eq!(username, &user);
            eprintln!("隧道：节点 {nodes:?}，走法 {route:?}，代理 {via:?}");
            // 模拟被屏蔽的网络时用 LOOV_E2E_EXPECT_ROUTE=proxy 断言确实改走了本机代理
            if let Ok(want) = std::env::var("LOOV_E2E_EXPECT_ROUTE") {
                assert_eq!(format!("{route:?}").to_lowercase(), want, "隧道走法不符");
            }
            (proxy_url.clone(), *http_port)
        }
        other => panic!("expected Ready, got {other:?}"),
    };
    assert!(loov::credentials::credentials_path().exists());
    assert!(loov::api_key().unwrap().starts_with("sk-"));
    // 按应用计费：hub 为每个应用单独建了 Key，互不相同，也不同于通用 Key；都不落盘
    let general = loov::api_key().unwrap();
    let per_app: Vec<String> = loov::attribution::APPS.iter().map(|a| loov::api_key_for(Some(a)).unwrap()).collect();
    assert!(per_app.iter().all(|k| k.starts_with("sk-") && *k != general), "应用 Key 应各自独立");
    assert_eq!(per_app.iter().collect::<std::collections::HashSet<_>>().len(), per_app.len());
    let cred_bytes = std::fs::read(loov::credentials::credentials_path()).unwrap();
    assert!(!per_app.iter().any(|k| cred_bytes.windows(k.len()).any(|w| w == k.as_bytes())), "应用 Key 不得落盘");

    // 2. 文件断言
    let port = router_port(&state);
    let settings_path = home.join(".claude").join("settings.json");
    let settings: serde_json::Value = serde_json::from_str(&read(&settings_path)).unwrap();
    let env_block = &settings["env"];
    assert_eq!(env_block["ANTHROPIC_BASE_URL"], format!("http://127.0.0.1:{port}"));
    let lat = env_block["ANTHROPIC_AUTH_TOKEN"].as_str().unwrap().to_string();
    assert!(lat.starts_with("lat_"), "settings.json 里应是本地令牌，实际 {lat}");
    assert_eq!(env_block["HTTPS_PROXY"], proxy_url);
    assert_eq!(env_block["NO_PROXY"], "127.0.0.1,localhost");
    assert_eq!(settings["skipWebFetchPreflight"], true);
    assert!(!read(&settings_path).contains("sk-"), "settings.json 不得含真实 Key");

    let codex_cfg = read(&home.join(".codex").join("config.toml"));
    assert!(codex_cfg.contains(&format!("http://127.0.0.1:{port}/v1")), "{codex_cfg}");
    assert!(codex_cfg.contains(&lat), "config.toml 应含本地令牌\n{codex_cfg}");
    assert!(codex_cfg.contains("plugins = false") && codex_cfg.contains("enabled = false"), "{codex_cfg}");
    // 权限档位（默认「标准」）写成各家自己的设置
    let settings_now: serde_json::Value = serde_json::from_str(&read(&settings_path)).unwrap();
    assert_eq!(settings_now["permissions"]["defaultMode"], "acceptEdits", "{settings_now}");
    assert!(codex_cfg.contains("approval_policy = \"on-request\"") && codex_cfg.contains("sandbox_mode = \"workspace-write\""), "{codex_cfg}");
    // Codex 自带的托管网页搜索经公司网关不可用：关掉，改用云端搜索 MCP
    assert!(codex_cfg.contains("web_search = \"disabled\""), "{codex_cfg}");
    assert!(!codex_cfg.contains("sk-"));

    // 2b. 云端搜索 MCP 已下发给两个 CLI，只带本地令牌
    let claude_json: serde_json::Value = serde_json::from_str(&read(&home.join(".claude.json"))).unwrap();
    let mcp = &claude_json["mcpServers"]["loov-search"];
    assert_eq!(mcp["type"], "http", "{claude_json}");
    assert_eq!(mcp["url"], format!("http://127.0.0.1:{port}/loov/mcp"));
    assert_eq!(mcp["headers"]["Authorization"], format!("Bearer {lat}"));
    let codex_cfg_now = read(&home.join(".codex").join("config.toml"));
    assert!(codex_cfg_now.contains("[mcp_servers.loov-search]"), "{codex_cfg_now}");
    assert!(!codex_cfg_now.contains("ldt_") && !read(&home.join(".claude.json")).contains("ldt_"));

    // 2c. 每个 agent 的模型选择器列出 Loov 的全部模型（聚合模式）：Claude Code 的 modelPicker、
    // Codex 的 model_catalog_json 都含 GPT / Gemini
    let settings: serde_json::Value = serde_json::from_str(&read(&settings_path)).unwrap();
    let picker: Vec<String> = settings["modelPicker"]["options"]
        .as_array()
        .map(|a| a.iter().filter_map(|o| o["model"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    eprintln!("Claude Code 模型选择器：{picker:?}");
    let claude_gemini = picker.iter().find(|m| m.contains("gemini-3.8-flash")).cloned().expect("Claude Code 选择器里应有 Gemini");
    assert!(picker.iter().any(|m| m.contains("gpt-")), "Claude Code 选择器里应有 GPT: {picker:?}");
    let codex_cfg_now: toml::Value = toml::from_str(&read(&home.join(".codex").join("config.toml"))).unwrap();
    let catalog_path = codex_cfg_now["model_catalog_json"].as_str().expect("Codex 应写 model_catalog_json").to_string();
    let catalog_path = if Path::new(&catalog_path).is_absolute() { PathBuf::from(&catalog_path) } else { home.join(".codex").join(&catalog_path) };
    let catalog: serde_json::Value = serde_json::from_str(&read(&catalog_path)).unwrap();
    let slugs: Vec<String> = catalog["models"].as_array().unwrap().iter().filter_map(|m| m["slug"].as_str().map(str::to_string)).collect();
    eprintln!("Codex 模型目录：{slugs:?}");
    assert!(slugs.iter().any(|m| m.starts_with("gpt-")) && slugs.iter().any(|m| m.starts_with("claude-")), "{slugs:?}");
    let codex_gemini = slugs.iter().find(|m| m.ends_with("/gemini-3.8-flash")).cloned().expect("Codex 目录里应有 Gemini");

    // 3. 本地路由鉴权
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    assert_eq!(http.get(format!("{base}/health")).send().await.unwrap().status(), 200);
    let body = serde_json::json!({"model": "claude-sonnet-5", "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]});
    let r = http.post(format!("{base}/v1/messages")).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 401, "无 bearer 应 401");
    let r = http.post(format!("{base}/v1/messages")).bearer_auth("PROXY_MANAGED").json(&body).send().await.unwrap();
    assert_eq!(r.status(), 401, "上游占位常量应 401");
    let r = http.post(format!("{base}/v1/messages")).bearer_auth(&lat).json(&body).send().await.unwrap();
    assert!(r.status().is_success(), "带本地令牌经隧道调模型应成功，实际 {} {}", r.status(), r.text().await.unwrap_or_default());
    // 3c. Claude Code 里选 Gemini：经聚合成员转成 Chat Completions 发往 new-api
    let gbody = serde_json::json!({"model": claude_gemini, "max_tokens": 256, "messages": [{"role": "user", "content": "Reply with exactly the word OK"}]});
    let r = http.post(format!("{base}/v1/messages")).bearer_auth(&lat).json(&gbody).send().await.unwrap();
    let (st, txt) = (r.status(), r.text().await.unwrap_or_default());
    assert!(st.is_success() && txt.contains("\"type\":\"message\""), "Claude Code 选 Gemini 应成功：{st} {txt}");
    // 3d. 模型故障回退：选 GPT 时上游出错（GPT-6 目前上游 500）就按 hub 的回退链改用 Claude，仍成功、仍计费
    let gpt = picker.iter().find(|m| m.ends_with("--gpt-6-sol")).cloned().expect("选择器里应有 gpt-6-sol");
    let fbody = serde_json::json!({"model": gpt, "max_tokens": 256, "messages": [{"role": "user", "content": "Reply with exactly the word OK"}]});
    let r = http.post(format!("{base}/v1/messages")).bearer_auth(&lat).json(&fbody).send().await.unwrap();
    let (st, txt) = (r.status(), r.text().await.unwrap_or_default());
    eprintln!("GPT 请求（可能回退）：{st} {}", &txt[..txt.len().min(300)]);
    assert!(st.is_success() && txt.contains("\"type\":\"message\""), "GPT 故障时应回退成功：{st} {txt}");
    // Codex 里选 Gemini：同理
    let r = http
        .post(format!("{base}/v1/responses"))
        .bearer_auth(&lat)
        .json(&serde_json::json!({"model": codex_gemini, "input": "Reply with exactly the word OK", "max_output_tokens": 256}))
        .send()
        .await
        .unwrap();
    let (st, txt) = (r.status(), r.text().await.unwrap_or_default());
    assert!(st.is_success() && txt.to_uppercase().contains("OK"), "Codex 选 Gemini 应成功：{st} {txt}");

    // 3b. 经本地路由调用云端搜索（hub /v1/mcp → SearXNG）
    let rpc = |id: u64, method: &str, params: serde_json::Value| {
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    };
    let mcp_url = format!("{base}/loov/mcp");
    let r = http.post(&mcp_url).json(&rpc(1, "initialize", serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "1"}}))).send().await.unwrap();
    assert_eq!(r.status(), 401, "MCP 也要求本地令牌");
    let r = http.post(&mcp_url).bearer_auth(&lat).header("accept", "application/json, text/event-stream").json(&rpc(1, "initialize", serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "1"}}))).send().await.unwrap();
    assert!(r.status().is_success(), "initialize: {}", r.status());
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["result"]["protocolVersion"], "2025-06-18", "{v}");
    let v: serde_json::Value = http.post(&mcp_url).bearer_auth(&lat).json(&rpc(2, "tools/list", serde_json::json!({}))).send().await.unwrap().json().await.unwrap();
    let names: Vec<&str> = v["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"web_search") && names.contains(&"web_fetch"), "{v}");
    let v: serde_json::Value = http.post(&mcp_url).bearer_auth(&lat).json(&rpc(3, "tools/call", serde_json::json!({"name": "web_search", "arguments": {"query": "Anthropic Claude", "max_results": 3}}))).send().await.unwrap().json().await.unwrap();
    assert_ne!(v["result"]["isError"], true, "{v}");
    let text = v["result"]["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("http"), "搜索结果应含链接: {text}");

    // 4. 隧道口独占 + 出口 IP 为节点
    assert!(std::net::TcpListener::bind(("127.0.0.1", http_port)).is_err());
    let via = reqwest::Client::builder().proxy(reqwest::Proxy::all(&proxy_url).unwrap()).build().unwrap();
    let ip = via.get("https://api.ipify.org").send().await.unwrap().text().await.unwrap();
    let creds = loov::credentials::load().unwrap().unwrap();
    let node_ips: Vec<String> = creds.bootstrap.as_ref().unwrap().tunnel.nodes.iter().map(|n| n.address.clone()).collect();
    assert!(node_ips.contains(&ip.trim().to_string()), "出口 {ip} 不在节点列表 {node_ips:?}");
    // 遥测上报域名在通道里被丢弃（agy 的「分享交互数据」靠这条规则真正关掉）
    let tele = via.get("https://play.googleapis.com/log").timeout(Duration::from_secs(15)).send().await;
    assert!(tele.is_err(), "play.googleapis.com 应被通道丢弃，实际 {:?}", tele.map(|r| r.status()));

    // 4b. 节点选择与网络模式（测试进程没有桌面会话，系统代理写不进去也不影响 sys-in 本身）
    let direct_ip = reqwest::Client::builder().no_proxy().build().unwrap().get("https://api.ipify.org").send().await.unwrap().text().await.unwrap();
    let net_of = |s: &Status| match s {
        Status::Ready { net, nodes, proxy_url: p, .. } => (net.clone(), nodes.clone(), p.clone()),
        other => panic!("expected Ready, got {other:?}"),
    };
    let s = loov_set_selection_test_hook(&state, None, Some(loov::netmode::NetMode::Global)).await.expect("global");
    let (net, _, p) = net_of(&s);
    assert_eq!(p, proxy_url, "换模式不应换掉 Loov 自己的代理地址（已启动的应用带着它）");
    let sys = net.sys_proxy.clone().or_else(|| Some("127.0.0.1:17890".into())).unwrap();
    eprintln!("全局模式：sys-in {sys}，系统代理错误 {:?}", net.sys_error);
    let sys_client = reqwest::Client::builder().proxy(reqwest::Proxy::all(format!("http://{sys}")).unwrap()).timeout(Duration::from_secs(20)).build().unwrap();
    let ip = sys_client.get("https://api.ipify.org").send().await.unwrap().text().await.unwrap();
    assert!(node_ips.contains(&ip.trim().to_string()), "全局模式出口 {ip} 应为节点");
    let s = loov_set_selection_test_hook(&state, None, Some(loov::netmode::NetMode::Rule)).await.expect("rule");
    assert_eq!(net_of(&s).0.mode, loov::netmode::NetMode::Rule);
    let ip = sys_client.get("https://api.ipify.org").send().await.unwrap().text().await.unwrap();
    assert!(node_ips.contains(&ip.trim().to_string()), "代理模式下国外网站出口 {ip} 应为节点");
    let cn = sys_client.get("https://myip.ipip.net").send().await.unwrap().text().await.unwrap();
    assert!(cn.contains(direct_ip.trim()), "代理模式下国内网站应直连（出口 {direct_ip}）：{cn}");
    // 手选节点：只用它；改回自动
    let last = node_ips.len();
    let pick = creds.bootstrap.as_ref().unwrap().tunnel.nodes.iter().find(|n| n.id == "tw1").or_else(|| creds.bootstrap.as_ref().unwrap().tunnel.nodes.last()).unwrap().clone();
    let s = loov_set_selection_test_hook(&state, Some(Some(pick.id.clone())), None).await.expect("pin");
    let (net, in_use, _) = net_of(&s);
    assert_eq!(in_use, vec![pick.id.clone()], "手选后只用这个节点");
    assert_eq!((net.active.as_deref(), net.pinned.as_deref()), (Some(pick.id.as_str()), Some(pick.id.as_str())));
    let ip = via.get("https://api.ipify.org").send().await.unwrap().text().await.unwrap();
    assert_eq!(ip.trim(), pick.address, "Loov 流量应改走手选的节点");
    let s = loov_set_selection_test_hook(&state, Some(None), Some(loov::netmode::NetMode::Direct)).await.expect("auto+direct");
    let (net, in_use, _) = net_of(&s);
    assert!(net.pinned.is_none() && net.sys_proxy.is_none());
    assert!(!in_use.is_empty() && in_use.len() <= last);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(std::net::TcpListener::bind(("127.0.0.1", 17890)).is_ok(), "直连模式下 sys-in 应已关闭");
    assert!(!loov::netmode::engaged(), "直连模式下不应接管系统代理");

    // 5. Claude Code 一轮对话（strace 零直连）。同时模拟员工 shell / 注册表里残留的变量：
    // 接管期间 settings.json 里的中和值必须压过它们，否则会直连 Vertex 或带错 Key / 模型名
    let shell_env = [
        ("CLAUDE_CODE_USE_VERTEX", "1"),
        ("ANTHROPIC_VERTEX_PROJECT_ID", "proj-shell"),
        ("CLOUD_ML_REGION", "global"),
        ("ANTHROPIC_API_KEY", "sk-ant-shell-bogus"),
        ("ANTHROPIC_MODEL", "claude-bogus-model"),
    ];
    let trace = home.join("claude.trace");
    let (ok, out, err) = run_traced_env(&home, &trace, "claude", &["-p", "Reply with exactly the word OK", "--output-format", "text"], Duration::from_secs(180), &shell_env);
    eprintln!("claude stdout: {out}\nstderr: {err}");
    assert!(ok, "claude 退出码非 0");
    assert!(out.to_uppercase().contains("OK"), "claude 回复里没有 OK: {out}");
    assert_only_loopback(&read(&trace), "claude");

    // 6. Codex 一轮对话
    if which("codex") {
        let trace = home.join("codex.trace");
        let _ = Command::new("git").args(["init", "-q"]).current_dir(&home).status();
        // 上游某个模型临时故障时可用 LOOV_E2E_CODEX_MODEL 覆盖（例如 claude-sonnet-5，走 new-api 原生 /v1/responses）
        let codex_model = std::env::var("LOOV_E2E_CODEX_MODEL").ok().filter(|m| !m.is_empty());
        let mut codex_args: Vec<&str> = vec!["exec", "--skip-git-repo-check"];
        if let Some(m) = codex_model.as_deref() {
            codex_args.extend(["-m", m]);
        }
        codex_args.push("Reply with exactly the word OK");
        let (ok, out, err) = run_traced(&home, &trace, "codex", &codex_args, Duration::from_secs(180));
        eprintln!("codex stdout: {out}\nstderr: {err}");
        assert!(ok, "codex 退出码非 0: {err}");
        assert!(out.to_uppercase().contains("OK"), "codex 回复里没有 OK: {out}");
        assert_only_loopback(&read(&trace), "codex");
    } else {
        eprintln!("codex 不在 PATH，跳过");
    }

    // 6b. Antigravity：经本地路由 /loov/gemini 走 new-api（与 Loov 页启动 agy 用同一份参数）
    let gem = format!("{base}/loov/gemini/v1beta/models");
    let gbody = serde_json::json!({"contents": [{"role": "user", "parts": [{"text": "Reply with exactly the word OK"}]}]});
    let r = http.post(format!("{gem}/gemini-3.8-flash:generateContent")).json(&gbody).send().await.unwrap();
    assert_eq!(r.status(), 401, "agy 入口也要求本地令牌");
    let r = http
        .post(format!("{gem}/gemini-3.8-flash:generateContent"))
        .header("x-goog-api-key", &lat)
        .json(&gbody)
        .send()
        .await
        .unwrap();
    let (st, txt) = (r.status(), r.text().await.unwrap_or_default());
    assert!(st.is_success() && txt.contains("candidates"), "Gemini 经 Loov 应成功：{st} {txt}");
    // agy 生成标题会用 new-api 上没有的模型，入口应改写成员工选的 Gemini
    let r = http
        .post(format!("{gem}/gemini-3.1-flash-lite-preview:generateContent"))
        .header("x-goog-api-key", &lat)
        .json(&gbody)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "未知模型应被改写：{}", r.status());
    if which("agy") {
        let (args, env) = loov::apps::agy_launch_spec(&state, &proxy_url).expect("agy launch spec");
        let extra: Vec<(&str, String)> = env.iter().filter_map(|(k, v)| v.clone().map(|v| (*k, v))).collect();
        let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let mut agy_args: Vec<&str> = args.iter().map(String::as_str).collect();
        agy_args.extend(["-p", "Reply with exactly the word OK"]);
        let trace = home.join("agy.trace");
        let (ok, out, err) = run_traced_env(&home, &trace, "agy", &agy_args, Duration::from_secs(240), &extra);
        eprintln!("agy stdout: {out}\nstderr: {err}");
        assert!(ok, "agy 退出码非 0: {err}");
        assert!(out.to_uppercase().contains("OK"), "agy 回复里没有 OK: {out}");
        assert_only_loopback(&read(&trace), "agy");
    } else {
        eprintln!("agy 不在 PATH，跳过真实 agy 对话");
    }

    // 6c. Claude Desktop：接入后 3p 网关配置指向本地路由，经 /claude-desktop 走 new-api
    let (gateway, profile) = loov_desktop_attach_test_hook(&state).await.expect("desktop attach");
    assert!(loov_desktop_routed_test_hook());
    let profile = profile.expect("Desktop profile 路径");
    let profile_text = read(Path::new(&profile));
    assert!(profile_text.contains(&format!("http://127.0.0.1:{port}/claude-desktop")), "{profile_text}");
    assert!(!profile_text.contains("sk-"), "Desktop 配置不得含真实 Key");
    let dbody = serde_json::json!({"model": "claude-sonnet-5", "max_tokens": 16, "messages": [{"role": "user", "content": "Reply with exactly the word OK"}]});
    let r = http.post(format!("{base}/claude-desktop/v1/messages")).json(&dbody).send().await.unwrap();
    assert!(r.status().is_client_error(), "Desktop 网关缺 token 应被拒：{}", r.status());
    let r = http.post(format!("{base}/claude-desktop/v1/messages")).bearer_auth(&gateway).json(&dbody).send().await.unwrap();
    let (st, txt) = (r.status(), r.text().await.unwrap_or_default());
    assert!(st.is_success() && txt.to_uppercase().contains("OK"), "Desktop 经 Loov 应成功：{st} {txt}");

    // 6d. Grok Build：接入后 ~/.grok/config.toml 指向本地路由；Claude 走原生 responses，Gemini 转 Chat Completions
    let rbody = |model: &str| serde_json::json!({"model": model, "input": "Reply with exactly the word OK", "max_output_tokens": 64});
    for model in ["claude-sonnet-5", "gemini-3.8-flash"] {
        loov_grok_attach_test_hook(&state, model).await.unwrap_or_else(|e| panic!("grok attach {model}: {e}"));
        let grok_cfg = read(&home.join(".grok").join("config.toml"));
        assert!(grok_cfg.contains(&format!("http://127.0.0.1:{port}/grokbuild/v1")), "{grok_cfg}");
        assert!(grok_cfg.contains(&lat) && !grok_cfg.contains("sk-"), "Grok 配置只能有本地令牌: {grok_cfg}");
        let r = http.post(format!("{base}/grokbuild/v1/responses")).json(&rbody("loov")).send().await.unwrap();
        assert_eq!(r.status(), 401, "Grok 入口也要求本地令牌");
        let r = http.post(format!("{base}/grokbuild/v1/responses")).bearer_auth(&lat).json(&rbody("loov")).send().await.unwrap();
        let (st, txt) = (r.status(), r.text().await.unwrap_or_default());
        assert!(st.is_success() && txt.to_uppercase().contains("OK"), "Grok 经 Loov 用 {model} 应成功：{st} {txt}");
    }

    // Grok 的模型选择器：Loov 的其余模型各一张表，经本地路由的 Codex 入口
    loov_grok_sync_models_test_hook(&state).expect("grok models");
    let grok_doc: toml::Value = toml::from_str(&read(&home.join(".grok").join("config.toml"))).unwrap();
    let tables = grok_doc["model"].as_table().unwrap();
    eprintln!("Grok 模型表：{:?}", tables.keys().collect::<Vec<_>>());
    let gem_table = tables.get("loov-gemini-3-8-flash").or_else(|| tables.get("loov-claude-sonnet-5")).expect("Grok 应有额外模型表");
    assert_eq!(gem_table["base_url"].as_str(), Some(format!("http://127.0.0.1:{port}/v1").as_str()));
    assert_eq!(gem_table["api_key"].as_str(), Some(lat.as_str()));
    let gm = gem_table["model"].as_str().unwrap().to_string();
    assert_eq!(gem_table["extra_headers"]["x-loov-app"].as_str(), Some("grok"), "额外模型表要声明来源，计费记在 Grok 名下");
    let r = http
        .post(format!("{base}/v1/responses"))
        .bearer_auth(&lat)
        .header("x-loov-app", "grok")
        .json(&rbody(&gm))
        .send()
        .await
        .unwrap();
    let (st, txt) = (r.status(), r.text().await.unwrap_or_default());
    assert!(st.is_success() && txt.to_uppercase().contains("OK"), "Grok 额外模型 {gm} 应成功：{st} {txt}");

    // 6e. Codex 桌面版没登录官方账号时发的官方默认模型（new-api 上没有）被换成员工选的 Codex 模型
    let r = http.post(format!("{base}/v1/responses")).bearer_auth(&lat).json(&rbody("gpt-5.5-codex")).send().await.unwrap();
    let (st, txt) = (r.status(), r.text().await.unwrap_or_default());
    assert!(st.is_success() && txt.to_uppercase().contains("OK"), "未知 Codex 模型应被改写后成功：{st} {txt}");

    // 6f. 按应用计费：hub /usage 按各应用的 Key 汇总 new-api 实扣金额，上面调过的应用都应出现
    let usage = loov::flows::usage("today").await.expect("usage");
    eprintln!("今日按应用花费：{:?}，搜索 {:?}", usage.by_app, usage.search);
    let apps: Vec<&str> = usage.by_app.iter().filter(|a| a.usd > 0.0).map(|a| a.app.as_str()).collect();
    for want in ["claude", "agy", "desktop", "grok"] {
        assert!(apps.contains(&want), "今日用量里应有 {want}：{apps:?}");
    }
    assert!(usage.search.calls >= 1, "上面调过一次云端搜索：{:?}", usage.search);
    assert!(usage.search.bytes > 0, "搜索应记下返回字节：{:?}", usage.search);

    // 7. 设备列表含本机，然后登出
    let devices = loov_devices_test_hook().await.unwrap();
    assert!(devices.iter().any(|d| d.current));
    loov_logout_test_hook(&state).await.unwrap();
    guard.done = true;
    assert!(!loov::credentials::credentials_path().exists());
    assert!(loov::api_key().is_none());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(std::net::TcpListener::bind(("127.0.0.1", http_port)).is_ok(), "xray 应已退出");
    let settings: serde_json::Value = serde_json::from_str(&read(&settings_path)).unwrap();
    assert_eq!(settings["env"]["HTTPS_PROXY"], loov::BLACKHOLE_PROXY);
    // 路由仍指向本地路由（fail-closed），但没有 Key 可用
    assert_eq!(settings["env"]["ANTHROPIC_BASE_URL"], format!("http://127.0.0.1:{port}"));
    let r = http.post(format!("{base}/v1/messages")).bearer_auth(&lat).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 401, "登出后调模型应 401");

    // 8. 恢复原配置：Vertex 变量、Codex 原 provider 回来，Loov 痕迹全部消失，用户字段保留
    assert!(loov::restore::snapshot_exists(), "登录前应已保存快照");
    let report = loov_restore_test_hook(&state).await.expect("restore");
    assert!(report.snapshot && report.claude && report.codex, "{report:?}");
    let restored: serde_json::Value = serde_json::from_str(&read(&settings_path)).unwrap();
    assert_eq!(restored["env"], original_settings["env"], "env 应完全回到原值: {restored}");
    assert_eq!(restored["model"], "claude-fable-5-1");
    assert!(restored.get("skipWebFetchPreflight").is_none());
    assert_eq!(restored["permissions"], original_settings["permissions"]);
    let codex_after = read(&home.join(".codex").join("config.toml"));
    let parsed: toml::Value = toml::from_str(&codex_after).unwrap();
    assert_eq!(parsed["model_provider"].as_str(), Some("newapi"), "{codex_after}");
    assert_eq!(parsed["model"].as_str(), Some("claude-opus-5-5"));
    assert_eq!(parsed["model_providers"]["newapi"]["base_url"].as_str(), Some("http://127.0.0.1:3001/v1"));
    assert!(parsed["model_providers"].get("custom").is_none(), "{codex_after}");
    assert!(parsed.get("features").is_none() && parsed.get("analytics").is_none(), "{codex_after}");
    assert!(!codex_after.contains("lat_") && !codex_after.contains("127.0.0.1:15721"), "{codex_after}");
    assert!(!loov::restore::snapshot_exists(), "快照应已归档");
    let claude_json: serde_json::Value = serde_json::from_str(&read(&home.join(".claude.json"))).unwrap();
    assert!(claude_json["mcpServers"].get("loov-search").is_none(), "恢复后应删除云端搜索 MCP");
    assert!(!codex_after.contains("loov-search"), "{codex_after}");
    let codex_doc: toml::Value = toml::from_str(&codex_after).unwrap();
    assert!(codex_doc.get("mcp_servers").is_none(), "原配置没有 mcp_servers，恢复后也不应有: {codex_after}");
    assert!(!loov::restore::managed());
    assert!(!loov_desktop_routed_test_hook(), "恢复后 Claude Desktop 应退出路由模式");
    let grok_after = std::fs::read_to_string(home.join(".grok").join("config.toml")).unwrap_or_default();
    assert!(!grok_after.contains("lat_") && !grok_after.contains("/grokbuild/v1"), "恢复后 Grok 不应再指向 Loov: {grok_after}");
    assert!(!grok_after.contains("[model.loov-"), "恢复后应删掉 Loov 写的 Grok 模型表: {grok_after}");
    let r = http.post(format!("{base}/v1/messages")).json(&body).send().await;
    assert!(r.map(|r| r.status() != 401).unwrap_or(true), "恢复后本地路由不再要求 Loov 令牌（或已停止）");
    let _ = state.proxy_service.stop().await;
}

/// 残留的 xray 必须被清掉，否则 SO_REUSEPORT 平分连接导致一半 407。
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphan_xray_is_killed_before_start() {
    if !gated() {
        return;
    }
    let _guard = test_mutex().lock().unwrap_or_else(|e| e.into_inner());
    reset_test_fs();
    let home = ensure_test_home().to_path_buf();
    let state = create_test_state().expect("state");
    let xray = loov::tunnel::locate_xray().expect("xray");
    let run_dir = loov::tunnel::run_dir();
    std::fs::create_dir_all(&run_dir).unwrap();

    // 一个只监听 socks 的「残留」进程，命令行引用本运行目录
    let cfg = run_dir.join("xray-orphan.json");
    std::fs::write(&cfg, r#"{"inbounds":[{"listen":"127.0.0.1","port":0,"protocol":"socks"}],"outbounds":[{"protocol":"freedom"}]}"#).unwrap();
    let mut orphan = Command::new(&xray).args(["run", "-c"]).arg(&cfg).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(matches!(orphan.try_wait(), Ok(None)), "孤儿进程应在运行");

    let killed = loov::tunnel::kill_orphans();
    assert!(killed >= 1, "应清理至少 1 个残留进程");
    std::thread::sleep(Duration::from_millis(600));
    assert!(!matches!(orphan.try_wait(), Ok(None)), "孤儿进程应已退出");
    assert!(!cfg.exists(), "旧配置应被删除");
    let _ = orphan.wait();

    // 正常登录 → 自检通过
    let user = env("LOOV_HUB_USER");
    let pass = env("LOOV_HUB_PASS");
    let status = loov_login_test_hook(&state, &user, &pass, true).await.unwrap();
    assert!(matches!(status, Status::Ready { .. }), "{status:?}");
    loov_logout_test_hook(&state).await.unwrap();
    let _ = state.proxy_service.stop().await;
    let _ = home;
}

/// 冷启动：有凭证时 startup 应拉起隧道并把客户端接回本地路由。
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_with_saved_credentials_restores_tunnel() {
    if !gated() {
        return;
    }
    let _guard = test_mutex().lock().unwrap_or_else(|e| e.into_inner());
    reset_test_fs();
    let home = ensure_test_home().to_path_buf();
    let state = create_test_state().expect("state");
    let user = env("LOOV_HUB_USER");
    let pass = env("LOOV_HUB_PASS");
    let status = loov_login_test_hook(&state, &user, &pass, true).await.unwrap();
    assert!(matches!(status, Status::Ready { .. }));
    let mut guard = RevokeGuard { state: &state, done: false };
    // 模拟退出：停 sidecar，但不登出
    loov_shutdown_test_hook().await;
    let _ = state.proxy_service.stop().await;
    let settings_path = home.join(".claude").join("settings.json");
    let before: serde_json::Value = serde_json::from_str(&read(&settings_path)).unwrap();

    // 模拟重启
    let status = loov_startup_test_hook(&state).await;
    let Status::Ready { proxy_url, .. } = status else { panic!("{status:?}") };
    let after: serde_json::Value = serde_json::from_str(&read(&settings_path)).unwrap();
    assert_eq!(after["env"]["HTTPS_PROXY"], proxy_url, "重启后应写入新的隧道口");
    assert_ne!(after["env"]["HTTPS_PROXY"], before["env"]["HTTPS_PROXY"], "端口/口令每次随机");
    assert_eq!(after["env"]["ANTHROPIC_AUTH_TOKEN"], before["env"]["ANTHROPIC_AUTH_TOKEN"], "重启不换本地令牌");
    assert!(state.proxy_service.is_running().await, "重启后本地路由应被上游 startup 拉起");

    loov_logout_test_hook(&state).await.unwrap();
    guard.done = true;
    let _ = state.proxy_service.stop().await;
}

fn which(bin: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|d| Path::new(d).join(bin).is_file())
}

/// 网络屏蔽了所有节点、又没有本机代理时：登录仍成功（fail-closed，CLI 走黑洞），状态给出逐节点原因。
/// 需要先用 iptables 挡住节点端口，所以单独开关：LOOV_E2E_EXPECT_FAIL=1。
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_network_reports_every_node() {
    if !gated() || std::env::var("LOOV_E2E_EXPECT_FAIL").as_deref() != Ok("1") {
        return;
    }
    let _guard = test_mutex().lock().unwrap_or_else(|e| e.into_inner());
    reset_test_fs();
    let state = create_test_state().expect("state");
    let status = loov_login_test_hook(&state, &env("LOOV_HUB_USER"), &env("LOOV_HUB_PASS"), true)
        .await
        .unwrap_or_else(|e| panic!("login failed: {e}"));
    let mut guard = RevokeGuard { state: &state, done: false };
    let Status::TunnelFailed { reason } = &status else { panic!("expected TunnelFailed, got {status:?}") };
    eprintln!("失败原因：{reason}");
    assert!(reason.contains("无法连接隧道节点"), "{reason}");
    let creds = loov::credentials::load().unwrap().unwrap();
    for n in &creds.bootstrap.as_ref().unwrap().tunnel.nodes {
        assert!(reason.contains(&format!("{}:{}", n.address, n.port)), "原因里缺节点 {}：{reason}", n.id);
    }
    assert!(reason.contains("TUN"), "{reason}");
    loov_logout_test_hook(&state).await.unwrap();
    guard.done = true;
    let _ = state.proxy_service.stop().await;
}
