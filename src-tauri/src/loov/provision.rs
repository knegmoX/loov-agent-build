//! 把 hub 下发的配置写进 CC Switch：统一供应商（Key 位是占位）、路由模式、CLI 加固键。

use serde_json::{json, Value};

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::live::engine::LiveFile;
use crate::live::patch::json::JsonPatch;
use crate::live::patch::toml::TomlPatch;
use crate::live::patch::KeyPath;
use crate::mode::operation::{AppWrite, FileChange};
use crate::mode::state::PendingTarget;
use crate::provider::{
    ClaudeDesktopMode, ClaudeDesktopModelRoute, ClaudeModelConfig, CodexModelConfig, GeminiModelConfig,
    Provider, ProviderMeta, UniversalProvider, UniversalProviderApps, UniversalProviderModels,
};
use crate::services::provider::{claude_direct, ProviderService};
use crate::store::AppState;

use super::hub_client::Bootstrap;
use super::{BLACKHOLE_PROXY, KEY_PLACEHOLDER, PROVIDER_ID};

/// 操作名（写进 pending 记录，便于排障）。
const OP_HARDEN: &str = "loov.harden";

pub fn claude_child_id() -> String {
    format!("universal-claude-{PROVIDER_ID}")
}

pub fn codex_child_id() -> String {
    format!("universal-codex-{PROVIDER_ID}")
}

/// Antigravity（agy）经 `/loov/gemini` 入口用的 Gemini 子供应商（统一供应商同步出来的）。
pub fn gemini_child_id() -> String {
    format!("universal-gemini-{PROVIDER_ID}")
}

/// Claude Desktop 的 Loov 供应商（统一供应商没有 Desktop 槽位，单独维护）。
pub const DESKTOP_PROVIDER_ID: &str = "loov-claude-desktop";

/// Grok Build 的 Loov 供应商（同样单独维护）。
pub const GROK_PROVIDER_ID: &str = "loov-grok";

/// 是否是 Loov 托管的供应商（Claude / Codex / Gemini 子供应商、Claude Desktop）。隐私整流器与真实 Key 注入守卫共用。
pub fn is_loov_provider(provider_id: &str) -> bool {
    provider_id == claude_child_id()
        || provider_id == codex_child_id()
        || provider_id == gemini_child_id()
        || provider_id == DESKTOP_PROVIDER_ID
        || provider_id == GROK_PROVIDER_ID
}

/// 能给 agy 当对话模型的 Gemini（排除向量、生图、语音等专用模型）。前端 `isAgentGemini` 同一规则。
pub fn is_agent_gemini(id: &str) -> bool {
    id.starts_with("gemini-") && !["embedding", "banana", "image", "tts", "live", "veo"].iter().any(|k| id.contains(k))
}

/// 按 hub 的模型目录挑默认模型：优先 recommended，其次按名字前缀。
pub struct ModelPick {
    pub claude: Option<String>,
    pub haiku: Option<String>,
    pub sonnet: Option<String>,
    pub opus: Option<String>,
    pub codex: Option<String>,
    /// Antigravity（agy）的默认 Gemini 模型。
    pub gemini: Option<String>,
}

pub fn pick_models(b: &Bootstrap) -> ModelPick {
    let ids: Vec<&str> = if b.model_catalog.is_empty() {
        b.models.iter().map(String::as_str).collect()
    } else {
        b.model_catalog.iter().map(|m| m.id.as_str()).collect()
    };
    let kind_of = |id: &str| -> &str {
        if let Some(m) = b.model_catalog.iter().find(|m| m.id == id) {
            if !m.kind.is_empty() {
                return match m.kind.as_str() {
                    "anthropic" => "anthropic",
                    "openai" => "openai",
                    "gemini" => "gemini",
                    _ => "other",
                };
            }
        }
        if id.starts_with("claude-") {
            "anthropic"
        } else if id.starts_with("gemini-") {
            "gemini"
        } else if id.starts_with("gpt-") || id.starts_with("codex") || id.chars().next() == Some('o') && id.chars().nth(1).is_some_and(|c| c.is_ascii_digit()) {
            "openai"
        } else {
            "other"
        }
    };
    let recommended = |kind: &str| -> Option<String> {
        b.model_catalog
            .iter()
            .find(|m| m.recommended && kind_of(&m.id) == kind)
            .map(|m| m.id.clone())
    };
    let first = |kind: &str| -> Option<String> {
        ids.iter().find(|id| kind_of(id) == kind).map(|s| s.to_string())
    };
    let contains = |kind: &str, needle: &str| -> Option<String> {
        ids.iter()
            .find(|id| kind_of(id) == kind && id.contains(needle))
            .map(|s| s.to_string())
    };
    let claude = recommended("anthropic").or_else(|| contains("anthropic", "sonnet")).or_else(|| first("anthropic"));
    // Gemini：推荐的 > 名字最短的 flash（gemini-3.8-flash 而不是 -cyber / -search 变体）> 第一个
    let agent_gemini = |id: &str| kind_of(id) == "gemini" && is_agent_gemini(id);
    let gemini = b
        .model_catalog
        .iter()
        .find(|m| m.recommended && agent_gemini(&m.id))
        .map(|m| m.id.clone())
        .or_else(|| ids.iter().filter(|id| agent_gemini(id) && id.contains("flash")).min_by_key(|id| id.len()).map(|s| s.to_string()))
        .or_else(|| ids.iter().find(|id| agent_gemini(id)).map(|s| s.to_string()));
    ModelPick {
        haiku: contains("anthropic", "haiku").or_else(|| claude.clone()),
        sonnet: contains("anthropic", "sonnet").or_else(|| claude.clone()),
        opus: contains("anthropic", "opus").or_else(|| claude.clone()),
        claude,
        // Codex 也能用 Claude 模型（new-api 原生 /v1/responses）：hub 只推荐了 Claude 时跟着用，
        // GPT 渠道故障期间把推荐列表改成只含 Claude，新登录的 Codex 就不会默认到坏掉的模型
        codex: recommended("openai").or_else(|| recommended("anthropic")).or_else(|| first("openai")),
        gemini,
    }
}

