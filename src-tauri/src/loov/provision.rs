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
    ClaudeDesktopMode, ClaudeDesktopModelRoute, ClaudeModelConfig, ClaudeStackModel, CodexModelConfig, GeminiModelConfig,
    Provider, ProviderMeta, UniversalProvider, UniversalProviderApps, UniversalProviderModels,
};
use crate::services::provider::{claude_direct, ProviderService};
use crate::store::AppState;

use super::hub_client::Bootstrap;
use super::{BLACKHOLE_PROXY, KEY_PLACEHOLDER, PROVIDER_ID};

/// 操作名（写进 pending 记录，便于排障）。
const OP_HARDEN: &str = "loov.harden";
const OP_GROK_MODELS: &str = "loov.grok-models";

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
        || MEMBERS.iter().any(|m| m.id == provider_id)
}

/// 聚合（Stack）成员：让每个 agent 自己的模型选择器里列出 Loov 的全部模型。主路由（Loov 子供应商）
/// 只认一种协议，另外几家模型由这些成员承接，本地路由按成员的 `apiFormat` 转换协议，都发往 new-api：
/// - Claude Code / Claude Desktop 说 Anthropic：GPT 走 new-api 原生 /v1/responses，Gemini 走
///   /v1/chat/completions（new-api 的 Gemini 不支持 responses）。
/// - Codex 说 Responses：GPT、Claude 由主路由直接发（new-api 原生 /v1/responses），Gemini 走 chat。
pub struct Member {
    pub id: &'static str,
    pub app: AppType,
    /// 承接哪一家的模型（`model_kind`）
    pub kind: &'static str,
    pub api_format: &'static str,
    pub name: &'static str,
    /// 内置图标名；也决定聚合 key（模型 id 里的 `ccs-claude-<key>--…`）
    pub icon: &'static str,
}

pub const MEMBERS: [Member; 5] = [
    Member { id: "loov-claude-gpt", app: AppType::Claude, kind: "openai", api_format: "openai_responses", name: "Loov GPT", icon: "openai" },
    Member { id: "loov-claude-gemini", app: AppType::Claude, kind: "gemini", api_format: "openai_chat", name: "Loov Gemini", icon: "gemini" },
    Member { id: "loov-codex-gemini", app: AppType::Codex, kind: "gemini", api_format: "openai_chat", name: "Loov Gemini", icon: "gemini" },
    Member { id: "loov-desktop-gpt", app: AppType::ClaudeDesktop, kind: "openai", api_format: "openai_responses", name: "Loov GPT", icon: "openai" },
    Member { id: "loov-desktop-gemini", app: AppType::ClaudeDesktop, kind: "gemini", api_format: "openai_chat", name: "Loov Gemini", icon: "gemini" },
];

/// 某一家的对话模型（Gemini 去掉向量、生图等专用模型），按 hub 目录顺序。
pub fn models_of_kind(b: &Bootstrap, kind: &str) -> Vec<String> {
    let ids: Vec<&str> = if b.model_catalog.is_empty() {
        b.models.iter().map(String::as_str).collect()
    } else {
        b.model_catalog.iter().map(|m| m.id.as_str()).collect()
    };
    ids.into_iter()
        .filter(|id| model_kind(b, id) == kind && (kind != "gemini" || is_agent_gemini(id)))
        .map(str::to_string)
        .collect()
}

