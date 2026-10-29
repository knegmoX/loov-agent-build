//! xray sidecar：按 hub 下发的节点生成客户端配置，管理子进程生命周期。
//!
//! 实测陷阱（TEST_REPORT.md）：Xray 监听用 SO_REUSEPORT，残留的旧进程会和新进程平分连接，
//! 约一半请求 407。所以：每次随机端口 + 随机口令；启动前清理引用了本目录配置的残留进程；
//! 启动后自检端口独占并经代理连发 5 次健康检查。
//!
//! 节点可达性因网络而异（北京直连东京 9443 实测不通）：启动时先给每个节点各开一个探测入站，
//! 直连和「经本机代理」两条路并发探一次，挑出能用的节点和走法后再定主入站的出站。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::credentials::{loov_dir, restrict_dir, write_private};
use super::hub_client::TunnelNode;
use super::netmode::{NetMode, SysPorts};
use super::sysproxy::{Upstream, UpstreamKind};

/// 允许运行的 xray 二进制（官方 release 解包后的可执行文件）的锁文件，见 `src-tauri/xray.lock.json`。
/// 构建脚本 `scripts/fetch-xray.sh` 与运行时校验用的是同一份。
const XRAY_LOCK: &str = include_str!("../../xray.lock.json");

/// 锁文件里所有平台的可执行文件 sha256（任一平台的官方二进制都放行）。
pub fn xray_allowlist() -> Vec<String> {
    let v: Value = serde_json::from_str(XRAY_LOCK).expect("xray.lock.json");
    v["targets"]
        .as_object()
        .map(|t| {
            t.values()
                .filter_map(|e| e["bin_sha256"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

const PROXY_USER: &str = "loov";
const READY_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// 本机代理出站的 tag，节点出站经 `sockopt.dialerProxy` 引用它。
const UPSTREAM_TAG: &str = "upstream";

#[derive(Debug, Clone, Copy)]
pub struct Ports {
    pub http: u16,
    pub socks: u16,
}

/// 节点出站怎么拨号：直连，或经本机代理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Route {
    Direct,
    Proxy,
}

impl Route {
    /// 出站 tag 前缀；balancer / observatory 按前缀选，两种走法互不混入。
    fn prefix(self) -> &'static str {
        match self {
            Route::Direct => "node-",
            Route::Proxy => "via-",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Route::Direct => "直连",
            Route::Proxy => "经本机代理",
        }
    }
}

/// 逐节点探测入站：本机端口 → 指定节点的指定走法。
#[derive(Debug, Clone)]
pub struct Probe {
    pub node_id: String,
    pub route: Route,
    pub port: u16,
}

impl Probe {
    fn tag(&self) -> String {
        format!("probe-{}-{}", self.node_id, if self.route == Route::Direct { "direct" } else { "proxy" })
    }
}

/// 「代理 / 全局」模式的 sys-in 入站（系统代理指向它）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SysLayout {
    pub ports: SysPorts,
    pub mode: NetMode,
}

/// 一份 xray 配置的形状：主入站走哪种出站、有没有本机代理、附带哪些探测入站、有没有 sys-in。
pub struct Layout<'a> {
    pub route: Route,
    pub upstream: Option<&'a Upstream>,
    pub probes: &'a [Probe],
    pub sys: Option<SysLayout>,
}

/// 起 xray 的选项。
pub struct StartOpts<'a> {
    pub route: Route,
    pub upstream: Option<&'a Upstream>,
    /// 给每个节点的每种走法各开一个探测入站（见 [`probe`]）
    pub with_probes: bool,
    pub sys: Option<SysLayout>,
    /// 重启（换节点 / 换模式）时沿用原来的端口和口令：已启动的应用环境变量里带着它们
    pub reuse: Option<(Ports, String)>,
}

/// 员工的选择：手选的节点（None = 自动）与网络模式。
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub node: Option<String>,
    pub mode: NetMode,
}

pub struct TunnelHandle {
    child: tokio::process::Child,
    pub http_port: u16,
    pub socks_port: u16,
    pub user: String,
    pass: String,
    pub config_path: PathBuf,
    pub node_ids: Vec<String>,
    pub route: Route,
    /// 经本机代理时的代理地址（不含口令）。
    pub via: Option<String>,
    pub probes: Vec<Probe>,
    /// 开着的 sys-in（「代理 / 全局」模式）。
    pub sys: Option<SysLayout>,
    /// 下面几项由 [`establish`] 填：换节点 / 换模式时不用重新探测。
    pub xray_bin: PathBuf,
    pub upstream: Option<Upstream>,
    /// 探测通过的节点（按下发顺序）。
    pub usable: Vec<TunnelNode>,
    /// 每个节点的探测结果：Ok(耗时毫秒) / Err(原因)，按下发顺序。
    pub probe_report: Vec<(TunnelNode, Result<u64, String>)>,
    /// 手选的节点没通过探测，这次回到了自动。
    pub pin_fallback: bool,
}

impl TunnelHandle {
    /// 现在走哪个节点：只用一个节点时就是它；多个（自动）时按探测耗时取最快的（xray 的 leastPing
    /// 用的是同一类测量，通常一致）。
    pub fn active_node(&self) -> Option<String> {
        if self.node_ids.len() == 1 {
            return self.node_ids.first().cloned();
        }
        self.probe_report
            .iter()
            .filter(|(n, r)| r.is_ok() && self.node_ids.contains(&n.id))
            .min_by_key(|(_, r)| *r.as_ref().unwrap())
            .map(|(n, _)| n.id.clone())
            .or_else(|| self.node_ids.first().cloned())
    }
}

