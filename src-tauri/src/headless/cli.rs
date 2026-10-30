//! 服务器模式的命令行：解析子命令，除 `serve` / `service` / `help` 外都经控制套接字交给常驻进程。

use serde_json::Value;

use super::ctl::{call, Request};

const HELP: &str = "Loov Agent 服务器模式

用法：loov-agent <命令> [参数]

  serve                      启动常驻服务（建议用 systemd，见 service install）
  service install            写入 systemd 用户服务（开机自启、退出登录后继续运行）
  login [-u 用户名] [--password-stdin] [--evict-oldest]
                             登录（密码默认在终端里输入，不回显）
  register --invite 邀请码 [-u 用户名] [--password-stdin]
  logout                     登出（吊销本设备）
  status [--json]            安全通道、节点、用户
  refresh                    重新拉取配置、重建安全通道
  node <节点id|auto>         固定节点或改回自动
  usage [today|month]        花费（按应用）
  devices                    已登录的设备
  install <claude|codex|agy|grok>
                             安装 CLI agent（经安全通道下载）
  run <agy|grok|claude|codex> [-- 参数…]
                             在当前终端运行 agent（claude / codex 也可以直接运行）
  safety <careful|standard|full>
                             agent 权限档位（写成各家自己的设置）
  restore                    恢复登录前的 Claude Code / Codex 配置并停用 Loov
  feishu connect             扫码连接飞书（终端里显示二维码）
  feishu status | disconnect
  feishu agent <claude|codex|agy|grok> | cwd <路径> | group <on|off> | <on|off>
";

pub fn run(cmd: &str, args: &[String]) -> i32 {
    match cmd {
        "serve" => super::serve::run(),
        "service" => service(args),
        "feishu" => feishu(args),
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            0
        }
        "version" | "--version" => {
            println!("loov-agent {}", env!("CARGO_PKG_VERSION"));
            0
        }
        _ => match request_for(cmd, args) {
            Ok(Some(req)) => send(cmd, args, &req),
            Ok(None) => 0,
            Err(msg) => {
                eprintln!("{msg}\n\n{HELP}");
                2
            }
        },
    }
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn opt(args: &[String], short: &str, long: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == short || w[0] == long).map(|w| w[1].clone())
}

fn positional(args: &[String]) -> Option<&String> {
    args.iter().find(|a| !a.starts_with('-'))
}

fn request_for(cmd: &str, args: &[String]) -> Result<Option<Request>, String> {
    Ok(Some(match cmd {
        "status" => Request::Status,
        "logout" => Request::Logout,
        "refresh" => Request::Refresh,
        "devices" => Request::Devices,
        "restore" => Request::Restore,
        "usage" => Request::Usage { range: positional(args).cloned().unwrap_or_else(|| "month".into()) },
        "node" => {
            let id = positional(args).ok_or("node 需要节点 id，或 auto")?;
            Request::Node { id: (id != "auto").then(|| id.clone()) }
        }
        "install" => Request::Install { app: positional(args).ok_or("install 需要应用名")?.clone() },
        "run" => Request::Spec { app: positional(args).ok_or("run 需要应用名")?.clone() },
        "safety" => {
            let level = positional(args).ok_or("safety 需要档位：careful / standard / full")?;
            Request::Safety {
                level: serde_json::from_value(Value::String(level.clone())).map_err(|_| format!("未知档位：{level}"))?,
            }
        }
        "login" | "register" => {
            let invite = if cmd == "register" { Some(opt(args, "--invite", "--invite").ok_or("register 需要 --invite 邀请码")?) } else { None };
            let username = match opt(args, "-u", "--username") {
                Some(u) => u,
                None => prompt("用户名：")?,
            };
            let password = if flag(args, "--password-stdin") {
                let mut s = String::new();
                std::io::stdin().read_line(&mut s).map_err(|e| e.to_string())?;
                s.trim_end_matches(['\r', '\n']).to_string()
            } else {
                read_password("密码：")?
            };
            let evict_oldest = flag(args, "--evict-oldest");
            match invite {
                Some(invite) => Request::Register { invite, username, password, evict_oldest },
                None => Request::Login { username, password, evict_oldest },
            }
        }
        _ => return Err(format!("未知命令：{cmd}")),
    }))
}

fn send(cmd: &str, args: &[String], req: &Request) -> i32 {
    let result = call(req, |p| {
        let stage = p["stage"].as_str().unwrap_or("");
        match p["percent"].as_f64() {
            Some(pct) => eprint!("\r{stage} {pct:.0}%   "),
            None => eprint!("\r{stage}…   "),
        }
    });
    match result {
        Err((code, message)) => {
            eprintln!("{message}");
            if code == "DEVICE_LIMIT" {
                eprintln!("设备数已满：加 --evict-oldest 顶替最久没用的设备");
            }
            1
        }
        Ok(data) => {
            if cmd == "run" {
                return exec_spec(&data, args);
            }
            if flag(args, "--json") {
                println!("{}", serde_json::to_string_pretty(&data).unwrap_or_default());
            } else {
                print_human(cmd, &data);
            }
            0
        }
    }
}

