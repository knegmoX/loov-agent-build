//! 登录 / 启动 / 登出等编排流程。Tauri 命令与测试钩子都只是这里的薄壳。

use std::time::Duration;

use serde::Serialize;

use crate::store::AppState;

use super::credentials::{self, Credentials};
use super::hub_client::{Account, Bootstrap, DeviceEntry, DeviceInput, HubClient, HubError};
use super::netmode::{self, NetMode, Prefs};
use super::{local_auth, provision, tunnel, NetView, Status, BLACKHOLE_PROXY};

const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(8);

/// 前端拿到的错误统一是扁平的 `{ code, message, devices? }`（见 `serialize` 实现），
/// `code` 用于分支，`message` 是可直接展示的中文。
#[derive(Debug, Clone, thiserror::Error)]
pub enum LoovError {
    #[error("{0}")]
    Hub(HubError),
    #[error("隧道：{message}")]
    Tunnel { message: String },
    #[error("{message}")]
    Internal { message: String },
    #[error("尚未登录")]
    NotLoggedIn,
    /// 启动台（安装 / 启动应用）的错误，`code` 供前端分支（如 DESKTOP_RESTART_REQUIRED、NODE_REQUIRED）。
    #[error("{message}")]
    App { code: String, message: String },
}

impl LoovError {
    pub fn code(&self) -> &str {
        match self {
            LoovError::Hub(e) => e.code(),
            LoovError::Tunnel { .. } => "TUNNEL",
            LoovError::Internal { .. } => "INTERNAL",
            LoovError::NotLoggedIn => "NOT_LOGGED_IN",
            LoovError::App { code, .. } => code,
        }
    }
}

impl Serialize for LoovError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let devices = match self {
            LoovError::Hub(HubError::DeviceLimit { devices, .. }) => Some(devices),
            _ => None,
        };
        let mut m = s.serialize_map(Some(if devices.is_some() { 3 } else { 2 }))?;
        m.serialize_entry("code", self.code())?;
        m.serialize_entry("message", &self.to_string())?;
        if let Some(d) = devices {
            m.serialize_entry("devices", d)?;
        }
        m.end()
    }
}

impl From<HubError> for LoovError {
    fn from(e: HubError) -> Self {
        LoovError::Hub(e)
    }
}

impl From<crate::error::AppError> for LoovError {
    fn from(e: crate::error::AppError) -> Self {
        LoovError::Internal { message: e.to_string() }
    }
}

