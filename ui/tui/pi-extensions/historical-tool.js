// Historical import names, same deliberately limited tool contract.
import { Type } from "@mariozechner/pi-ai";
import { Type as LegacyType } from "@sinclair/typebox";
import { defineTool } from "@mariozechner/pi-coding-agent";

export default (pi) => pi.registerTool(defineTool({
  name: "historical_greet",
  label: "Historical greet",
  description: "Exercise historical Pi imports",
  parameters: Type.Object({ name: LegacyType.String() }),
  async execute(_id, { name }) {
    return { content: [{ type: "text", text: `Historical hello, ${name}!` }], details: { name } };
  },
}));