/// 在当前终端里执行 agent：exec 替换本进程，信号、终端都交给 agent。
fn exec_spec(spec: &Value, args: &[String]) -> i32 {
    use std::os::unix::process::CommandExt;
    let spec: crate::loov::apps::CliSpec = match serde_json::from_value(spec.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("服务返回的启动参数无法解析：{e}");
            return 1;
        }
    };
    let extra: Vec<&String> = args.iter().skip_while(|a| *a != "--").skip(1).collect();
    let mut command = std::process::Command::new(&spec.program);
    command.args(&spec.args).args(extra);
    for (k, v) in &spec.env {
        match v {
            Some(v) => command.env(k, v),
            None => command.env_remove(k),
        };
    }
    let err = command.exec();
    eprintln!("启动 {} 失败：{err}（没装的话先运行 loov-agent install {}）", spec.program, spec.program);
    1
}

fn print_human(cmd: &str, data: &Value) {
    match cmd {
        "status" | "login" | "register" | "refresh" | "node" => {
            let state = data["state"].as_str().unwrap_or("?");
            match state {
                "ready" => {
                    let net = &data["net"];
                    let active = net["active"].as_str().unwrap_or("-");
                    let name = net["nodes"]
                        .as_array()
                        .and_then(|n| n.iter().find(|x| x["id"] == active))
                        .and_then(|x| x["name"].as_str())
                        .unwrap_or(active);
                    println!("安全通道已连接 · 用户 {} · 节点 {name}{}", data["username"].as_str().unwrap_or(""), if net["pinned"].is_null() { "（自动）" } else { "" });
                    println!("Claude Code / Codex 已经指向 Loov，直接运行 claude 或 codex 即可（经 Loov 计费）");
                }
                "noCredentials" => println!("未登录：loov-agent login"),
                "starting" => println!("正在连接…"),
                "tunnelFailed" | "offline" => println!("安全通道未连接：{}", data["reason"].as_str().unwrap_or("")),
                "reloginRequired" => println!("登录已失效：loov-agent login"),
                other => println!("{other}"),
            }
        }
        "usage" => {
            println!("合计 ${:.4}（本月 ${:.4}）", data["totalUsd"].as_f64().unwrap_or(0.0), data["monthUsd"].as_f64().unwrap_or(0.0));
            for a in data["byApp"].as_array().into_iter().flatten() {
                println!("  {:<10} ${:.4}", a["app"].as_str().unwrap_or(""), a["usd"].as_f64().unwrap_or(0.0));
            }
            println!("  云端搜索   {} 次 · ${:.4}", data["search"]["calls"].as_u64().unwrap_or(0), data["search"]["usd"].as_f64().unwrap_or(0.0));
            if let Some(b) = data["budgetUsd"].as_f64() {
                println!("  月预算     ${b}{}", if data["budgetBlocked"].as_bool() == Some(true) { "（已用完，调用已暂停）" } else { "" });
            }
        }
        "devices" => {
            for d in data.as_array().into_iter().flatten() {
                println!("{}  {}  {}{}", d["id"].as_str().unwrap_or(""), d["platform"].as_str().unwrap_or(""), d["name"].as_str().unwrap_or(""), if d["current"].as_bool() == Some(true) { "  ← 本机" } else { "" });
            }
        }
        "install" => {
            eprintln!();
            println!("已安装");
        }
        "logout" => println!("已登出"),
        "restore" => println!("已恢复原配置：{data}"),
        "safety" => println!("已更新 agent 权限档位"),
        _ => println!("{data}"),
    }
}

fn prompt(label: &str) -> Result<String, String> {
    use std::io::Write;
    eprint!("{label}");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).map_err(|e| e.to_string())?;
    Ok(s.trim().to_string())
}

/// 终端里读密码，不回显。
fn read_password(label: &str) -> Result<String, String> {
    use std::io::Write;
    eprint!("{label}");
    let _ = std::io::stderr().flush();
    let fd = libc::STDIN_FILENO;
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    let is_tty = unsafe { libc::tcgetattr(fd, &mut term) } == 0;
    if is_tty {
        let mut quiet = term;
        quiet.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) };
    }
    let mut s = String::new();
    let read = std::io::stdin().read_line(&mut s);
    if is_tty {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
        eprintln!();
    }
    read.map_err(|e| e.to_string())?;
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

