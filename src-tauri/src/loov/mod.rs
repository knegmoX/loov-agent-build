//! loov: Loov Agent 专属后端。
//!
//! 职责：登录 hub → 拉取设备配置（new-api Key、隧道节点）→ 起 xray sidecar → 把本地路由的上游
//! 代理指向 sidecar → 以统一供应商的形式写进 CC Switch，并给 Claude Code / Codex 注入加固键。
//!
//! 安全模型：
//! - 真实 new-api Key 只在本进程内存（[`api_key`]），供应商行里存占位 [`KEY_PLACEHOLDER`]，
//!   转发时在 `proxy/forwarder.rs` 替换；
//! - CLI 配置里写的是本地路由 bearer（`local_auth`），不是真实 Key；
//! - DB 里的全局代理固定为黑洞 [`BLACKHOLE_PROXY`]，真实隧道地址只应用到内存里的 HTTP
//!   客户端，任何失败态都不会退回直连（fail-closed）。
//!
//! 上游代码的挂载点全部带 `// loov:` 注释，rebase 时 `grep -rn "// loov:"` 核对。

pub mod apps;
pub mod attribution;
pub mod commands;
pub mod credentials;
pub mod fallback;
pub mod flows;
pub mod gemini_route;
pub mod hub_client;
pub mod local_auth;
pub mod mcp_proxy;
pub mod netmode;
pub mod privacy;
pub mod provision;
pub mod restore;
pub mod safety;
pub mod sysproxy;
pub mod tunnel;

use std::sync::{OnceLock, RwLock};

use credentials::Credentials;
use tunnel::TunnelHandle;

/// 仅在调试 / 测试（test-hooks feature）构建读取的环境变量覆盖（审计 S1-06）。
/// release 发布包一律忽略，防止同机进程用环境变量把 hub 指向钓鱼地址或跳过 xray 校验。
pub(crate) fn dev_env_override(key: &str) -> Option<String> {
    #[cfg(any(debug_assertions, feature = "test-hooks"))]
    {
        std::env::var(key).ok().filter(|v| !v.trim().is_empty())
    }
    #[cfg(not(any(debug_assertions, feature = "test-hooks")))]
    {
        let _ = key;
        None
    }
}

/// 启动挂载点的顺序（审计 S6-02）：黑洞代理 → Loov 起隧道 → 上游 `controller::startup` → 写加固键。
/// 这是顺序语义，合并上游时调用被挪动编译器察觉不到，所以各挂载点打标记、上游入口检查前置阶段；
/// 另有源码顺序测试（见 tests）。只有 `lib.rs` 打下第一个标记后才开始跟踪，单测 / 集成测试直接调
/// 各函数时不受影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    BlackholePinned = 1,
    LoovStarted = 2,
    UpstreamStarted = 3,
    Hardened = 4,
}

static PHASE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// 挂载点到达 `p`。必须紧接上一阶段；第一个阶段（黑洞）开启跟踪。
pub fn startup_mark(p: Phase) {
    use std::sync::atomic::Ordering;
    let cur = PHASE.load(Ordering::SeqCst);
    if cur == 0 && p != Phase::BlackholePinned {
        // 没开启跟踪：要么是测试直接调用，要么 lib.rs 的黑洞挂载点丢了（由源码顺序测试兜住）
        return;
    }
    if cur + 1 != p as u8 {
        startup_order_violation(&format!("到达 {p:?} 时上一阶段是 {cur}"));
    }
    PHASE.store(p as u8, Ordering::SeqCst);
}

/// 上游代码里的检查点：要求当前恰好处于 `p`。
pub fn startup_expect(p: Phase) {
    let cur = PHASE.load(std::sync::atomic::Ordering::SeqCst);
    if cur != 0 && cur != p as u8 {
        startup_order_violation(&format!("期望处于 {p:?}，实际是 {cur}"));
    }
}

/// 顺序被破坏：调试构建直接 panic 让问题在开发期暴露；发布构建 fail-closed——出站全部指向黑洞，
/// 状态置为隧道失败，界面上能看到。
fn startup_order_violation(msg: &str) {
    log::error!("[loov] 启动顺序被破坏：{msg}");
    if cfg!(debug_assertions) {
        panic!("[loov] 启动顺序被破坏：{msg}");
    }
    let _ = crate::proxy::http_client::apply_proxy(Some(BLACKHOLE_PROXY));
    set_status(Status::TunnelFailed { reason: format!("启动顺序异常，已阻断出站：{msg}") });
}

