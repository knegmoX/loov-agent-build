//! Tauri 命令（前端 `src/lib/api/loov.ts` 调用）与测试钩子。

use crate::store::AppState;

use super::flows::{self, LoovError};
use super::hub_client::{Account, DeviceEntry, Meta};
use super::Status;

#[tauri::command]
pub async fn loov_status() -> Result<Status, LoovError> {
    Ok(super::status())
}

#[tauri::command]
pub async fn loov_login(
    state: tauri::State<'_, AppState>,
    username: String,
    password: String,
    evict_oldest: Option<bool>,
) -> Result<Status, LoovError> {
    flows::login(state.inner(), &username, &password, evict_oldest.unwrap_or(false)).await
}

#[tauri::command]
pub async fn loov_register(
    state: tauri::State<'_, AppState>,
    invite: String,
    username: String,
    password: String,
    evict_oldest: Option<bool>,
) -> Result<Status, LoovError> {
    flows::register(state.inner(), &invite, &username, &password, evict_oldest.unwrap_or(false)).await
}

#[tauri::command]
pub async fn loov_logout(state: tauri::State<'_, AppState>) -> Result<(), LoovError> {
    flows::logout(state.inner()).await
}

#[tauri::command]
pub async fn loov_refresh(state: tauri::State<'_, AppState>) -> Result<Status, LoovError> {
    flows::refresh(state.inner()).await
}

#[tauri::command]
pub async fn loov_account() -> Result<Account, LoovError> {
    flows::account().await
}

#[tauri::command]
pub async fn loov_redeem(code: String) -> Result<f64, LoovError> {
    flows::redeem(&code).await
}

#[tauri::command]
pub async fn loov_devices() -> Result<Vec<DeviceEntry>, LoovError> {
    flows::devices().await
}

#[tauri::command]
pub async fn loov_revoke_device(id: String) -> Result<(), LoovError> {
    flows::revoke_device(&id).await
}

/// 停用 Loov 并把 Claude Code / Codex 配置恢复到首次登录前。
#[tauri::command]
pub async fn loov_restore_original(state: tauri::State<'_, AppState>) -> Result<super::restore::RestoreReport, LoovError> {
    super::restore::restore_original(state.inner()).await.map_err(LoovError::from)
}

/// 客户端版本（tauri.conf.json 的 version），前端拿它和 hub /meta 的 min / latest 比较。
#[tauri::command]
pub async fn loov_app_version(app: tauri::AppHandle) -> Result<String, LoovError> {
    Ok(app.package_info().version.to_string())
}

/// Loov 是否接管过本机 CLI 配置（决定是否显示「恢复原配置」入口）。
#[tauri::command]
pub async fn loov_managed() -> Result<bool, LoovError> {
    Ok(super::restore::managed())
}

#[tauri::command]
pub async fn loov_meta() -> Result<Meta, LoovError> {
    Ok(super::hub_client::HubClient::default_client().meta().await?)
}

/// 当前登录信息（用户名 / 设备 / 充值地址 / 模型目录），不含 Key。
#[derive(serde::Serialize)]
pub struct LoovSession {
    pub username: String,
    pub device_id: String,
    pub topup_url: String,
    pub models: Vec<String>,
    pub model_catalog: Vec<super::hub_client::ModelEntry>,
    pub api_base_url: String,
}

#[tauri::command]
pub async fn loov_session() -> Result<Option<LoovSession>, LoovError> {
    Ok(super::credentials().map(|c| {
        let b = c.bootstrap;
        LoovSession {
            username: c.username,
            device_id: c.device_id,
            topup_url: b.as_ref().map(|b| b.topup_url.clone()).unwrap_or_default(),
            models: b.as_ref().map(|b| b.models.clone()).unwrap_or_default(),
            model_catalog: b.as_ref().map(|b| b.model_catalog.clone()).unwrap_or_default(),
            api_base_url: b.as_ref().map(|b| b.api.base_url.clone()).unwrap_or_default(),
        }
    }))
}

