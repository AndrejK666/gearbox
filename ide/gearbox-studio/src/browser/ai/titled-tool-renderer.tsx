import React from "@theia/core/shared/react";
import { injectable } from "@theia/core/shared/inversify";
import { ToolCallChatResponseContent } from "@theia/ai-chat/lib/common";
import type { ChatResponseContent } from "@theia/ai-chat/lib/common";
import { ToolCallPartRenderer } from "@theia/ai-chat-ui/lib/browser/chat-response-renderer/toolcall-part-renderer";
import type { ResponseNode } from "@theia/ai-chat-ui/lib/browser/chat-tree-view";
import { toolTitle, type TitledToolRequest } from "../../common/tool-presentation";
import { TitledToolCard } from "./titled-tool-card";

/** Display MCP titles without changing the response/tool identity or confirmation code. */
@injectable()
export class TitledToolRenderer extends ToolCallPartRenderer {
  override canHandle(response: ChatResponseContent): number {
    if (!ToolCallChatResponseContent.is(response) || !response.name) return -1;
    const tool = this.toolInvocationRegistry.getFunction(response.name) as TitledToolRequest | undefined;
    return tool && toolTitle(tool) !== tool.name ? 11 : -1;
  }

  override render(response: ToolCallChatResponseContent, parentNode: ResponseNode): React.ReactNode {
    const tool = this.toolInvocationRegistry.getFunction(response.name!) as TitledToolRequest;
    return <TitledToolCard title={toolTitle(tool)} response={response}
      native={super.render(response,parentNode)} renderResult={()=>this.renderResult(response)}
      cancelled={parentNode.response.isCanceled} />;
  }
  // Compact confirmation remains Theia's exact native implementation.
}