/// hub 地址（调试 / 测试可用环境变量 `LOOV_HUB_BASE` 覆盖；release 固定为公司地址）。
pub fn hub_base() -> String {
    dev_env_override("LOOV_HUB_BASE").unwrap_or_else(|| "https://me.loov.cc/hub".to_string())
}

/// 统一供应商 id；子供应商为 `universal-claude-loov` / `universal-codex-loov`。
pub const PROVIDER_ID: &str = "loov";
/// 供应商行里的 Key 占位；转发时换成内存里的真实 Key。
pub const KEY_PLACEHOLDER: &str = "LOOV_MANAGED";
/// 黑洞代理：没有任何进程监听的本机端口，连接立即被拒。
pub const BLACKHOLE_PROXY: &str = "http://127.0.0.1:1";

/// 运行状态（给前端 / 测试看）。
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Status {
    /// 没有凭证，等待登录。
    NoCredentials,
    /// 有凭证，启动时正在拉配置 / 建隧道（走本机代理时要几秒）。界面据此显示「正在连接」并加快轮询，
    /// 而不是把还没就绪的初始状态当成「未登录」。
    Starting,
    /// 有凭证但 hub 不可达且无缓存。
    Offline { reason: String },
    /// hub 拒绝了设备令牌，需要重新登录。
    ReloginRequired { code: String },
    /// 隧道启动或自检失败。
    TunnelFailed { reason: String },
    /// 隧道就绪。
    Ready {
        username: String,
        device_id: String,
        proxy_url: String,
        http_port: u16,
        /// 实际在用的节点（探测后可用的那些）。
        nodes: Vec<String>,
        /// 直连节点，还是经本机代理连节点。
        route: tunnel::Route,
        /// 经本机代理时的代理地址（不含口令）。
        via: Option<String>,
        /// 节点与网络模式（界面上的节点下拉、模式开关）。
        net: NetView,
    },
}

/// 一个节点在界面上的样子。
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct NodeView {
    pub id: String,
    pub name: String,
    /// 探测通过时的耗时（毫秒）；None = 不可用
    pub latency_ms: Option<u64>,
    /// 不可用的原因
    pub error: Option<String>,
}

/// 节点与网络模式的状态。
#[derive(Debug, Clone, Default, serde::Serialize, PartialEq)]
pub struct NetView {
    /// 全部下发的节点（含不可用的），按下发顺序
    pub nodes: Vec<NodeView>,
    /// 现在走的节点
    pub active: Option<String>,
    /// 员工手选了节点（否则是自动）
    pub pinned: Option<String>,
    /// 手选的节点不可用，这次回到了自动
    pub pin_fallback: bool,
    pub mode: netmode::NetMode,
    /// 「代理 / 全局」模式下系统代理指向的地址
    pub sys_proxy: Option<String>,
    /// 设置系统代理失败的原因（隧道照常可用）
    pub sys_error: Option<String>,
}

impl NetView {
    pub fn of(handle: &TunnelHandle, prefs: &netmode::Prefs, sys_error: Option<String>) -> Self {
        NetView {
            nodes: handle
                .probe_report
                .iter()
                .map(|(n, r)| NodeView {
                    id: n.id.clone(),
                    name: n.name.clone(),
                    latency_ms: r.as_ref().ok().copied(),
                    error: r.as_ref().err().cloned(),
                })
                .collect(),
            active: handle.active_node(),
            pinned: prefs.node.clone(),
            pin_fallback: handle.pin_fallback,
            mode: prefs.mode,
            sys_proxy: handle.sys.filter(|_| sys_error.is_none()).map(|s| format!("127.0.0.1:{}", s.ports.http)),
            sys_error,
        }
    }
}

