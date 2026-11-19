# Optional Pi JavaScript extensions

`pi::extensions` runs a **limited ExtensionAPI compatibility bridge**, not a Pi
agent or a universal API shim. Rotary keeps its conversation loop, native tools,
scope checks, approvals, and policy. No JavaScript runs unless the host supplies
explicit extension paths and a Bun runtime configuration.

The API reference used here is Pi revision
`d4d188646c6bbd5d103238cbd05f1e5c01feb9e4`, especially
`packages/coding-agent/src/core/extensions/types.ts` and the unmodified
`examples/extensions/hello.ts` and `permission-gate.ts`. That revision has no
`examples/extensions/custom-tool.ts`; `hello.ts` is its minimal custom-tool example.

## Host preferences

The TUI and `tk exec` read `plugins.pi_extensions` from
`~/.telekinesis/prefs.json`. This setting is independent of `plugins.model_mode`.
Merge the following object into your existing preferences, keeping other settings:

```json
{
	"plugins": {
		"pi_extensions": {
			"enabled": true,
			"paths": [
				"/absolute/path/to/pi/packages/coding-agent/examples/extensions/hello.ts",
				"/absolute/path/to/pi/packages/coding-agent/examples/extensions/permission-gate.ts"
			],
			"bun": "/absolute/path/to/bun",
			"typebox_module": "/absolute/path/to/telekinesis/ui/tui/pi-extensions/typebox.mjs"
		}
	}
}
```

The paths above are placeholders for operator-selected files, not downloaded
packages. Bun and the optional TypeBox dependency must already be installed.
Use absolute paths: extension and TypeBox paths otherwise resolve against the
process working directory, not the preferences directory. The host does not
expand `~` in these fields.

The configuration fields are:

| Field | Default and behavior |
| --- | --- |
| `enabled` | `false`; omitting the entire `pi_extensions` object also disables the bridge |
| `paths` | Empty list; enabled installation requires at least one explicit extension path |
| `bun` | Optional; absent or `null` uses `bun` from `PATH` |
| `typebox_module` | Optional; absent or `null` supports hook-only extensions without schema imports |

Disabled configuration does not start Bun or load extension paths. Enabled
configuration requires a binary built with `pi-compat`, which is a default feature.
The preferences loader uses a fixed 30-second request timeout. It does not expose
a timeout preference.

TUI startup installs extensions after session restoration, so `session_start`
can read the restored conversation. `tk exec` installs them before its prompt.
Both surfaces retain the runtime owner and call `shutdown` on normal exit.
Startup failures are returned rather than ignored. ACP and direct embed SDK use
do not automatically apply this preferences object.

The host adds registered custom tools to its extra-tool allowance, but its scope
hook admits those tools only in Coding scope. Other scopes do not gain blanket
custom-tool exemptions. Custom tools remain Process-effect tools and still pass
native policy and approvals. The named host-hook replacement preserves the bridge
hook across scope changes; the current TUI and exec integration does not need to
reattach it. A TUI session still presents `hasUI: false` to extensions because this
bridge does not implement Pi UI dialogs.

## Host contract

```rust,ignore
pub fn install(
	agent: &mut rx4::Agent,
	paths: &[std::path::PathBuf],
	config: RuntimeConfig,
) -> anyhow::Result<ExtensionRuntime>;

pub struct RuntimeConfig {
	pub bun: std::path::PathBuf,
	pub typebox_module: Option<std::path::PathBuf>,
	pub request_timeout: std::time::Duration,
}
```

`install` loads the paths in order, awaits each default export, checks native tool
name conflicts, and awaits `session_start`. It then registers tools in the existing
Rotary registry and appends a `BeforeTool` hook. A startup error returns before
native tools or hooks change. Arbitrary JavaScript startup side effects cannot be
rolled back.

`install` does **not** grant tool scope exceptions or change approval policy. The
host decides which names returned by `runtime.tool_names()` belong in its explicit
extra-tool allowance. Existing host hooks still run first and can deny a call.

The returned owner must remain alive while the Agent uses its tools. Its methods
are:

- `tool_names() -> &[String]`: registered extension tools in registration order.
- `attach_hooks(&self, &mut rx4::Agent)`: append the bridge hook after host hooks.
  The initial install already does this. An embedding host that replaces the
  entire hook registry must reattach the bridge. The current host replaces only
  its named hook, so scope changes preserve the bridge. Repeated attachment to an
  unchanged registry dispatches handlers more than once.