impl TunnelHandle {
    /// 给 reqwest / CLI 用的 HTTP 代理地址（含口令）。
    pub fn proxy_url(&self) -> String {
        format!("http://{}:{}@127.0.0.1:{}", self.user, self.pass, self.http_port)
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub async fn stop(mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await;
        let _ = std::fs::remove_file(&self.config_path);
        let _ = std::fs::remove_file(pid_file());
    }

    /// 同步版本：进程退出路径里没有 tokio 运行时可用时调用。
    pub fn stop_blocking(mut self) {
        let _ = self.child.start_kill();
        std::thread::sleep(Duration::from_millis(200));
        let _ = std::fs::remove_file(&self.config_path);
        let _ = std::fs::remove_file(pid_file());
    }
}

pub fn run_dir() -> PathBuf {
    loov_dir().join("run")
}

fn pid_file() -> PathBuf {
    run_dir().join("xray.pid")
}

pub fn log_file() -> PathBuf {
    run_dir().join("xray.log")
}

/// 绑定 :0 取 n 个互不相同的空闲端口（全部绑住后再一起释放）。释放后到 xray 真正监听之间有极小的
/// 竞争窗口，由 `self_check` 兜底。
fn free_ports(n: usize) -> Result<Vec<u16>, String> {
    let listeners = (0..n)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("分配端口失败：{e}")))
        .collect::<Result<Vec<_>, _>>()?;
    listeners
        .iter()
        .map(|l| l.local_addr().map(|a| a.port()).map_err(|e| e.to_string()))
        .collect()
}

pub fn pick_ports() -> Result<Ports, String> {
    let p = free_ports(2)?;
    Ok(Ports { http: p[0], socks: p[1] })
}

