import type { ToolRequest } from "@theia/ai-core";

/** MCP display metadata; names and IDs remain the execution/permission identity. */
export type TitledToolRequest = ToolRequest & {
  title?: string;
  annotations?: { title?: string };
};

export function toolTitle(tool: Pick<TitledToolRequest, "name" | "title" | "annotations">): string {
  return tool.title?.trim() || tool.annotations?.title?.trim() || tool.name;
}

/** Preserve standard MCP title metadata across the SDK boundary. */
export function mcpToolDefinition(tool: TitledToolRequest) {
  return {name:tool.name, title:toolTitle(tool), description:tool.description,
    inputSchema:{...tool.parameters,type:"object" as const}};
}
