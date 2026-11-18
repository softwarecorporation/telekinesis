//! Optional host configuration; Rotary still owns every agent loop.

use std::sync::Arc;

use futures::StreamExt;
use parking_lot::Mutex;
use rx4::agent::{ToolDefinition, ToolEffect, ToolResult};
use rx4::provider::{Message, StreamEvent};
use rx4::subagent::SubagentManager;

use crate::app::ConfiguredProvider;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Plugins {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_mode: Option<ModelMode>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ModelMode {
    #[serde(default)]
    pub enabled: bool,
    pub main: Option<ModelTarget>,
    pub oracle: Option<ModelTarget>,
    pub subagents: Option<ModelTarget>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelTarget {
    pub provider: String,
    pub model: String,
}

#[derive(Clone)]
pub(crate) struct ResolvedTarget {
    pub provider: ConfiguredProvider,
    pub model: String,
}

#[derive(Default)]
pub(crate) struct ResolvedMode {
    pub main: Option<ResolvedTarget>,
    oracle: Option<ResolvedTarget>,
    subagents: Option<ResolvedTarget>,
}

impl Plugins {
    pub(crate) fn resolve(
        &self,
        providers: &[(ConfiguredProvider, String)],
    ) -> anyhow::Result<ResolvedMode> {
        let Some(mode) = self.model_mode.as_ref().filter(|mode| mode.enabled) else {
            return Ok(ResolvedMode::default());
        };
        let resolve = |role: &str, target: &Option<ModelTarget>| {
            target
                .as_ref()
                .map(|target| {
                    let canonical = crate::provider_catalog::find(&target.provider)
                        .map(|spec| spec.id)
                        .unwrap_or(&target.provider);
                    let provider = providers
                        .iter()
                        .find(|(p, _)| p.id == canonical)
                        .map(|(p, _)| p.clone())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "model_mode.{role}: provider {} is not configured",
                                target.provider
                            )
                        })?;
                    anyhow::ensure!(
                        !target.model.trim().is_empty(),
                        "model_mode.{role}: model is empty"
                    );
                    let model = crate::provider_catalog::by_id(&provider.id)
                        .map(|spec| crate::provider_catalog::normalize_model(spec, &target.model))
                        .unwrap_or_else(|| target.model.clone());
                    Ok(ResolvedTarget { provider, model })
                })
                .transpose()
        };
        Ok(ResolvedMode {
            main: resolve("main", &mode.main)?,
            oracle: resolve("oracle", &mode.oracle)?,
            subagents: resolve("subagents", &mode.subagents)?,
        })
    }

    pub(crate) fn has_subagent_model(&self) -> bool {
        self.model_mode
            .as_ref()
            .is_some_and(|m| m.enabled && m.subagents.is_some())
    }
}

impl ResolvedMode {
    pub(crate) fn install(&self, agent: &mut rx4::Agent, manager: &Arc<Mutex<SubagentManager>>) {
        if let Some(target) = &self.subagents {
            let mut manager = manager.lock();
            manager.set_model(target.model.clone());
            manager.set_provider(target.provider.client.clone());
        }
        if let Some(target) = &self.oracle {
            let target = target.clone();
            agent.tools.register(ToolDefinition::new_boxed(
                "oracle", "Consult the configured oracle model. Supply a self-contained question and relevant context; it cannot read files or run tools.",
                r#"{"type":"object","properties":{"question":{"type":"string","minLength":1}},"required":["question"],"additionalProperties":false}"#,
                Box::new(move |_ctx, args| {
                    let target = target.clone();
                    Box::pin(async move {
                        let question = serde_json::from_str::<serde_json::Value>(&args)
                            .ok().and_then(|v| v["question"].as_str().map(str::to_string));
                        let Some(question) = question.filter(|s| !s.trim().is_empty()) else {
                            return ToolResult::err("oracle", "question must be a nonempty string");
                        };
                        match tokio::time::timeout(std::time::Duration::from_secs(120), consult(&target, &question)).await {
                            Ok(Ok(text)) => ToolResult::ok("oracle", text),
                            Ok(Err(error)) => ToolResult::err("oracle", error.to_string()),
                            Err(_) => ToolResult::err("oracle", "oracle timed out after 120 seconds"),
                        }
                    })
                }),
            ).with_effect(ToolEffect::Network));
            agent.allow_extra_tools(["oracle"]);
            manager.lock().set_tools(agent.tools.clone());
            crate::host::apply_scope(agent, agent.scope);
        }
    }
}

