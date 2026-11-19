use rx4::{ModelInfo, ModelRegistry};

use std::collections::HashMap;

use crate::app::ConfiguredProvider;
use crate::opencode_go;
use crate::provider_catalog;
use crate::providers::{ZAI_DEFAULT_MODEL, ZAI_MODELS};

pub(crate) const GPT_5_CONTEXT_WINDOW: usize = 1_050_000;

/// pi 0.83.0 `openai-codex.json` — models beyond rs_ai_oauth's
/// `CHATGPT_CODEX_MODELS`, completing the exact ChatGPT Codex catalog:
/// gpt-5.3-codex-spark, gpt-5.4, gpt-5.4-mini, gpt-5.5, gpt-5.6-luna,
/// gpt-5.6-sol, gpt-5.6-terra.
pub(crate) const PI_CODEX_GPT56: [&str; 3] = ["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra"];

/// ChatGPT Codex models the offline fallback must not offer.
///
/// A ChatGPT account cannot call these — the Codex API answers
/// `400 {"detail":"The 'gpt-5.3-codex-spark' model is not supported when using
/// Codex with a ChatGPT account."}` and `GET /codex/models` omits them. The
/// account listing is authoritative; this only keeps the fallback honest.
pub(crate) const CODEX_UNSUPPORTED_ON_CHATGPT: [&str; 1] = ["gpt-5.3-codex-spark"];

/// `GET {base}/codex/models`, which reports what the signed-in plan can use.
const CODEX_MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";

/// The backend gates that listing on a client version and returns an empty
/// list below the newest model's `minimal_client_version` (0.155.0 today).
/// This is the Codex protocol level tk implements, not tk's own version.
pub(crate) const CODEX_CLIENT_VERSION: &str = "1.0.0";

#[derive(serde::Deserialize)]
struct CodexModelEntry {
    slug: String,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    input_modalities: Vec<String>,
    /// Level objects (`{effort, description}`); presence is what matters.
    #[serde(default)]
    supported_reasoning_levels: Vec<serde_json::Value>,
    #[serde(default)]
    visibility: Option<String>,
}

/// Models the signed-in ChatGPT plan can actually call, with the windows and
/// reasoning levels the backend reports. Internal entries (`visibility:
/// "hide"`, e.g. the reserve and auto-review models) stay out of the picker.
pub(crate) async fn fetch_codex_models(
    token: String,
    account_id: Option<String>,
) -> Vec<ModelInfo> {
    let Some(client) = models_client() else {
        return Vec::new();
    };
    let mut request = client
        .get(CODEX_MODELS_URL)
        .query(&[("client_version", CODEX_CLIENT_VERSION)])
        .header("originator", "telekinesis")
        .header("accept", "application/json")
        .bearer_auth(&token);
    if let Some(account_id) = account_id.filter(|id| !id.trim().is_empty()) {
        request = request.header("chatgpt-account-id", account_id);
    }
    let Ok(response) = request.send().await else {
        return Vec::new();
    };
    let Ok(response) = response.error_for_status() else {
        return Vec::new();
    };
    let Ok(value) = response.json::<serde_json::Value>().await else {
        return Vec::new();
    };
    codex_model_infos(&value)
}

/// Offline ChatGPT Codex catalog: the pi list minus models a ChatGPT account
/// cannot call. `fetch_codex_models` replaces it once the account answers.
pub(crate) fn offline_codex_models() -> impl Iterator<Item = &'static str> {
    rs_ai_oauth::codex::CHATGPT_CODEX_MODELS
        .iter()
        .chain(PI_CODEX_GPT56.iter())
        .copied()
        .filter(|id| !CODEX_UNSUPPORTED_ON_CHATGPT.contains(id))
}

/// Parse a `/codex/models` payload into host metadata.
pub(crate) fn codex_model_infos(value: &serde_json::Value) -> Vec<ModelInfo> {
    let Some(entries) = value.get("models").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| serde_json::from_value::<CodexModelEntry>(entry.clone()).ok())
        .filter(|entry| entry.visibility.as_deref() != Some("hide"))
        .map(|entry| {
            let mut info = host_model_info("openai-codex", &entry.slug);
            if let Some(context_window) = entry
                .context_window
                .and_then(|window| usize::try_from(window).ok())
                .filter(|window| *window > 0)
            {
                info.context_window = context_window;
            }
            info.supports_vision = entry
                .input_modalities
                .iter()
                .any(|modality| modality == "image");
            info.supports_reasoning = !entry.supported_reasoning_levels.is_empty();
            info.supports_reasoning_effort = info.supports_reasoning;
            info
        })
        .collect()
}

/// pi 0.83.0 `openai.json` GPT-5.x family, injected for the API-key provider
/// and deduped against rx4's registry.
pub(crate) const PI_OPENAI_GPT5: [&str; 21] = [
    "gpt-5",
    "gpt-5-chat-latest",
    "gpt-5-mini",
    "gpt-5-nano",
    "gpt-5-pro",
    "gpt-5.1",
    "gpt-5.2",
    "gpt-5.2-chat-latest",
    "gpt-5.2-pro",
    "gpt-5.3-chat-latest",
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gpt-5.4-pro",
    "gpt-5.5",
    "gpt-5.5-pro",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
];

pub(crate) fn context_window_for_model(model: &str) -> usize {
    // pi 0.83.0 context windows for models outside rx4's registry; newer
    // models synced from the models.dev snapshot (2026-08).
    if model.starts_with("gpt-5.5") || model.starts_with("gpt-5.6") {
        GPT_5_CONTEXT_WINDOW
    } else {
        let lower = model.to_ascii_lowercase();
        match model {
            "gpt-5.4-pro" => GPT_5_CONTEXT_WINDOW,
            "gpt-5.4-nano" | "gpt-5-mini" | "gpt-5-nano" | "gpt-5-pro" | "gpt-5.1" | "gpt-5.2"
            | "gpt-5.2-pro" | "gpt-5.3-codex" => 400_000,
            "gpt-5-chat-latest"
            | "gpt-5.2-chat-latest"
            | "gpt-5.3-chat-latest"
            | "gpt-5.3-codex-spark" => 128_000,
            _ => context_from_family(&lower),
        }
    }
}

/// Prefix-based windows for the models.dev catalog generation. Matched on
/// lowercased ids so provider prefixes (`cline-pass/…`) don't hide the family.
fn context_from_family(lower: &str) -> usize {
    const M1M: usize = 1_000_000;
    const M2M: usize = 2_000_000;
    if lower.starts_with("deepseek-v4") {
        return M1M;
    }
    if lower.starts_with("deepseek") {
        return M1M; // deepseek-chat/reasoner also 1M in models.dev
    }
    if lower.starts_with("grok-4.20") || lower.contains("grok-4-1-fast") {
        return M2M;
    }
    if lower.starts_with("grok-4") || lower.starts_with("grok-3") {
        return M1M;
    }
    if lower.starts_with("claude-opus-4") || lower.starts_with("claude-sonnet-4") {
        return M1M;
    }
    if lower.starts_with("claude-haiku") {
        return 200_000;
    }
    if lower.starts_with("gemini-3") || lower.starts_with("gemini-2.5") {
        return 1_048_576;
    }
    if lower.starts_with("kimi-k2") {
        return 262_144;
    }
    if lower.starts_with("minimax-m2") || lower.starts_with("minimax-m") {
        return 204_800;
    }
    if lower.starts_with("qwen3.6-max") || lower.starts_with("qwen3.6-plus") {
        return 262_144;
    }
    if lower.starts_with("qwen3.6") || lower.starts_with("qwen3.5") {
        return M1M;
    }
    if lower.starts_with("glm-5") || lower.starts_with("glm-4") {
        return 200_000;
    }
    if lower.starts_with("mimo-v2") {
        return 256_000;
    }
    128_000
}