- `shutdown(&self) -> anyhow::Result<()>`: await `session_shutdown`, then kill and
  reap Bun even if a handler fails. Drop kills and reaps without running JavaScript
  shutdown handlers. Shutdown is terminal, not a reload mechanism.

The install and lifecycle calls are synchronous and bounded by `request_timeout`.
Rotary's synchronous `BeforeTool` hook waits for each JavaScript promise to settle.
Tool execution uses `spawn_blocking` for IPC. The host must not hold a session write
lock while it calls these methods.

Tools read `ToolContext.session_messages` from the **executing Agent**, never a
captured parent transcript. Registry reuse therefore does not expose the parent's
messages through tool context. Custom tools refuse execution in another Agent
until the host explicitly attaches permission hooks to that Agent. The bridge
checks the executing session's identity without reading a captured transcript.
Foreground native-tool permission hooks are not automatically propagated to child
agents. The host must attach them explicitly. JavaScript module state remains
shared within one runtime. Separate runtime owners are required for separate trust
domains. Removing hooks after attachment violates this contract.

## Supported JavaScript contract

| Surface | Supported behavior |
| --- | --- |
| Default export | Function receiving `pi`; synchronous or async initialization |
| `pi.registerTool` | Startup-only registration of `name`, `label`, `description`, `parameters`, and `execute` |
| `pi.on` | `session_start`, `session_shutdown`, and `tool_call`; returns an unsubscribe function |
| Permission handler | Ordered, awaited handlers; `{ block: true, reason }` denies before the native executor; input mutations become native `ModifyArgs` |
| Tool execution | Ordered Process-effect tools; TypeBox validation before JavaScript `execute`; native policy and approvals still run |
| Tool result | Text content blocks, JSON-serializable `details`, optional `isError`; preserved together as JSON in Rotary's text result |
| Context | `cwd`, `mode: "rpc"`, `hasUI: false`, read-only session projection |
| Session methods | `getEntries`, `getBranch`, `getEntry`, `getLeafId`; `getSessionId` and `getSessionName` only in lifecycle and permission hooks |

`label` is accepted metadata, not a custom TUI renderer. The bridge supplies
`undefined` for `signal` and `onUpdate`, as permitted by the upstream tool
signature. Native cancellation kills the runtime during an active custom-tool
request rather than delivering a JavaScript `AbortSignal`. The native call ID
remains intact in `tool_call` events and provider results. `execute` currently
receives a fresh bridge execution UUID because Rotary's executor context does not
carry the originating call ID. Extensions that correlate those two IDs are outside
this subset.

Session entries are a linear snapshot of current messages, with `type: "message"`,
snapshot-local string IDs, parent IDs, text content blocks, and assistant tool-call
blocks. `getEntries()` and `getBranch()` return the same projection. They are not
Pi's persisted JSONL tree: custom entries, original timestamps, usage metadata,
branch navigation, and stable entry IDs are unavailable. Mutating the snapshot
does not change the native session. Tool context cannot supply a native session
ID or name, so those methods throw there rather than reuse the parent's identity.

Unknown ExtensionAPI, context, UI, event, and ToolDefinition members throw
`Pi capability unavailable: ...`. Commands, providers, UI dialogs and renderers,
`pi.exec`, message injection, resource discovery, model mutation, nested tool calls,
dynamic registration, streaming updates, and additional lifecycle events are not
implemented. Tool results with images or unsupported fields fail explicitly.

## Imports and optional dependencies

Bun `1.4.0` is the tested external runtime. The bridge does not install or update it.

Bun parses TypeScript and bundles each explicit entrypoint with its local imports.
The bridge clears TypeScript path aliases during that build so an upstream
checkout's monorepo aliases cannot redirect the supported package imports to a
complete Pi agent.

The supported virtual module exports are deliberately small:

- `@earendil-works/pi-coding-agent` and `@mariozechner/pi-coding-agent`:
  `defineTool`, the upstream identity helper. Type-only imports are erased by Bun.
- `@earendil-works/pi-ai`, `@mariozechner/pi-ai`, and `@sinclair/typebox`:
  `Type`, re-exported from the operator's real TypeBox module.