/// `feishu …`：飞书渠道。connect 在终端里显示二维码，轮询到扫码完成。
fn feishu(args: &[String]) -> i32 {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    let arg = args.get(1).cloned();
    let on_off = |v: &Option<String>| v.as_deref().map(|s| s == "on");
    let req = match sub {
        "status" => Request::FeishuStatus,
        "disconnect" => Request::FeishuDisconnect,
        "agent" => Request::FeishuConfigure { agent: arg, cwd: None, allow_group: None, enabled: None },
        "cwd" => Request::FeishuConfigure { agent: None, cwd: arg, allow_group: None, enabled: None },
        "group" => Request::FeishuConfigure { agent: None, cwd: None, allow_group: on_off(&arg), enabled: None },
        "on" | "off" => Request::FeishuConfigure { agent: None, cwd: None, allow_group: None, enabled: Some(sub == "on") },
        "connect" => {
            let reg = match call(&Request::FeishuBegin, |_| {}) {
                Ok(v) => v,
                Err((_, m)) => {
                    eprintln!("{m}");
                    return 1;
                }
            };
            let url = reg["url"].as_str().unwrap_or_default();
            println!("{}", crate::loov::feishu::qr_text(url));
            println!("用飞书扫上面的二维码，或在飞书里打开：\n  {url}\n（10 分钟内有效）");
            let code = reg["deviceCode"].as_str().unwrap_or_default().to_string();
            let mut interval = reg["intervalSecs"].as_u64().unwrap_or(5);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(reg["expiresInSecs"].as_u64().unwrap_or(600));
            while std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_secs(interval));
                match call(&Request::FeishuPoll { device_code: code.clone() }, |_| {}) {
                    Ok(v) if v.as_bool() == Some(true) => {
                        println!("已连接飞书。现在在飞书里私聊这个机器人就能用 agent（默认 Claude Code，可用 loov-agent feishu agent codex 换）。");
                        return 0;
                    }
                    Ok(_) => {}
                    Err((_, m)) if m.contains("太快") => interval += 5,
                    Err((_, m)) => {
                        eprintln!("{m}");
                        return 1;
                    }
                }
            }
            eprintln!("二维码已过期，请重新运行 loov-agent feishu connect");
            return 1;
        }
        other => {
            eprintln!("未知的 feishu 子命令：{other}\n\n{HELP}");
            return 2;
        }
    };
    match call(&req, |_| {}) {
        Ok(v) => {
            if v.is_null() {
                println!("已断开飞书（飞书里的应用可在开放平台「我的应用」里删除）");
            } else {
                let link = v["link"]["state"].as_str().unwrap_or("stopped");
                println!(
                    "飞书：{}｜应用 {}｜agent {}｜目录 {}｜群聊 {}｜连接 {link}",
                    if v["enabled"].as_bool() == Some(true) { "已启用" } else { "未启用" },
                    v["appId"].as_str().filter(|s| !s.is_empty()).unwrap_or("未连接"),
                    v["agent"].as_str().unwrap_or(""),
                    v["cwd"].as_str().filter(|s| !s.is_empty()).unwrap_or("主目录"),
                    if v["allowGroup"].as_bool() == Some(true) { "开" } else { "关" },
                );
            }
            0
        }
        Err((_, m)) => {
            eprintln!("{m}");
            1
        }
    }
}

/// `service install`：写 systemd 用户服务。
fn service(args: &[String]) -> i32 {
    if positional(args).map(String::as_str) != Some("install") {
        eprintln!("用法：loov-agent service install");
        return 2;
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("找不到可执行文件路径：{e}");
            return 1;
        }
    };
    let dir = crate::config::get_home_dir().join(".config/systemd/user");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("创建 {} 失败：{e}", dir.display());
        return 1;
    }
    let unit = format!(
        "[Unit]\nDescription=Loov Agent（服务器模式）\nAfter=network-online.target\nWants=network-online.target\n\n\
         [Service]\nExecStart={} serve\nRestart=on-failure\nRestartSec=5\nKillMode=control-group\n\n\
         [Install]\nWantedBy=default.target\n",
        exe.display()
    );
    let path = dir.join("loov-agent.service");
    if let Err(e) = std::fs::write(&path, unit) {
        eprintln!("写入 {} 失败：{e}", path.display());
        return 1;
    }
    println!("已写入 {}", path.display());
    println!("接着运行：");
    println!("  systemctl --user daemon-reload");
    println!("  systemctl --user enable --now loov-agent");
    println!("  sudo loginctl enable-linger $USER   # 退出 SSH 后继续运行");
    println!("  loov-agent login");
    0
}