pub(crate) fn host_model_info(provider: &str, id: &str) -> ModelInfo {
    let mut info = ModelInfo::new(provider, id, context_window_for_model(id), 8_192);
    info.supports_tools = true;
    info.supports_reasoning = id.contains("reason")
        || id.starts_with("o1")
        || id.starts_with("o3")
        || id.starts_with("gpt-5");
    info.supports_reasoning_effort = info.supports_reasoning;
    info
}

/// A configured provider that publishes a live `/models` listing.
pub(crate) struct LiveModelsTarget {
    pub(crate) provider: String,
    pub(crate) url: String,
    pub(crate) key: String,
    pub(crate) api: provider_catalog::ProviderApi,
}

/// Live `/models` targets for the configured providers.
///
/// The offline catalog only moves when a new `rs_ai_providers` release ships,
/// so a provider that adds a model today should appear in the picker today.
/// OAuth plans come from `rs_ai_oauth` discovery and OpenRouter keeps its
/// richer capability parsing, so both stay on their dedicated paths.
pub(crate) fn live_models_targets(provider_ids: &[String]) -> Vec<LiveModelsTarget> {
    live_models_targets_with(provider_ids, |spec| match spec.id {
        "zai" => crate::providers::api_key(&["ZAI_API_KEY"], spec.id),
        "opencode-go" => {
            crate::providers::api_key(&["OPENCODE_GO_API_KEY", "OPENCODE_API_KEY"], spec.id)
        }
        "google" => crate::providers::api_key(&crate::providers::GOOGLE_KEY_ENV, spec.id),
        _ => provider_catalog::env_key(spec),
    })
}

fn live_models_targets_with(
    provider_ids: &[String],
    key_of: impl Fn(&provider_catalog::ProviderSpec) -> Option<String>,
) -> Vec<LiveModelsTarget> {
    provider_ids
        .iter()
        .filter(|id| id.as_str() != "openrouter")
        .filter_map(|id| {
            let spec = provider_catalog::by_id(id)?;
            if spec.base_url.is_empty() || matches!(spec.api, provider_catalog::ProviderApi::Custom)
            {
                return None;
            }
            Some(LiveModelsTarget {
                provider: id.clone(),
                url: live_models_url(spec, &crate::providers::openai_base_url()),
                key: key_of(spec)?,
                api: spec.api,
            })
        })
        .collect()
}

fn live_models_url(spec: &provider_catalog::ProviderSpec, openai_base: &str) -> String {
    let base = match spec.id {
        "openai" => openai_base,
        "zai" => crate::providers::ZAI_CHAT_URL.trim_end_matches("/chat/completions"),
        // The host's Gemini chat adapter uses OpenAI compatibility, but the
        // native listing has token limits and page tokens the adapter lacks.
        "google" => "https://generativelanguage.googleapis.com/v1beta",
        _ => spec.base_url,
    };
    format!("{}/models", base.trim_end_matches('/'))
}

fn host_supports_model(provider: &str, id: &str) -> bool {
    provider != opencode_go::OPENCODE_GO_ID || opencode_go::OPENCODE_GO_MODELS.contains(&id)
}

/// Shared client for the model-listing endpoints: bounded, so one slow
/// provider cannot hold a refresh open.
fn models_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()
}

/// Fetch one provider's live `/models` listing. A provider that fails, times
/// out, or answers in an unexpected shape contributes nothing.
pub(crate) async fn fetch_live_models(target: LiveModelsTarget) -> (String, Vec<ModelInfo>) {
    let LiveModelsTarget {
        provider,
        url,
        key,
        api,
    } = target;
    let Some(client) = models_client() else {
        return (provider, Vec::new());
    };
    let entries = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        fetch_live_pages(&provider, &url, |page_url| {
            let request = match api {
                provider_catalog::ProviderApi::Anthropic => client
                    .get(page_url)
                    .header("x-api-key", &key)
                    .header("anthropic-version", "2023-06-01"),
                _ if provider == "google" => client.get(page_url).header("x-goog-api-key", &key),
                _ => client.get(page_url).bearer_auth(&key),
            };
            async move {
                request
                    .send()
                    .await
                    .ok()?
                    .error_for_status()
                    .ok()?
                    .json()
                    .await
                    .ok()
            }
        }),
    )
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    let known: Vec<_> = entries
        .iter()
        .filter_map(|entry| Some((provider.clone(), entry.get("id")?.as_str()?.to_string())))
        .collect();
    let metadata = fetch_models_dev_models(&known).await;
    let metadata = metadata
        .into_iter()
        .flat_map(|(_, infos)| infos)
        .map(|info| (info.id.clone(), info))
        .collect::<HashMap<_, _>>();
    let infos = live_infos_with_metadata(&provider, entries, &metadata);
    (provider, infos)
}

fn live_infos_with_metadata(
    provider: &str,
    entries: Vec<serde_json::Value>,
    metadata: &HashMap<String, ModelInfo>,
) -> Vec<ModelInfo> {
    entries
        .into_iter()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.trim();
            if id.is_empty() || !host_supports_model(provider, id) {
                return None;
            }
            let mut info = metadata
                .get(id)
                .cloned()
                .unwrap_or_else(|| host_model_info(provider, id));
            apply_live_metadata(&mut info, &entry);
            Some(info)
        })
        .collect()
}

fn apply_live_metadata(info: &mut ModelInfo, entry: &serde_json::Value) {
    let limit = |fields: &[&str]| {
        fields
            .iter()
            .find_map(|key| entry.get(*key))
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .filter(|v| *v > 0)
    };
    if let Some(context) = limit(&[
        "context_length",
        "context_window",
        "max_context_length",
        "max_input_tokens",
        "inputTokenLimit",
    ])
    .or_else(|| {
        entry
            .get("top_provider")?
            .get("context_length")?
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .filter(|v| *v > 0)
    }) {
        info.context_window = context;
    }
    if let Some(output) = limit(&[
        "max_output_tokens",
        "max_completion_tokens",
        "max_tokens",
        "outputTokenLimit",
    ]) {
        info.max_output_tokens = output;
    }
    for (key, target) in [
        ("tool_call", &mut info.supports_tools),
        ("reasoning", &mut info.supports_reasoning),
        ("vision", &mut info.supports_vision),
    ] {
        if let Some(value) = entry.get(key).and_then(serde_json::Value::as_bool) {
            *target = value;
        }
    }
    if let Some(thinking) = entry.get("thinking").and_then(serde_json::Value::as_bool) {
        info.supports_reasoning = thinking;
    }
    if let Some(capabilities) = entry.get("capabilities") {
        for (key, target) in [
            ("function_calling", &mut info.supports_tools),
            ("reasoning", &mut info.supports_reasoning),
            ("vision", &mut info.supports_vision),
        ] {
            if let Some(value) = capabilities.get(key).and_then(serde_json::Value::as_bool) {
                *target = value;
            }
        }
        for (key, target) in [
            ("thinking", &mut info.supports_reasoning),
            ("image_input", &mut info.supports_vision),
            ("effort", &mut info.supports_reasoning_effort),
        ] {
            if let Some(value) = capabilities
                .get(key)
                .and_then(|v| v.get("supported"))
                .and_then(serde_json::Value::as_bool)
            {
                *target = value;
            }
        }
    }
    if !info.supports_reasoning {
        info.supports_reasoning_effort = false;
    }
}

