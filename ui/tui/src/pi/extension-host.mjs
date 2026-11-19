// Deliberately limited Pi ExtensionAPI adapter, running only in an explicitly selected Bun.
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

const MAX = 1024 * 1024;
const output = process.stdout.write.bind(process.stdout);
// Console output must never become protocol output. Trusted plugins can still access raw stdio.
console.log = console.info = console.debug = (...args) => console.error(...args);
const capability = (name) => { throw new Error(`Pi capability unavailable: ${name}`); };
const strict = (name, values) => new Proxy(values, {
  get(target, key) {
    if (key === "then") return undefined;
    if (typeof key === "symbol") return target[key];
    if (!Object.hasOwn(target, key)) return capability(`${name}.${key}`);
    return target[key];
  },
});

let check;
let typeModule;
const tools = new Map();
const handlers = new Map();
let loading = false;
const supportedEvents = new Set(["session_start", "session_shutdown", "tool_call"]);

const imports = {
  name: "tk-pi-supported-imports",
  setup(build) {
    build.onResolve({ filter: /^@(earendil-works|mariozechner)\/pi-(ai|coding-agent)$|^@sinclair\/typebox$/ }, ({ path }) => ({ path, namespace: "tk-pi" }));
    build.onLoad({ filter: /.*/, namespace: "tk-pi" }, ({ path }) => {
      if (path.endsWith("pi-coding-agent")) {
        return { contents: "export const defineTool = (definition) => definition;", loader: "js" };
      }
      if (!typeModule) capability(`import ${path}: configure typebox_module (typebox 1.3.7)`);
      return { contents: `export { Type } from ${JSON.stringify(typeModule)};`, loader: "js" };
    });
  },
};

const api = strict("ExtensionAPI", {
  registerTool(tool) {
    if (!loading) capability("dynamic tool registration");
    const allowed = new Set(["name", "label", "description", "parameters", "execute"]);
    for (const key of Object.keys(tool)) if (!allowed.has(key)) capability(`ToolDefinition.${key}`);
    if (!tool.name || typeof tool.execute !== "function" || !tool.parameters || typeof tool.description !== "string") {
      throw new Error("Invalid tool definition");
    }
    if (tools.has(tool.name)) throw new Error(`Duplicate extension tool: ${tool.name}`);
    tools.set(tool.name, tool);
  },
  on(name, handler) {
    if (!loading) capability("dynamic event registration");
    if (!supportedEvents.has(name)) capability(`event ${name}`);
    const list = handlers.get(name) ?? [];
    list.push(handler);
    handlers.set(name, list);
    return () => { const index = list.indexOf(handler); if (index >= 0) list.splice(index, 1); };
  },
});

function context(snapshot) {
  return strict("ExtensionContext", {
    cwd: snapshot.cwd,
    mode: "rpc",
    hasUI: false,
    ui: strict("ExtensionContext.ui", {}),
    sessionManager: strict("sessionManager", {
      getSessionId: () => snapshot.sessionId ?? capability("sessionManager.getSessionId in tool context"),
      getSessionName: () => snapshot.sessionName ?? capability("sessionManager.getSessionName in tool context"),
      getEntries: () => structuredClone(snapshot.entries),
      getBranch: () => structuredClone(snapshot.branch),
      getLeafId: () => snapshot.leafId,
      getEntry: (id) => structuredClone(snapshot.entries.find((entry) => entry.id === id)),
    }),
  });
}

async function dispatch(request) {
  if (request.op === "init") {
    if (request.typeboxModule) {
      typeModule = request.typeboxModule;
      const types = await import(pathToFileURL(typeModule).href);
      // Value.Check is the actual TypeBox validator, not an imitation schema interpreter.
      check = types.Value?.Check;
      if (!check) throw new Error("typebox_module must export Value.Check (use pi-extensions/typebox.mjs)");
    }
    loading = true;
    try {
      for (const path of request.paths) {
        const build = await Bun.build({ entrypoints: [path], target: "bun", write: false,
          tsconfig: { compilerOptions: { paths: {} } }, plugins: [imports] });
        if (!build.success) throw new Error(`Pi capability unavailable or extension import failed: ${build.logs.join("\n")}`);
        const source = await build.outputs[0].text();
        const extension = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
        if (typeof extension.default !== "function") throw new Error(`Extension has no default function: ${path}`);
        await extension.default(api);
      }
    } finally { loading = false; }
    if (tools.size && !check) capability("tool argument validation: configure typebox_module");
    return [...tools.values()].map(({ name, description, parameters }) => ({ name, description, parameters }));
  }
  const ctx = context(request.context);
  if (request.op === "event") {
    if (!supportedEvents.has(request.event.type)) capability(`event ${request.event.type}`);
    for (const handler of handlers.get(request.event.type) ?? []) {
      const result = await handler(request.event, ctx);
      if (request.event.type === "tool_call") {
        if (result && Object.keys(result).some((key) => !["block", "reason"].includes(key))) capability("tool_call result field");
        if (result?.block) return { block: true, reason: result.reason ?? "Blocked by Pi extension" };
      } else if (result !== undefined) capability(`${request.event.type} return value`);
    }
    return request.event.type === "tool_call" ? { input: request.event.input } : null;
  }
  if (request.op === "execute") {
    const tool = tools.get(request.name);
    if (!tool) throw new Error(`Unknown extension tool: ${request.name}`);
    if (!check) capability("tool argument validation: configure typebox_module");
    if (!check(tool.parameters, request.input)) throw new Error(`Invalid arguments for ${request.name}`);
    const result = await tool.execute(request.toolCallId, request.input, undefined, undefined, ctx);
    if (!result || !Array.isArray(result.content) || result.content.some((c) => c.type !== "text" || typeof c.text !== "string")) capability("tool result: only text content is supported");
    if (Object.keys(result).some((key) => !["content", "details", "isError"].includes(key))) capability("tool result field");
    return result;
  }
  throw new Error(`Unknown bridge operation: ${request.op}`);
}

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  let response;
  try {
    if (Buffer.byteLength(line) > MAX) throw new Error("Pi IPC request exceeds 1 MiB");
    response = { ok: true, value: await dispatch(JSON.parse(line)) };
  } catch (error) {
    response = { ok: false, error: `${String(error?.stack ?? error)}${error?.errors ? "\n" + error.errors.map(String).join("\n") : ""}` };
  }
  let encoded = JSON.stringify(response);
  if (Buffer.byteLength(encoded) > MAX) encoded = JSON.stringify({ ok: false, error: "Pi IPC response exceeds 1 MiB" });
  output(encoded + "\n");
}
