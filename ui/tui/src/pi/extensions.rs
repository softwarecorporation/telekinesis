//! Optional, trusted Pi JavaScript extensions in a separately installed Bun process.
//! Rotary still owns tool scheduling, policy, approval and the conversation loop.

use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use rx4::agent::{ToolDefinition, ToolEffect, ToolResult};
use rx4::{Agent, HookDecision, HookEvent, HookRegistry};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Weak};
use std::time::Duration;

const MAX_FRAME: usize = 1024 * 1024;

/// No defaults enable JavaScript, install dependencies, or discover project code.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub bun: PathBuf,
    /// Module exporting real TypeBox `Type` and `Value` (see pi-extensions/typebox.mjs).
    pub typebox_module: Option<PathBuf>,
    pub request_timeout: Duration,
}

type Reply = mpsc::SyncSender<Result<Value, String>>;
type SessionMessages = parking_lot::RwLock<Vec<rx4::provider::Message>>;
type AttachedSessions = Arc<Mutex<Vec<Weak<SessionMessages>>>>;

struct Bridge {
    requests: mpsc::SyncSender<(Vec<u8>, Reply)>,
    child: Arc<Mutex<Child>>,
    timeout: Duration,
    // Keep the embedded host on disk until Bun has finished loading it.
    _host: tempfile::NamedTempFile,
    _config: tempfile::NamedTempFile,
}

impl Bridge {
    fn start(config: &RuntimeConfig, cwd: &std::path::Path) -> Result<Self> {
        if config.request_timeout.is_zero() {
            bail!("Pi request_timeout must be nonzero");
        }
        let mut host = tempfile::Builder::new().suffix(".mjs").tempfile()?;
        host.write_all(include_bytes!("extension-host.mjs"))?;
        let bun_config = tempfile::Builder::new().suffix(".toml").tempfile()?;
        let mut config_argument = std::ffi::OsString::from("--config=");
        config_argument.push(bun_config.path());
        let mut child = Command::new(&config.bun)
            .arg("--no-install")
            .arg("--no-env-file")
            .arg(config_argument)
            .arg(host.path())
            .current_dir(cwd)
            .env("DO_NOT_TRACK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("start optional Pi Bun runtime {}", config.bun.display()))?;
        let mut input = child.stdin.take().context("Bun stdin missing")?;
        let output = child.stdout.take().context("Bun stdout missing")?;
        let child = Arc::new(Mutex::new(child));
        let (requests, queue) = mpsc::sync_channel::<(Vec<u8>, Reply)>(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            while let Ok((frame, reply)) = queue.recv() {
                let response = (|| -> Result<Value> {
                    input.write_all(&frame)?;
                    input.flush()?;
                    let mut line = Vec::new();
                    reader
                        .by_ref()
                        .take((MAX_FRAME + 2) as u64)
                        .read_until(b'\n', &mut line)?;
                    if line.len() > MAX_FRAME + 1 || line.last() != Some(&b'\n') {
                        bail!("Pi IPC response missing, truncated, or exceeds 1 MiB");
                    }
                    let envelope: Value = serde_json::from_slice(&line)
                        .context("invalid Pi IPC JSON (plugin must not write raw stdout)")?;
                    if envelope["ok"] != true {
                        bail!(
                            "Pi extension failed: {}",
                            envelope["error"].as_str().unwrap_or("invalid response")
                        );
                    }
                    Ok(envelope["value"].clone())
                })();
                let failed = response.is_err();
                let _ = reply.send(response.map_err(|error| error.to_string()));
                // Errors fail closed. Never risk pairing a late response with another request.
                if failed {
                    break;
                }
            }
        });
        Ok(Self {
            requests,
            child,
            timeout: config.request_timeout,
            _host: host,
            _config: bun_config,
        })
    }

    fn request(&self, value: Value) -> Result<Value> {
        let mut frame = serde_json::to_vec(&value)?;
        if frame.len() > MAX_FRAME {
            bail!("Pi IPC request exceeds 1 MiB");
        }
        frame.push(b'\n');
        let (tx, rx) = mpsc::sync_channel(1);
        self.requests
            .try_send((frame, tx))
            .map_err(|_| anyhow::anyhow!("Pi runtime unavailable or busy"))?;
        match rx.recv_timeout(self.timeout) {
            Ok(Ok(value)) => Ok(value),
            outcome => {
                self.stop();
                match outcome {
                    Ok(Err(error)) => bail!("{error}"),
                    _ => bail!("Pi runtime response timed out or disconnected"),
                }
            }
        }
    }