async fn fetch_live_pages<F, Fut>(
    provider: &str,
    url: &str,
    fetch: F,
) -> Option<Vec<serde_json::Value>>
where
    F: Fn(reqwest::Url) -> Fut,
    Fut: std::future::Future<Output = Option<serde_json::Value>>,
{
    let base = reqwest::Url::parse(url).ok()?;
    let mut next = base.clone();
    let mut visited = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    let mut infos = Vec::new();
    for _ in 0..20 {
        if !visited.insert(next.to_string()) {
            return None;
        }
        let value = fetch(next.clone()).await?;
        let entries = value
            .get("data")
            .or_else(|| value.get("models"))
            .and_then(serde_json::Value::as_array)
            .or_else(|| value.as_array())?;
        for entry in entries {
            let mut entry = entry.clone();
            if provider == "google" {
                let Some(name) = entry.get("name").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let id = name.strip_prefix("models/").unwrap_or(name).to_string();
                entry["id"] = serde_json::Value::String(id);
            }
            let Some(id) = entry
                .get("id")
                .or_else(|| entry.get("name"))
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let id = id.to_string();
            entry["id"] = serde_json::Value::String(id.clone());
            if !id.trim().is_empty() && ids.insert(id) {
                infos.push(entry);
            }
        }
        let link = value
            .get("next")
            .or_else(|| value.get("next_page"))
            .or_else(|| value.get("links").and_then(|links| links.get("next")))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty());
        if let Some(token) = value
            .get("nextPageToken")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            next = base.clone();
            next.query_pairs_mut().append_pair("pageToken", token);
        } else if let Some(link) = link {
            next = base.join(link).ok()?;
            // Never send this provider's credential to an arbitrary next URL.
            if next.origin() != base.origin()
                || next.path() != base.path()
                || !next.username().is_empty()
                || next.password().is_some()
            {
                return None;
            }
        } else if value.get("has_more").and_then(serde_json::Value::as_bool) == Some(true) {
            let last = value
                .get("last_id")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.get("data")?.as_array()?.last()?.get("id")?.as_str())
                .filter(|id| !id.is_empty())?;
            next = base.clone();
            next.query_pairs_mut().append_pair("after_id", last);
            if !matches!(
                provider_catalog::find(provider).map(|spec| spec.api),
                Some(provider_catalog::ProviderApi::Anthropic)
            ) {
                next = base.clone();
                next.query_pairs_mut().append_pair("after", last);
            }
        } else {
            return Some(infos);
        }
    }
    None // Do not report a silently truncated account listing as complete.
}

/// OpenRouter's `/models` listing, which reports capability fields the generic
/// parser ignores. No key means the provider is not configured.
pub(crate) async fn fetch_openrouter_models(key: Option<String>) -> Vec<ModelInfo> {
    let Some(key) = key else {
        return Vec::new();
    };
    let Some(client) = models_client() else {
        return Vec::new();
    };
    let response = match client
        .get("https://openrouter.ai/api/v1/models")
        .bearer_auth(key)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        _ => return Vec::new(),
    };
    let Ok(value) = response.json::<serde_json::Value>().await else {
        return Vec::new();
    };
    value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .map(|entries| entries.iter().filter_map(openrouter_model_info).collect())
        .unwrap_or_default()
}

const MODELS_DEV_API: &str = "https://api.github.com/repos/anomalyco/models.dev";
const MODELS_DEV_RAW: &str = "https://raw.githubusercontent.com/anomalyco/models.dev/dev";
const MODELS_DEV_TTL: u64 = 60 * 60;
// Serve the last complete public snapshot for at most seven days when GitHub
// fails; account membership and credentials are never stored in this cache.
const MODELS_DEV_STALE: u64 = 7 * 24 * MODELS_DEV_TTL;

#[derive(serde::Deserialize, serde::Serialize)]
struct ModelsDevModel {
    #[serde(default = "default_callable_text")]
    callable_text: bool,
    #[serde(default)]
    source_capabilities: bool,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    reasoning_effort: Option<bool>,
    #[serde(default)]
    attachment: bool,
    #[serde(default)]
    modalities: Option<ModelsDevModalities>,
    #[serde(default)]
    limit: Option<ModelsDevLimit>,
}