impl From<String> for LoovError {
    fn from(message: String) -> Self {
        LoovError::Internal { message }
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn hub() -> HubClient {
    HubClient::default_client()
}

fn health_url() -> String {
    format!("{}/hub/healthz", hub().origin())
}

fn token() -> Result<String, LoovError> {
    super::credentials()
        .map(|c| c.device_token)
        .ok_or(LoovError::NotLoggedIn)
}

/// 上游代理：DB 里固定黑洞，真实隧道地址只在内存。
fn pin_blackhole(state: &AppState) {
    match state.db.get_global_proxy_url() {
        Ok(Some(url)) if url == BLACKHOLE_PROXY => {}
        _ => {
            if let Err(e) = state.db.set_global_proxy_url(Some(BLACKHOLE_PROXY)) {
                log::error!("[loov] 写入黑洞代理失败: {e}");
            }
        }
    }
}

fn route_upstream(proxy_url: Option<&str>) {
    let url = proxy_url.unwrap_or(BLACKHOLE_PROXY);
    if let Err(e) = crate::proxy::http_client::apply_proxy(Some(url)) {
        log::error!("[loov] 应用上游代理失败: {e}");
    }
}

async fn stop_tunnel() {
    if let Some(h) = super::state().tunnel.lock().await.take() {
        h.stop().await;
    }
}

/// 「代理 / 全局」模式下把系统代理指向 sys-in，直连模式还原。返回设置失败的原因（隧道照常可用）。
fn apply_system_proxy(handle: &tunnel::TunnelHandle) -> Option<String> {
    match handle.sys {
        Some(sys) => netmode::engage(sys.ports).err().inspect(|e| log::error!("[loov] 设置系统代理失败：{e}")),
        None => {
            netmode::release();
            None
        }
    }
}

fn ready_status(username: &str, device_id: &str, handle: &tunnel::TunnelHandle, prefs: &Prefs, sys_error: Option<String>) -> Status {
    Status::Ready {
        username: username.to_string(),
        device_id: device_id.to_string(),
        proxy_url: handle.proxy_url(),
        http_port: handle.http_port,
        nodes: handle.node_ids.clone(),
        route: handle.route,
        via: handle.via.clone(),
        net: NetView::of(handle, prefs, sys_error),
    }
}

/// 有了 bootstrap 之后把链路拉起来：隧道 → 上游代理 → 供应商行。失败也保持 fail-closed。
async fn bring_up(state: &AppState, b: &Bootstrap) -> Status {
    // 隧道重建期间不接管系统代理：旧的 sys-in 马上要停，员工不能断网；探测时读系统代理也不会读到自己
    netmode::release();
    pin_blackhole(state);
    super::privacy::load_config(&state.db);
    if let Err(e) = local_auth::init(&state.db) {
        return Status::TunnelFailed { reason: format!("本地令牌初始化失败：{e}") };
    }
    if let Err(e) = provision::apply(state, b) {
        log::error!("[loov] 写入供应商失败: {e}");
    }
    stop_tunnel().await;
    let prefs = netmode::load_prefs();
    let sel = tunnel::Selection { node: prefs.node.clone(), mode: prefs.mode };
    let status = match tunnel::locate_xray() {
        Err(e) => Status::TunnelFailed { reason: e },
        Ok(bin) => match tunnel::establish(&b.tunnel.nodes, &bin, &health_url(), &sel).await {
            Err(e) => Status::TunnelFailed { reason: e },
            Ok(handle) => {
                let sys_error = apply_system_proxy(&handle);
                let s = ready_status(&b.user.username, &b.device.id, &handle, &prefs, sys_error);
                *super::state().tunnel.lock().await = Some(handle);
                s
            }
        },
    };
    match &status {
        Status::Ready { proxy_url, .. } => route_upstream(Some(proxy_url)),
        _ => route_upstream(None),
    }
    super::set_status(status.clone());
    status
}

/// 接上路由模式并写 CLI 加固键。登录时调用；启动时由上游 `controller::startup` 接上后调 [`post_startup`]。
pub async fn attach_and_harden(state: &AppState) -> Result<(), LoovError> {
    provision::attach(state).await?;
    harden(state);
    sync_mcp(state).await;
    Ok(())
}

/// 下发 / 刷新云端搜索 MCP（放在 harden 之后：harden_codex 会确保 ~/.codex 存在）。失败只记日志。
async fn sync_mcp(state: &AppState) {
    if let Err(e) = provision::apply_mcp(state).await {
        log::error!("[loov] 下发云端搜索 MCP 失败: {e}");
    }
}

fn harden(state: &AppState) {
    let proxy = match super::status() {
        Status::Ready { proxy_url, .. } => Some(proxy_url),
        _ => None,
    };
    if let Err(e) = provision::harden_claude(&state.db, proxy.as_deref()) {
        log::error!("[loov] 写 Claude 加固键失败: {e}");
    }
    if let Err(e) = provision::harden_codex(&state.db) {
        log::error!("[loov] 写 Codex 加固键失败: {e}");
    }
}

/// 被 SIGTERM / SIGINT / SIGHUP 结束时（注销会话、kill、终端关闭）进程直接退出，不走 Tauri 的退出
/// 清理，xray 会成为孤儿一直开着隧道口，直到下次启动才被清掉。收到信号先停 xray 再按原信号码退出。
#[cfg(unix)]
fn install_signal_cleanup() {
    use tokio::signal::unix::{signal, SignalKind};
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for (kind, code) in [(SignalKind::terminate(), 15), (SignalKind::interrupt(), 2), (SignalKind::hangup(), 1)] {
            let Ok(mut s) = signal(kind) else { continue };
            tokio::spawn(async move {
                if s.recv().await.is_some() {
                    log::warn!("[loov] 收到信号 {code}，还原系统代理、停止隧道后退出");
                    netmode::release();
                    stop_tunnel().await;
                    std::process::exit(128 + code);
                }
            });
        }
    });
}

#[cfg(not(unix))]
fn install_signal_cleanup() {}