impl Status {
    /// 写日志用：隧道代理地址里的随机口令换成 `***`。
    pub fn redacted(&self) -> Status {
        let mut s = self.clone();
        if let Status::Ready { proxy_url, .. } = &mut s {
            if let Ok(mut u) = url::Url::parse(proxy_url) {
                if u.password().is_some() {
                    let _ = u.set_password(Some("***"));
                    *proxy_url = u.to_string().trim_end_matches('/').to_string();
                }
            }
        }
        s
    }
}

pub struct LoovState {
    creds: RwLock<Option<Credentials>>,
    status: RwLock<Status>,
    tunnel: tokio::sync::Mutex<Option<TunnelHandle>>,
}

static STATE: OnceLock<LoovState> = OnceLock::new();

pub(crate) fn state() -> &'static LoovState {
    STATE.get_or_init(|| LoovState {
        creds: RwLock::new(None),
        status: RwLock::new(Status::NoCredentials),
        tunnel: tokio::sync::Mutex::new(None),
    })
}

/// 是否已登录（有凭证）。退出时据此决定是否跳过上游的「指回直连」。
pub fn active() -> bool {
    state().creds.read().map(|c| c.is_some()).unwrap_or(false)
}

/// 内存里某个应用的 Key（hub 为每个应用单独建的，见 [`attribution`]）；没有就退回通用 Key。
pub fn api_key_for(app: Option<&str>) -> Option<String> {
    let per_app = app.and_then(|a| {
        state()
            .creds
            .read()
            .ok()
            .and_then(|c| c.as_ref().and_then(|c| c.bootstrap.as_ref().and_then(|b| b.api.keys.get(a).cloned())))
    });
    per_app.filter(|k| !k.is_empty()).or_else(api_key)
}

/// 内存里的真实 new-api Key。
pub fn api_key() -> Option<String> {
    state()
        .creds
        .read()
        .ok()
        .and_then(|c| c.as_ref().and_then(|c| c.bootstrap.as_ref().map(|b| b.api.key.clone())))
        // 从磁盘读回、尚未向 hub 换到 Key 时是空串：按未登录处理（fail-closed）
        .filter(|k| !k.is_empty())
}

/// bootstrap 下发的受信 API 地址 host（如 me.loov.cc）。
pub fn trusted_api_host() -> Option<String> {
    state()
        .creds
        .read()
        .ok()
        .and_then(|c| c.as_ref().and_then(|c| c.bootstrap.as_ref().map(|b| b.api.base_url.clone())))
        .and_then(|u| host_of(&u))
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
}

/// loov: 真实 Key 注入守卫（审计 S1-01）。只有当供应商是 Loov 托管的子供应商、且出站 base_url 的 host
/// 等于 bootstrap 下发的受信 host 时，才允许把内存里的真实公司 Key 注入到该出站请求。
/// 否则（员工自建占位供应商、或把 base_url 指向任意地址）一律拒绝，防止真实 Key 被导出。
pub fn may_inject_key(provider_id: &str, base_url: &str) -> bool {
    may_inject_key_with(provider_id, base_url, trusted_api_host().as_deref())
}

/// forwarder 的凭据注入决策（审计 S8-02：从 forwarder 抽出来，fail-closed 分支才能进 CI 单测）。
/// 供应商行的 Key 不是占位 → `Ok(None)`，原样转发；是占位且守卫通过、已登录、隧道就绪 →
/// `Ok(Some(真实 Key))`；其余一律 `Err`，请求被拒而不是带着占位或错误的 Key 发出去。
///
/// 未登录 / 隧道未连用 400（`ConfigError`）：CLI 对 5xx 会反复重试、卡几分钟，对 401 会提示
/// 「请 /login」误导员工；400 直接把原因显示出来。
pub fn resolve_placeholder_key(
    provider_id: &str,
    base_url: &str,
    row_key: &str,
    headers: &axum::http::HeaderMap,
) -> Result<Option<String>, crate::proxy::ProxyError> {
    let tunnel = match status() {
        Status::Ready { .. } => Ok(()),
        Status::TunnelFailed { reason } | Status::Offline { reason } => Err(reason),
        _ => Err(String::new()),
    };
    let key = api_key_for(attribution::app_of(provider_id, headers));
    resolve_placeholder_key_with(provider_id, base_url, row_key, trusted_api_host().as_deref(), key.as_deref(), tunnel)
}