fn default_callable_text() -> bool {
    true
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ModelsDevModalities {
    #[serde(default)]
    input: Vec<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ModelsDevLimit {
    context: Option<u64>,
    output: Option<u64>,
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
struct ModelsDevProvider {
    #[serde(default)]
    models: HashMap<String, ModelsDevModel>,
}

/// models.dev metadata for the models this host already lists, grouped by
/// provider.
///
/// Membership stays with the provider's own listing; models.dev supplies what
/// that listing leaves out (context/output windows, tool, reasoning, vision).
/// Providers whose ids are vendor-namespaced (`z-ai/glm-5.3-flash` where
/// models.dev lists `cline-pass/glm-5.3-flash`) match on the last segment.
pub(crate) async fn fetch_models_dev_models(
    known: &[(String, String)],
) -> Vec<(String, Vec<ModelInfo>)> {
    if known.is_empty() {
        return Vec::new();
    }
    let mut catalog = HashMap::new();
    let mut attempted = std::collections::HashSet::new();
    for (provider, _) in known {
        if attempted.insert(provider) {
            if let Some(models) = cached_models_dev_provider(provider).await {
                catalog.insert(provider.clone(), models);
            }
        }
    }
    enrich_models_dev_known(known, &catalog)
}

fn enrich_models_dev_known(
    known: &[(String, String)],
    catalog: &HashMap<String, ModelsDevProvider>,
) -> Vec<(String, Vec<ModelInfo>)> {
    let mut by_provider: HashMap<String, Vec<ModelInfo>> = HashMap::new();
    for (provider, id) in known {
        let Some(key) = models_dev_key(catalog, provider) else {
            continue;
        };
        let Some(models) = catalog.get(&key).map(|entry| &entry.models) else {
            continue;
        };
        let Some(model) = models.get(id).or_else(|| {
            let segment = id.rsplit('/').next();
            models
                .iter()
                .find(|(candidate, _)| candidate.rsplit('/').next() == segment)
                .map(|(_, model)| model)
        }) else {
            continue;
        };
        let mut info = host_model_info(provider, id);
        apply_models_dev(&mut info, model);
        by_provider.entry(provider.clone()).or_default().push(info);
    }
    by_provider.into_iter().collect()
}

/// Membership fallback only: callers must not append this to a successful
/// account listing. Enrichment above uses the same provider-only public cache.
pub(crate) async fn fetch_models_dev_fallback(provider: &str) -> Vec<ModelInfo> {
    let Some(catalog) = cached_models_dev_provider(provider).await else {
        return Vec::new();
    };
    models_dev_fallback_infos(provider, catalog)
}

fn models_dev_fallback_infos(provider: &str, catalog: ModelsDevProvider) -> Vec<ModelInfo> {
    let mut infos: Vec<_> = catalog
        .models
        .into_iter()
        .filter(|(id, model)| model.callable_text && host_supports_model(provider, id))
        .map(|(id, model)| {
            let mut info = host_model_info(provider, &id);
            apply_models_dev(&mut info, &model);
            info
        })
        .collect();
    infos.sort_by(|a, b| a.id.cmp(&b.id));
    infos
}

fn source_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.:@+~ ".contains(&c))
        })
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ModelsDevCache {
    fetched_at: u64,
    provider: ModelsDevProvider,
}

async fn cached_models_dev_provider(provider: &str) -> Option<ModelsDevProvider> {
    let client = models_client()?;
    let cache_dir = dirs::cache_dir().map(|dir| dir.join("telekinesis/models-dev-source-v2"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    cached_models_dev_with(provider, cache_dir.as_deref(), now, |url| {
        let client = client.clone();
        async move {
            client
                .get(url)
                .header("User-Agent", "telekinesis-model-discovery")
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?
                .text()
                .await
                .ok()
        }
    })
    .await
}

async fn cached_models_dev_with<F, Fut>(
    provider: &str,
    cache_dir: Option<&std::path::Path>,
    now: u64,
    fetch: F,
) -> Option<ModelsDevProvider>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    if !source_path(provider) || provider.contains('/') {
        return None;
    }
    let path = cache_dir.map(|dir| dir.join(format!("{provider}.json")));
    let cached = path
        .as_ref()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<ModelsDevCache>(&bytes).ok())
        .filter(|cache| cache.fetched_at <= now && now - cache.fetched_at <= MODELS_DEV_STALE);
    if cached
        .as_ref()
        .is_some_and(|cache| now - cache.fetched_at < MODELS_DEV_TTL)
    {
        return cached.map(|cache| cache.provider);
    }
    let mut candidates = HashMap::from([(provider.to_string(), ModelsDevProvider::default())]);
    if let Some(spec) = provider_catalog::find(provider) {
        candidates.insert(spec.id.to_string(), ModelsDevProvider::default());
        for alias in spec.aliases {
            candidates.insert((*alias).to_string(), ModelsDevProvider::default());
        }
    }
    // Use the same alias ordering as enrichment, without requesting a global
    // provider index. Each unsuccessful candidate is removed before retrying.
    while let Some(key) = models_dev_key(&candidates, provider) {
        candidates.remove(&key);
        if let Some(models) = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            fetch_models_dev_source(&key, &fetch),
        )
        .await
        .ok()
        .flatten()
        {
            let cache = ModelsDevCache {
                fetched_at: now,
                provider: models,
            };
            if let Some(path) = &path {
                if let Some(dir) = path.parent() {
                    let _ = (|| -> std::io::Result<()> {
                        std::fs::create_dir_all(dir)?;
                        let mut file = tempfile::NamedTempFile::new_in(dir)?;
                        serde_json::to_writer(file.as_file_mut(), &cache)?;
                        file.persist(path).map_err(|error| error.error)?;
                        Ok(())
                    })();
                }
            }
            return Some(cache.provider);
        }
    }
    cached.map(|cache| cache.provider)
}

async fn fetch_models_dev_source<F, Fut>(provider: &str, fetch: &F) -> Option<ModelsDevProvider>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    use futures::{stream, StreamExt};
    if !source_path(provider) || provider.contains('/') {
        return None;
    }
    let contents: serde_json::Value = serde_json::from_str(
        &fetch(format!(
            "{MODELS_DEV_API}/contents/providers/{provider}?ref=dev"
        ))
        .await?,
    )
    .ok()?;
    let sha = contents
        .as_array()?
        .iter()
        .find(|entry| entry["name"] == "models" && entry["type"] == "dir")?
        .get("sha")?
        .as_str()?;
    if !sha.bytes().all(|c| c.is_ascii_hexdigit()) || sha.is_empty() {
        return None;
    }
    let tree: serde_json::Value = serde_json::from_str(
        &fetch(format!("{MODELS_DEV_API}/git/trees/{sha}?recursive=1")).await?,
    )
    .ok()?;
    if tree.get("truncated").and_then(serde_json::Value::as_bool) != Some(false) {
        return None;
    }
    let paths: Vec<_> = tree
        .get("tree")?
        .as_array()?
        .iter()
        .filter(|entry| entry["type"] == "blob")
        .filter_map(|entry| entry["path"].as_str())
        .filter(|path| path.ends_with(".toml") && source_path(path))
        .collect();
    let models: Vec<_> = stream::iter(paths.into_iter().map(|path| async move {
        let child: toml::Value = toml::from_str(
            &fetch(format!(
                "{MODELS_DEV_RAW}/providers/{provider}/models/{path}"
            ))
            .await?,
        )
        .ok()?;
        let mut resolved = if let Some(base) = child.get("base_model").and_then(toml::Value::as_str)
        {
            if !source_path(base) {
                return None;
            }
            toml::from_str(&fetch(format!("{MODELS_DEV_RAW}/models/{base}.toml")).await?).ok()?
        } else {
            toml::Value::Table(Default::default())
        };
        merge_models_dev(&mut resolved, &child);
        if let Some(omit) = child.get("base_model_omit").and_then(toml::Value::as_array) {
            for key in omit {
                let mut parts = key.as_str()?.split('.').peekable();
                let mut target = &mut resolved;
                while let Some(part) = parts.next() {
                    if parts.peek().is_none() {
                        if let Some(table) = target.as_table_mut() {
                            table.remove(part);
                        }
                        break;
                    }
                    let Some(next) = target.get_mut(part) else {
                        break;
                    };
                    target = next;
                }
            }
        }
        Some((
            path.strip_suffix(".toml")?.to_string(),
            source_model_metadata(&resolved)?,
        ))
    }))
    .buffer_unordered(8)
    .collect()
    .await;
    let models: Option<HashMap<_, _>> = models.into_iter().collect();
    let models = models?;
    if models.is_empty() {
        return None;
    }
    Some(ModelsDevProvider { models })
}

fn merge_models_dev(base: &mut toml::Value, child: &toml::Value) {
    if let (Some(base), Some(child)) = (base.as_table_mut(), child.as_table()) {
        for (key, value) in child {
            if let Some(existing) = base.get_mut(key) {
                merge_models_dev(existing, value);
            } else {
                base.insert(key.clone(), value.clone());
            }
        }
    } else {
        *base = child.clone();
    }
}