/// 聚合成员的供应商行；这一家在 Loov 目录里没有模型时为 None（不加入聚合）。
pub fn member_provider(b: &Bootstrap, m: &Member) -> Option<Provider> {
    let models = models_of_kind(b, m.kind);
    let first = models.first()?.clone();
    let base = b.api.base_url.trim_end_matches('/');
    let mut meta = ProviderMeta { api_format: Some(m.api_format.to_string()), ..Default::default() };
    let settings = match m.app {
        AppType::Codex => {
            let v1 = if base.ends_with("/v1") { base.to_string() } else { format!("{base}/v1") };
            let config = format!(
                "model_provider = \"loov\"\nmodel = {model}\n\n[model_providers.loov]\nname = \"Loov\"\nbase_url = {base}\nwire_api = \"responses\"\nrequires_openai_auth = true\n",
                model = toml_str(&first),
                base = toml_str(&v1),
            );
            // 协议由 meta.apiFormat（openai_chat）决定；这份配置只用来生成聚合目录，不写进员工的 config.toml
            let catalog: Vec<Value> = models.iter().map(|id| json!({ "model": id, "displayName": id })).collect();
            json!({ "auth": { "OPENAI_API_KEY": KEY_PLACEHOLDER }, "config": config, "modelCatalog": { "models": catalog } })
        }
        _ => {
            if m.app == AppType::ClaudeDesktop {
                meta.claude_desktop_mode = Some(ClaudeDesktopMode::Proxy);
                meta.claude_desktop_model_routes = models
                    .iter()
                    .map(|id| (id.clone(), ClaudeDesktopModelRoute { model: id.clone(), label_override: Some(id.clone()), supports_1m: None }))
                    .collect();
            } else {
                meta.stack_models = Some(
                    models.iter().map(|id| ClaudeStackModel { model: id.clone(), display_name: Some(id.clone()), one_m: false }).collect(),
                );
            }
            json!({ "env": {
                "ANTHROPIC_BASE_URL": base,
                "ANTHROPIC_AUTH_TOKEN": KEY_PLACEHOLDER,
                "ANTHROPIC_MODEL": first,
            }})
        }
    };
    let mut p = Provider::with_id(m.id.to_string(), m.name.to_string(), settings, Some(b.topup_url.clone()).filter(|s| !s.is_empty()));
    p.category = Some("aggregator".to_string());
    p.notes = Some("由 Loov Agent 管理：Key 不落盘，请勿手工编辑".to_string());
    p.icon = Some(m.icon.to_string());
    p.sort_index = Some(1);
    p.meta = Some(meta);
    Some(p)
}

/// 写入（或删掉不再有模型的）聚合成员行。
fn save_members(state: &AppState, b: &Bootstrap) -> Result<(), AppError> {
    for m in &MEMBERS {
        match member_provider(b, m) {
            Some(p) => state.db.save_provider(m.app.as_str(), &p)?,
            None => {
                let _ = state.db.delete_provider(m.app.as_str(), m.id);
            }
        }
    }
    Ok(())
}

/// 让 `app` 进入聚合模式：默认路由 `route`，成员按现有的行加入、没有行的移出。
/// 已在聚合模式时只同步成员（每次变化会重写客户端配置）。
async fn enter_stack(state: &AppState, app: &AppType, route: &str) -> Result<(), String> {
    crate::mode::controller::enter_with_route(state, app, true, Some(route)).await?;
    for m in MEMBERS.iter().filter(|m| &m.app == app) {
        let exists = state.db.get_provider_by_id(m.id, app.as_str()).ok().flatten().is_some();
        if let Err(e) = crate::mode::controller::set_stack_member(state, app, m.id, exists).await {
            // 成员加不上不影响主路由：员工仍能用默认模型，只是选择器里少了这一家
            log::error!("[loov] {} 聚合成员 {} 同步失败：{}", app.as_str(), m.id, e.message);
        }
    }
    Ok(())
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

/// 模型属于哪一家：hub 目录里标了就用标的，否则按名字前缀猜。
fn model_kind(b: &Bootstrap, id: &str) -> &'static str {
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
}

