import { Type } from "@earendil-works/pi-ai";
import { defineTool } from "@earendil-works/pi-coding-agent";

export default function (pi) {
  let started = false;
  pi.on("session_start", async (_event, ctx) => {
    await Promise.resolve();
    started = !!ctx.sessionManager.getSessionId();
  });
  pi.registerTool(defineTool({
    name: "session_probe",
    label: "Session probe",
    description: "Read current branch messages",
    parameters: Type.Object({}),
    async execute(id, _params, _signal, _update, ctx) {
      const branch = ctx.sessionManager.getBranch();
      return { content: [{ type: "text", text: JSON.stringify({ id, started, branch }) }], details: {} };
    },
  }));
}