/// 换节点 / 换网络模式。`node`：Some(None) = 改回自动，Some(Some(id)) = 手选；`mode` 为 None 不改。
/// 隧道就绪时用已探测的结果重启 xray（端口、口令不变，已启动的应用不受影响）；手选的节点之前没通过
/// 探测时整条链路重建一次（重新探测，仍不通就回到自动并在状态里标出来）。
pub async fn set_selection(state: &AppState, node: Option<Option<String>>, mode: Option<NetMode>) -> Result<Status, LoovError> {
    let mut prefs = netmode::load_prefs();
    if let Some(n) = node {
        prefs.node = n.filter(|s| !s.is_empty());
    }
    if let Some(m) = mode {
        prefs.mode = m;
    }
    netmode::save_prefs(&prefs).map_err(|message| LoovError::Internal { message })?;
    let (username, device_id) = match super::status() {
        Status::Ready { username, device_id, .. } => (username, device_id),
        // 没连上：下次建隧道时生效
        s => return Ok(s),
    };
    let sel = tunnel::Selection { node: prefs.node.clone(), mode: prefs.mode };
    let mut guard = super::state().tunnel.lock().await;
    let Some(old) = guard.take() else { return Ok(super::status()) };
    // 先放开系统代理：xray 重启的这一两秒里浏览器不至于连到停掉的端口
    netmode::release();
    match tunnel::reconfigure(old, &health_url(), &sel).await {
        Ok(handle) => {
            let sys_error = apply_system_proxy(&handle);
            let s = ready_status(&username, &device_id, &handle, &prefs, sys_error);
            route_upstream(Some(&handle.proxy_url()));
            *guard = Some(handle);
            super::set_status(s.clone());
            Ok(s)
        }
        Err((e, back)) => {
            log::warn!("[loov] 切换节点 / 模式没能直接生效（{e}），重建安全通道");
            if let Some(h) = back {
                h.stop().await;
            }
            drop(guard);
            reconnect(state).await
        }
    }
}

/// 改 agent 权限档位：存进 prefs，并立即重写 Claude Code / Codex 的配置（Grok 在下次从 Loov 页启动时写，
/// agy 在启动参数里带）。员工自己在 agent 里改过的值不覆盖。
pub fn set_safety(state: &AppState, level: super::safety::SafetyLevel) -> Result<(), LoovError> {
    let mut prefs = netmode::load_prefs();
    prefs.safety = level;
    netmode::save_prefs(&prefs).map_err(|message| LoovError::Internal { message })?;
    if super::active() {
        harden(state);
    }
    Ok(())
}

/// 用缓存的配置整条重建链路（重新探测节点），并重写加固键。
async fn reconnect(state: &AppState) -> Result<Status, LoovError> {
    let b = super::credentials().and_then(|c| c.bootstrap).ok_or(LoovError::NotLoggedIn)?;
    let s = bring_up(state, &b).await;
    harden(state);
    Ok(s)
}

/// 每 15 秒看一眼 xray 还活着没有。意外退出时：先还原系统代理（员工不能断网）、出站回黑洞，
/// 再自动重建一次链路。
fn spawn_watchdog(state: &AppState) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let state = state.clone();
    ONCE.call_once(move || {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            let mut n: u64 = 0;
            loop {
                tick.tick().await;
                n += 1;
                // 每 5 分钟刷新一次模型健康度与回退链（hub 按 new-api 日志判断）
                if n % 20 == 0 {
                    refresh_models().await;
                }
                let died = {
                    let mut g = super::state().tunnel.lock().await;
                    let dead = g.as_mut().is_some_and(|h| !h.is_alive());
                    if dead { g.take() } else { None }
                };
                let Some(dead) = died else { continue };
                log::error!("[loov] xray 意外退出，重建安全通道");
                netmode::release();
                route_upstream(None);
                super::set_status(Status::TunnelFailed { reason: "安全通道进程意外退出，正在重连…".into() });
                dead.stop().await;
                if let Err(e) = reconnect(&state).await {
                    log::error!("[loov] 重建安全通道失败：{e}");
                }
            }
        });
    });
}

