//! `loov-agent serve`：服务器上的常驻进程。启动顺序与桌面版相同（core_boot），之后监听控制套接字；
//! SIGTERM / SIGINT 正常退出（退出码 0，systemd 不当成失败），SIGHUP 重新拉配置、重建链路。

use std::sync::Arc;

use crate::store::AppState;

pub fn run() -> i32 {
    super::logging::init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _lock = match crate::core_boot::acquire_core_lock() {
        Ok(f) => f,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    crate::loov::set_headless();
    tauri::async_runtime::block_on(serve())
}

async fn serve() -> i32 {
    match crate::database::Database::stored_user_version_exceeds_supported(&crate::config::get_app_config_dir().join("cc-switch.db")) {
        Ok(Some(v)) => {
            eprintln!("数据库版本过新（v{v}），请升级 Loov Agent");
            return 1;
        }
        Ok(None) => {}
        Err(e) => log::warn!("预检数据库版本失败，继续：{e}"),
    }
    let db = match crate::database::Database::init() {
        Ok(db) => Arc::new(db),
        Err(e) => {
            eprintln!("打开数据库失败：{e}");
            return 1;
        }
    };
    let state = AppState::new(db);
    crate::mode::operation::recover_on_startup(&state.db);

    crate::core_boot::init_outbound(&state.db);
    let status = crate::core_boot::start(&state).await;
    log::info!("[loov] 服务已启动：{:?}", status.redacted());

    let ctl_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = super::ctl::serve(ctl_state).await {
            log::error!("[loov] 控制套接字退出：{e}");
        }
    });

    use tokio::signal::unix::{signal, SignalKind};
    let (Ok(mut term), Ok(mut int), Ok(mut hup)) =
        (signal(SignalKind::terminate()), signal(SignalKind::interrupt()), signal(SignalKind::hangup()))
    else {
        eprintln!("注册信号处理失败");
        return 1;
    };
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = int.recv() => break,
            _ = hup.recv() => {
                log::info!("[loov] 收到 SIGHUP：重新拉取配置");
                if let Err(e) = crate::loov::flows::refresh(&state).await {
                    log::warn!("[loov] 刷新失败：{e}");
                }
            }
        }
    }
    log::info!("[loov] 正在停止");
    crate::loov::flows::shutdown().await;
    let _ = state.proxy_service.stop().await;
    let _ = std::fs::remove_file(super::ctl::socket_path());
    0
}