async fn consult(target: &ResolvedTarget, question: &str) -> anyhow::Result<String> {
    let mut stream = target.provider.client.stream(
        &[Message::user(question)],
        &Some("Answer the supplied question using only its context. State missing evidence. You have no tools and cannot change files.".into()),
        &target.model, &[], None,
    ).await?;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::Delta(delta) => text.push_str(&delta),
            StreamEvent::Done => {
                anyhow::ensure!(!text.trim().is_empty(), "oracle returned no text");
                return Ok(text);
            }
            StreamEvent::ToolCall(_) => {
                anyhow::bail!("oracle requested a tool despite having no tools")
            }
            StreamEvent::Usage(_) => {}
        }
    }
    anyhow::bail!("oracle stream ended before completion")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rx4::provider::{Provider, ProviderError, Role, StreamResult};

    struct Echo {
        id: &'static str,
        seen: Arc<Mutex<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl Provider for Echo {
        fn id(&self) -> &str {
            self.id
        }
        fn name(&self) -> &str {
            self.id
        }
        async fn stream(
            &self,
            messages: &[Message],
            _: &Option<String>,
            model: &str,
            tools: &[serde_json::Value],
            _: Option<&str>,
        ) -> Result<StreamResult, ProviderError> {
            self.seen.lock().push((self.id.into(), model.into()));
            let events = if self.id == "main-provider"
                && !messages.iter().any(|m| m.role == Role::Tool)
            {
                assert!(tools.iter().any(|t| t["name"] == "oracle"));
                vec![
                    StreamEvent::ToolCall(rx4::agent::ToolCall {
                        id: "consult".into(),
                        name: "oracle".into(),
                        arguments: r#"{"question":"Review asymmetric input 3 versus 8"}"#.into(),
                    }),
                    StreamEvent::Done,
                ]
            } else {
                if self.id == "oracle-provider" {
                    assert!(tools.is_empty());
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                }
                if self.id == "main-provider" {
                    let has_answer = messages
                        .iter()
                        .any(|m| m.content.contains("oracle-provider/think"));
                    if !has_answer {
                        assert!(messages.iter().any(|m| m.role == Role::Tool
                            && m.content.contains("[running in background]")));
                        return Ok(Box::new(futures::stream::iter([
                            Ok(StreamEvent::Delta("Waiting for the oracle".into())),
                            Ok(StreamEvent::Done),
                        ])));
                    }
                }
                vec![
                    StreamEvent::Delta(format!("{}/{model}", self.id)),
                    StreamEvent::Done,
                ]
            };
            Ok(Box::new(futures::stream::iter(events.into_iter().map(Ok))))
        }
    }

    #[tokio::test]
    async fn distinct_roles_use_their_own_provider_and_model_through_engine() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let providers = ["main-provider", "oracle-provider", "child-provider"].map(|id| {
            (
                ConfiguredProvider {
                    id: id.into(),
                    name: id.into(),
                    client: Arc::new(Echo {
                        id,
                        seen: seen.clone(),
                    }),
                },
                "unused".into(),
            )
        });
        let plugins: Plugins = serde_json::from_value(serde_json::json!({"model_mode": {
            "enabled":true,
            "main":{"provider":"main-provider","model":"work"},
            "oracle":{"provider":"oracle-provider","model":"think"},
            "subagents":{"provider":"child-provider","model":"small"}
        }}))
        .unwrap();
        let mode = plugins.resolve(&providers).unwrap();
        let main = mode.main.as_ref().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (mut agent, manager) = crate::host::build_agent(
            Some(main.provider.client.clone()),
            &main.model,
            "low",
            dir.path().into(),
            rx4::ModelRegistry::new(),
            &[],
        );
        mode.install(&mut agent, &manager);
        agent.async_tools.sync_window = std::time::Duration::from_millis(1);
        agent.set_approver(Arc::new(rx4::permissions::AlwaysAllow));
        agent.prompt("consult oracle").await.unwrap();
        assert_eq!(
            agent.messages_handle().read().last().unwrap().content,
            "main-provider/work",
            "the main response must consume the completed oracle answer"
        );
        let workspace = dir.path().to_path_buf();
        std::thread::spawn(move || {
            manager
                .lock()
                .spawn(
                    rx4::subagent::SubagentConfig::default(),
                    "independent child",
                    &workspace,
                )
                .unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(
            seen.lock()
                .iter()
                .cloned()
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from([
                ("main-provider".into(), "work".into()),
                ("oracle-provider".into(), "think".into()),
                ("child-provider".into(), "small".into()),
            ])
        );
    }

    #[test]
    fn disabled_mode_needs_no_credentials_and_registers_nothing() {
        let plugins: Plugins = serde_json::from_value(serde_json::json!({
            "model_mode": {"enabled":false,"oracle":{"provider":"missing","model":"private"}}
        }))
        .unwrap();
        let mode = plugins.resolve(&[]).unwrap();
        let mut agent = rx4::Agent::new();
        let manager = Arc::new(Mutex::new(SubagentManager::new()));
        mode.install(&mut agent, &manager);
        assert!(agent.tools.definitions().is_empty());
    }

    #[test]
    fn enabled_mode_rejects_unconfigured_role_before_inference() {
        let plugins: Plugins = serde_json::from_value(serde_json::json!({
            "model_mode": {"enabled":true,"subagents":{"provider":"missing","model":"private"}}
        }))
        .unwrap();
        assert!(plugins
            .resolve(&[])
            .err()
            .unwrap()
            .to_string()
            .contains("model_mode.subagents"));
    }
}
