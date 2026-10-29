import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { randomUUID } from "node:crypto";
import { createRequire } from "node:module";
import { CancellationToken } from "@theia/core";
import { LanguageModel, LanguageModelStreamResponsePart, ToolInvocationContext, UserRequest, formatToolCallContentForModel, hasToolCallError } from "@theia/ai-core";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";

// The agent SDK is ESM-only and is loaded with `require` below, so its types are
// named in import mode. Its options then expect the MCP SDK's ESM `McpServer`,
// while this CommonJS file constructs the CJS build of the same class: two
// declarations of one object, so the instance is handed over as the ESM type.
type ClaudeAgentSdk = typeof import("@anthropic-ai/claude-agent-sdk", { with: { "resolution-mode": "import" } });
type EsmMcpServer = import("@modelcontextprotocol/sdk/server/mcp.js", { with: { "resolution-mode": "import" } }).McpServer;
import { CallToolRequestSchema, ListToolsRequestSchema } from "@modelcontextprotocol/sdk/types.js";
import { mcpToolDefinition } from "../common/tool-presentation";
import { ClaudeCodeResponseStream } from "./claude-code-response-stream";

export const CLAUDE_CODE_MODEL = "gearbox/claude-code";

/** Locate the installed vendor CLI. Authentication remains entirely with Claude Code. */
export function findClaudeCode(): string | undefined {
  const candidates = [process.env.GEARBOX_CLAUDE_BIN, path.join(os.homedir(), ".local/bin/claude"), "/opt/homebrew/bin/claude"];
  const extensions = path.join(os.homedir(), ".vscode/extensions");
  if (fs.existsSync(extensions)) {
    for (const name of fs.readdirSync(extensions).filter(name => /^anthropic\.claude-code-\d/.test(name)).sort((a,b) => b.localeCompare(a, undefined, {numeric:true}))) {
      candidates.push(path.join(extensions, name, "resources/native-binary/claude"));
    }
  }
  return candidates.find(candidate => candidate && path.isAbsolute(candidate) && fs.existsSync(candidate));
}

export async function claudeCodeModels(): Promise<LanguageModel[]> {
  const executable = findClaudeCode();
  if (!executable) return [];
  let authenticated = false;
  try {
    const {stdout} = await promisify(execFile)(executable, ["auth", "status", "--json"], {timeout:5000, maxBuffer:65536});
    authenticated = JSON.parse(stdout).loggedIn === true;
  } catch { /* Availability is metadata; never expose CLI output or account details. */ }
  return [{
    id:CLAUDE_CODE_MODEL, name:"Claude Code · existing sign-in", vendor:"Anthropic", family:"Claude Code",
    status:{status:authenticated ? "ready" : "unavailable", message:authenticated ? undefined : "Sign in using your installed Claude Code."},
    request:async (request, cancellation) => ({stream:claudeCodeStream(executable, request, cancellation)}),
  }];
}

/** Only request-scoped Theia tools cross this boundary, including their confirmation handlers. */
export async function* claudeCodeStream(executable: string, request: UserRequest, cancellation?: CancellationToken): AsyncGenerator<LanguageModelStreamResponsePart> {
  if (request.messages.some(message => message.type === "image")) throw Error("Claude Code chat currently accepts text and Gearbox tool results only.");
  const abort = new AbortController();
  const cancel = cancellation?.onCancellationRequested(() => abort.abort());
  const timeout = setTimeout(() => abort.abort(), 180000);
  // Only the delegate's second argument is a live token. The similarly named
  // request field crossed JSON-RPC and can be a plain serialized object.
  if (cancellation?.isCancellationRequested) abort.abort();
  const server = new McpServer({name:"gearbox", version:"1.0.0"});
  server.server.registerCapabilities({tools:{}});
  const tools = request.tools ?? [];
  const response = new ClaudeCodeResponseStream();
  const onAbort = () => response.dispose();
  abort.signal.addEventListener("abort", onAbort, {once:true});
  server.server.setRequestHandler(ListToolsRequestSchema, async () => ({tools:tools.map(mcpToolDefinition)}));
  server.server.setRequestHandler(CallToolRequestSchema, async ({params}) => {
    const tool = tools.find(tool => tool.name === params.name);
    if (!tool || abort.signal.aborted) return {isError:true,content:[{type:"text",text:"Tool is unavailable for this request."}]};
    const id=randomUUID(), args=JSON.stringify(params.arguments ?? {});
    await response.publish({tool_calls:[{id,finished:false,function:{name:tool.name,arguments:args}}]});
    if (abort.signal.aborted) throw Error("Chat request was cancelled.");
    const result=await tool.handler(args,ToolInvocationContext.create(id,cancellation));
    await response.publish({tool_calls:[{id,finished:true,function:{name:tool.name,arguments:args},result}]});
    return {isError:hasToolCallError(result),content:[{type:"text",text:formatToolCallContentForModel(result)}]};
  });
  // No built-in filesystem/shell tools, local plugins, hooks, project settings or persisted SDK sessions.
  // The CLI reads/refreshes its own Keychain login; Gearbox never reads or stores the OAuth token.
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),"gearbox-chat-"));
  try {
    // The vendor ESM SDK uses import.meta.url for its resources. Keep it external
    // to Theia's CommonJS esbuild bundle rather than rewriting that URL away.
    const {query}=createRequire(__filename)("@anthropic-ai/claude-agent-sdk") as ClaudeAgentSdk;
    const system=request.messages.filter(message => message.actor === "system" && message.type === "text").map(message => message.type === "text" ? message.text : "").join("\n\n");
    const conversation=request.messages.filter(message => message.actor !== "system");
    const stream=query({prompt:JSON.stringify(conversation),options:{
      pathToClaudeCodeExecutable:executable, cwd:directory, abortController:abort,
      systemPrompt:system, model:"sonnet", settingSources:[], persistSession:false,
      tools:[], strictMcpConfig:true, mcpServers:{gearbox:{type:"sdk",name:"gearbox",instance:server as unknown as EsmMcpServer}},
      maxTurns:12,
      canUseTool:async (name,input) => tools.some(tool => "mcp__gearbox__"+tool.name === name)
        ? {behavior:"allow",updatedInput:input}
        : {behavior:"deny",message:"Only the request's Gearbox tools are available."},
    }});
    const produce = async () => {
      try {
        for await (const message of stream) {
          if (message.type === "assistant") {
            for (const block of message.message.content) if (block.type === "text") await response.publish({content:block.text});
          }
          if (message.type === "result" && message.subtype !== "success") throw Error("Claude Code could not finish this response. Check the existing Claude Code sign-in or usage limits.");
        }
        response.finish();
      } catch (error) {
        response.finish(error);
      }
    };
    // SDK tool callbacks run while its iterator is waiting for the tool result.
    // Consume their frames concurrently, rather than waiting for the next SDK message.
    void produce();
    yield* response.consume();
  } finally {
    abort.abort(); response.dispose(); clearTimeout(timeout); cancel?.dispose();
    abort.signal.removeEventListener("abort", onAbort);
    await server.close(); fs.rmSync(directory,{recursive:true,force:true});
  }
}
