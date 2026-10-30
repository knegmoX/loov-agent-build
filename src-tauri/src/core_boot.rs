//! loov: 不依赖 Tauri 的启动步骤。桌面版（`lib.rs` 的 setup）与服务器常驻进程（`headless::serve`）共用，
//! Loov 的四个启动挂载点（黑洞 → 拉配置建隧道 → 上游接上代理模式 → 写加固键）只在这里排一次顺序，
//! 由 `loov::tests::startup_mount_points_stay_in_order` 校验。

use crate::database::Database;
use crate::store::AppState;

/// 本机只允许一个 Loov 核心（桌面版或服务器模式 `serve`）：两者会抢同一个本地路由端口，
/// 启动时还会清理对方的 xray。返回的文件句柄要一直持有到进程退出。
pub fn acquire_core_lock() -> Result<std::fs::File, String> {
    let dir = crate::loov::credentials::loov_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("core.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("打开 {} 失败：{e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(
            "Loov Agent 已经在运行（桌面版或服务器模式 serve），同一用户同时只能运行一个".to_string(),
        ),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("锁定 {} 失败：{e}", path.display())),
    }
}

/// 出站 HTTP 客户端。已登录的设备上游代理固定黑洞，进程第一毫秒起就不会直连；
/// 保存的代理无效时清掉并退回直连初始化。
pub fn init_outbound(db: &Database) {
    if crate::loov::credentials::credentials_path().exists() {
        let _ = db.set_global_proxy_url(Some(crate::loov::BLACKHOLE_PROXY));
    }
    crate::loov::startup_mark(crate::loov::Phase::BlackholePinned);
    let proxy_url = db.get_global_proxy_url().ok().flatten();

    if let Err(e) = crate::proxy::http_client::init(proxy_url.as_deref()) {
        log::error!("[GlobalProxy] [GP-005] Failed to initialize with saved config: {e}");

        // 清除无效的代理配置
        if proxy_url.is_some() {
            log::warn!("[GlobalProxy] [GP-006] Clearing invalid proxy config from database");
            if let Err(clear_err) = db.set_global_proxy_url(None) {
                log::error!("[GlobalProxy] [GP-007] Failed to clear invalid config: {clear_err}");
            }
        }

        // 使用直连模式重新初始化
        if let Err(fallback_err) = crate::proxy::http_client::init(None) {
            log::error!("[GlobalProxy] [GP-008] Failed to initialize direct connection: {fallback_err}");
        }
    }
}

/// 异常退出恢复 + 代理状态自动恢复：先拉配置、起隧道、写供应商行，再让上游把代理模式的应用接上，
/// 最后写 CLI 加固键。返回 Loov 的启动状态。
pub async fn start(state: &AppState) -> crate::loov::Status {
    // 必须排在 auto-extract 之前：先把历史泄漏进 Gemini 共享片段的凭据
    // 清干净，否则紧接着的提取会基于被污染的 live 再写一遍。
    if let Err(e) = crate::services::provider::ProviderService::scrub_leaked_gemini_common_config(state).await {
        log::warn!("清理 Gemini 通用配置泄漏凭据失败: {e}");
    }

    crate::initialize_common_config_snippets(state);

    // 定下各应用的直连 / 代理模式（处理旧版遗留的接管状态），再把代理模式的
    // 应用接上。要排在通用配置片段的自动提取之后：它读的是直连的 live。
    let loov_status = crate::loov::flows::startup(state).await;
    log::info!("[loov] 启动状态: {:?}", loov_status.redacted());
    crate::loov::startup_mark(crate::loov::Phase::LoovStarted);
    crate::mode::controller::startup(state).await;
    crate::loov::startup_mark(crate::loov::Phase::UpstreamStarted);
    // 接上之后写 CLI 加固键（HTTPS_PROXY 等不在投影范围内，单独补）
    crate::loov::flows::post_startup(state).await;
    crate::loov::startup_mark(crate::loov::Phase::Hardened);
    // #8268 测试版给 agy 装过包装脚本、改过 shell 启动文件，撤掉带标记的部分。
    if let Err(e) = crate::services::provider::agy_direct::migrate_legacy_launcher(&state.db) {
        log::warn!("清理旧版 agy 启动器失败: {e}");
    }
    crate::loov::status()
}