fn random_secret(bytes: usize) -> String {
    use aes_gcm::aead::rand_core::RngCore;
    use base64::Engine;
    let mut buf = vec![0u8; bytes];
    aes_gcm::aead::OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn node_outbound(n: &TunnelNode, route: Route) -> Value {
    let mut ob = json!({
        "tag": format!("{}{}", route.prefix(), n.id),
        "protocol": "vless",
        "settings": { "vnext": [{
            "address": n.address,
            "port": n.port,
            "users": [{ "id": n.uuid, "flow": n.flow, "encryption": n.encryption }]
        }]},
        "streamSettings": {
            "network": "raw",
            "security": "reality",
            "realitySettings": {
                "serverName": n.reality.server_name,
                "fingerprint": n.reality.fingerprint,
                "password": n.reality.public_key,
                "shortId": n.reality.short_id,
                "mldsa65Verify": n.reality.mldsa65_verify,
                "spiderX": "/"
            }
        }
    });
    if route == Route::Proxy {
        ob["streamSettings"]["sockopt"] = json!({ "dialerProxy": UPSTREAM_TAG });
    }
    ob
}

fn upstream_outbound(u: &Upstream) -> Value {
    let mut server = json!({ "address": u.host, "port": u.port });
    if let Some(user) = &u.user {
        server["users"] = json!([{ "user": user, "pass": u.pass.clone().unwrap_or_default() }]);
    }
    let protocol = match u.kind {
        UpstreamKind::Http => "http",
        UpstreamKind::Socks => "socks",
    };
    json!({ "tag": UPSTREAM_TAG, "protocol": protocol, "settings": { "servers": [server] } })
}

/// 生成 xray 客户端配置。字段形状移植自 loov-hub/test/integration/xray-client.ts。
pub fn build_config(nodes: &[TunnelNode], ports: Ports, user: &str, pass: &str, access_log: Option<&Path>, layout: &Layout) -> Value {
    let accounts = json!([{ "user": user, "pass": pass }]);
    let mut outbounds: Vec<Value> = nodes.iter().map(|n| node_outbound(n, Route::Direct)).collect();
    if let Some(u) = layout.upstream {
        outbounds.extend(nodes.iter().map(|n| node_outbound(n, Route::Proxy)));
        outbounds.push(upstream_outbound(u));
    }
    outbounds.push(json!({ "tag": "block", "protocol": "blackhole" }));
    if layout.sys.is_some() {
        // 「代理」模式的国内网站、两种模式的内网地址经 sys-in 进来时直连
        outbounds.push(json!({ "tag": "direct", "protocol": "freedom" }));
    }

    let mut log = json!({ "loglevel": "warning" });
    if let Some(p) = access_log {
        log["access"] = Value::String(p.to_string_lossy().into_owned());
    }

    let mut inbounds = vec![
        json!({ "tag": "http-in", "listen": "127.0.0.1", "port": ports.http, "protocol": "http",
                "settings": { "accounts": accounts, "allowTransparent": false } }),
        json!({ "tag": "socks-in", "listen": "127.0.0.1", "port": ports.socks, "protocol": "socks",
                "settings": { "auth": "password", "accounts": accounts, "udp": false } }),
    ];
    let prefix = layout.route.prefix();
    // 主入站 / sys-in 的去向：多节点时交给 balancer，单节点直接指过去
    let to_nodes = |mut rule: Value| -> Value {
        if nodes.len() > 1 {
            rule["balancerTag"] = json!("auto");
        } else if let Some(n) = nodes.first() {
            rule["outboundTag"] = json!(format!("{prefix}{}", n.id));
        }
        rule
    };

    let mut rules = Vec::new();
    // sys-in（系统代理）的规则放最前，自成一套：员工自己的上网流量，内网直连，不套下面给 Loov 流量的
    // 内网丢弃与遥测拦截
    if let Some(sys) = layout.sys {
        inbounds.push(json!({ "tag": "sys-http", "listen": "127.0.0.1", "port": sys.ports.http, "protocol": "http",
                              "settings": { "allowTransparent": false } }));
        inbounds.push(json!({ "tag": "sys-socks", "listen": "127.0.0.1", "port": sys.ports.socks, "protocol": "socks",
                              "settings": { "auth": "noauth", "udp": false } }));
        let sys_in = json!(["sys-http", "sys-socks"]);
        rules.push(json!({ "type": "field", "inboundTag": sys_in, "ip": ["geoip:private"], "outboundTag": "direct" }));
        if sys.mode == NetMode::Rule {
            rules.push(json!({ "type": "field", "inboundTag": sys_in, "domain": ["geosite:cn"], "outboundTag": "direct" }));
            rules.push(to_nodes(json!({ "type": "field", "inboundTag": sys_in, "domain": ["geosite:geolocation-!cn"] })));
            rules.push(json!({ "type": "field", "inboundTag": sys_in, "ip": ["geoip:cn"], "outboundTag": "direct" }));
            // 兜底规则按 IP 写：两份域名表都没认出的域名先解析（IPIfNonMatch），国内 IP 走上一条直连
            rules.push(to_nodes(json!({ "type": "field", "inboundTag": sys_in, "ip": ["0.0.0.0/0", "::/0"] })));
        } else {
            rules.push(to_nodes(json!({ "type": "field", "inboundTag": sys_in, "network": "tcp,udp" })));
        }
    }
    // 内网 / 链路本地（含云元数据 169.254.169.254）一律丢弃，服务端也有同样的规则；
    // 经通道的遥测上报也丢弃（agy 不论引导里勾没勾「分享数据」都会往 Clearcut 报，见 TELEMETRY_BLOCK）
    rules.push(json!({ "type": "field", "ip": ["geoip:private"], "outboundTag": "block" }));
    rules.push(json!({ "type": "field", "domain": TELEMETRY_BLOCK, "outboundTag": "block" }));
    for p in layout.probes {
        inbounds.push(json!({ "tag": p.tag(), "listen": "127.0.0.1", "port": p.port, "protocol": "http",
                              "settings": { "accounts": accounts, "allowTransparent": false } }));
        rules.push(json!({ "type": "field", "inboundTag": [p.tag()], "outboundTag": format!("{}{}", p.route.prefix(), p.node_id) }));
    }

    let main_in = json!(["http-in", "socks-in"]);
    // 「代理」模式要按 IP 认国内网站（见上面 sys-in 的兜底规则）；其余规则都在第一轮按入站命中，不会多解析
    let strategy = if layout.sys.is_some_and(|s| s.mode == NetMode::Rule) { "IPIfNonMatch" } else { "AsIs" };
    let mut cfg = json!({
        "log": log,
        "inbounds": inbounds,
        "outbounds": outbounds,
        "routing": { "domainStrategy": strategy, "rules": [] }
    });

    if nodes.len() > 1 {
        cfg["observatory"] = json!({
            "subjectSelector": [prefix],
            "probeURL": "https://www.gstatic.com/generate_204",
            "probeInterval": "1m",
            "enableConcurrency": true
        });
        cfg["routing"]["balancers"] = json!([{
            "tag": "auto", "selector": [prefix],
            "strategy": { "type": "leastPing" },
            "fallbackTag": format!("{prefix}{}", nodes[0].id)
        }]);
    }
    if !nodes.is_empty() {
        rules.push(to_nodes(json!({ "type": "field", "inboundTag": main_in, "network": "tcp,udp" })));
    }
    cfg["routing"]["rules"] = Value::Array(rules);
    cfg
}

/// 经安全通道一律丢弃的遥测上报域名。`play.googleapis.com` 是 Google 的 Clearcut 日志上报：
/// Loov 页启动的 agy（API Key 模式）不管首次引导里「分享交互数据」勾没勾、`telemetryEnabled` 设没设，
/// 每轮对话都会往这里报（2026-10-26 实测 agy 1.4.7）。Claude Code / Codex 用不到它。
pub const TELEMETRY_BLOCK: &[&str] = &["full:play.googleapis.com"];

/// 找 xray 可执行文件：`LOOV_XRAY_BIN` → 可执行文件同目录 → `<app_config_dir>/loov/bin`。
/// 找到后校验 sha256 在白名单内（`LOOV_XRAY_ALLOW_UNVERIFIED=1` 可跳过，仅限开发）。
pub fn locate_xray() -> Result<PathBuf, String> {
    // (路径, 是否随应用打包)。随包的 sidecar 与主程序同在应用目录内，信任边界相同；macOS 打包时还会
    // ad-hoc 签名改写二进制，哈希必然与官方文件不同，所以只校验「外部」来源的 xray。
    let mut candidates: Vec<(PathBuf, bool)> = Vec::new();
    // 审计 S1-06：仅调试 / 测试构建允许用环境变量指定 xray；release 只用随包 / 用户目录
    if let Some(p) = super::dev_env_override("LOOV_XRAY_BIN") {
        candidates.push((PathBuf::from(p), false));
    }
    let names: &[&str] = if cfg!(windows) {
        &["xray.exe", "xray-x86_64-pc-windows-msvc.exe"]
    } else if cfg!(target_os = "macos") {
        &["xray", "xray-aarch64-apple-darwin", "xray-x86_64-apple-darwin"]
    } else {
        &["xray", "xray-x86_64-unknown-linux-gnu"]
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for n in names {
                candidates.push((dir.join(n), true));
            }
            // Tauri 的 resources 目录（AppImage / deb 里 lib/<app>/ 下）
            candidates.push((dir.join("resources").join("xray"), true));
            candidates.push((dir.join("../lib/loov-agent/xray"), true));
        }
    }
    for n in names {
        candidates.push((loov_dir().join("bin").join(n), false));
    }
    let (found, bundled) = candidates
        .into_iter()
        .find(|(p, _)| p.is_file())
        .ok_or_else(|| "找不到 xray 可执行文件（可设置 LOOV_XRAY_BIN）".to_string())?;
    if bundled {
        if let Ok(bytes) = std::fs::read(&found) {
            log::info!("[loov] 使用随包 xray（sha256 {}…）", &format!("{:x}", Sha256::digest(&bytes))[..12]);
        }
    } else {
        verify_xray(&found)?;
    }
    Ok(found)
}

fn verify_xray(path: &Path) -> Result<(), String> {
    // 审计 S1-06：跳过校验的逃生门仅在调试 / 测试构建有效
    if super::dev_env_override("LOOV_XRAY_ALLOW_UNVERIFIED").as_deref() == Some("1") {
        return Ok(());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("读取 xray 失败：{e}"))?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if xray_allowlist().iter().any(|h| *h == digest) {
        Ok(())
    } else {
        Err(format!("xray 校验失败（sha256 {}… 不在白名单）", &digest[..12]))
    }
}