// ---- 测试钩子：集成测试（tests/loov_e2e.rs）直接驱动流程，不经 Tauri 运行时 ----

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_login_test_hook(state: &AppState, username: &str, password: &str, evict_oldest: bool) -> Result<Status, LoovError> {
    flows::login(state, username, password, evict_oldest).await
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_register_test_hook(state: &AppState, invite: &str, username: &str, password: &str) -> Result<Status, LoovError> {
    flows::register(state, invite, username, password, false).await
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_logout_test_hook(state: &AppState) -> Result<(), LoovError> {
    flows::logout(state).await
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_startup_test_hook(state: &AppState) -> Status {
    let s = flows::startup(state).await;
    crate::mode::controller::startup(state).await;
    flows::post_startup(state).await;
    s
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub fn loov_status_test_hook() -> Status {
    super::status()
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_refresh_test_hook(state: &AppState) -> Result<Status, LoovError> {
    flows::refresh(state).await
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_devices_test_hook() -> Result<Vec<DeviceEntry>, LoovError> {
    flows::devices().await
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_restore_test_hook(state: &AppState) -> Result<super::restore::RestoreReport, LoovError> {
    super::restore::restore_original(state).await.map_err(LoovError::from)
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_shutdown_test_hook() {
    flows::shutdown().await
}

/// E2E：让 Claude Desktop 接入 Loov，返回本地网关 token 与写入的 profile 路径。
#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_desktop_attach_test_hook(state: &AppState) -> Result<(String, Option<String>), LoovError> {
    super::provision::attach_desktop(state).await?;
    let token = crate::claude_desktop_config::get_or_create_gateway_token(&state.db)?;
    let status = crate::claude_desktop_config::get_status(&state.db, true)?;
    Ok((token, status.profile_path))
}

/// E2E：让 Grok Build 接入 Loov 并换到指定模型（与 Loov 页启动 Grok、切换模型同一条路径）。
#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_grok_attach_test_hook(state: &AppState, model: &str) -> Result<(), LoovError> {
    super::provision::set_grok_model(state, model).await?;
    super::provision::attach_grok(state).await?;
    Ok(())
}

/// E2E：Claude Desktop 是否在路由模式。
#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub fn loov_desktop_routed_test_hook() -> bool {
    crate::mode::current::is_proxy(&crate::app_config::AppType::ClaudeDesktop)
}

#[tauri::command]
pub async fn loov_apps_status(state: tauri::State<'_, AppState>) -> Result<Vec<super::apps::LoovAppStatus>, LoovError> {
    Ok(super::apps::status(state.inner()).await)
}

#[tauri::command]
pub async fn loov_app_set_model(state: tauri::State<'_, AppState>, id: String, model: String) -> Result<(), LoovError> {
    super::apps::set_model(state.inner(), &id, &model).await
}

#[tauri::command]
pub async fn loov_app_install(app: tauri::AppHandle, id: String) -> Result<(), LoovError> {
    super::apps::install(app, &id).await
}

#[tauri::command]
pub async fn loov_app_launch(
    state: tauri::State<'_, AppState>,
    id: String,
    restart: Option<bool>,
) -> Result<(), LoovError> {
    super::apps::launch(state.inner(), &id, restart.unwrap_or(false)).await
}


/// 手选节点（`node` 为空 = 自动，选最快的）。
#[tauri::command]
pub async fn loov_set_node(state: tauri::State<'_, AppState>, node: Option<String>) -> Result<Status, LoovError> {
    flows::set_selection(state.inner(), Some(node), None).await
}

/// 网络模式：direct（仅 Loov 应用走安全通道）/ rule（国外网站走通道）/ global（全部走通道）。
#[tauri::command]
pub async fn loov_set_net_mode(state: tauri::State<'_, AppState>, mode: super::netmode::NetMode) -> Result<Status, LoovError> {
    flows::set_selection(state.inner(), None, Some(mode)).await
}

/// E2E：同 `loov_set_node` / `loov_set_net_mode`。
#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn loov_set_selection_test_hook(
    state: &AppState,
    node: Option<Option<String>>,
    mode: Option<super::netmode::NetMode>,
) -> Result<Status, LoovError> {
    flows::set_selection(state, node, mode).await
}

/// E2E：把 Loov 的全部模型写进 Grok 的模型选择器（与 Loov 页启动 Grok 同一路径）。
#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub fn loov_grok_sync_models_test_hook(state: &AppState) -> Result<(), LoovError> {
    super::provision::sync_grok_models(state).map_err(|e| LoovError::Internal { message: e.to_string() })
}