    fn stop(&self) {
        let mut child = self.child.lock();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Owns the process lifetime; hooks/tools become fail-closed after shutdown or Drop.
pub struct ExtensionRuntime {
    bridge: Arc<Bridge>,
    names: Vec<String>,
    attached: AttachedSessions,
    session: Arc<parking_lot::RwLock<rx4::session::Session>>,
    cwd: PathBuf,
}

impl ExtensionRuntime {
    pub fn tool_names(&self) -> &[String] {
        &self.names
    }

    /// Append after host hooks. Call again only if the host replaced the hook registry.
    pub fn attach_hooks(&self, agent: &mut Agent) {
        self.attached
            .lock()
            .push(Arc::downgrade(&agent.messages_handle()));
        let bridge = Arc::clone(&self.bridge);
        let session = agent.session_handle();
        let cwd = agent.workspace_root.clone();
        agent
            .hooks
            .get_or_insert_with(HookRegistry::new)
            .add(move |event| {
                let HookEvent::BeforeTool { tool } = event else {
                    return HookDecision::Allow;
                };
                let input = match serde_json::from_str::<Value>(&tool.arguments) {
                    Ok(input) => input,
                    Err(error) => {
                        return HookDecision::Deny {
                            reason: format!("Pi tool arguments: {error}"),
                        }
                    }
                };
                let event = json!({"type":"tool_call", "toolCallId":tool.id,
                "toolName":rx4::agent::normalize_tool_name(&tool.name), "input":input});
                match bridge.request(
                    json!({"op":"event", "event":event, "context":session_context(&session, &cwd)}),
                ) {
                    Ok(result) if result["block"] == true => HookDecision::Deny {
                        reason: result["reason"]
                            .as_str()
                            .unwrap_or("Blocked by Pi extension")
                            .into(),
                    },
                    Ok(result) if result["input"] != input => HookDecision::ModifyArgs {
                        arguments: result["input"].to_string(),
                    },
                    Ok(_) => HookDecision::Allow,
                    Err(error) => HookDecision::Deny {
                        reason: error.to_string(),
                    },
                }
            });
    }

    /// Wait for async shutdown handlers, then always kill and reap the runtime.
    pub fn shutdown(&self) -> Result<()> {
        let result = self
            .bridge
            .request(json!({"op":"event", "event":{"type":"session_shutdown"},
            "context":session_context(&self.session, &self.cwd)}));
        self.bridge.stop();
        result.map(|_| ())
    }
}

impl Drop for ExtensionRuntime {
    fn drop(&mut self) {
        self.bridge.stop();
    }
}

#[derive(Deserialize)]
struct RegisteredTool {
    name: String,
    description: String,
    parameters: Value,
}

/// Install only explicit paths. Does not grant scope exceptions or modify approval policy.
pub fn install(
    agent: &mut Agent,
    paths: &[PathBuf],
    config: RuntimeConfig,
) -> Result<ExtensionRuntime> {
    if paths.is_empty() {
        bail!("Pi extensions require at least one explicit path");
    }
    let paths = paths
        .iter()
        .map(std::fs::canonicalize)
        .collect::<std::io::Result<Vec<_>>>()?;
    let types = config
        .typebox_module
        .as_ref()
        .map(std::fs::canonicalize)
        .transpose()?;
    let bridge = Arc::new(Bridge::start(&config, &agent.workspace_root)?);
    let registered: Vec<RegisteredTool> = serde_json::from_value(
        bridge.request(json!({"op":"init", "paths":paths, "typeboxModule":types}))?,
    )?;
    for tool in &registered {
        if tool.name.is_empty()
            || rx4::agent::normalize_tool_name(&tool.name) != tool.name
            || agent.tools.contains(&tool.name)
        {
            bail!("Pi tool conflicts with native tool/alias: {}", tool.name);
        }
    }
    let runtime = ExtensionRuntime {
        bridge: Arc::clone(&bridge),
        names: registered.iter().map(|t| t.name.clone()).collect(),
        attached: Arc::new(Mutex::new(Vec::new())),
        session: agent.session_handle(),
        cwd: agent.workspace_root.clone(),
    };
    bridge.request(json!({"op":"event", "event":{"type":"session_start"},
        "context":session_context(&runtime.session, &runtime.cwd)}))?;
    for tool in registered {
        let bridge = Arc::clone(&bridge);
        let attached = Arc::clone(&runtime.attached);
        let name = tool.name.clone();
        let definition = ToolDefinition::new_boxed(
            tool.name,
            tool.description,
            tool.parameters.to_string(),
            Box::new(move |ctx, args| {
            let bridge = Arc::clone(&bridge);
            let attached = Arc::clone(&attached);
            let name = name.clone();
            Box::pin(async move {
                let id = uuid::Uuid::new_v4().to_string();
                let cancellation = ctx.cancellation.clone();
                let running = Arc::clone(&bridge);
                let task = tokio::task::spawn_blocking(move || -> Result<Value> {
                    if ctx.cancellation.is_canceled() { bail!("Pi tool cancelled"); }
                    let session = ctx.session_messages.as_ref().context("Pi tool needs executing Agent session context")?;
                    let identity = Arc::downgrade(session);
                    if !attached.lock().iter().any(|known| Weak::ptr_eq(known, &identity)) {
                        bail!("Pi capability unavailable: custom tool execution requires permission hooks attached to the executing Agent");
                    }
                    let messages = session.read();
                    let context = messages_context(&messages, &ctx.workspace_root);
                    drop(messages);
                    bridge.request(json!({"op":"execute", "name":name, "input":serde_json::from_str::<Value>(&args)?,
                        "toolCallId":id, "context":context}))
                });
                let outcome = match cancellation.run(task).await {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        running.stop();
                        return ToolResult::err("", "Pi tool cancelled; runtime stopped");
                    }
                };
                match outcome {
                    Ok(Ok(value)) => {
                        // Rotary's result is text-only; preserve Pi content and details together.
                        let text = value.to_string();
                        if value["isError"] == true { ToolResult::err("", text) } else { ToolResult::ok("", text) }
                    }
                    Ok(Err(error)) => ToolResult::err("", error.to_string()),
                    Err(error) => ToolResult::err("", format!("Pi worker failed: {error}")),
                }
            })
            }),
        ).with_effect(ToolEffect::Process);
        agent.tools.register(definition);
    }
    runtime.attach_hooks(agent);
    Ok(runtime)
}

fn messages_context(messages: &[rx4::provider::Message], cwd: &std::path::Path) -> Value {
    let entries: Vec<Value> = messages.iter().enumerate().map(|(index, message)| {
        let mut content = vec![json!({"type":"text", "text":message.content})];
        content.extend(message.tool_calls.iter().map(|call| json!({"type":"toolCall", "id":call.id,
            "name":call.name, "arguments":serde_json::from_str::<Value>(&call.arguments).unwrap_or(Value::Null)})));
        let role = if message.role == rx4::provider::Role::Tool { "toolResult".into() } else { message.role.to_string() };
        json!({"type":"message", "id":index.to_string(), "parentId":index.checked_sub(1).map(|id| id.to_string()),
            "message":{"role":role, "content":content, "toolCallId":message.tool_call_id}})
    }).collect();
    json!({"cwd":cwd, "entries":entries, "branch":entries, "leafId":messages.len().checked_sub(1).map(|id| id.to_string())})
}

fn session_context(
    session: &parking_lot::RwLock<rx4::session::Session>,
    cwd: &std::path::Path,
) -> Value {
    let session = session.read();
    let mut snapshot = messages_context(&session.live_messages(), cwd);
    snapshot["sessionId"] = json!(session.id);
    snapshot["sessionName"] = json!(session.name);
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use rx4::agent::ToolCall;
    use rx4::provider::{Message, Provider, ProviderError, Role, StreamEvent, StreamResult};

    struct Recording {
        calls: Mutex<Option<Vec<ToolCall>>>,
        seen: Arc<Mutex<Vec<Message>>>,
    }

    #[async_trait::async_trait]
    impl Provider for Recording {
        fn id(&self) -> &str {
            "pi-test"
        }
        fn name(&self) -> &str {
            "pi-test"
        }
        async fn stream(
            &self,
            messages: &[Message],
            _: &Option<String>,
            _: &str,
            tools: &[Value],
            _: Option<&str>,
        ) -> Result<StreamResult, ProviderError> {
            *self.seen.lock() = messages.to_vec();
            let mut events = Vec::new();
            if let Some(calls) = self.calls.lock().take() {
                for call in calls {
                    assert!(tools.iter().any(|tool| tool["name"] == call.name));
                    events.push(Ok(StreamEvent::ToolCall(call)));
                }
            } else {
                events.push(Ok(StreamEvent::Delta("done".into())));
            }
            events.push(Ok(StreamEvent::Done));
            Ok(Box::new(futures::stream::iter(events)))
        }
    }

    fn config() -> RuntimeConfig {
        RuntimeConfig {
            bun: std::env::var_os("TK_PI_TEST_BUN")
                .expect("set TK_PI_TEST_BUN")
                .into(),
            typebox_module: Some(
                std::env::var_os("TK_PI_TEST_TYPEBOX")
                    .expect("set TK_PI_TEST_TYPEBOX")
                    .into(),
            ),
            request_timeout: Duration::from_secs(10),
        }
    }

    fn upstream(name: &str) -> PathBuf {
        PathBuf::from(
            std::env::var_os("TK_PI_TEST_UPSTREAM")
                .expect("set TK_PI_TEST_UPSTREAM to pinned Pi checkout"),
        )
        .join("packages/coding-agent/examples/extensions")
        .join(name)
    }

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: format!("call-{name}"),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    fn fixture(name: &str) -> PathBuf {
        std::env::var_os("TK_PI_TEST_FIXTURES")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("pi-extensions"))
            .join(name)
    }