fn resolve_placeholder_key_with(
    provider_id: &str,
    base_url: &str,
    row_key: &str,
    trusted_host: Option<&str>,
    real_key: Option<&str>,
    tunnel: Result<(), String>,
) -> Result<Option<String>, crate::proxy::ProxyError> {
    use crate::proxy::ProxyError;
    if row_key != KEY_PLACEHOLDER {
        return Ok(None);
    }
    if !may_inject_key_with(provider_id, base_url, trusted_host) {
        return Err(ProxyError::AuthError("该供应商不被 Loov Agent 信任，已拒绝注入凭据".to_string()));
    }
    let Some(key) = real_key else {
        return Err(ProxyError::ConfigError("Loov Agent 未登录：请打开 Loov Agent 登录后重试".to_string()));
    };
    if let Err(reason) = tunnel {
        let detail = if reason.is_empty() { String::new() } else { format!("（{reason}）") };
        return Err(ProxyError::ConfigError(format!(
            "Loov 安全通道未连接{detail}：请打开 Loov Agent 查看原因并点「重试」"
        )));
    }
    Ok(Some(key.to_string()))
}

fn may_inject_key_with(provider_id: &str, base_url: &str, trusted_host: Option<&str>) -> bool {
    if !provision::is_loov_provider(provider_id) {
        return false;
    }
    match (host_of(base_url), trusted_host) {
        (Some(h), Some(trusted)) => h == trusted,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_guard_only_for_loov_provider_and_trusted_host() {
        let loov = provision::claude_child_id();
        let trusted = Some("me.loov.cc");
        // 正常：Loov 子供应商 + 受信 host
        assert!(may_inject_key_with(&loov, "https://me.loov.cc", trusted));
        assert!(may_inject_key_with(&loov, "https://me.loov.cc/v1", trusted));
        // 攻击面：Loov 供应商但 base_url 指向任意地址 → 拒绝（S1-01 核心）
        assert!(!may_inject_key_with(&loov, "http://127.0.0.1:9", trusted));
        assert!(!may_inject_key_with(&loov, "https://evil.example.com", trusted));
        // 非 Loov 供应商即便指向受信 host 也拒绝
        assert!(!may_inject_key_with("attacker-provider", "https://me.loov.cc", trusted));
        assert!(!may_inject_key_with("universal-claude-other", "https://me.loov.cc", trusted));
        // 未登录（无受信 host）一律拒绝
        assert!(!may_inject_key_with(&loov, "https://me.loov.cc", None));
        // host 比较不区分大小写、忽略端口差异之外的主机名
        assert!(may_inject_key_with(&loov, "https://ME.LOOV.CC", trusted));
    }

    /// 审计 S6-02：合并上游后挂载点若被挪动 / 删除，这里先失败。
    #[test]
    fn lib_rs_mount_points_stay_in_order() {
        let lib = include_str!("../lib.rs");
        let pos = |needle: &str| lib.find(needle).unwrap_or_else(|| panic!("lib.rs 缺少挂载点 `{needle}`"));
        let order = [
            "startup_mark(crate::loov::Phase::BlackholePinned)",
            "crate::proxy::http_client::init(proxy_url.as_deref())",
            "crate::loov::flows::startup(&state).await",
            "startup_mark(crate::loov::Phase::LoovStarted)",
            "crate::mode::controller::startup(&state).await",
            "startup_mark(crate::loov::Phase::UpstreamStarted)",
            "crate::loov::flows::post_startup(&state).await",
            "startup_mark(crate::loov::Phase::Hardened)",
        ];
        for w in order.windows(2) {
            assert!(pos(w[0]) < pos(w[1]), "`{}` 必须在 `{}` 之前", w[0], w[1]);
        }
        let controller = include_str!("../mode/controller.rs");
        assert!(
            controller.contains("crate::loov::startup_expect(crate::loov::Phase::LoovStarted)"),
            "controller::startup 入口缺少 Loov 顺序检查"
        );
    }

    #[test]
    fn redacted_status_hides_tunnel_password() {
        let s = Status::Ready {
            username: "u".into(),
            device_id: "d".into(),
            proxy_url: "http://loov:SeCrEt123@127.0.0.1:40397".into(),
            http_port: 40397,
            nodes: vec![],
            route: tunnel::Route::Direct,
            via: None,
            net: NetView::default(),
        };
        let text = format!("{:?}", s.redacted());
        assert!(!text.contains("SeCrEt123"), "{text}");
        assert!(text.contains("http://loov:***@127.0.0.1:40397"), "{text}");
    }

    #[test]
    fn placeholder_key_resolution_is_fail_closed() {
        use crate::proxy::ProxyError;
        let loov = provision::claude_child_id();
        let codex = provision::codex_child_id();
        let t = Some("me.loov.cc");
        let real = Some("sk-real");
        let up = || Ok(());
        let r = |id: &str, url: &str, key: &str, host: Option<&str>, real: Option<&str>, tunnel: Result<(), String>| {
            resolve_placeholder_key_with(id, url, key, host, real, tunnel)
        };
        // 非占位：不碰（用户自己的供应商照常转发，隧道状态无关）
        assert!(matches!(r("mine", "https://x.example", "sk-user", t, real, Err("x".into())), Ok(None)));
        assert!(matches!(r(&loov, "https://me.loov.cc", "sk-user", t, real, up()), Ok(None)));
        // 占位 + 守卫通过 + 已登录 + 隧道就绪：注入真实 Key（Claude / Codex 子供应商都行）
        assert_eq!(r(&loov, "https://me.loov.cc", KEY_PLACEHOLDER, t, real, up()).unwrap(), Some("sk-real".into()));
        assert_eq!(r(&codex, "https://me.loov.cc/v1", KEY_PLACEHOLDER, t, real, up()).unwrap(), Some("sk-real".into()));
        // 占位 + 未登录：400 明确提示，不能带着占位发出去
        match r(&loov, "https://me.loov.cc", KEY_PLACEHOLDER, t, None, up()) {
            Err(ProxyError::ConfigError(m)) => assert!(m.contains("未登录"), "{m}"),
            other => panic!("{other:?}"),
        }
        // 占位 + 隧道未连：400 并带上原因（CLI 不重试，直接显示）
        match r(&loov, "https://me.loov.cc", KEY_PLACEHOLDER, t, real, Err("无法连接隧道节点：台湾 TCP 连不上".into())) {
            Err(ProxyError::ConfigError(m)) => assert!(m.contains("安全通道未连接") && m.contains("台湾"), "{m}"),
            other => panic!("{other:?}"),
        }
        // 占位 + 员工自建供应商 / 改了 base_url / 没有受信 host：401 拒绝（先于登录与隧道检查）
        for (id, url, host) in [
            ("attacker", "https://me.loov.cc", t),
            (loov.as_str(), "https://evil.example.com", t),
            (loov.as_str(), "https://me.loov.cc", None),
        ] {
            match r(id, url, KEY_PLACEHOLDER, host, real, up()) {
                Err(ProxyError::AuthError(m)) => assert!(m.contains("不被 Loov Agent 信任"), "{id} {url}"),
                other => panic!("{id} {url}: {other:?}"),
            }
        }
    }
}

pub fn status() -> Status {
    state().status.read().map(|s| s.clone()).unwrap_or(Status::NoCredentials)
}

pub(crate) fn set_status(s: Status) {
    if let Ok(mut g) = state().status.write() {
        *g = s;
    }
}

pub(crate) fn credentials() -> Option<Credentials> {
    state().creds.read().ok().and_then(|c| c.clone())
}

pub(crate) fn set_credentials(c: Option<Credentials>) {
    if let Ok(mut g) = state().creds.write() {
        *g = c;
    }
}

/// 当前设备的名字：系统主机名（Windows / macOS / Linux 通用），取不到就用平台名。
pub fn device_name() -> String {
    let host = gethostname::gethostname().to_string_lossy().trim().to_string();
    // 只取第一段：Linux 云主机常是完整 FQDN（workspace-linux-sg.asia-southeast1-b.c.xxx.internal），
    // 设备列表里截断后认不出；macOS 的 `xxx.local` 同理
    let host = host.split('.').next().unwrap_or_default().to_string();
    if host.is_empty() {
        platform().to_string()
    } else {
        host.chars().take(64).collect()
    }
}

pub fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}