fn source_model_metadata(value: &toml::Value) -> Option<ModelsDevModel> {
    let mut model: ModelsDevModel = value.clone().try_into().ok()?;
    model.callable_text = value
        .get("type")
        .and_then(toml::Value::as_str)
        .unwrap_or("chat")
        == "chat"
        && value.get("v2_only").and_then(toml::Value::as_bool) != Some(true)
        && !matches!(
            value.get("status").and_then(toml::Value::as_str),
            Some("alpha" | "deprecated")
        )
        && value
            .get("modalities")
            .and_then(|v| v.get("output"))
            .and_then(toml::Value::as_array)
            .is_none_or(|output| {
                output
                    .iter()
                    .any(|v| matches!(v.as_str(), Some("text" | "text/plain")))
            });
    if let Some(capabilities) = value.get("capabilities") {
        model.source_capabilities = true;
        let supported = |key: &str| {
            capabilities
                .get(key)
                .and_then(|v| v.get("supported"))
                .and_then(toml::Value::as_bool)
                .unwrap_or(false)
        };
        model.tool_call = supported("tools");
        model.reasoning = supported("reasoning");
        model.reasoning_effort = Some(
            capabilities
                .get("reasoning")
                .and_then(|v| v.get("effort"))
                .and_then(toml::Value::as_array)
                .is_some_and(|efforts| !efforts.is_empty()),
        );
    }
    if let Some(modalities) = &mut model.modalities {
        for modality in &mut modalities.input {
            if modality.starts_with("image/") {
                *modality = "image".to_string();
            }
        }
    }
    Some(model)
}

/// models.dev provider key for a host provider id: the id itself, the catalog
/// id it resolves to, or one of that provider's aliases.
fn models_dev_key(
    catalog: &HashMap<String, ModelsDevProvider>,
    provider_id: &str,
) -> Option<String> {
    let spec = provider_catalog::find(provider_id);
    let mut candidates: Vec<&str> = vec![provider_id];
    if let Some(spec) = spec {
        candidates.push(spec.id);
        candidates.extend(spec.aliases.iter().copied());
    }
    candidates
        .into_iter()
        .find(|candidate| catalog.contains_key(*candidate))
        .map(str::to_string)
}

/// Sparse legacy entries only add capabilities. Resolved source capabilities
/// replace host guesses; the caller reapplies any explicit live API metadata.
fn apply_models_dev(info: &mut ModelInfo, model: &ModelsDevModel) {
    if let Some(context) = model
        .limit
        .as_ref()
        .and_then(|limit| limit.context)
        .and_then(|context| usize::try_from(context).ok())
        .filter(|context| *context > 0)
    {
        info.context_window = context;
    }
    if let Some(output) = model
        .limit
        .as_ref()
        .and_then(|limit| limit.output)
        .and_then(|output| usize::try_from(output).ok())
        .filter(|output| *output > 0)
    {
        info.max_output_tokens = output;
    }
    if model.source_capabilities {
        info.supports_tools = model.tool_call;
        info.supports_reasoning = model.reasoning;
        info.supports_reasoning_effort = model.reasoning && model.reasoning_effort.unwrap_or(false);
    } else {
        if model.tool_call {
            info.supports_tools = true;
        }
        if model.reasoning {
            info.supports_reasoning = true;
            if model.reasoning_effort.unwrap_or(true) {
                info.supports_reasoning_effort = true;
            }
        }
    }
    let image_input = model.attachment
        || model
            .modalities
            .as_ref()
            .is_some_and(|modalities| modalities.input.iter().any(|modality| modality == "image"));
    if image_input || model.modalities.is_some() {
        info.supports_vision = image_input;
    }
}

/// Build the metadata Rotary receives from this host's configured providers.
/// Dynamic provider snapshots extend this registry later; these entries only
/// keep the picker and compaction useful before the first network refresh.
pub(crate) fn initial_model_registry(providers: &[(ConfiguredProvider, String)]) -> ModelRegistry {
    let mut registry = ModelRegistry::new();
    for (provider, default_model) in providers {
        registry.register(host_model_info(&provider.id, default_model));
        if provider.id == "openai-codex" {
            for id in offline_codex_models() {
                registry.register(host_model_info(&provider.id, id));
            }
        }
        if let Some(spec) = provider_catalog::by_id(&provider.id) {
            // `zai` and `opencode-go` are wired explicitly with curated model
            // lists; the shared catalog still carries stale entries for them.
            if provider.id != "zai" && provider.id != opencode_go::OPENCODE_GO_ID {
                for id in spec.models {
                    registry.register(host_model_info(&provider.id, id));
                }
            }
        }
        if provider.id == "zai" {
            registry.register(host_model_info(&provider.id, ZAI_DEFAULT_MODEL));
            for id in ZAI_MODELS {
                registry.register(host_model_info(&provider.id, id));
            }
        }
        if provider.id == opencode_go::OPENCODE_GO_ID {
            for id in opencode_go::OPENCODE_GO_MODELS {
                registry.register(host_model_info(&provider.id, id));
            }
        }
        if provider.id == "openai" {
            for id in PI_OPENAI_GPT5 {
                registry.register(host_model_info(&provider.id, id));
            }
        }
    }
    registry
}

pub(crate) fn oauth_model_info(provider: &str, model: rs_ai_oauth::ModelInfo) -> ModelInfo {
    let context_window = model
        .limits
        .context_window
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| context_window_for_model(&model.id));
    let max_output_tokens = model
        .limits
        .max_output_tokens
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(8_192);
    let mut info = ModelInfo::new(provider, model.id, context_window, max_output_tokens);
    info.supports_tools = model.capabilities.contains("tool_calling")
        || model
            .supported_parameters
            .iter()
            .any(|parameter| matches!(parameter.as_str(), "tools" | "tool_choice"));
    info.supports_vision = model.capabilities.contains("image_input")
        || model
            .input_modalities
            .iter()
            .any(|modality| modality == "image");
    info.supports_reasoning = model.capabilities.contains("extended_thinking")
        || model
            .supported_parameters
            .iter()
            .any(|parameter| matches!(parameter.as_str(), "reasoning" | "include_reasoning"));
    info.supports_reasoning_effort = info.supports_reasoning;
    info
}