/// 应用启动：在上游 `mode::controller::startup` **之前**调用。
pub async fn startup(state: &AppState) -> Status {
    install_signal_cleanup();
    // 上次没正常退出（崩溃、断电）时系统代理可能还指着已经不在的 sys-in：先还原，员工才能上网
    if netmode::engaged() {
        netmode::release();
    }
    spawn_watchdog(state);
    let creds = match credentials::load() {
        Ok(Some(c)) => c,
        Ok(None) => {
            super::set_status(Status::NoCredentials);
            return Status::NoCredentials;
        }
        Err(e) => {
            log::error!("[loov] 凭据无法读取，视为未登录: {e}");
            super::set_status(Status::NoCredentials);
            return Status::NoCredentials;
        }
    };
    super::set_credentials(Some(creds.clone()));
    super::set_status(Status::Starting);
    pin_blackhole(state);
    let _ = local_auth::init(&state.db);
    let fresh = tokio::time::timeout(BOOTSTRAP_TIMEOUT, hub().bootstrap(&creds.device_token)).await;
    // 超时与网络错误同样处理：有缓存就用缓存，没有就离线
    let fresh: Result<Bootstrap, HubError> = match fresh {
        Ok(r) => r,
        Err(_) => Err(HubError::Network { message: "连接服务器超时".into() }),
    };
    let bootstrap = match fresh {
        Ok(b) => {
            let updated = Credentials { bootstrap: Some(b.clone()), cached_at: now(), ..creds };
            let _ = credentials::save(&updated);
            super::set_credentials(Some(updated));
            b
        }
        Err(e) if e.is_relogin() => {
            log::warn!("[loov] 设备令牌已失效（{}），需要重新登录", e.code());
            let _ = credentials::clear();
            super::set_credentials(None);
            route_upstream(None);
            let s = Status::ReloginRequired { code: e.code().to_string() };
            super::set_status(s.clone());
            return s;
        }
        // 审计 S1-02 起 Key 不落盘：缓存里没有 Key 就起不了可用的链路，按离线处理（旧版写下的带 Key 缓存仍可用一次）
        Err(e) => match creds.bootstrap.clone().filter(|b| !b.api.key.is_empty()) {
            Some(cached) => {
                log::warn!("[loov] 拉取配置失败，使用缓存: {e}");
                cached
            }
            None => {
                route_upstream(None);
                let s = Status::Offline { reason: e.to_string() };
                super::set_status(s.clone());
                return s;
            }
        },
    };
    bring_up(state, &bootstrap).await
}

/// 上游 `controller::startup` 之后：有凭证就写加固键（隧道未就绪时写黑洞）并刷新云端搜索 MCP。
pub async fn post_startup(state: &AppState) {
    if super::active() {
        harden(state);
        sync_mcp(state).await;
    }
}

async fn complete_auth(state: &AppState, auth: super::hub_client::AuthResult) -> Result<Status, LoovError> {
    let creds = Credentials {
        device_token: auth.device_token,
        username: auth.bootstrap.user.username.clone(),
        device_id: auth.bootstrap.device.id.clone(),
        bootstrap: Some(auth.bootstrap.clone()),
        cached_at: now(),
    };
    credentials::save(&creds)?;
    super::set_credentials(Some(creds));
    // 每次登录换一枚本地令牌；attach 会把它重新投影进 CLI 配置
    local_auth::rotate(&state.db)?;
    let status = bring_up(state, &auth.bootstrap).await;
    attach_and_harden(state).await?;
    Ok(status)
}

pub async fn login(state: &AppState, username: &str, password: &str, evict_oldest: bool) -> Result<Status, LoovError> {
    // 首次接管前存下原配置（在调 hub 之前：存不下来就不登录，免得服务端白建设备）
    let fresh_snapshot = super::restore::snapshot_if_absent()?;
    let name = super::device_name();
    let auth = hub()
        .login(username.trim(), password, DeviceInput { name: &name, platform: super::platform() }, evict_oldest)
        .await;
    complete_auth(state, discard_snapshot_on_error(fresh_snapshot, auth)?).await
}

/// 账号密码 / 邀请码被拒时撤掉本次刚拍的快照：快照在就会被当成「已接管」（隐藏官方入口、退出时
/// 不指回直连），而这次其实什么都没接管。
fn discard_snapshot_on_error<T>(fresh_snapshot: bool, r: Result<T, HubError>) -> Result<T, HubError> {
    if r.is_err() && fresh_snapshot {
        super::restore::discard_snapshot();
    }
    r
}

pub async fn register(state: &AppState, invite: &str, username: &str, password: &str, evict_oldest: bool) -> Result<Status, LoovError> {
    let fresh_snapshot = super::restore::snapshot_if_absent()?;
    let name = super::device_name();
    let auth = hub()
        .register(invite.trim(), username.trim(), password, DeviceInput { name: &name, platform: super::platform() }, evict_oldest)
        .await;
    complete_auth(state, discard_snapshot_on_error(fresh_snapshot, auth)?).await
}

/// 登出：服务端吊销（失败也继续）→ 停隧道 → 黑洞 → 清凭证。路由模式保持接上（fail-closed）。
pub async fn logout(state: &AppState) -> Result<(), LoovError> {
    if let Ok(t) = token() {
        if let Err(e) = hub().logout(&t).await {
            log::warn!("[loov] 服务端登出失败（本地仍清理）: {e}");
        }
    }
    shutdown().await;
    credentials::clear()?;
    super::set_credentials(None);
    super::set_status(Status::NoCredentials);
    if let Err(e) = provision::harden_claude(&state.db, None) {
        log::error!("[loov] 登出时写黑洞代理失败: {e}");
    }
    Ok(())
}