/// 统一供应商定义。Key 位写占位，真实 Key 在转发时由 forwarder 注入。
pub fn universal_provider(b: &Bootstrap, existing: Option<&UniversalProvider>) -> UniversalProvider {
    let pick = pick_models(b);
    // 用户在 UI 里改过模型就保留，只在没有值时用推荐值
    let prev_claude = existing.and_then(|e| e.models.claude.clone()).unwrap_or_default();
    let prev_codex = existing.and_then(|e| e.models.codex.clone()).unwrap_or_default();
    // 之前选的 Gemini 已下架就换回推荐值（agy 会被 new-api 拒）
    let gemini = existing
        .and_then(|e| e.models.gemini.as_ref())
        .and_then(|g| g.model.clone())
        .filter(|m| b.models.is_empty() || b.models.iter().any(|x| x == m))
        .or(pick.gemini);
    let mut p = UniversalProvider::new(
        PROVIDER_ID.to_string(),
        "Loov".to_string(),
        "newapi".to_string(),
        b.api.base_url.trim_end_matches('/').to_string(),
        KEY_PLACEHOLDER.to_string(),
    );
    p.apps = UniversalProviderApps {
        claude: true,
        codex: pick.codex.is_some() || prev_codex.model.is_some(),
        gemini: gemini.is_some(),
    };
    p.models = UniversalProviderModels {
        claude: Some(ClaudeModelConfig {
            model: prev_claude.model.or(pick.claude),
            haiku_model: prev_claude.haiku_model.or(pick.haiku),
            sonnet_model: prev_claude.sonnet_model.or(pick.sonnet),
            opus_model: prev_claude.opus_model.or(pick.opus),
        }),
        codex: Some(CodexModelConfig {
            model: prev_codex.model.or(pick.codex),
            reasoning_effort: prev_codex.reasoning_effort.or_else(|| Some("medium".to_string())),
        }),
        gemini: gemini.map(|m| GeminiModelConfig { model: Some(m) }),
    };
    p.website_url = Some(b.topup_url.clone()).filter(|s| !s.is_empty());
    p.notes = Some("由 Loov Agent 管理：Key 不落盘，请勿手工编辑".to_string());
    p.icon = Some("newapi".to_string());
    p.created_at = existing.and_then(|e| e.created_at).or_else(|| Some(chrono::Utc::now().timestamp()));
    p.sort_index = existing.and_then(|e| e.sort_index).or(Some(0));
    p
}

/// 写入 / 更新统一供应商并同步到 Claude、Codex、Gemini 的子供应商；Claude Desktop 的供应商一并刷新。
pub fn apply(state: &AppState, b: &Bootstrap) -> Result<(), AppError> {
    let existing = ProviderService::get_universal(state, PROVIDER_ID)?;
    let provider = universal_provider(b, existing.as_ref());
    ProviderService::upsert_universal(state, provider)?;
    ProviderService::sync_universal_to_apps(state, PROVIDER_ID)?;
    if let Some(desktop) = desktop_provider(b) {
        state.db.save_provider(AppType::ClaudeDesktop.as_str(), &desktop)?;
    }
    let keep = grok_model(state).filter(|m| b.models.is_empty() || b.models.iter().any(|x| x == m));
    if let Some(grok) = grok_provider(b, keep.or_else(|| pick_models(b).claude).as_deref()) {
        state.db.save_provider(AppType::GrokBuild.as_str(), &grok)?;
    }
    Ok(())
}