/// Codex 能选的模型：GPT 在前，Claude 在后（new-api 原生 /v1/responses 都支持；Gemini 不支持）。
/// 与前端启动台 Codex 下拉的列表一致。
pub fn codex_models(b: &Bootstrap) -> Vec<String> {
    let ids: Vec<&str> = if b.model_catalog.is_empty() {
        b.models.iter().map(String::as_str).collect()
    } else {
        b.model_catalog.iter().map(|m| m.id.as_str()).collect()
    };
    let of = |kind: &str| ids.iter().filter(|id| model_kind(b, id) == kind).map(|s| s.to_string()).collect::<Vec<_>>();
    [of("openai"), of("anthropic")].concat()
}

/// Codex 子供应商的模型目录（`modelCatalog.models`）。上游据此写 `model_catalog_json`：Codex 命令行
/// 的 /model 与 Codex 桌面版的模型选择器都列出公司模型，桌面版不登录 ChatGPT 也能切换。
fn codex_model_catalog(b: &Bootstrap) -> Option<Value> {
    let models: Vec<Value> = codex_models(b)
        .into_iter()
        .map(|id| {
            let mut row = json!({ "model": id, "displayName": id });
            if model_kind(b, &id) == "anthropic" {
                row["contextWindow"] = json!(200_000);
            }
            row
        })
        .collect();
    (!models.is_empty()).then(|| json!({ "models": models }))
}

/// 把模型目录写进 Codex 子供应商。在 `sync_universal_to_apps` 之前调用：同步时子供应商的已有配置
/// 与统一供应商合并（统一供应商不管 `modelCatalog`，保留这里写的），随后投影到 config.toml。
fn seed_codex_catalog(state: &AppState, provider: &UniversalProvider, b: &Bootstrap) -> Result<(), AppError> {
    let (Some(child), Some(catalog)) = (provider.to_codex_provider(), codex_model_catalog(b)) else {
        return Ok(());
    };
    let mut row = state.db.get_provider_by_id(&child.id, AppType::Codex.as_str())?.unwrap_or(child);
    if row.settings_config.get("modelCatalog") == Some(&catalog) {
        return Ok(());
    }
    if let Some(obj) = row.settings_config.as_object_mut() {
        obj.insert("modelCatalog".to_string(), catalog);
    }
    state.db.save_provider(AppType::Codex.as_str(), &row)
}