Other package exports are not emulated. Unavailable imports fail at startup.
Ordinary local imports and installed dependencies use Bun's resolver. Dynamic
package loaders, extension-relative asset lookup through `import.meta.url`, and
native package addons are not compatibility guarantees of this bundled subset.

`ui/tui/pi-extensions/package.json` pins the optional MIT-licensed `typebox` package
to `1.3.7`. `typebox.mjs` exports its real builders and `Value.Check` validator.
This dependency is separate from Cargo and native defaults. The bridge never runs
an installer and starts Bun with `--no-install`. An operator-managed installation
can use `bun install --ignore-scripts` in that isolated directory, then pass the
absolute path to `typebox.mjs`. An already installed equivalent module may also be
supplied explicitly. `None` supports hook-only extensions without schema imports;
custom-tool registration requires the validator.

Bun receives an empty explicit configuration and `--no-env-file`. Project
`bunfig.toml` preloads and automatic dotenv files therefore do not run before the
explicit extension list. The process still inherits the operator's environment.

## Trust, IPC, and failures

**Extensions are trusted user code, not sandboxed code.** Bun has the user's
filesystem, environment, network, and process access. Rotary policy gates
model-requested tools; it cannot restrict arbitrary JavaScript inside an extension.
The bridge adds no telemetry and sets `DO_NOT_TRACK=1`. That does not constrain
telemetry in operator-selected extensions or dependencies.

IPC consists of one request and one response per JSON line. Each direction has a
1 MiB frame limit. A dedicated worker serializes IO through a bounded request queue
of capacity one, with a separate reply channel per request. The response deadline
covers blocked stdin writes as well as reads. Console log, info, and debug output
go to stderr; stderr is inherited, not accumulated in native memory. Raw plugin
writes to stdout corrupt the protocol and fail closed.

Startup import errors, unsupported capabilities, invalid tool arguments, plugin
exceptions, malformed responses, and response timeouts are visible errors. A failed
response terminates the runtime; subsequent hooks deny instead of bypassing it.
An oversized native request is rejected before writing it. Drop, cancellation,
and shutdown kill and reap the direct Bun child. Descendant processes deliberately
spawned by trusted extensions are not managed by this bridge. There is no embedded
VM memory limit, process sandbox, restart loop, package discovery, or orchestration.

## Runtime choice

The removed QuickJS implementation at Telekinesis commit `f931c25` used `rquickjs`
with per-extension memory and stack limits. It removed TypeScript annotations with
regular expressions and expected global `__hostcall` and `__execute_tool` functions.
Its tests exercised handwritten globals rather than upstream ExtensionAPI modules.
It did not implement normal package imports or an awaitable extension event loop.

The external Bun process adds a runtime requirement and process overhead, and loses
QuickJS's in-process heap limit. In return, it provides real TypeScript parsing,
ES modules, local dependencies, and JavaScript promises without adding a VM or a
TypeScript parser to native defaults. Process termination gives the native host a
deadline even when JavaScript loops forever. This is an execution boundary, not a
security boundary. Neither runtime supplies a universal Pi API shim.

## Verification commands

The ordinary locked offline gate runs the missing-runtime test and marks Bun tests
as explicitly ignored:

```sh
cargo test --manifest-path ui/tui/Cargo.toml --locked --offline --all-features
```

The real-runtime tests require an explicitly installed Bun and TypeBox plus the
pinned upstream checkout:

```sh
TK_PI_TEST_BUN="$(command -v bun)" \
TK_PI_TEST_TYPEBOX="$PWD/ui/tui/pi-extensions/typebox.mjs" \
TK_PI_TEST_UPSTREAM="$PWD/../pi" \
cargo test --manifest-path ui/tui/Cargo.toml --locked --offline --all-features \
	pi::extensions::tests -- --include-ignored --nocapture
```

The tests run unmodified upstream `hello.ts` and `permission-gate.ts` through a real
Rotary Agent with a recording provider. They check the greeting and details,
current session reads, a denied bash call whose native executor never runs,
TypeBox rejection, native approval denial, historical imports, child-session
isolation, plugin errors, console separation, frame bounds, timeout, and process
reaping. No provider credentials or network calls are required.