    fn agent(calls: Vec<ToolCall>) -> (Agent, Arc<Mutex<Vec<Message>>>) {
        let mut agent = Agent::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        agent.set_provider(Arc::new(Recording {
            calls: Mutex::new(Some(calls)),
            seen: seen.clone(),
        }));
        agent.set_approver(Arc::new(rx4::permissions::AlwaysAllow));
        (agent, seen)
    }

    #[test]
    fn missing_runtime_is_explicit_and_does_not_install_tools() {
        let mut agent = Agent::new();
        let file = tempfile::NamedTempFile::new().unwrap();
        let config = RuntimeConfig {
            bun: PathBuf::from("/not-installed/tk-bun"),
            typebox_module: None,
            request_timeout: Duration::from_secs(1),
        };
        let error = install(&mut agent, &[file.path().into()], config)
            .err()
            .unwrap();
        assert!(error.to_string().contains("start optional Pi Bun runtime"));
        assert_eq!(agent.tools.count(), 0);
        assert!(agent.hooks.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires explicit Bun, TypeBox and pinned upstream Pi paths; see PI_EXTENSIONS.md"]
    async fn bun_upstream_hello_and_permission_gate_through_rotary() {
        let (mut agent, seen) = agent(vec![
            call("hello", r#"{"name":"Ada"}"#),
            call("session_probe", "{}"),
            call("bash", r#"{"command":"sudo echo must-not-execute"}"#),
        ]);
        let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = executions.clone();
        agent.tools.register(
            ToolDefinition::new_boxed(
                "bash",
                "record bash",
                r#"{"type":"object"}"#,
                Box::new(move |_, _| {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Box::pin(async { ToolResult::ok("", "bash executed") })
                }),
            )
            .with_effect(ToolEffect::Process),
        );
        let before = agent.policy.clone();
        let runtime = install(
            &mut agent,
            &[
                upstream("hello.ts"),
                upstream("permission-gate.ts"),
                fixture("session-tool.ts"),
            ],
            config(),
        )
        .unwrap();
        assert_eq!(runtime.tool_names(), &["hello", "session_probe"]);
        assert_eq!(format!("{:?}", agent.policy), format!("{before:?}"));
        agent
            .prompt("read the current session and greet Ada")
            .await
            .unwrap();
        let seen = seen.lock();
        assert!(seen.iter().any(|m| m.role == Role::Tool
            && m.content.contains("Hello, Ada!")
            && m.content.contains("\"greeted\":\"Ada\"")));
        assert!(seen
            .iter()
            .any(|m| m.role == Role::Tool && m.content.contains("Dangerous command blocked")));
        assert!(seen.iter().any(|m| m.role == Role::Tool
            && m.content.contains("started\\\":true")
            && m.content.contains("greet Ada")));
        assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 0);
        runtime.shutdown().unwrap();
        assert!(runtime.bridge.child.lock().try_wait().unwrap().is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires explicit Bun, TypeBox and pinned upstream Pi paths; see PI_EXTENSIONS.md"]
    async fn bun_historical_imports_and_shared_registry_use_executing_session() {
        let (mut parent, _) = agent(vec![]);
        parent.append_message(Message::user("parent-private-message"));
        let runtime = install(
            &mut parent,
            &[fixture("session-tool.ts"), fixture("historical-tool.js")],
            config(),
        )
        .unwrap();
        let (mut unattached, denied) = agent(vec![call("session_probe", "{}")]);
        unattached.tools = Arc::clone(&parent.tools);
        unattached.prompt("unattached child").await.unwrap();
        assert!(denied
            .lock()
            .iter()
            .any(|message| message.role == Role::Tool
                && message
                    .content
                    .contains("requires permission hooks attached")));
        let (mut child, seen) = agent(vec![
            call("session_probe", "{}"),
            call("historical_greet", r#"{"name":"Grace"}"#),
        ]);
        child.tools = Arc::clone(&parent.tools);
        runtime.attach_hooks(&mut child);
        child.prompt("child-only-message").await.unwrap();
        let seen = seen.lock();
        let probe = seen
            .iter()
            .find(|m| m.role == Role::Tool && m.content.contains("started"))
            .unwrap();
        assert!(probe.content.contains("child-only-message"));
        assert!(!probe.content.contains("parent-private-message"));
        assert!(seen
            .iter()
            .any(|m| m.role == Role::Tool && m.content.contains("Historical hello, Grace!")));
        runtime.shutdown().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires explicit Bun, TypeBox and pinned upstream Pi paths; see PI_EXTENSIONS.md"]
    async fn bun_schema_validation_and_policy_still_gate_custom_tools() {
        let (mut invalid, seen) = agent(vec![call("hello", r#"{"name":42}"#)]);
        let _runtime = install(&mut invalid, &[upstream("hello.ts")], config()).unwrap();
        invalid.prompt("invalid").await.unwrap();
        assert!(seen
            .lock()
            .iter()
            .any(|m| m.role == Role::Tool && m.content.contains("Invalid arguments for hello")));

        let (mut denied, seen) = agent(vec![call("hello", r#"{"name":"Ada"}"#)]);
        denied.set_approver(Arc::new(rx4::permissions::AlwaysDeny));
        let _runtime = install(&mut denied, &[upstream("hello.ts")], config()).unwrap();
        denied.prompt("denied").await.unwrap();
        assert!(seen
            .lock()
            .iter()
            .any(|m| m.role == Role::Tool && m.content.contains("denied by policy")));
        assert!(!seen.lock().iter().any(|m| m.content == "Hello, Ada!"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires explicit Bun, TypeBox and pinned upstream Pi paths; see PI_EXTENSIONS.md"]
    async fn bun_plugin_errors_are_visible_and_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        for source in [
            "export default pi => pi.registerCommand('no', {});",
            "export default pi => pi.on('agent_start', () => {});",
            "export default () => { throw new Error('plugin startup boom'); };",
        ] {
            let path = dir.path().join("unsupported.ts");
            std::fs::write(&path, source).unwrap();
            let mut agent = Agent::new();
            let error = install(&mut agent, &[path], config())
                .err()
                .unwrap()
                .to_string();
            assert!(
                error.contains("Pi capability unavailable")
                    || error.contains("plugin startup boom"),
                "{error}"
            );
            assert_eq!(agent.tools.count(), 0);
        }
        let path = dir.path().join("throws.ts");
        std::fs::write(&path, "export default pi => pi.on('tool_call', async () => { await Promise.resolve(); throw new Error('permission hook boom'); });").unwrap();
        let (mut agent, seen) = agent(vec![call("hello", r#"{"name":"Ada"}"#)]);
        let runtime = install(&mut agent, &[upstream("hello.ts"), path], config()).unwrap();
        agent.prompt("hook error").await.unwrap();
        assert!(seen
            .lock()
            .iter()
            .any(|m| m.role == Role::Tool && m.content.contains("permission hook boom")));
        assert!(runtime.bridge.child.lock().try_wait().unwrap().is_some());
    }

    #[test]
    #[ignore = "requires explicit Bun and TypeBox paths; see PI_EXTENSIONS.md"]
    fn bun_ipc_timeout_and_frame_bounds_stop_the_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hang.js");
        std::fs::write(&path, "export default pi => { console.log('extension diagnostic'); pi.on('session_shutdown', () => new Promise(() => {})); };").unwrap();
        let mut agent = Agent::new();
        let mut config = config();
        config.request_timeout = Duration::from_millis(500);
        let runtime = install(&mut agent, &[path], config).unwrap();
        assert!(runtime
            .bridge
            .request(json!({"op":"unused", "oversized":"x".repeat(MAX_FRAME)}))
            .unwrap_err()
            .to_string()
            .contains("exceeds 1 MiB"));
        assert!(runtime
            .shutdown()
            .unwrap_err()
            .to_string()
            .contains("timed out"));
        assert!(runtime.bridge.child.lock().try_wait().unwrap().is_some());
    }
}