pub(crate) fn openrouter_model_info(value: &serde_json::Value) -> Option<ModelInfo> {
    let id = value.get("id")?.as_str()?;
    let context_window = value
        .get("top_provider")
        .and_then(|provider| provider.get("context_length"))
        .or_else(|| value.get("context_length"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(128_000);
    let max_output_tokens = value
        .get("top_provider")
        .and_then(|provider| provider.get("max_completion_tokens"))
        .or_else(|| value.get("max_output_tokens"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(8_192);
    let mut info = ModelInfo::new("openrouter", id, context_window, max_output_tokens);
    if let Some(parameters) = value
        .get("supported_parameters")
        .and_then(serde_json::Value::as_array)
    {
        info.supports_tools = parameters
            .iter()
            .any(|parameter| matches!(parameter.as_str(), Some("tools") | Some("tool_choice")));
        info.supports_reasoning = parameters.iter().any(|parameter| {
            matches!(
                parameter.as_str(),
                Some("reasoning") | Some("include_reasoning")
            )
        });
        info.supports_reasoning_effort = info.supports_reasoning;
    }
    info.supports_vision = value
        .get("architecture")
        .and_then(|architecture| architecture.get("input_modalities"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|modalities| {
            modalities
                .iter()
                .any(|modality| modality.as_str() == Some("image"))
        });
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn discovery_endpoints_follow_host_configuration() {
        assert_eq!(
            live_models_url(
                provider_catalog::by_id("openai").unwrap(),
                "https://proxy.invalid/v2/"
            ),
            "https://proxy.invalid/v2/models"
        );
        assert_eq!(
            live_models_url(provider_catalog::by_id("zai").unwrap(), "unused"),
            "https://api.z.ai/api/coding/paas/v4/models"
        );
        assert_eq!(
            live_models_url(provider_catalog::by_id("google").unwrap(), "unused"),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
    }

    #[test]
    fn fallback_publishes_only_callable_text_models_with_metadata() {
        let catalog = || {
            ModelsDevProvider { models: [
            ("deepseek-v4.1-flash", ""),
            ("muse-spark-1.3-contributor", "status = 'beta'"),
            ("new-chat", ""),
            ("embedding", "type = 'embedding'"),
            ("audio", "type = 'audio'"),
            ("alpha", "status = 'alpha'"),
            ("deprecated", "status = 'deprecated'"),
            ("v2-only", "v2_only = true"),
            ("no-text", "[modalities]\noutput = ['audio']"),
        ].into_iter().map(|(id, fields)| {
            let value: toml::Value = toml::from_str(&format!("{fields}\n[limit]\ncontext = 65536\noutput = 4096\n[capabilities]\ntools = {{ supported = true }}")).unwrap();
            (id.to_string(), source_model_metadata(&value).unwrap())
        }).collect() }
        };
        let infos = models_dev_fallback_infos("fixture", catalog());
        assert_eq!(
            infos
                .iter()
                .map(|info| info.id.as_str())
                .collect::<Vec<_>>(),
            [
                "deepseek-v4.1-flash",
                "muse-spark-1.3-contributor",
                "new-chat"
            ]
        );
        assert!(infos.iter().all(|info| info.context_window == 65536
            && info.max_output_tokens == 4096
            && info.supports_tools));
        let go_infos = models_dev_fallback_infos("opencode-go", catalog());
        assert_eq!(go_infos.len(), 2);
        assert!(go_infos
            .iter()
            .all(|info| opencode_go::OPENCODE_GO_MODELS.contains(&info.id.as_str())));
        let live = live_infos_with_metadata(
            "opencode-go",
            vec![
                serde_json::json!({"id":"new-chat"}),
                serde_json::json!({"id":"deepseek-v4.1-flash"}),
            ],
            &HashMap::new(),
        );
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, "deepseek-v4.1-flash");
    }

    #[tokio::test]
    async fn anthropic_pages_follow_last_id_and_keep_all_models() {
        let calls = std::sync::Mutex::new(Vec::new());
        let entries = fetch_live_pages("anthropic", "https://fixture.invalid/v1/models", |url| {
            calls.lock().unwrap().push(url.to_string());
            let page = if url.query().is_none() {
                serde_json::json!({"data":[{"id":"one"}], "has_more":true, "last_id":"one"})
            } else {
                assert_eq!(url.query(), Some("after_id=one"));
                serde_json::json!({"data":[{"id":"two"},{"id":"one"}],"has_more":false})
            };
            std::future::ready(Some(page))
        })
        .await
        .unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|e| e["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn native_anthropic_and_mistral_metadata_override_guesses() {
        let mut info = host_model_info("anthropic", "claude-new");
        apply_live_metadata(
            &mut info,
            &serde_json::json!({
                "max_input_tokens":1000000, "max_tokens":128000,
                "capabilities":{"image_input":{"supported":true},"thinking":{"supported":true},"effort":{"supported":true}}
            }),
        );
        assert_eq!(info.context_window, 1000000);
        assert_eq!(info.max_output_tokens, 128000);
        assert!(info.supports_vision && info.supports_reasoning && info.supports_reasoning_effort);
        apply_live_metadata(
            &mut info,
            &serde_json::json!({
                "max_context_length":32768,
                "capabilities":{"function_calling":false,"reasoning":false,"vision":false}
            }),
        );
        assert_eq!(info.context_window, 32768);
        assert!(!info.supports_tools && !info.supports_reasoning && !info.supports_vision);
    }

    #[test]
    fn source_false_capabilities_and_array_overrides_replace_family_guesses() {
        let mut base: toml::Value = toml::from_str(
            r#"
[modalities]
input = ["text", "image/png"]
[capabilities]
tools = { supported = true }
reasoning = { supported = true, effort = ["low", "high"] }
"#,
        )
        .unwrap();
        let child = toml::from_str(
            r#"
[modalities]
input = ["text"]
[capabilities]
tools = { supported = false }
reasoning = { supported = false, effort = [] }
"#,
        )
        .unwrap();
        merge_models_dev(&mut base, &child);
        let model = source_model_metadata(&base).unwrap();
        let mut info = host_model_info("fixture", "gpt-5");
        info.supports_vision = true;
        apply_models_dev(&mut info, &model);
        assert!(
            !info.supports_tools
                && !info.supports_reasoning
                && !info.supports_reasoning_effort
                && !info.supports_vision
        );
    }

    #[tokio::test]
    async fn openai_pages_handle_cursor_and_next_url_and_reject_partial_failure() {
        let entries = fetch_live_pages("openai", "https://fixture.invalid/v1/models", |url| {
            let page = match url.query() {
                None => serde_json::json!({"data":[{"id":"one"}],"has_more":true,"last_id":"one"}),
                Some("after=one") => serde_json::json!({"data":[{"id":"two"}],"next":"?after=two"}),
                Some("after=two") => serde_json::json!({"data":[{"id":"three"}]}),
                _ => panic!("unexpected request {url}"),
            };
            std::future::ready(Some(page))
        })
        .await
        .unwrap();
        assert_eq!(entries.len(), 3);
        let partial = fetch_live_pages("anthropic", "https://fixture.invalid/v1/models", |url| {
            std::future::ready(if url.query().is_some() {
                None
            } else {
                Some(serde_json::json!({"data":[{"id":"one"}],"has_more":true,"last_id":"one"}))
            })
        })
        .await;
        assert!(partial.is_none());
        let foreign = fetch_live_pages("openai", "https://fixture.invalid/v1/models", |_| {
            std::future::ready(Some(
                serde_json::json!({"data":[],"next":"https://other.invalid/v1/models"}),
            ))
        })
        .await;
        assert!(foreign.is_none());
        let repeated = fetch_live_pages("anthropic", "https://fixture.invalid/v1/models", |_| {
            std::future::ready(Some(
                serde_json::json!({"data":[{"id":"one"}],"has_more":true,"last_id":"one"}),
            ))
        })
        .await;
        assert!(repeated.is_none());
    }

    #[tokio::test]
    async fn google_native_listing_and_together_array_are_not_openai_envelopes() {
        let entries = fetch_live_pages("google", "https://fixture.invalid/v1beta/models", |url| {
            std::future::ready(Some(if url.query().is_none() {
                serde_json::json!({"models":[{"name":"models/gemini-new","inputTokenLimit":128000,"outputTokenLimit":8192}],"nextPageToken":"page-two"})
            } else {
                assert_eq!(url.query(), Some("pageToken=page-two"));
                serde_json::json!({"models":[{"name":"models/gemini-other"}]})
            }))
        }).await.unwrap();
        assert_eq!(entries[0]["id"], "gemini-new");
        let infos = live_infos_with_metadata("google", entries, &HashMap::new());
        assert_eq!(infos[0].context_window, 128000);
        assert_eq!(infos[0].max_output_tokens, 8192);
        let entries = fetch_live_pages("togetherai", "https://fixture.invalid/v1/models", |_| {
            std::future::ready(Some(serde_json::json!([{"id":"lab/new"}])))
        })
        .await
        .unwrap();
        assert_eq!(entries[0]["id"], "lab/new");
    }

    #[test]
    fn explicit_api_defaults_and_false_capabilities_override_public_metadata() {
        let mut public = host_model_info("fixture", "new");
        public.context_window = 1000000;
        public.max_output_tokens = 128000;
        public.supports_tools = true;
        public.supports_vision = true;
        public.supports_reasoning = true;
        public.supports_reasoning_effort = true;
        let entries = vec![
            serde_json::json!({"id":"new", "context_length":128000, "max_output_tokens":8192,
            "capabilities":{"function_calling":false,"vision":false,"reasoning":false}}),
        ];
        let infos = live_infos_with_metadata(
            "fixture",
            entries,
            &HashMap::from([("new".to_string(), public)]),
        );
        assert_eq!(infos[0].context_window, 128000);
        assert_eq!(infos[0].max_output_tokens, 8192);
        assert!(
            !infos[0].supports_tools
                && !infos[0].supports_vision
                && !infos[0].supports_reasoning
                && !infos[0].supports_reasoning_effort
        );
    }

    #[tokio::test]
    async fn provider_discovery_uses_catalog_alias_without_global_index() {
        let requests = std::sync::Mutex::new(Vec::new());
        let provider = cached_models_dev_with("clinepass", None, 1, |url: String| {
            requests.lock().unwrap().push(url.clone());
            std::future::ready(source_fixture(
                &url.replace("/providers/cline-pass", "/providers/fixture"),
            ))
        })
        .await
        .unwrap();
        assert!(provider.models.contains_key("vendor/new-model"));
        assert!(requests
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.contains("/providers/clinepass?")));
        assert!(requests
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.contains("/providers/cline-pass?")));
    }

    fn source_fixture(url: &str) -> Option<String> {
        let payload = match url {
            "https://api.github.com/repos/anomalyco/models.dev/contents/providers/fixture?ref=dev" =>
                r#"[{"name":"models","type":"dir","sha":"abc123"}]"#,
            "https://api.github.com/repos/anomalyco/models.dev/git/trees/abc123?recursive=1" =>
                r#"{"truncated":false,"tree":[{"type":"blob","path":"vendor/new-model.toml"},{"type":"blob","path":"standalone.toml"}]}"#,
            "https://raw.githubusercontent.com/anomalyco/models.dev/dev/providers/fixture/models/vendor/new-model.toml" => r#"
schema = 2
base_model = "lab/base"
base_model_omit = ["limit.output"]
[limit]
context = 64000
[capabilities]
reasoning = { effort = ["low", "high"] }
"#,
            "https://raw.githubusercontent.com/anomalyco/models.dev/dev/models/lab/base.toml" => r#"
schema = 2
[limit]
context = 128000
output = 32000
[capabilities]
tools = { supported = true }
reasoning = { supported = true }
[modalities]
input = ["text", "image/png"]
"#,
            "https://raw.githubusercontent.com/anomalyco/models.dev/dev/providers/fixture/models/standalone.toml" => r#"
schema = 2
[limit]
context = 16000
output = 4000
[capabilities]
tools = { supported = true }
"#,
            _ => return None,
        };
        Some(payload.to_string())
    }

    #[tokio::test]
    async fn provider_source_discovery_merges_metadata_and_preserves_membership() {
        let requests = std::sync::Mutex::new(Vec::new());
        let catalog = fetch_models_dev_source("fixture", &|url: String| {
            requests.lock().unwrap().push(url.clone());
            std::future::ready(source_fixture(&url))
        })
        .await
        .unwrap();
        assert_eq!(catalog.models.len(), 2);
        let model = &catalog.models["vendor/new-model"];
        assert_eq!(model.limit.as_ref().unwrap().context, Some(64000));
        assert_eq!(model.limit.as_ref().unwrap().output, None);
        assert!(model.tool_call && model.reasoning);
        let mut info = host_model_info("fixture", "vendor/new-model");
        apply_models_dev(&mut info, model);
        assert!(info.supports_vision && info.supports_tools && info.supports_reasoning);
        let known = vec![("fixture".to_string(), "account/new-model".to_string())];
        let enriched =
            enrich_models_dev_known(&known, &HashMap::from([("fixture".to_string(), catalog)]));
        assert_eq!(enriched.len(), 1);
        assert_eq!(enriched[0].1.len(), 1);
        assert_eq!(enriched[0].1[0].id, "account/new-model");
        assert_eq!(enriched[0].1[0].context_window, 64000);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert!(
            requests.iter().all(|url| source_fixture(url).is_some()),
            "no global index or unrelated provider requested"
        );
    }

    #[tokio::test]
    async fn provider_cache_reuses_refreshes_and_survives_failed_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let fetch = |url: String| {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            std::future::ready(source_fixture(&url))
        };
        let first = cached_models_dev_with("fixture", Some(dir.path()), 100, &fetch)
            .await
            .unwrap();
        assert!(first.models.contains_key("vendor/new-model"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 5);
        cached_models_dev_with("fixture", Some(dir.path()), 101, &fetch)
            .await
            .unwrap();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            5,
            "fresh cache makes no requests"
        );
        let failed = |_| std::future::ready(None);
        let stale =
            cached_models_dev_with("fixture", Some(dir.path()), 100 + MODELS_DEV_TTL, failed)
                .await
                .unwrap();
        assert_eq!(stale.models.len(), 2);
        assert!(cached_models_dev_with(
            "fixture",
            Some(dir.path()),
            101 + MODELS_DEV_STALE,
            failed
        )
        .await
        .is_none());
        cached_models_dev_with("fixture", Some(dir.path()), 100 + MODELS_DEV_TTL, &fetch)
            .await
            .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 10);
        let cache: ModelsDevCache =
            serde_json::from_slice(&std::fs::read(dir.path().join("fixture.json")).unwrap())
                .unwrap();
        assert_eq!(cache.fetched_at, 100 + MODELS_DEV_TTL);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "atomic temp file was persisted"
        );
        assert!(
            cached_models_dev_with("unrelated", Some(dir.path()), 102, failed)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn source_failure_does_not_cache_partial_provider_or_truncated_tree() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = |url: String| {
            std::future::ready(if url.ends_with("models/lab/base.toml") {
                None
            } else {
                source_fixture(&url)
            })
        };
        assert!(
            cached_models_dev_with("fixture", Some(dir.path()), 1, fetch)
                .await
                .is_none()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let fetch = |url: String| {
            std::future::ready(
                source_fixture(&url)
                    .map(|s| s.replace("\"truncated\":false", "\"truncated\":true")),
            )
        };
        assert!(fetch_models_dev_source("fixture", &fetch).await.is_none());
    }

    fn configured(id: &str, default_model: &str) -> (ConfiguredProvider, String) {
        (
            ConfiguredProvider {
                id: id.to_string(),
                name: id.to_string(),
                client: Arc::new(rx4::provider::OpenAIProvider::with_base_url(
                    "https://example.invalid/v1",
                    "test-key-not-real",
                    id,
                    id,
                )),
            },
            default_model.to_string(),
        )
    }

    fn model_ids(registry: &ModelRegistry, provider: &str) -> Vec<String> {
        let mut ids: Vec<String> = registry
            .models()
            .filter(|info| info.provider == provider)
            .map(|info| info.id.clone())
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn registry_registers_only_curated_go_models() {
        let registry = initial_model_registry(&[configured("opencode-go", "deepseek-v4.1-flash")]);
        assert_eq!(
            model_ids(&registry, "opencode-go"),
            vec![
                "deepseek-v4.1-flash".to_string(),
                "muse-spark-1.3-contributor".to_string(),
            ]
        );
    }

    #[test]
    fn registry_registers_only_curated_zai_models() {
        let registry = initial_model_registry(&[configured("zai", "glm-5.3")]);
        assert_eq!(
            model_ids(&registry, "zai"),
            vec!["glm-5.3".to_string(), "glm-5.3-flash".to_string()]
        );
    }

    #[test]
    fn parses_live_listing_ids_and_windows() {
        let value = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "glm-5.3-flash"},
                {"id": "glm-5.3", "context_length": 131_072},
                {"id": "glm-5.3:batch", "top_provider": {"context_length": 65_536}},
                {"id": ""},
                {"object": "model"},
            ],
        });
        let infos = live_infos_with_metadata(
            "zai-coding-plan",
            value["data"].as_array().unwrap().clone(),
            &HashMap::new(),
        );
        let ids: Vec<&str> = infos.iter().map(|info| info.id.as_str()).collect();
        assert_eq!(ids, ["glm-5.3-flash", "glm-5.3", "glm-5.3:batch"]);
        // glm-5* has no reported window, so the family heuristic applies.
        assert_eq!(infos[0].context_window, 200_000);
        assert_eq!(infos[1].context_window, 131_072);
        assert_eq!(infos[2].context_window, 65_536);
        assert!(infos.iter().all(|info| info.provider == "zai-coding-plan"));
    }

    #[tokio::test]
    async fn live_listing_ignores_unexpected_payloads() {
        for value in [serde_json::json!({}), serde_json::json!({"data":"nope"})] {
            assert!(
                fetch_live_pages("zai-coding-plan", "https://fixture.invalid/models", |_| {
                    std::future::ready(Some(value.clone()))
                })
                .await
                .is_none()
            );
        }
    }

    #[test]
    fn live_targets_cover_catalog_providers_only() {
        let ids = vec![
            "zai-coding-plan".to_string(),
            "openrouter".to_string(),
            "openai-codex".to_string(),
        ];
        let targets = live_models_targets_with(&ids, |_| Some("test-key".to_string()));
        assert_eq!(targets.len(), 1, "only the catalog provider is fetched");
        assert_eq!(targets[0].provider, "zai-coding-plan");
        assert_eq!(targets[0].url, "https://api.z.ai/api/coding/paas/v4/models");
        assert!(
            targets.iter().all(|target| target.provider != "openrouter"),
            "OpenRouter keeps its dedicated capability parsing"
        );
    }

    #[test]
    fn live_targets_require_a_key() {
        let ids = vec!["zai-coding-plan".to_string()];
        assert!(live_models_targets_with(&ids, |_| None).is_empty());
    }

    #[test]
    fn models_dev_metadata_enriches_without_removing_capabilities() {
        let model: ModelsDevModel = serde_json::from_value(serde_json::json!({
            "id": "glm-5.3-flash",
            "tool_call": true,
            "reasoning": true,
            "attachment": true,
            "modalities": {"input": ["text", "image"], "output": ["text"]},
            "limit": {"context": 1_000_000, "output": 131_072},
        }))
        .unwrap();
        let mut info = host_model_info("zai-coding-plan", "glm-5.3-flash");
        apply_models_dev(&mut info, &model);
        assert_eq!(info.context_window, 1_000_000);
        assert_eq!(info.max_output_tokens, 131_072);
        assert!(info.supports_tools);
        assert!(info.supports_reasoning);
        assert!(info.supports_vision);

        // A sparse entry leaves the host's own capabilities alone.
        let sparse: ModelsDevModel = serde_json::from_value(serde_json::json!({})).unwrap();
        let mut info = host_model_info("zai-coding-plan", "glm-5.3-flash");
        apply_models_dev(&mut info, &sparse);
        assert_eq!(info.context_window, 200_000);
        assert!(info.supports_tools);
        assert!(!info.supports_vision);
    }

    #[test]
    fn codex_listing_keeps_only_the_plan_models() {
        let value = serde_json::json!({"models": [
            {
                "slug": "gpt-6-astra",
                "context_window": 272_000,
                "input_modalities": ["text", "image"],
                "supported_reasoning_levels": [{"effort": "xhigh"}, {"effort": "max"}],
                "visibility": "list",
            },
            {
                "slug": "codex-auto-review",
                "context_window": 272_000,
                "input_modalities": ["text"],
                "visibility": "hide",
            },
            {"slug": "gpt-5.5", "input_modalities": ["text"]},
            {"nope": true},
        ]});
        let infos = codex_model_infos(&value);
        let ids: Vec<&str> = infos.iter().map(|info| info.id.as_str()).collect();
        assert_eq!(ids, ["gpt-6-astra", "gpt-5.5"], "hidden entries stay out");
        assert_eq!(infos[0].context_window, 272_000);
        assert!(infos[0].supports_vision);
        assert!(infos[0].supports_reasoning);
        assert!(infos[0].supports_reasoning_effort);
        assert!(!infos[1].supports_vision, "text-only model");
        assert!(!infos[1].supports_reasoning);
        assert!(infos.iter().all(|info| info.provider == "openai-codex"));
    }

    #[test]
    fn offline_codex_fallback_drops_models_a_chatgpt_account_cannot_call() {
        let ids: Vec<&str> = offline_codex_models().collect();
        assert!(!ids.contains(&"gpt-5.3-codex-spark"));
        assert!(ids.contains(&"gpt-5.5"));
        assert!(ids.contains(&"gpt-5.6-luna"));
    }

    #[test]
    fn models_dev_keys_follow_catalog_aliases() {
        let catalog: HashMap<String, ModelsDevProvider> = [
            ("cline-pass".to_string(), ModelsDevProvider::default()),
            ("moonshotai".to_string(), ModelsDevProvider::default()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            models_dev_key(&catalog, "clinepass").as_deref(),
            Some("cline-pass")
        );
        assert_eq!(
            models_dev_key(&catalog, "moonshot").as_deref(),
            Some("moonshotai")
        );
        // OAuth-only providers have no models.dev key of their own.
        assert_eq!(models_dev_key(&catalog, "openai-codex"), None);
    }
}