/// Grok Build 的 Loov 供应商：一张模型表 `loov`，地址是 new-api、Key 位是占位（路由模式下 forwarder 注入真实 Key）。
/// Grok 只会发 Responses：Claude / GPT 走 new-api 原生 `/v1/responses`；Gemini 在 new-api 上不支持 responses，
/// 标成 `openai_chat`，由本地路由转成 Chat Completions。
pub fn grok_provider(b: &Bootstrap, model: Option<&str>) -> Option<Provider> {
    let model = model.filter(|m| !m.is_empty())?;
    let base = b.api.base_url.trim_end_matches('/');
    let base = if base.ends_with("/v1") { base.to_string() } else { format!("{base}/v1") };
    let config = format!(
        "[models]\ndefault = \"loov\"\n\n[model.loov]\nname = \"Loov\"\nmodel = {model}\nbase_url = {base}\napi_key = {key}\napi_backend = \"responses\"\ncontext_window = 200000\n",
        model = toml_str(model),
        base = toml_str(&base),
        key = toml_str(KEY_PLACEHOLDER),
    );
    let mut p = Provider::with_id(
        GROK_PROVIDER_ID.to_string(),
        "Loov".to_string(),
        json!({ "config": config }),
        Some(b.topup_url.clone()).filter(|s| !s.is_empty()),
    );
    p.category = Some("aggregator".to_string());
    p.notes = Some("由 Loov Agent 管理：Key 不落盘，请勿手工编辑".to_string());
    p.icon = Some("newapi".to_string());
    p.sort_index = Some(0);
    p.meta = Some(ProviderMeta {
        api_format: model.starts_with("gemini-").then(|| "openai_chat".to_string()),
        ..Default::default()
    });
    Some(p)
}

/// TOML 字符串字面量（带引号与转义）。
fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// Grok Build 当前的模型（Loov 供应商表里的 `model`）。
pub fn grok_model(state: &AppState) -> Option<String> {
    let p = state.db.get_provider_by_id(GROK_PROVIDER_ID, AppType::GrokBuild.as_str()).ok().flatten()?;
    let text = p.settings_config.get("config")?.as_str()?;
    let doc: toml::Value = text.parse().ok()?;
    doc.get("model")?.get("loov")?.get("model")?.as_str().map(str::to_string)
}

/// 员工在 Loov 页给 Grok Build 换模型：重写供应商；已在路由模式时同步到 ~/.grok/config.toml。
pub async fn set_grok_model(state: &AppState, model: &str) -> Result<(), String> {
    let b = super::credentials()
        .and_then(|c| c.bootstrap)
        .ok_or_else(|| "尚未登录".to_string())?;
    if !b.models.is_empty() && !b.models.iter().any(|m| m == model) {
        return Err(format!("Loov 账号下没有模型 {model}"));
    }
    let p = grok_provider(&b, Some(model)).ok_or_else(|| "模型无效".to_string())?;
    state.db.save_provider(AppType::GrokBuild.as_str(), &p).map_err(|e| e.to_string())?;
    if crate::mode::current::is_proxy(&AppType::GrokBuild) {
        crate::mode::controller::resync_route(state, &AppType::GrokBuild).await?;
    }
    Ok(())
}

/// 让 Grok Build 进入路由模式并路由到 Loov（Loov 页启动 Grok 时调用）。
pub async fn attach_grok(state: &AppState) -> Result<(), String> {
    let exists = state
        .db
        .get_provider_by_id(GROK_PROVIDER_ID, AppType::GrokBuild.as_str())
        .map(|p| p.is_some())
        .unwrap_or(false);
    if !exists {
        return Err("Loov 账号下没有可给 Grok 用的模型".to_string());
    }
    let routed = crate::mode::current::is_proxy(&AppType::GrokBuild)
        && crate::mode::current::provider_in_use(&state.db, &AppType::GrokBuild)
            .ok()
            .flatten()
            .is_some_and(|p| p.id == GROK_PROVIDER_ID);
    if routed {
        return Ok(());
    }
    crate::mode::controller::enter_with_route(state, &AppType::GrokBuild, false, Some(GROK_PROVIDER_ID)).await
}

/// Codex 桌面版没登录官方 ChatGPT 账号时只能选官方默认模型（new-api 上没有），请求会被拒。
/// 发往 Loov Codex 子供应商的请求里，模型不在 Loov 目录就换成员工在 Loov 页选的 Codex 模型。
/// 返回换成的模型（没换为 None）。
pub fn coerce_codex_model(provider: &Provider, body: &mut Value) -> Option<String> {
    if provider.id != codex_child_id() {
        return None;
    }
    let models = super::credentials().and_then(|c| c.bootstrap).map(|b| b.models).unwrap_or_default();
    coerce_model_in(provider, body, &models)
}

fn coerce_model_in(provider: &Provider, body: &mut Value, models: &[String]) -> Option<String> {
    let current = body.get("model").and_then(Value::as_str)?;
    if models.is_empty() || models.iter().any(|m| m == current) {
        return None;
    }
    let text = provider.settings_config.get("config").and_then(Value::as_str)?;
    let doc: toml::Value = text.parse().ok()?;
    let chosen = doc.get("model").and_then(toml::Value::as_str).filter(|m| !m.is_empty())?.to_string();
    log::info!("[loov] Codex 请求的模型 {current} 不在 Loov 目录，改用 {chosen}");
    body["model"] = Value::String(chosen.clone());
    Some(chosen)
}