pub fn pick_models(b: &Bootstrap) -> ModelPick {
    let ids: Vec<&str> = if b.model_catalog.is_empty() {
        b.models.iter().map(String::as_str).collect()
    } else {
        b.model_catalog.iter().map(|m| m.id.as_str()).collect()
    };
    let kind_of = |id: &str| model_kind(b, id);
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
    ProviderService::upsert_universal(state, provider.clone())?;
    seed_codex_catalog(state, &provider, b)?;
    ProviderService::sync_universal_to_apps(state, PROVIDER_ID)?;
    save_members(state, b)?;
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
        "[models]\ndefault = \"loov\"\n\n[model.loov]\nname = {model}\nmodel = {model}\nbase_url = {base}\napi_key = {key}\napi_backend = \"responses\"\ncontext_window = 200000\n",
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

/// Grok Build 额外模型表的表名前缀（`[model.loov-<模型>]`）。主表 `[model.loov]` 由上游投影维护。
const GROK_EXTRA_PREFIX: &str = "loov-";

fn grok_table_key(model: &str) -> String {
    let slug: String = model.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    format!("{GROK_EXTRA_PREFIX}{slug}")
}

/// Grok Build 额外模型表的内容：(表名, 显示名, 发出去的模型 id, 上下文窗口)。
/// 上游投影一个供应商只写一张表，Grok 的 `/model` 只能列出员工在 Loov 页选的那个；这里给 Loov 的
/// 其余模型各写一张，都发往本地路由的 Codex 入口（`/v1/responses`，同样经 new-api 计费）：
/// Claude、GPT 由 Loov 的 Codex 子供应商原样发往 new-api；Gemini 用 Codex 聚合里 Gemini 成员发布的
/// id（`ccs-<key>/<模型>`），由成员转成 Chat Completions。
pub fn grok_extra_models(b: &Bootstrap, gemini_ids: &[String], skip: Option<&str>) -> Vec<(String, String, String, Option<u64>)> {
    let mut out = Vec::new();
    for kind in ["anthropic", "openai"] {
        for id in models_of_kind(b, kind) {
            let window = (kind == "anthropic").then_some(200_000);
            out.push((grok_table_key(&id), id.clone(), id, window));
        }
    }
    for published in gemini_ids {
        let model = published.rsplit('/').next().unwrap_or(published).to_string();
        out.push((grok_table_key(&model), model, published.clone(), None));
    }
    out.retain(|(_, name, _, _)| Some(name.as_str()) != skip);
    out
}

/// 现在 ~/.grok/config.toml 里由 Loov 写的额外模型表名。
fn grok_extra_tables_on_disk() -> Vec<String> {
    let text = std::fs::read_to_string(crate::grok_config::get_grok_config_path()).unwrap_or_default();
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else { return Vec::new() };
    doc.get("model")
        .and_then(|m| m.as_table_like())
        .map(|t| t.iter().map(|(k, _)| k.to_string()).filter(|k| k.starts_with(GROK_EXTRA_PREFIX)).collect())
        .unwrap_or_default()
}

/// 把 Loov 的全部模型写进 Grok Build 的模型选择器（每次从 Loov 页启动 Grok 时同步：模型目录、本地
/// 令牌都可能变了）。Codex 不在代理模式时 Codex 入口不可用，只清掉旧表。
pub fn sync_grok_models(state: &AppState) -> Result<(), AppError> {
    let mut set = Vec::new();
    if crate::mode::current::is_proxy(&AppType::Codex) {
        if let (Some(b), Some(token)) = (super::credentials().and_then(|c| c.bootstrap), Some(super::local_auth::live_token()).filter(|t| t.starts_with("lat_"))) {
            let gemini_ids: Vec<String> = crate::mode::controller::stack_views(state, &AppType::Codex)
                .map(|v| v.members.into_iter().filter(|m| m.provider_id == "loov-codex-gemini").flat_map(|m| m.model_ids).collect())
                .unwrap_or_default();
            let (host, port) = state.db.get_proxy_client_endpoint_sync();
            let base = format!("{}/v1", crate::services::proxy::proxy_origin(&host, port));
            for (key, name, model, window) in grok_extra_models(&b, &gemini_ids, grok_model(state).as_deref()) {
                let mut t = toml_edit::Table::new();
                t["name"] = toml_edit::value(name);
                t["model"] = toml_edit::value(model);
                t["base_url"] = toml_edit::value(base.clone());
                t["api_key"] = toml_edit::value(token.clone());
                t["api_backend"] = toml_edit::value("responses");
                // 经 Codex 入口转发：声明来源，计费与用量记在 Grok 名下（loov::attribution）
                let mut headers = toml_edit::InlineTable::new();
                headers.insert(super::attribution::APP_HEADER, "grok".into());
                t["extra_headers"] = toml_edit::Item::Value(toml_edit::Value::InlineTable(headers));
                if let Some(w) = window {
                    t["context_window"] = toml_edit::value(w as i64);
                }
                set.push((KeyPath::new(&["model", &key]), toml_edit::Item::Table(t)));
            }
        }
    }
    // 权限档位写成 Grok 的 `[ui] permission_mode`（员工自己改过的不动）
    let grok_applied = {
        use super::safety::*;
        let prefs = super::netmode::load_prefs();
        let want = grok_mode(prefs.safety);
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(crate::grok_config::get_grok_config_path())
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or_default();
        let cur = toml_str(&doc, &["ui", "permission_mode"]);
        should_write(cur.as_deref(), prefs.safety_applied.get(GROK_KEY).map(String::as_str), want).then(|| {
            set.push((KeyPath::new(&["ui", "permission_mode"]), toml_edit::value(want)));
            want
        })
    };
    write_grok_extra(&state.db, set, Vec::new())?;
    if let Some(v) = grok_applied {
        record_safety(super::safety::GROK_KEY, v);
    }
    Ok(())
}

/// 清掉 Loov 写的额外模型表与权限档位（恢复原配置时；权限档位只在还是 Loov 写的值时删）。
pub fn remove_grok_models(db: &Database) -> Result<(), AppError> {
    let mut extra = Vec::new();
    if let Some(applied) = super::netmode::load_prefs().safety_applied.get(super::safety::GROK_KEY) {
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(crate::grok_config::get_grok_config_path())
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or_default();
        if super::safety::toml_str(&doc, &["ui", "permission_mode"]).as_deref() == Some(applied.as_str()) {
            extra.push(KeyPath::new(&["ui", "permission_mode"]));
        }
    }
    write_grok_extra(db, Vec::new(), extra)
}

fn write_grok_extra(db: &Database, set: Vec<(KeyPath, toml_edit::Item)>, extra_remove: Vec<KeyPath>) -> Result<(), AppError> {
    let keep: Vec<String> = set.iter().filter(|(k, _)| k.0.first().map(String::as_str) == Some("model")).filter_map(|(k, _)| k.0.last().cloned()).collect();
    let remove: Vec<KeyPath> = grok_extra_tables_on_disk()
        .into_iter()
        .filter(|k| !keep.contains(k))
        .map(|k| KeyPath::new(&["model", &k]))
        .chain(extra_remove)
        .collect();
    if set.is_empty() && remove.is_empty() {
        return Ok(());
    }
    let path = crate::grok_config::get_grok_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let patch = TomlPatch { remove, set, ..Default::default() };
    let write = AppWrite::begin(db, AppType::GrokBuild.as_str())?;
    let changes = [FileChange { file: LiveFile::private(path), patch: &patch }];
    write.run(OP_GROK_MODELS, &changes, PendingTarget::default())?;
    Ok(())
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
    enter_stack(state, &AppType::ClaudeDesktop, DESKTOP_PROVIDER_ID).await
}

/// 让 Claude Code 与 Codex 进入聚合模式：默认路由到 Loov 的子供应商，其余几家模型经聚合成员（[`MEMBERS`]）。
pub async fn attach(state: &AppState) -> Result<(), String> {
    enter_stack(state, &AppType::Claude, &claude_child_id()).await?;
    let has_codex = state
        .db
        .get_provider_by_id(&codex_child_id(), "codex")
        .map(|p| p.is_some())
        .unwrap_or(false);
    if has_codex {
        enter_stack(state, &AppType::Codex, &codex_child_id()).await?;
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
    let mut patch = claude_harden_patch(proxy_url);
    let applied = claude_safety(&mut patch);
    claude_direct::run(db, OP_HARDEN, Some(&patch), PendingTarget::default())?;
    if let Some(v) = applied {
        record_safety(super::safety::CLAUDE_KEY, v);
    }
    Ok(())
}

/// 把权限档位写成 Claude Code 的 `permissions.defaultMode`（员工自己改过的不动）。返回写入的值。
fn claude_safety(patch: &mut JsonPatch) -> Option<&'static str> {
    use super::safety::*;
    let prefs = super::netmode::load_prefs();
    let want = claude_mode(prefs.safety);
    let current: Value = std::fs::read_to_string(crate::config::get_claude_settings_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let cur = json_str(&current, &["permissions", "defaultMode"]);
    if !should_write(cur, prefs.safety_applied.get(CLAUDE_KEY).map(String::as_str), want) {
        return None;
    }
    patch.set.push((KeyPath::new(&["permissions", "defaultMode"]), json!(want)));
    Some(want)
}

/// 记下 Loov 写进某个配置的权限值（下次判断员工有没有改过、恢复时清除）。
fn record_safety(key: &str, value: &str) {
    let mut prefs = super::netmode::load_prefs();
    if prefs.safety_applied.get(key).map(String::as_str) != Some(value) {
        prefs.safety_applied.insert(key.to_string(), value.to_string());
        let _ = super::netmode::save_prefs(&prefs);
    }
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
/// Codex 加固键。`web_search = "disabled"`：Codex 自带的网页搜索是 OpenAI 托管工具，经公司网关不可用
/// （new-api 日志：GPT 直接 400「Hosted tools are not supported on Google Cloud」，转给 Claude 时被丢弃），
/// 关掉后模型改用 Loov 下发的云端搜索 MCP（loov-search），同样经公司计费。
pub fn codex_harden_patch() -> TomlPatch {
    TomlPatch {
        set: vec![
            (KeyPath::new(&["features", "plugins"]), toml_edit::value(false)),
            (KeyPath::new(&["analytics", "enabled"]), toml_edit::value(false)),
            (KeyPath::new(&["web_search"]), toml_edit::value("disabled")),
        ],
        ..Default::default()
    }
}

pub fn harden_codex(db: &Database) -> Result<(), AppError> {
    let mut patch = codex_harden_patch();
    let path = crate::codex_config::get_codex_config_path();
    let applied = codex_safety(&mut patch, &path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let write = AppWrite::begin(db, AppType::Codex.as_str())?;
    let changes = [FileChange { file: LiveFile::private(path), patch: &patch }];
    write.run(OP_HARDEN, &changes, PendingTarget::default())?;
    for (key, v) in applied {
        record_safety(key, v);
    }
    Ok(())
}

/// 把权限档位写成 Codex 的 `approval_policy` / `sandbox_mode`（顶层键；员工自己改过的不动）。
fn codex_safety(patch: &mut TomlPatch, path: &std::path::Path) -> Vec<(&'static str, &'static str)> {
    use super::safety::*;
    let prefs = super::netmode::load_prefs();
    let (approval, sandbox) = codex_policy(prefs.safety);
    let doc: toml_edit::DocumentMut = std::fs::read_to_string(path).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    let mut applied = Vec::new();
    for (key, toml_key, want) in [(CODEX_APPROVAL_KEY, "approval_policy", approval), (CODEX_SANDBOX_KEY, "sandbox_mode", sandbox)] {
        let cur = toml_str(&doc, &[toml_key]);
        if should_write(cur.as_deref(), prefs.safety_applied.get(key).map(String::as_str), want) {
            patch.set.push((KeyPath::new(&[toml_key]), toml_edit::value(want)));
            applied.push((key, want));
        }
    }
    applied
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
            api: ApiInfo { base_url: "https://me.loov.cc/".into(), key: "sk-x".into(), keys: Default::default() },
            model_fallbacks: Default::default(),
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
                ModelEntry { id: "claude-sonnet-5".into(), kind: "anthropic".into(), recommended: false, health: None },
                ModelEntry { id: "claude-opus-5-5".into(), kind: "anthropic".into(), recommended: true, health: None },
                ModelEntry { id: "gpt-6-sol".into(), kind: "openai".into(), recommended: true, health: None },
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
                ModelEntry { id: "claude-sonnet-5".into(), kind: "anthropic".into(), recommended: true, health: None },
                ModelEntry { id: "gpt-6-sol".into(), kind: "openai".into(), recommended: false, health: None },
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
        // 顶层键写在第一张表之前，Codex 才会当作顶层设置读
        let ws = s.find("web_search = \"disabled\"").expect("web_search");
        assert!(ws < s.find("[features]").unwrap(), "{s}");
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
                ModelEntry { id: "gemini-3.8-flash".into(), kind: "gemini".into(), recommended: false, health: None },
                ModelEntry { id: "gemini-4-argon".into(), kind: "gemini".into(), recommended: true, health: None },
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
    fn stack_members_cover_the_other_model_families() {
        let b = bootstrap(&["claude-sonnet-5", "gpt-6-sol", "gemini-3.8-flash", "gemini-embedding-001", "gemini-4-argon"], vec![]);
        let by_id = |id: &str| MEMBERS.iter().find(|m| m.id == id).unwrap();
        // Claude Code 的 GPT 成员：Responses 协议，发布 GPT 模型
        let p = member_provider(&b, by_id("loov-claude-gpt")).unwrap();
        assert!(is_loov_provider(&p.id));
        let meta = p.meta.as_ref().unwrap();
        assert_eq!(meta.api_format.as_deref(), Some("openai_responses"));
        let models: Vec<&str> = meta.stack_models.as_ref().unwrap().iter().map(|m| m.model.as_str()).collect();
        assert_eq!(models, ["gpt-6-sol"]);
        assert_eq!(p.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"], KEY_PLACEHOLDER);
        // Gemini 成员去掉向量模型，走 Chat Completions
        let p = member_provider(&b, by_id("loov-claude-gemini")).unwrap();
        let meta = p.meta.as_ref().unwrap();
        assert_eq!(meta.api_format.as_deref(), Some("openai_chat"));
        assert_eq!(meta.stack_models.as_ref().unwrap().len(), 2);
        // Codex 的 Gemini 成员：模型目录 + chat
        let p = member_provider(&b, by_id("loov-codex-gemini")).unwrap();
        assert_eq!(p.meta.as_ref().unwrap().api_format.as_deref(), Some("openai_chat"));
        assert_eq!(p.settings_config["modelCatalog"]["models"][1]["model"], "gemini-4-argon");
        assert!(p.settings_config["config"].as_str().unwrap().contains("https://me.loov.cc/v1"));
        // Desktop 成员：路由表
        let p = member_provider(&b, by_id("loov-desktop-gpt")).unwrap();
        assert!(p.meta.as_ref().unwrap().claude_desktop_model_routes.contains_key("gpt-6-sol"));
        // 没有这家模型就不建成员
        assert!(member_provider(&bootstrap(&["claude-sonnet-5"], vec![]), by_id("loov-claude-gpt")).is_none());
    }

    #[test]
    fn grok_extra_models_list_every_family_and_skip_the_main_one() {
        let b = bootstrap(&["claude-sonnet-5", "claude-opus-5-5", "gpt-6-sol", "gemini-3.8-flash"], vec![]);
        let extra = grok_extra_models(&b, &["ccs-gemini/gemini-3.8-flash".to_string()], Some("claude-sonnet-5"));
        let names: Vec<&str> = extra.iter().map(|e| e.1.as_str()).collect();
        assert_eq!(names, ["claude-opus-5-5", "gpt-6-sol", "gemini-3.8-flash"]);
        assert_eq!(extra[0].0, "loov-claude-opus-5-5");
        assert_eq!(extra[0].3, Some(200_000));
        // Gemini 发 Codex 聚合的 id，由 Gemini 成员转协议
        assert_eq!(extra[2].2, "ccs-gemini/gemini-3.8-flash");
    }

    #[test]
    fn codex_catalog_lists_gpt_then_claude_without_gemini() {
        let b = bootstrap(&["claude-sonnet-5", "gpt-6-sol", "gemini-3.8-flash", "claude-opus-5-5"], vec![]);
        assert_eq!(codex_models(&b), ["gpt-6-sol", "claude-sonnet-5", "claude-opus-5-5"]);
        let cat = codex_model_catalog(&b).unwrap();
        assert_eq!(cat["models"][0]["model"], "gpt-6-sol");
        assert!(cat["models"][0].get("contextWindow").is_none());
        assert_eq!(cat["models"][1]["contextWindow"], 200_000);
        assert!(codex_model_catalog(&bootstrap(&["gemini-3.8-flash"], vec![])).is_none());
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
