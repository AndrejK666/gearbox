// Explicit live integration check; uses the installed Claude Code's existing login.
// No token is read by this script and no product or filesystem tool is offered.
const assert = require("node:assert/strict");
const { claudeCodeModels } = require("../gearbox-studio/lib/node/claude-code-model");

if (!process.argv.includes("--live")) {
  console.error("Use --live to authorize a model request through the existing Claude Code login.");
  process.exitCode = 1;
} else {
  (async () => {
    const models = await claudeCodeModels();
    assert.equal(models[0]?.status.status, "ready", "Existing Claude Code login must be ready");
    let calls = 0;
    const parts = [];
    const response = await models[0].request({
      sessionId: "gearbox-provider-smoke", requestId: "gearbox-provider-smoke",
      // Match the JSON-RPC boundary: methods on this request field do not survive serialization.
      cancellationToken: {},
      messages: [
        {actor:"system", type:"text", text:"Use the provided tool to obtain the marker. Never guess it."},
        {actor:"user", type:"text", text:"Call gearbox_probe once, then reply with its marker."},
      ],
      tools: [{
        id:"gearbox_probe", name:"gearbox_probe", description:"Read the test marker.",
        parameters:{type:"object", properties:{}},
        handler:async (_args, context) => {
          assert(parts.some(part => part.tool_calls?.some(call =>
            !call.finished && call.id === context.toolCallId && call.function.name === "gearbox_probe"
          )), "Theia must receive the matching tool start before its confirmation/handler runs");
          calls++;
          return "GEARBOX_CLAUDE_TOOL_OK";
        },
      }],
    });
    for await (const part of response.stream) parts.push(part);
    assert.equal(calls, 1);
    assert(parts.some(part => part.tool_calls?.some(call => call.finished && call.function.name === "gearbox_probe")));
    assert(parts.some(part => part.content?.includes("GEARBOX_CLAUDE_TOOL_OK")));
    console.log("Existing Claude Code login and request-scoped MCP tool callback passed.");
  })().catch(error => { console.error(error.message); process.exitCode = 1; });
}