/// Claude Desktop 的 Loov 供应商：本地路由模式（Desktop → 本地路由 `/claude-desktop` → new-api），
/// 模型菜单就是 Loov 目录里的 Claude 模型（id 本身就是 `claude-` 开头，路由 id 与上游同名）。
/// Key 位是占位，真实 Key 由 forwarder 注入。没有 Claude 模型时不建。
pub fn desktop_provider(b: &Bootstrap) -> Option<Provider> {
    let ids: Vec<&str> = if b.model_catalog.is_empty() {
        b.models.iter().map(String::as_str).collect()
    } else {
        b.model_catalog.iter().filter(|m| m.kind.is_empty() || m.kind == "anthropic").map(|m| m.id.as_str()).collect()
    };
    let routes: std::collections::HashMap<String, ClaudeDesktopModelRoute> = ids
        .into_iter()
        .filter(|id| crate::claude_desktop_config::is_claude_safe_model_id(id))
        .map(|id| (id.to_string(), ClaudeDesktopModelRoute { model: id.to_string(), label_override: None, supports_1m: None }))
        .collect();
    if routes.is_empty() {
        return None;
    }
    let mut p = Provider::with_id(
        DESKTOP_PROVIDER_ID.to_string(),
        "Loov".to_string(),
        json!({ "env": {
            "ANTHROPIC_BASE_URL": b.api.base_url.trim_end_matches('/'),
            "ANTHROPIC_AUTH_TOKEN": KEY_PLACEHOLDER,
        }}),
        Some(b.topup_url.clone()).filter(|s| !s.is_empty()),
    );
    p.category = Some("aggregator".to_string());
    p.notes = Some("由 Loov Agent 管理：Key 不落盘，请勿手工编辑".to_string());
    p.icon = Some("newapi".to_string());
    p.sort_index = Some(0);
    p.meta = Some(ProviderMeta {
        claude_desktop_mode: Some(ClaudeDesktopMode::Proxy),
        claude_desktop_model_routes: routes,
        ..Default::default()
    });
    Some(p)
}

/// 让 Claude Desktop 进入路由模式并路由到 Loov（Loov 页启动 Desktop 时调用；写 Desktop 的 3p 网关配置）。
pub async fn attach_desktop(state: &AppState) -> Result<(), String> {
    let exists = state
        .db
        .get_provider_by_id(DESKTOP_PROVIDER_ID, AppType::ClaudeDesktop.as_str())
        .map(|p| p.is_some())
        .unwrap_or(false);
    if !exists {
        return Err("Loov 账号下没有可用的 Claude 模型".to_string());
    }
    crate::mode::controller::enter_with_route(state, &AppType::ClaudeDesktop, false, Some(DESKTOP_PROVIDER_ID)).await
}

/// 让 Claude Code 与 Codex 进入路由模式并路由到 Loov 的子供应商。
pub async fn attach(state: &AppState) -> Result<(), String> {
    crate::mode::controller::enter_with_route(state, &AppType::Claude, false, Some(&claude_child_id())).await?;
    let has_codex = state
        .db
        .get_provider_by_id(&codex_child_id(), "codex")
        .map(|p| p.is_some())
        .unwrap_or(false);
    if has_codex {
        crate::mode::controller::enter_with_route(state, &AppType::Codex, false, Some(&codex_child_id())).await?;
    }
    Ok(())
}

fn env_key(name: &str) -> KeyPath {
    KeyPath::new(&["env", name])
}

/// 代理契约里补写的中和值。`settings.json` 的 `env` 优先于 shell / 注册表里的同名变量，
/// 但契约只写了地址、令牌和模型档位，员工系统环境里的 `CLAUDE_CODE_USE_VERTEX=1`、
/// `ANTHROPIC_API_KEY`、`ANTHROPIC_MODEL` 仍会生效——前者让 Claude Code 直连云厂商绕过 Loov，
/// 后两者触发鉴权冲突提示或把请求发成路由不认识的模型名。这些键都是关键字段，切走时由投影清掉。
///
/// `route_env` 是 Loov 供应商行的 env：其中 `ANTHROPIC_MODEL` 是员工在 Loov 页选的默认模型。代理契约
/// 只写了各档映射，不写默认档，Claude Code 会自己挑 opus 档（实测 0.1.3 选 sonnet 仍全走 opus）。
/// 这里把所选模型换成它所在的档位别名写进 `ANTHROPIC_MODEL`，不在任何档位时直接写模型 id。
pub fn neutralize_claude_env(env: &mut serde_json::Map<String, Value>, route_env: &serde_json::Map<String, Value>) {
    for key in crate::live::floor::CLAUDE_PROTOCOL_SELECTORS {
        env.insert((*key).to_string(), json!("0"));
    }
    if let Some(model) = default_model_alias(route_env) {
        env.insert("ANTHROPIC_MODEL".to_string(), json!(model));
    }
    for key in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_MODEL", "ANTHROPIC_SMALL_FAST_MODEL"] {
        env.entry(key.to_string()).or_insert_with(|| json!(""));
    }
}