/// 重新拉取配置并按需重启隧道（节点变化 / 隧道已死）。
pub async fn refresh(state: &AppState) -> Result<Status, LoovError> {
    let creds = super::credentials().ok_or(LoovError::NotLoggedIn)?;
    let b = match hub().bootstrap(&creds.device_token).await {
        Ok(b) => b,
        Err(e) if e.is_relogin() => {
            let _ = credentials::clear();
            super::set_credentials(None);
            shutdown().await;
            let s = Status::ReloginRequired { code: e.code().to_string() };
            super::set_status(s.clone());
            return Err(e.into());
        }
        Err(e) => return Err(e.into()),
    };
    let nodes_changed = creds.bootstrap.as_ref().map(|old| old.tunnel != b.tunnel).unwrap_or(true);
    let updated = Credentials { bootstrap: Some(b.clone()), cached_at: now(), ..creds };
    credentials::save(&updated)?;
    super::set_credentials(Some(updated));
    let alive = {
        let mut g = super::state().tunnel.lock().await;
        g.as_mut().map(|h| h.is_alive()).unwrap_or(false)
    };
    if nodes_changed || !alive || !matches!(super::status(), Status::Ready { .. }) {
        let s = bring_up(state, &b).await;
        harden(state);
        sync_mcp(state).await;
        Ok(s)
    } else {
        if let Err(e) = provision::apply(state, &b) {
            log::error!("[loov] 刷新供应商失败: {e}");
        }
        Ok(super::status())
    }
}

pub async fn account() -> Result<Account, LoovError> {
    Ok(hub().account(&token()?).await?)
}

/// 刷新内存里的模型目录、健康度与回退链（不落盘、不重建链路）。失败只记日志。
pub async fn refresh_models() {
    let Ok(t) = token() else { return };
    if !matches!(super::status(), Status::Ready { .. }) {
        return;
    }
    match hub().models(&t).await {
        Ok(list) => {
            if let Some(mut c) = super::credentials() {
                if let Some(b) = c.bootstrap.as_mut() {
                    if !list.models.is_empty() {
                        b.models = list.models;
                    }
                    b.model_catalog = list.model_catalog;
                    b.model_fallbacks = list.model_fallbacks;
                }
                super::set_credentials(Some(c));
            }
        }
        Err(e) => log::debug!("[loov] 刷新模型目录失败：{e}"),
    }
}

pub async fn usage(range: &str) -> Result<super::hub_client::Usage, LoovError> {
    Ok(hub().usage(&token()?, range).await?)
}

pub async fn redeem(code: &str) -> Result<f64, LoovError> {
    Ok(hub().redeem(&token()?, code.trim()).await?)
}

pub async fn devices() -> Result<Vec<DeviceEntry>, LoovError> {
    Ok(hub().devices(&token()?).await?)
}

pub async fn revoke_device(id: &str) -> Result<(), LoovError> {
    Ok(hub().revoke_device(&token()?, id).await?)
}

/// 退出前：还原系统代理，停 sidecar，上游代理回黑洞。
pub async fn shutdown() {
    netmode::release();
    stop_tunnel().await;
    if super::active() {
        route_upstream(None);
    }
}

/// 同步版本（进程退出路径没有运行时可用时）。
pub fn shutdown_blocking() {
    netmode::release();
    if let Ok(mut g) = super::state().tunnel.try_lock() {
        if let Some(h) = g.take() {
            h.stop_blocking();
        }
    }
    let _ = tunnel::kill_orphans();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loov::hub_client::DeviceEntry;

    #[test]
    fn errors_serialize_flat_for_frontend() {
        let e = LoovError::Hub(HubError::DeviceLimit {
            message: "满了".into(),
            devices: vec![DeviceEntry {
                id: "a".into(),
                name: "old".into(),
                platform: "linux".into(),
                created_at: 1,
                last_seen_at: None,
                current: false,
            }],
        });
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["code"], "DEVICE_LIMIT");
        assert_eq!(v["message"], "满了");
        assert_eq!(v["devices"][0]["id"], "a");
        assert_eq!(v.as_object().unwrap().len(), 3, "不得出现重复 / 多余字段: {v}");

        let v = serde_json::to_value(LoovError::Hub(HubError::Unauthorized {
            code: "SESSION_EXPIRED".into(),
            message: "请重新登录".into(),
        }))
        .unwrap();
        assert_eq!(v, serde_json::json!({"code": "SESSION_EXPIRED", "message": "请重新登录"}));
        assert_eq!(serde_json::to_value(LoovError::NotLoggedIn).unwrap()["code"], "NOT_LOGGED_IN");
    }
}