/// geoip.dat 所在目录：`LOOV_XRAY_ASSETS` → xray 同目录 → Tauri 资源目录（deb/AppImage 在
/// `../lib/loov-agent/xray-assets/`，开发态在可执行文件旁的 `resources/xray-assets/`）。
fn asset_dir(xray_bin: &Path) -> PathBuf {
    if let Some(p) = super::dev_env_override("LOOV_XRAY_ASSETS") {
        return PathBuf::from(p);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(d) = xray_bin.parent() {
        candidates.push(d.to_path_buf());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            candidates.push(d.join("../lib/loov-agent/xray-assets"));
            candidates.push(d.join("../lib/Loov Agent/xray-assets"));
            candidates.push(d.join("resources/xray-assets"));
            candidates.push(d.join("xray-assets"));
            candidates.push(d.join("../Resources/xray-assets")); // macOS .app
        }
    }
    candidates.push(loov_dir().join("bin"));
    candidates
        .into_iter()
        .find(|d| d.join("geoip.dat").is_file())
        .unwrap_or_else(|| xray_bin.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")))
}

/// 列出命令行里含 `needle` 的进程 pid（不含自己）。Linux 走 /proc，其它平台用 pgrep。
fn processes_with_arg(needle: &str) -> Vec<u32> {
    let me = std::process::id();
    let mut out = Vec::new();
    #[cfg(target_os = "linux")]
    {
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
                if pid == me {
                    continue;
                }
                if let Ok(cmd) = std::fs::read(e.path().join("cmdline")) {
                    if String::from_utf8_lossy(&cmd).contains(needle) {
                        out.push(pid);
                    }
                }
            }
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        if let Ok(o) = std::process::Command::new("pgrep").args(["-f", needle]).output() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Ok(pid) = line.trim().parse::<u32>() {
                    if pid != me {
                        out.push(pid);
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    {
        // Windows 没有 /proc 与 pgrep：用 CIM 列出 xray.exe 的命令行再比对
        use std::os::windows::process::CommandExt;
        let script = "Get-CimInstance Win32_Process -Filter \"Name='xray.exe'\" | ForEach-Object { \"$($_.ProcessId)`t$($_.CommandLine)\" }";
        if let Ok(o) = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
        {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Some((pid, cmdline)) = line.split_once('\t') {
                    if cmdline.to_lowercase().contains(&needle.to_lowercase()) {
                        if let Ok(pid) = pid.trim().parse::<u32>() {
                            if pid != me {
                                out.push(pid);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output();
    }
}

/// 清理上次残留：任何命令行引用了本运行目录配置文件的 xray 进程都结束掉，并删掉旧配置。
pub fn kill_orphans() -> usize {
    let dir = run_dir();
    let needle = dir.to_string_lossy().into_owned();
    let pids = processes_with_arg(&needle);
    for pid in &pids {
        log::warn!("[loov] 结束残留的 xray 进程 pid={pid}");
        kill_pid(*pid);
    }
    if !pids.is_empty() {
        std::thread::sleep(Duration::from_millis(400));
        #[cfg(unix)]
        for pid in &pids {
            unsafe {
                libc::kill(*pid as i32, libc::SIGKILL);
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("xray-") && name.ends_with(".json") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::remove_file(pid_file());
    pids.len()
}

/// 起 sidecar：清残留 → 分配端口 → 写配置(0600) → spawn → 等端口可连。
pub async fn start(nodes: &[TunnelNode], xray_bin: &Path, opts: &StartOpts<'_>) -> Result<TunnelHandle, String> {
    let StartOpts { route, upstream, with_probes, sys, .. } = *opts;
    if nodes.is_empty() {
        return Err("服务器没有下发任何隧道节点".into());
    }
    let dir = run_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建运行目录失败：{e}"))?;
    restrict_dir(&dir);
    kill_orphans();

    let mut routes = vec![Route::Direct];
    if upstream.is_some() {
        routes.push(Route::Proxy);
    }
    let probe_count = if with_probes { nodes.len() * routes.len() } else { 0 };
    let all_ports = free_ports(2 + probe_count)?;
    let (ports, pass) = match &opts.reuse {
        Some((p, pass)) => (*p, pass.clone()),
        None => (Ports { http: all_ports[0], socks: all_ports[1] }, random_secret(18)),
    };
    let mut probe_ports = all_ports[2..].iter();
    let probes: Vec<Probe> = if with_probes {
        nodes
            .iter()
            .flat_map(|n| routes.iter().map(move |r| (n, *r)))
            .map(|(n, r)| Probe { node_id: n.id.clone(), route: r, port: *probe_ports.next().unwrap() })
            .collect()
    } else {
        Vec::new()
    };

    let access_log = if super::dev_env_override("LOOV_XRAY_ACCESS_LOG").as_deref() == Some("1") {
        Some(dir.join("access.log"))
    } else {
        None
    };
    let layout = Layout { route, upstream, probes: &probes, sys };
    let cfg = build_config(nodes, ports, PROXY_USER, &pass, access_log.as_deref(), &layout);
    let config_path = dir.join(format!("xray-{}.json", random_secret(6)));
    write_private(&config_path, serde_json::to_string_pretty(&cfg).unwrap().as_bytes())?;

    let log = std::fs::File::create(log_file()).map_err(|e| format!("创建日志失败：{e}"))?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = tokio::process::Command::new(xray_bin);
    cmd.args(["run", "-c"])
        .arg(&config_path)
        .env("XRAY_LOCATION_ASSET", asset_dir(xray_bin))
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：不弹控制台窗口
    }
    let child = cmd.spawn().map_err(|e| format!("启动 xray 失败：{e}"))?;
    if let Some(pid) = child.id() {
        let _ = write_private(&pid_file(), format!("{pid}\n{}\n", config_path.display()).as_bytes());
    }
    let mut handle = TunnelHandle {
        child,
        http_port: ports.http,
        socks_port: ports.socks,
        user: PROXY_USER.to_string(),
        pass,
        config_path,
        node_ids: nodes.iter().map(|n| n.id.clone()).collect(),
        route,
        via: if route == Route::Proxy { upstream.map(|u| u.to_string()) } else { None },
        probes,
        sys,
        xray_bin: xray_bin.to_path_buf(),
        upstream: upstream.cloned(),
        usable: nodes.to_vec(),
        probe_report: Vec::new(),
        pin_fallback: false,
    };

    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        if !handle.is_alive() {
            let tail = log_tail(5);
            handle.stop().await;
            return Err(format!("xray 启动后立即退出：{tail}"));
        }
        if tokio::net::TcpStream::connect(("127.0.0.1", ports.http)).await.is_ok() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            handle.stop().await;
            return Err("xray 在 5 秒内没有开始监听".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(handle)
}

/// xray 日志最后 n 行，用「 | 」连起来（略过每条连接都会打的 accepted 行）。
fn log_tail(n: usize) -> String {
    let text = std::fs::read_to_string(log_file()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty() && !l.contains(" accepted ")).collect();
    lines[lines.len().saturating_sub(n)..].join(" | ")
}

/// xray 日志里最后 n 条告警 / 错误（启动横幅和「started」不算）。
fn log_problems(n: usize) -> String {
    let text = std::fs::read_to_string(log_file()).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("[Error]") || (l.contains("[Warning]") && !l.contains(" started")))
        .collect();
    lines[lines.len().saturating_sub(n)..].join(" | ")
}

/// 错误链最底层的原因（如「Connection refused」「operation timed out」）。
fn root_cause(e: &(dyn std::error::Error + 'static)) -> String {
    let mut cur = e;
    while let Some(src) = cur.source() {
        cur = src;
    }
    cur.to_string()
}

/// 本进程直接对节点做一次 TCP 连接：区分「端口不通」和「TCP 通但隧道握手被干扰」。
async fn tcp_reach(address: &str, port: u16) -> Result<(), String> {
    match tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect((address, port))).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("超时".into()),
    }
}

/// 把错误连同 `source()` 链展开。reqwest 的 Display 只有「error sending request for url」，
/// 真正的原因（连接被拒、超时、代理 407……）都在 source 里。
pub fn err_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = vec![e.to_string()];
    let mut cur = e.source();
    while let Some(src) = cur {
        let m = src.to_string();
        if !m.is_empty() && !parts.iter().any(|p| p.contains(&m)) {
            parts.push(m);
        }
        cur = src.source();
    }
    parts.join("：")
}

fn client_via(proxy_url: &str, timeout: Duration) -> Result<reqwest::Client, String> {
    let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| e.to_string())?;
    reqwest::Client::builder()
        .proxy(proxy)
        .pool_max_idle_per_host(0)
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

/// 自检：端口必须被 xray 独占；经代理连发 5 次健康检查全部成功（残留进程平分连接时约一半 407）。
pub async fn self_check(handle: &TunnelHandle, health_url: &str) -> Result<(), String> {
    match std::net::TcpListener::bind(("127.0.0.1", handle.http_port)) {
        Ok(_) => return Err("xray 没有占住分配的端口".into()),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
        Err(e) => return Err(format!("端口探测失败：{e}")),
    }
    let client = client_via(&handle.proxy_url(), Duration::from_secs(10))?;
    for i in 1..=5 {
        let resp = client
            .get(health_url)
            .send()
            .await
            .map_err(|e| format!("第 {i} 次健康检查失败：{}", err_chain(&e)))?;
        if resp.status() == reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED {
            return Err("代理口令被拒绝：可能有残留的 xray 进程抢占了端口".into());
        }
        if !resp.status().is_success() {
            return Err(format!("第 {i} 次健康检查返回 HTTP {}", resp.status()));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub node_id: String,
    pub route: Route,
    /// Ok(经节点做一次健康检查的耗时毫秒) / Err(原因)
    pub result: Result<u64, String>,
}

/// 经每个探测入站各发一次健康检查（并发，单次最长 [`PROBE_TIMEOUT`]）。直连失败的再补一次 TCP 探测，
/// 让报错能分清是端口被封还是握手被干扰（经 xray 的请求失败时只能看到「连接被重置」）。
pub async fn probe(handle: &TunnelHandle, nodes: &[TunnelNode], health_url: &str) -> Vec<ProbeResult> {
    let tasks = handle.probes.iter().map(|p| {
        let url = format!("http://{}:{}@127.0.0.1:{}", handle.user, handle.pass, p.port);
        let node = nodes.iter().find(|n| n.id == p.node_id);
        async move {
            let result = async {
                let client = client_via(&url, PROBE_TIMEOUT)?;
                let started = std::time::Instant::now();
                let resp = client.get(health_url).send().await.map_err(|e| root_cause(&e))?;
                if resp.status().is_success() {
                    Ok(started.elapsed().as_millis() as u64)
                } else {
                    Err(format!("HTTP {}", resp.status()))
                }
            }
            .await;
            let result = match (result, node) {
                (Err(cause), Some(n)) if p.route == Route::Direct => Err(match tcp_reach(&n.address, n.port).await {
                    Err(e) => format!("TCP 连不上（{e}）"),
                    Ok(()) => format!("TCP 可达但隧道握手失败（{cause}），可能被网络干扰"),
                }),
                (r, _) => r,
            };
            ProbeResult { node_id: p.node_id.clone(), route: p.route, result }
        }
    });
    futures::future::join_all(tasks).await
}

/// 按「直连可用 > 经代理可用」、同档按下发顺序挑节点。全部不通时返回逐节点的原因汇总。
pub fn choose(nodes: &[TunnelNode], results: &[ProbeResult], upstream: Option<&Upstream>) -> Result<(Route, Vec<TunnelNode>), String> {
    let ok = |n: &TunnelNode, r: Route| results.iter().any(|x| x.node_id == n.id && x.route == r && x.result.is_ok());
    for route in [Route::Direct, Route::Proxy] {
        let picked: Vec<TunnelNode> = nodes.iter().filter(|n| ok(n, route)).cloned().collect();
        if !picked.is_empty() {
            return Ok((route, picked));
        }
    }
    let mut lines = Vec::new();
    for n in nodes {
        let mut parts = Vec::new();
        for r in results.iter().filter(|x| x.node_id == n.id) {
            let label = match (r.route, upstream) {
                (Route::Proxy, Some(u)) => format!("{} {u}", r.route.label()),
                _ => r.route.label().to_string(),
            };
            parts.push(format!("{label}：{}", r.result.as_ref().err().map(String::as_str).unwrap_or("未知错误")));
        }
        lines.push(format!("{}（{}:{}）{}", n.name, n.address, n.port, parts.join("；")));
    }
    let hint = match upstream {
        None => "当前网络可能屏蔽了隧道节点。如在使用 VPN，请开启 TUN / 全局模式，或打开系统代理后重试",
        Some(_) => "直连和经本机代理都连不上隧道节点，请确认代理可用，或开启 VPN 的 TUN / 全局模式后重试",
    };
    Err(format!("无法连接隧道节点：{}。{hint}", lines.join("；")))
}

/// 建隧道：探测 → 选节点和走法 → （必要时）按结果重启 xray → 5 次健康检查。
/// 全部节点直连都通、没有手选节点、也不开 sys-in 时直接沿用探测用的进程，常见情况不多一次重启。
pub async fn establish(nodes: &[TunnelNode], xray_bin: &Path, health_url: &str, sel: &Selection) -> Result<TunnelHandle, String> {
    let upstream = super::sysproxy::detect();
    if let Some(u) = &upstream {
        log::info!("[loov] 检测到本机代理 {u}（{}），直连不通时经它连接节点", u.source);
    }
    let probing = start(
        nodes,
        xray_bin,
        &StartOpts { route: Route::Direct, upstream: upstream.as_ref(), with_probes: true, sys: None, reuse: None },
    )
    .await?;
    let results = probe(&probing, nodes, health_url).await;
    for r in &results {
        if let Err(e) = &r.result {
            log::warn!("[loov] 节点 {} {}不可用：{e}", r.node_id, r.route.label());
        }
    }
    let (route, usable) = match choose(nodes, &results, upstream.as_ref()) {
        Ok(v) => v,
        Err(e) => {
            let tail = log_problems(3);
            probing.stop().await;
            return Err(if tail.is_empty() { e } else { format!("{e}（xray：{tail}）") });
        }
    };
    let report: Vec<(TunnelNode, Result<u64, String>)> = nodes
        .iter()
        .map(|n| {
            let r = results
                .iter()
                .find(|x| x.node_id == n.id && x.route == route)
                .or_else(|| results.iter().find(|x| x.node_id == n.id))
                .map(|x| x.result.clone())
                .unwrap_or_else(|| Err("未探测".into()));
            (n.clone(), r)
        })
        .collect();
    let (picked, pin_fallback) = pick_nodes(&usable, sel.node.as_deref());
    let sys = sys_layout(sel.mode)?;
    let mut handle = if route == Route::Direct && picked.len() == nodes.len() && sys.is_none() {
        probing
    } else {
        log::info!(
            "[loov] 用{}连接节点 {:?}（模式 {:?}）",
            route.label(),
            picked.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            sel.mode
        );
        probing.stop().await;
        start(&picked, xray_bin, &StartOpts { route, upstream: upstream.as_ref(), with_probes: false, sys, reuse: None }).await?
    };
    handle.usable = usable;
    handle.probe_report = report;
    handle.pin_fallback = pin_fallback;
    if let Err(e) = self_check(&handle, health_url).await {
        handle.stop().await;
        return Err(e);
    }
    Ok(handle)
}

/// 手选的节点通过了探测就只用它；没通过（或没手选）用全部可用节点，前者标 fallback。
fn pick_nodes(usable: &[TunnelNode], preferred: Option<&str>) -> (Vec<TunnelNode>, bool) {
    match preferred {
        Some(id) => match usable.iter().find(|n| n.id == id) {
            Some(n) => (vec![n.clone()], false),
            None => (usable.to_vec(), true),
        },
        None => (usable.to_vec(), false),
    }
}

fn sys_layout(mode: NetMode) -> Result<Option<SysLayout>, String> {
    Ok(match mode {
        NetMode::Direct => None,
        m => Some(SysLayout { ports: super::netmode::pick_sys_ports()?, mode: m }),
    })
}

/// 换节点 / 换模式：沿用已探测的结果、原来的端口和口令重启 xray（已启动的应用不受影响），再做自检。
/// 手选的节点不在可用节点里时返回 Err，由调用方决定是否重新探测。
pub async fn reconfigure(old: TunnelHandle, health_url: &str, sel: &Selection) -> Result<TunnelHandle, (String, Option<TunnelHandle>)> {
    if let Some(id) = sel.node.as_deref() {
        if !old.usable.iter().any(|n| n.id == id) {
            return Err((format!("节点 {id} 当前不可用"), Some(old)));
        }
    }
    let (picked, _) = pick_nodes(&old.usable, sel.node.as_deref());
    // 模式没变就沿用 sys-in 端口（系统代理不用重设）
    let sys = match (sel.mode, old.sys) {
        (NetMode::Direct, _) => None,
        (m, Some(s)) => Some(SysLayout { ports: s.ports, mode: m }),
        (m, None) => match sys_layout(m) {
            Ok(s) => s,
            Err(e) => return Err((e, Some(old))),
        },
    };
    let reuse = Some((Ports { http: old.http_port, socks: old.socks_port }, old.pass.clone()));
    let (bin, upstream, usable, report, route) =
        (old.xray_bin.clone(), old.upstream.clone(), old.usable.clone(), old.probe_report.clone(), old.route);
    old.stop().await;
    let mut handle = start(&picked, &bin, &StartOpts { route, upstream: upstream.as_ref(), with_probes: false, sys, reuse })
        .await
        .map_err(|e| (e, None))?;
    handle.usable = usable;
    handle.probe_report = report;
    if let Err(e) = self_check(&handle, health_url).await {
        handle.stop().await;
        return Err((e, None));
    }
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loov::hub_client::Reality;

    fn node(id: &str) -> TunnelNode {
        TunnelNode {
            id: id.into(),
            name: "东京".into(),
            region: "JP".into(),
            address: "203.0.113.10".into(),
            port: 9443,
            flow: "xtls-rprx-vision".into(),
            encryption: "mlkem768x25519plus.native.0rtt.AAA".into(),
            reality: Reality {
                server_name: "www.amazon.co.jp".into(),
                fingerprint: "chrome".into(),
                public_key: "pk".into(),
                short_id: "0a1b".into(),
                mldsa65_verify: "vk".into(),
            },
            uuid: "11111111-1111-1111-1111-111111111111".into(),
        }
    }

    #[test]
    fn single_node_config_matches_hub_test_shape() {
        let cfg = build_config(&[node("jp1")], Ports { http: 18089, socks: 18088 }, "loov", "pw", None, &direct());
        assert_eq!(cfg["inbounds"][0]["protocol"], "http");
        assert_eq!(cfg["inbounds"][0]["settings"]["accounts"][0]["pass"], "pw");
        assert_eq!(cfg["inbounds"][1]["settings"]["auth"], "password");
        let ob = &cfg["outbounds"][0];
        assert_eq!(ob["tag"], "node-jp1");
        assert_eq!(ob["streamSettings"]["security"], "reality");
        assert_eq!(ob["streamSettings"]["realitySettings"]["password"], "pk");
        assert_eq!(ob["streamSettings"]["realitySettings"]["mldsa65Verify"], "vk");
        assert_eq!(ob["settings"]["vnext"][0]["users"][0]["encryption"], "mlkem768x25519plus.native.0rtt.AAA");
        assert_eq!(cfg["outbounds"][1]["protocol"], "blackhole");
        assert_eq!(cfg["routing"]["rules"][0]["ip"][0], "geoip:private");
        assert_eq!(cfg["routing"]["rules"][1]["domain"][0], "full:play.googleapis.com");
        assert_eq!(cfg["routing"]["rules"][1]["outboundTag"], "block");
        assert_eq!(cfg["routing"]["rules"][2]["outboundTag"], "node-jp1");
        assert!(cfg.get("observatory").is_none());
    }

    fn sys(mode: NetMode) -> Layout<'static> {
        Layout {
            route: Route::Direct,
            upstream: None,
            probes: &[],
            sys: Some(SysLayout { ports: SysPorts { http: 17890, socks: 17891 }, mode }),
        }
    }

    fn rules_for<'a>(cfg: &'a Value, inbound: &str) -> Vec<&'a Value> {
        cfg["routing"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["inboundTag"].as_array().is_some_and(|t| t.iter().any(|x| x == inbound)))
            .collect()
    }

    #[test]
    fn direct_mode_has_no_open_inbound() {
        let cfg = build_config(&[node("jp1")], Ports { http: 1, socks: 2 }, "loov", "pw", None, &direct());
        let tags: Vec<&str> = cfg["inbounds"].as_array().unwrap().iter().map(|i| i["tag"].as_str().unwrap()).collect();
        assert_eq!(tags, ["http-in", "socks-in"]);
        assert!(cfg["outbounds"].as_array().unwrap().iter().all(|o| o["tag"] != "direct"));
        assert_eq!(cfg["routing"]["domainStrategy"], "AsIs");
    }

    #[test]
    fn global_mode_sends_everything_but_lan_to_nodes() {
        let cfg = build_config(&[node("jp1"), node("tw1")], Ports { http: 1, socks: 2 }, "loov", "pw", None, &sys(NetMode::Global));
        let sys_in = cfg["inbounds"].as_array().unwrap().iter().find(|i| i["tag"] == "sys-http").unwrap();
        assert_eq!(sys_in["port"], 17890);
        assert_eq!(sys_in["listen"], "127.0.0.1");
        assert!(sys_in["settings"].get("accounts").is_none(), "系统代理用的入站不带口令");
        let rules = rules_for(&cfg, "sys-http");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0]["outboundTag"], "direct");
        assert_eq!(rules[0]["ip"][0], "geoip:private");
        assert_eq!(rules[1]["balancerTag"], "auto");
        // sys-in 的规则排在给 Loov 流量的内网丢弃之前
        assert_eq!(cfg["routing"]["rules"][0]["inboundTag"][0], "sys-http");
        assert_eq!(cfg["routing"]["domainStrategy"], "AsIs");
        assert!(cfg["outbounds"].as_array().unwrap().iter().any(|o| o["tag"] == "direct" && o["protocol"] == "freedom"));
        // Loov 自己的入站照旧：带口令，经 balancer
        let main = rules_for(&cfg, "http-in");
        assert_eq!(main.len(), 1);
        assert_eq!(main[0]["balancerTag"], "auto");
    }

    #[test]
    fn rule_mode_sends_china_direct_and_resolves_unknown_domains() {
        let cfg = build_config(&[node("tw1")], Ports { http: 1, socks: 2 }, "loov", "pw", None, &sys(NetMode::Rule));
        let rules = rules_for(&cfg, "sys-socks");
        let pick = |i: usize| (rules[i]["domain"][0].as_str().or(rules[i]["ip"][0].as_str()).unwrap(), rules[i]["outboundTag"].as_str().unwrap());
        assert_eq!(pick(0), ("geoip:private", "direct"));
        assert_eq!(pick(1), ("geosite:cn", "direct"));
        assert_eq!(pick(2), ("geosite:geolocation-!cn", "node-tw1"));
        assert_eq!(pick(3), ("geoip:cn", "direct"));
        assert_eq!(pick(4), ("0.0.0.0/0", "node-tw1"));
        assert_eq!(cfg["routing"]["domainStrategy"], "IPIfNonMatch");
    }

    #[test]
    fn preferred_node_is_pinned_or_falls_back() {
        let usable = vec![node("jp4"), node("tw1")];
        let (p, fb) = pick_nodes(&usable, Some("tw1"));
        assert_eq!((p.len(), p[0].id.as_str(), fb), (1, "tw1", false));
        let (p, fb) = pick_nodes(&usable, Some("jp1"));
        assert_eq!((p.len(), fb), (2, true), "手选的不通：回到自动并标出来");
        let (p, fb) = pick_nodes(&usable, None);
        assert_eq!((p.len(), fb), (2, false));
    }

    #[test]
    fn multi_node_uses_balancer() {
        let cfg = build_config(&[node("jp1"), node("tw1")], Ports { http: 1, socks: 2 }, "loov", "pw", None, &direct());
        assert_eq!(cfg["observatory"]["subjectSelector"][0], "node-");
        assert_eq!(cfg["routing"]["balancers"][0]["fallbackTag"], "node-jp1");
        assert_eq!(cfg["routing"]["rules"][2]["balancerTag"], "auto");
    }

    fn direct() -> Layout<'static> {
        Layout { route: Route::Direct, upstream: None, probes: &[], sys: None }
    }

    fn upstream() -> Upstream {
        crate::loov::sysproxy::parse_url("socks5://u:p@127.0.0.1:10808", "t").unwrap()
    }

    #[test]
    fn probes_route_by_inbound_and_main_rule_is_scoped() {
        let up = upstream();
        let probes = vec![
            Probe { node_id: "jp1".into(), route: Route::Direct, port: 30001 },
            Probe { node_id: "jp1".into(), route: Route::Proxy, port: 30002 },
        ];
        let layout = Layout { route: Route::Direct, upstream: Some(&up), probes: &probes, sys: None };
        let cfg = build_config(&[node("jp1")], Ports { http: 1, socks: 2 }, "loov", "pw", None, &layout);
        let inbounds = cfg["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 4);
        assert_eq!(inbounds[3]["tag"], "probe-jp1-proxy");
        assert_eq!(inbounds[3]["port"], 30002);
        assert_eq!(inbounds[3]["settings"]["accounts"][0]["pass"], "pw");
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        assert_eq!(rules[0]["ip"][0], "geoip:private");
        assert_eq!(rules[1]["outboundTag"], "block");
        assert_eq!(rules[2]["outboundTag"], "node-jp1");
        assert_eq!(rules[3]["outboundTag"], "via-jp1");
        // 主规则只管主入站，排在探测规则之后
        assert_eq!(rules[4]["inboundTag"], json!(["http-in", "socks-in"]));
        assert_eq!(rules[4]["outboundTag"], "node-jp1");
        let obs = cfg["outbounds"].as_array().unwrap();
        let via = obs.iter().find(|o| o["tag"] == "via-jp1").unwrap();
        assert_eq!(via["streamSettings"]["sockopt"]["dialerProxy"], "upstream");
        let up_ob = obs.iter().find(|o| o["tag"] == "upstream").unwrap();
        assert_eq!(up_ob["protocol"], "socks");
        assert_eq!(up_ob["settings"]["servers"][0]["port"], 10808);
        assert_eq!(up_ob["settings"]["servers"][0]["users"][0]["user"], "u");
        assert!(obs.iter().find(|o| o["tag"] == "node-jp1").unwrap()["streamSettings"].get("sockopt").is_none());
    }

    #[test]
    fn proxy_route_uses_via_outbounds() {
        let up = upstream();
        let layout = Layout { route: Route::Proxy, upstream: Some(&up), probes: &[], sys: None };
        let cfg = build_config(&[node("jp1"), node("tw1")], Ports { http: 1, socks: 2 }, "loov", "pw", None, &layout);
        assert_eq!(cfg["routing"]["balancers"][0]["selector"][0], "via-");
        assert_eq!(cfg["routing"]["balancers"][0]["fallbackTag"], "via-jp1");
        assert_eq!(cfg["observatory"]["subjectSelector"][0], "via-");
    }

    fn res(id: &str, route: Route, ok: bool) -> ProbeResult {
        ProbeResult { node_id: id.into(), route, result: if ok { Ok(10) } else { Err("connection refused".into()) } }
    }

    #[test]
    fn choose_prefers_direct_then_proxy_and_keeps_order() {
        let nodes = vec![node("jp1"), node("tw1")];
        let (route, picked) = choose(&nodes, &[res("jp1", Route::Direct, false), res("tw1", Route::Direct, true)], None).unwrap();
        assert_eq!(route, Route::Direct);
        assert_eq!(picked.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["tw1"]);

        let up = upstream();
        let results = [
            res("jp1", Route::Direct, false),
            res("jp1", Route::Proxy, true),
            res("tw1", Route::Direct, false),
            res("tw1", Route::Proxy, true),
        ];
        let (route, picked) = choose(&nodes, &results, Some(&up)).unwrap();
        assert_eq!(route, Route::Proxy);
        assert_eq!(picked.len(), 2);
    }

    #[test]
    fn choose_reports_every_node_when_all_fail() {
        let nodes = vec![node("jp1")];
        let up = upstream();
        let err = choose(&nodes, &[res("jp1", Route::Direct, false), res("jp1", Route::Proxy, false)], Some(&up)).unwrap_err();
        assert!(err.contains("203.0.113.10:9443"), "{err}");
        assert!(err.contains("直连：connection refused"), "{err}");
        assert!(err.contains("经本机代理 socks5://127.0.0.1:10808"), "{err}");
        assert!(!err.contains(":p@"), "不应泄露代理口令：{err}");
        let err = choose(&nodes, &[res("jp1", Route::Direct, false)], None).unwrap_err();
        assert!(err.contains("TUN"), "{err}");
    }

    #[test]
    fn err_chain_includes_sources() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("error sending request")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let e = Outer(std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"));
        assert_eq!(err_chain(&e), "error sending request：connect timed out");
    }

    #[test]
    fn ports_are_distinct_and_free() {
        let p = pick_ports().unwrap();
        assert_ne!(p.http, p.socks);
        assert!(std::net::TcpListener::bind(("127.0.0.1", p.http)).is_ok());
    }

    #[test]
    fn verify_rejects_unknown_binary() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"not xray").unwrap();
        assert!(verify_xray(tmp.path()).is_err());
    }
}