/// 供应商行里选定的默认模型对应的 Claude Code 档位别名（`haiku` / `sonnet` / `opus`）。
fn default_model_alias(route_env: &serde_json::Map<String, Value>) -> Option<String> {
    let get = |k: &str| route_env.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let model = get("ANTHROPIC_MODEL")?;
    for (tier, key) in [
        ("sonnet", "ANTHROPIC_DEFAULT_SONNET_MODEL"),
        ("opus", "ANTHROPIC_DEFAULT_OPUS_MODEL"),
        ("haiku", "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
    ] {
        if get(key) == Some(model) {
            return Some(tier.to_string());
        }
    }
    Some(model.to_string())
}

/// Claude Code 加固键：这些键都不在 `live/floor.rs` 的关键字段里，CC Switch 的投影不读不清。
/// `proxy_url` 为 `None` 时写黑洞代理：登出 / 隧道未就绪时 WebFetch 等自发流量也不会直连。
pub fn claude_harden_patch(proxy_url: Option<&str>) -> JsonPatch {
    let proxy = proxy_url.unwrap_or(BLACKHOLE_PROXY);
    JsonPatch {
        set: vec![
            (KeyPath::root().child("skipWebFetchPreflight"), Value::Bool(true)),
            (env_key("HTTPS_PROXY"), json!(proxy)),
            (env_key("HTTP_PROXY"), json!(proxy)),
            (env_key("NO_PROXY"), json!("127.0.0.1,localhost")),
            (env_key("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"), json!("1")),
            (env_key("DISABLE_TELEMETRY"), json!("1")),
            (env_key("DISABLE_AUTOUPDATER"), json!("1")),
            // auto mode 的服务端分类检查要求网关原样透传 safeguards 字段，Loov 的上游（new-api → Vertex）
            // 不支持；不关会每个会话提示「this session isn't eligible」。关掉后分类请求照旧经 Loov 计费
            (env_key("CLAUDE_CODE_AUTO_MODE_SERVER"), json!("0")),
        ],
        ..Default::default()
    }
}

pub fn harden_claude(db: &Database, proxy_url: Option<&str>) -> Result<(), AppError> {
    let patch = claude_harden_patch(proxy_url);
    claude_direct::run(db, OP_HARDEN, Some(&patch), PendingTarget::default())?;
    Ok(())
}

/// 撤掉加固键（用户彻底退出 Loov 模式时）。
pub fn unharden_claude(db: &Database) -> Result<(), AppError> {
    let patch = JsonPatch {
        remove: vec![
            KeyPath::root().child("skipWebFetchPreflight"),
            env_key("HTTPS_PROXY"),
            env_key("HTTP_PROXY"),
            env_key("NO_PROXY"),
            env_key("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
            env_key("DISABLE_TELEMETRY"), // 审计 S2-02：harden 写入了它，unharden 需对应移除
            env_key("DISABLE_AUTOUPDATER"),
            env_key("CLAUDE_CODE_AUTO_MODE_SERVER"),
        ],
        ..Default::default()
    };
    claude_direct::run(db, "loov.unharden", Some(&patch), PendingTarget::default())?;
    Ok(())
}

/// Codex 加固：关闭插件市场与遥测外连（否则会连 api.github.com / chatgpt.com）。
pub fn codex_harden_patch() -> TomlPatch {
    TomlPatch {
        set: vec![
            (KeyPath::new(&["features", "plugins"]), toml_edit::value(false)),
            (KeyPath::new(&["analytics", "enabled"]), toml_edit::value(false)),
        ],
        ..Default::default()
    }
}

pub fn harden_codex(db: &Database) -> Result<(), AppError> {
    let patch = codex_harden_patch();
    let path = crate::codex_config::get_codex_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let write = AppWrite::begin(db, AppType::Codex.as_str())?;
    let changes = [FileChange { file: LiveFile::private(path), patch: &patch }];
    write.run(OP_HARDEN, &changes, PendingTarget::default())?;
    Ok(())
}

/// 云端搜索 MCP 条目的保留 id（UnifiedMcpPanel 对 tag=loov 的条目禁止编辑 / 删除）。
pub const MCP_ID: &str = "loov-search";
pub const MCP_TAG: &str = "loov";

/// 本地路由上的 MCP 地址（跟随用户配置的路由端口）。
pub fn mcp_url(state: &AppState) -> String {
    let (host, port) = state.db.get_proxy_client_endpoint_sync();
    format!("{}/loov/mcp", crate::services::proxy::proxy_origin(&host, port))
}

/// 下发给 Claude Code / Codex 的 MCP 定义：只带本地路由令牌 lat_。
pub fn mcp_server_def(url: &str, local_token: &str) -> crate::app_config::McpServer {
    crate::app_config::McpServer {
        id: MCP_ID.to_string(),
        name: "Loov 云端搜索".to_string(),
        server: json!({
            "type": "http",
            "url": url,
            "headers": { "Authorization": format!("Bearer {local_token}") },
            "startup_timeout_sec": 20,
            "tool_timeout_sec": 120
        }),
        apps: crate::app_config::McpApps { claude: true, codex: true, ..Default::default() },
        description: Some("由 Loov Agent 托管：经公司安全通道提供 web_search / web_fetch".to_string()),
        homepage: None,
        docs: None,
        tags: vec![MCP_TAG.to_string()],
    }
}

/// 写入 / 更新云端搜索 MCP（令牌轮换后要重写）。持 claude、codex 的切换锁，避免与切换并发读改写。
pub async fn apply_mcp(state: &AppState) -> Result<(), AppError> {
    let _claude = state.proxy_service.lock_switch_for_app("claude").await;
    let _codex = state.proxy_service.lock_switch_for_app("codex").await;
    let def = mcp_server_def(&mcp_url(state), &super::local_auth::live_token());
    crate::services::McpService::upsert_server(state, def)
}

/// 恢复原配置时删除云端搜索 MCP。
pub async fn remove_mcp(state: &AppState) -> Result<(), AppError> {
    let _claude = state.proxy_service.lock_switch_for_app("claude").await;
    let _codex = state.proxy_service.lock_switch_for_app("codex").await;
    crate::services::McpService::delete_server(state, MCP_ID).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loov::hub_client::*;

    fn bootstrap(models: &[&str], catalog: Vec<ModelEntry>) -> Bootstrap {
        Bootstrap {
            user: HubUser { username: "u".into() },
            device: HubDevice { id: "d".into(), name: "n".into() },
            api: ApiInfo { base_url: "https://me.loov.cc/".into(), key: "sk-x".into() },
            tunnel: TunnelProfile { version: 1, nodes: vec![] },
            models: models.iter().map(|s| s.to_string()).collect(),
            model_catalog: catalog,
            topup_url: "https://me.loov.cc/console/topup".into(),
        }
    }

    #[test]
    fn picks_models_by_prefix_without_catalog() {
        let b = bootstrap(&["gemini-4-argon", "claude-haiku-5-5", "claude-sonnet-5", "gpt-6-sol"], vec![]);
        let p = pick_models(&b);
        assert_eq!(p.claude.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(p.haiku.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(p.opus.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(p.codex.as_deref(), Some("gpt-6-sol"));
    }

    #[test]
    fn recommended_wins() {
        let b = bootstrap(
            &[],
            vec![
                ModelEntry { id: "claude-sonnet-5".into(), kind: "anthropic".into(), recommended: false },
                ModelEntry { id: "claude-opus-5-5".into(), kind: "anthropic".into(), recommended: true },
                ModelEntry { id: "gpt-6-sol".into(), kind: "openai".into(), recommended: true },
            ],
        );
        let p = pick_models(&b);
        assert_eq!(p.claude.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(p.codex.as_deref(), Some("gpt-6-sol"));
    }

    #[test]
    fn codex_falls_back_to_recommended_claude_when_no_gpt_recommended() {
        let b = bootstrap(
            &[],
            vec![
                ModelEntry { id: "claude-sonnet-5".into(), kind: "anthropic".into(), recommended: true },
                ModelEntry { id: "gpt-6-sol".into(), kind: "openai".into(), recommended: false },
            ],
        );
        let p = pick_models(&b);
        assert_eq!(p.claude.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(p.codex.as_deref(), Some("claude-sonnet-5"));
    }

    #[test]
    fn universal_provider_uses_placeholder_key_and_trimmed_base() {
        let b = bootstrap(&["claude-sonnet-5", "gpt-6-sol"], vec![]);
        let p = universal_provider(&b, None);
        assert_eq!(p.api_key, KEY_PLACEHOLDER);
        assert_eq!(p.base_url, "https://me.loov.cc");
        assert!(p.apps.claude && p.apps.codex && !p.apps.gemini);
        let claude = p.to_claude_provider().unwrap();
        assert_eq!(claude.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"], KEY_PLACEHOLDER);
        let codex = p.to_codex_provider().unwrap();
        assert_eq!(codex.settings_config["auth"]["OPENAI_API_KEY"], KEY_PLACEHOLDER);
        assert!(codex.settings_config["config"].as_str().unwrap().contains("https://me.loov.cc/v1"));
    }

    #[test]
    fn neutralize_overrides_selectors_but_keeps_written_keys() {
        let mut env = serde_json::Map::new();
        env.insert("ANTHROPIC_AUTH_TOKEN".into(), json!("lat_x"));
        env.insert("ANTHROPIC_BASE_URL".into(), json!("http://127.0.0.1:15721"));
        neutralize_claude_env(&mut env, &serde_json::Map::new());
        assert_eq!(env["CLAUDE_CODE_USE_VERTEX"], "0");
        assert_eq!(env["CLAUDE_CODE_USE_BEDROCK"], "0");
        assert_eq!(env["CLAUDE_CODE_USE_FOUNDRY"], "0");
        assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "lat_x");
        assert_eq!(env["ANTHROPIC_API_KEY"], "");
        assert_eq!(env["ANTHROPIC_MODEL"], "");
        assert_eq!(env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:15721");
        // 全是关键字段：切走 / 登出时由投影清空，不会残留
        assert!(env.keys().all(|k| crate::live::floor::claude_floor_env(k)));
    }

    #[test]
    fn neutralize_writes_tier_alias_of_chosen_default_model() {
        let route = |m: &str| -> serde_json::Map<String, Value> {
            serde_json::from_value(json!({
                "ANTHROPIC_MODEL": m,
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-haiku-5-5",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-sonnet-5",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-5-5"
            }))
            .unwrap()
        };
        for (chosen, want) in [
            ("claude-sonnet-5", "sonnet"),
            ("claude-opus-5-5", "opus"),
            ("claude-haiku-5-5", "haiku"),
            ("claude-fable-5-1", "claude-fable-5-1"),
        ] {
            let mut env = serde_json::Map::new();
            neutralize_claude_env(&mut env, &route(chosen));
            assert_eq!(env["ANTHROPIC_MODEL"], want, "{chosen}");
        }
        // 供应商行没有默认模型：置空，交给 Claude Code
        let mut env = serde_json::Map::new();
        neutralize_claude_env(&mut env, &serde_json::Map::new());
        assert_eq!(env["ANTHROPIC_MODEL"], "");
    }

    #[test]
    fn mcp_def_only_carries_local_token() {
        let def = mcp_server_def("http://127.0.0.1:15721/loov/mcp", "lat_abc");
        assert_eq!(def.id, MCP_ID);
        assert!(def.apps.claude && def.apps.codex && !def.apps.gemini);
        assert_eq!(def.server["type"], "http");
        assert_eq!(def.server["headers"]["Authorization"], "Bearer lat_abc");
        let s = def.server.to_string();
        assert!(!s.contains("ldt_") && !s.contains("sk-"));
        assert_eq!(def.tags, vec!["loov".to_string()]);
    }

    #[test]
    fn harden_keys_are_outside_claude_floor() {
        let patch = claude_harden_patch(Some("http://loov:p@127.0.0.1:1234"));
        for (path, _) in &patch.set {
            if path.0.first().map(String::as_str) == Some("env") {
                assert!(!crate::live::floor::claude_floor_env(&path.0[1]), "{}", path);
            }
        }
    }

    #[test]
    fn claude_patch_applies_in_place() {
        let mut doc = json!({"env": {"ANTHROPIC_MODEL": "x", "HTTPS_PROXY": "old"}, "other": 1});
        claude_harden_patch(None)
            .apply_to(std::path::Path::new("settings.json"), &mut doc)
            .unwrap();
        assert_eq!(doc["env"]["HTTPS_PROXY"], BLACKHOLE_PROXY);
        assert_eq!(doc["env"]["ANTHROPIC_MODEL"], "x");
        assert_eq!(doc["skipWebFetchPreflight"], true);
        assert_eq!(doc["other"], 1);
    }

    #[test]
    fn codex_patch_creates_tables() {
        let mut doc: toml_edit::DocumentMut = "model = \"gpt-6-sol\"\n".parse().unwrap();
        codex_harden_patch().apply_to(std::path::Path::new("config.toml"), &mut doc).unwrap();
        let s = doc.to_string();
        assert!(s.contains("[features]") && s.contains("plugins = false"), "{s}");
        assert!(s.contains("[analytics]") && s.contains("enabled = false"), "{s}");
        assert!(s.starts_with("model = \"gpt-6-sol\""));
    }

    #[test]
    fn picks_agent_gemini_and_skips_special_models() {
        let b = bootstrap(
            &["gemini-embedding-001", "gemini-nano-banana-2.1", "gemini-3.8-flash-cyber", "gemini-4-argon", "gemini-3.8-flash", "claude-sonnet-5"],
            vec![],
        );
        assert_eq!(pick_models(&b).gemini.as_deref(), Some("gemini-3.8-flash"));
        let none = bootstrap(&["gemini-embedding-001", "claude-sonnet-5"], vec![]);
        assert_eq!(pick_models(&none).gemini, None);
        let rec = bootstrap(
            &[],
            vec![
                ModelEntry { id: "gemini-3.8-flash".into(), kind: "gemini".into(), recommended: false },
                ModelEntry { id: "gemini-4-argon".into(), kind: "gemini".into(), recommended: true },
            ],
        );
        assert_eq!(pick_models(&rec).gemini.as_deref(), Some("gemini-4-argon"));
    }

    #[test]
    fn universal_provider_enables_gemini_and_drops_retired_choice() {
        let b = bootstrap(&["claude-sonnet-5", "gemini-3.8-flash", "gemini-4-argon"], vec![]);
        let first = universal_provider(&b, None);
        assert!(first.apps.gemini);
        assert_eq!(first.models.gemini.as_ref().and_then(|g| g.model.as_deref()), Some("gemini-3.8-flash"));

        let mut chosen = first.clone();
        chosen.models.gemini = Some(GeminiModelConfig { model: Some("gemini-4-argon".into()) });
        let kept = universal_provider(&b, Some(&chosen));
        assert_eq!(kept.models.gemini.as_ref().and_then(|g| g.model.as_deref()), Some("gemini-4-argon"));

        chosen.models.gemini = Some(GeminiModelConfig { model: Some("gemini-2.5-pro".into()) });
        let replaced = universal_provider(&b, Some(&chosen));
        assert_eq!(replaced.models.gemini.as_ref().and_then(|g| g.model.as_deref()), Some("gemini-3.8-flash"), "已下架的模型换回推荐值");

        let no_gemini = universal_provider(&bootstrap(&["claude-sonnet-5"], vec![]), None);
        assert!(!no_gemini.apps.gemini);
    }

    #[test]
    fn desktop_provider_routes_claude_models_through_loov() {
        let b = bootstrap(&["claude-sonnet-5", "claude-opus-5-5", "gpt-6-sol", "gemini-3.8-flash"], vec![]);
        let p = desktop_provider(&b).expect("desktop provider");
        assert_eq!(p.id, DESKTOP_PROVIDER_ID);
        assert!(is_loov_provider(&p.id));
        assert_eq!(p.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"], KEY_PLACEHOLDER);
        assert_eq!(p.settings_config["env"]["ANTHROPIC_BASE_URL"], "https://me.loov.cc");
        let meta = p.meta.as_ref().unwrap();
        assert_eq!(meta.claude_desktop_mode, Some(ClaudeDesktopMode::Proxy));
        let mut routes: Vec<_> = meta.claude_desktop_model_routes.keys().cloned().collect();
        routes.sort();
        assert_eq!(routes, ["claude-opus-5-5", "claude-sonnet-5"]);
        crate::claude_desktop_config::validate_proxy_provider(&p).expect("Desktop 路由模式校验通过");
        assert!(desktop_provider(&bootstrap(&["gpt-6-sol"], vec![])).is_none());
    }

    #[test]
    fn gemini_child_is_loov_managed() {
        assert!(is_loov_provider("universal-gemini-loov"));
        assert!(!is_loov_provider("universal-gemini-other"));
        assert!(is_agent_gemini("gemini-4-argon"));
        assert!(!is_agent_gemini("gemini-embedding-001"));
        assert!(!is_agent_gemini("claude-sonnet-5"));
    }

    #[test]
    fn grok_provider_uses_responses_for_claude_and_chat_for_gemini() {
        let b = bootstrap(&["claude-sonnet-5", "gemini-3.8-flash"], vec![]);
        let p = grok_provider(&b, Some("claude-sonnet-5")).unwrap();
        assert_eq!(p.id, GROK_PROVIDER_ID);
        assert!(is_loov_provider(&p.id));
        let text = p.settings_config["config"].as_str().unwrap();
        crate::grok_config::validate_config_toml(text).expect("Grok 配置校验通过");
        assert!(text.contains("base_url = \"https://me.loov.cc/v1\""), "{text}");
        assert!(text.contains(&format!("api_key = \"{KEY_PLACEHOLDER}\"")));
        assert!(p.meta.as_ref().unwrap().api_format.is_none());
        let g = grok_provider(&b, Some("gemini-3.8-flash")).unwrap();
        assert_eq!(g.meta.as_ref().unwrap().api_format.as_deref(), Some("openai_chat"));
        assert!(grok_provider(&b, None).is_none());
    }

    #[test]
    fn codex_unknown_model_is_coerced_to_the_chosen_one() {
        let mut p = Provider::with_id(
            codex_child_id(),
            "Loov".into(),
            json!({ "config": "model_provider = \"loov\"\nmodel = \"claude-sonnet-5\"\n" }),
            None,
        );
        let models = vec!["claude-sonnet-5".to_string(), "gpt-6-sol".to_string()];
        let mut body = json!({ "model": "gpt-5.5-codex", "input": [] });
        assert_eq!(coerce_model_in(&p, &mut body, &models).as_deref(), Some("claude-sonnet-5"));
        assert_eq!(body["model"], "claude-sonnet-5");
        let mut known = json!({ "model": "gpt-6-sol" });
        assert!(coerce_model_in(&p, &mut known, &models).is_none(), "目录里有的不换");
        assert!(coerce_model_in(&p, &mut json!({ "model": "x" }), &[]).is_none(), "目录未知时不换");
        p.id = "other".into();
        assert!(coerce_codex_model(&p, &mut json!({ "model": "x" })).is_none(), "只管 Loov 的 Codex 子供应商");
    }
}
