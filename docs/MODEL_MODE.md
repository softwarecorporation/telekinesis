# Configure model roles

`model_mode` is an opt-in native module for the TUI and `tk exec`. It uses the
existing preferences, configured providers, and tool registry. It is not Pi
JavaScript plugin compatibility or a general plugin loader. ACP and the embed
SDK do not apply these role preferences.

## Enable model_mode

Configure and authenticate the providers first using the normal Telekinesis
provider setup. Then merge this example into `~/.telekinesis/prefs.json`, keeping
your other preferences. Replace the provider and model values with ones you use.

```json
{
	"plugins": {
		"model_mode": {
			"enabled": true,
			"main": { "provider": "openai", "model": "gpt-5.5" },
			"oracle": { "provider": "openai", "model": "gpt-5.5" },
			"subagents": { "provider": "openai", "model": "gpt-5.5" }
		}
	}
}
```

Start `tk` or `tk exec "Explain this project"` after saving the file. The example
uses one provider and model for all roles, but each role accepts its own
`provider` and `model` pair, including a different provider.

`enabled` defaults to `false`. Omit `model_mode` or set `enabled` to `false` to
disable it. Disabled role settings do not require provider credentials. When
enabled, every supplied role must name a configured provider and a nonempty
model. Invalid roles fail at startup, even if an exec flag overrides `main`.
Model availability still depends on the provider.

## Select the main and subagent models

- `main` is one shared main/worker role for the foreground agent. There is no
  separate `worker` setting. If `main` is absent, normal model selection applies.
- `oracle` registers the `oracle` tool with its own provider and model. If this
  role is absent, the module does not register that tool.
- `subagents` sets the provider and model on the existing subagent manager. If
  absent, subagents retain the normal host configuration.

In `tk exec`, an explicit `--provider` or `--model` bypasses the configured
`main` pair. A nonempty `TK_PROVIDER` also bypasses it. Oracle and subagent
settings still apply. For example:

```sh
tk exec --provider openai --model gpt-5.5 "Review this change"
```

In the TUI, use `/model` to select the main model. The host persists the selected
provider and model in `model_mode.main` while the module is enabled. This does
not replace a separately configured `subagents` role or change the oracle role.

These settings do not add automatic lane switching, a roadmap workflow, fanout,
or new orchestration. Rotary still owns the agent loop and existing subagent
execution.

## Consult the oracle

The main agent can call `oracle` when stuck or when it needs a second opinion.
The tool automatically includes the executing agent's current conversation:
user requests, assistant messages, tool calls and tool results. It accepts one
nonempty `question` string for a specific question and any additional evidence:

```json
{ "question": "Review this retry rule: retry only HTTP 429 and 503, at most twice. What failure cases does it miss?" }
```

Each invocation makes one model call with the question and a snapshot of the
current session. The transcript is quoted evidence, not executable tool
protocol. The newest 64 KiB are retained; longer transcripts explicitly report
that older context was truncated. The question is always supplied in full.
Shared tools read the executing agent's history, not a captured parent session.
Workspace files are available only when their contents have entered the
conversation, for example through a read tool. The oracle has no tools and
cannot inspect paths or change files. The call times out after 120 seconds.
Provider failures, empty answers, and attempted tool calls return tool errors.

Nested oracle usage is not included in the parent agent's usage or cost totals.
Provider billing still applies.

## Pi extensions and Markdown skills

Pi session compatibility covers JSONL v3 sessions, tool-name aliases such as
`read_file` → `read` and `run_command` → `bash`, and the Rust embed SDK
`create_agent_session`. The optional JavaScript extension bridge and its
supported API subset are documented in [Pi extensions](PI_EXTENSIONS.md).
It is separate from model-role configuration and Markdown skills.

Markdown skills are separate from `model_mode`. Build the TUI with
`--features skills`, or use `--features full` to include skills with the other
optional capabilities:

```sh
cargo build --manifest-path ui/tui/Cargo.toml --features skills
```

The host loads skills at startup from `~/.agents/skills`, the bundled `skills`
directory, and the workspace's `.telekinesis/skills`. Discovery reads Markdown
files directly in each directory and `<name>/SKILL.md` one directory deep. It
does not recursively search deeper directories.

For example, a workspace skill at `.telekinesis/skills/retry-review/SKILL.md`
can contain the following text. This is a file-content example, not a Pi
extension or an installer command:

```markdown
---
name: retry-review
description: Review retry rules for unsafe repeats.
trigger_patterns: [retry-review]
---
# Instructions
Check whether repeated requests can duplicate writes. State missing evidence.
```

Start the skills-enabled binary from that workspace, then enter
`retry-review: inspect our retry policy`. Prompt matching uses case-insensitive
substring matches against `trigger_patterns`. A stock Pi `description` alone
does not trigger activation. A Markdown `# Triggers` list is also supported.

Implementation references: [role configuration and oracle](../ui/tui/src/model_mode.rs),
[preferences and skill directories](../ui/tui/src/host.rs),
[exec selection](../ui/tui/src/exec.rs),
[TUI startup](../ui/tui/src/tui.rs),
[model selection and persistence](../ui/tui/src/app.rs), and
[Pi compatibility](../ui/tui/src/pi/mod.rs). Skill parsing and matching use
Rotary's `src/skill_engine/engine.rs` and `registry.rs` at the revision pinned in
[`ui/tui/Cargo.toml`](../ui/tui/Cargo.toml).
