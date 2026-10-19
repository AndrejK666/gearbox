// The agent the chat has been missing.
//
// **Until this existed, the chat could not answer at all.** `@theia/ai-chat`
// registers no user-facing agent -- Theia's `Universal` and `Coder` live in
// `@theia/ai-ide`, which this application deliberately does not install (it
// drags `puppeteer-core` and the AI terminal surface ADR-0011 withdrew). Nothing
// else bound a `ChatAgent`, so every request reached `ChatService` and came back
// as "No agent was found to handle this request".
//
// That is also why `gearbox_toggle_gear` had never run: a tool is offered to the
// model only if an agent names it in `functions`, and there was no agent to name
// it.
//
// **`AbstractStreamParsingChatAgent` rather than a text-parsing base**, because
// the answers here are prose with tool calls threaded through them, not a
// structured document to be parsed back out.

import { inject, injectable } from "@theia/core/shared/inversify";
import { AbstractStreamParsingChatAgent, ChatAgentLocation } from "@theia/ai-chat";
import { MarkdownChatResponseContentImpl } from "@theia/ai-chat/lib/common/chat-model";
import type { MutableChatRequestModel } from "@theia/ai-chat/lib/common/chat-model";
import { FrontendLanguageModelRegistry } from "@theia/ai-core";
import { LanguageModelAliasRegistry } from "@theia/ai-core";
import type { LanguageModelRequirement } from "@theia/ai-core";

import { GearboxService } from "../../common/protocol";

import { explainConnectivity, looksLikeTransportFailure } from "./connectivity-report";

import { SHOW_SETTINGS } from "../shell/session-command-ids";

import { ProductGearTool } from "./product-tools";
import { GEARBOX_TOOLS } from "./gearbox-tools";
import { GEARBOX_VARIABLES } from "./gearbox-context";
import { GEARBOX_SYSTEM_PROMPT } from "./gearbox-prompt";

@injectable()
export class GearboxChatAgent extends AbstractStreamParsingChatAgent {
  /**
   * The agent's id, as a constant because three bindings name it: the agent
   * itself, and the default and fallback ids the chat resolves a request
   * through when the operator names no agent.
   */
  static readonly ID = "Gearbox";

  readonly id = GearboxChatAgent.ID;
  readonly name = GearboxChatAgent.ID;

  override readonly description =
    "Answers about the open Gearbox product from the resolver's own output: the " +
    "selection, the resolved topology, and the diagnostics. Previews changes; never " +
    "writes without asking.";

  override readonly iconClass = "codicon codicon-circuit-board";

  override readonly locations = [ChatAgentLocation.Panel];

  override readonly tags = ["Gearbox"];

  readonly languageModelRequirements: LanguageModelRequirement[] = [
    {
      purpose: "chat",
      identifier: "gearbox/chat",
    },
  ];

  protected readonly defaultLanguageModelPurpose = "chat";

  /**
   * The tools this agent may call.
   *
   * Named explicitly rather than "everything registered": a tool that appears in
   * the model's list without anybody deciding it should is how a chat acquires a
   * capability nobody reviewed.
   */
  override readonly functions = [
    ...GEARBOX_TOOLS.map((tool) => tool.ID),
    ProductGearTool.ID,
  ];

  override readonly variables = GEARBOX_VARIABLES.map((variable) => variable.name);

  override readonly prompts = [GEARBOX_SYSTEM_PROMPT];

  protected override systemPromptId: string | undefined = GEARBOX_SYSTEM_PROMPT.id;

  // The *frontend* registry, which is the one that resolves an alias like
  // `default/universal` to a concrete model and knows whether it is ready.
  // `LanguageModelRegistry` alone has neither.
  @inject(FrontendLanguageModelRegistry)
  protected readonly models!: FrontendLanguageModelRegistry;

  @inject(LanguageModelAliasRegistry)
  protected readonly modelAliases!: LanguageModelAliasRegistry;

  // Studio's own backend, asked what it can reach when a request dies in the
  // transport. The chat's provider cannot answer that: it is the thing failing.
  @inject(GearboxService)
  protected readonly service!: GearboxService;

  /**
   * Refuse in Gearbox's words when there is no model, rather than in Theia's.
   *
   * Without this the answer is *"Couldn't find a ready language model for agent
   * Gearbox. Please check your setup!"* -- true, and useless to an integrator who
   * has no reason to know that a language model is configured by an API key, let
   * alone where. It names neither the key nor the place to put it, and it arrives
   * as an error balloon, which is the shape reserved for things that went wrong
   * rather than things that were never set up.
   *
   * Checked before the request rather than caught after it: the throw happens
   * inside `getLanguageModelForRequest`, several frames down, and by then the
   * only thing left to do is replace one error with another.
   */
  override async invoke(request: MutableChatRequestModel): Promise<void> {
    await this.modelAliases.ready;
    if (!this.modelAliases.getAliases().some(alias => alias.id === "gearbox/chat")) {
      this.modelAliases.addAlias({id:"gearbox/chat",description:"Gearbox chat",defaultModelIds:["gearbox/claude-code",...(this.modelAliases.resolveAlias("default/universal") ?? [])]});
    }
    const selector = this.getLanguageModelSelector(this.defaultLanguageModelPurpose);
    const identifier = selector?.identifier;
    if (identifier !== undefined) {
      const ready = await this.models.getReadyLanguageModel(identifier);
      if (ready === undefined) {
        request.response.response.addContent(
          new MarkdownChatResponseContentImpl(
            `I have no language model to answer with, so nothing I said would be grounded.\n\n` +
              `Sign in with your installed Claude Code, set an Anthropic API key in **${SHOW_SETTINGS.label}**, or start Studio's ` +
              `backend with \`ANTHROPIC_API_KEY\` in its environment. The key is used only ` +
              `for this chat -- resolving, generating and every diagnostic work without it.`,
          ),
        );
        // Complete rather than error: nothing failed, something is unset, and the
        // two deserve different shapes on screen.
        request.response.complete();
        return;
      }
    }
    return super.invoke(request);
  }

  /**
   * Replace `Connection error.` with the reason for it.
   *
   * **Why `handleError` and not a `try/catch` around `super.invoke`.**
   * `AbstractChatAgent.invoke` catches its own failures and routes them here
   * (`chat-agents.js:157-159`), so nothing is thrown out of the call to wrap.
   *
   * **Why an extra round trip instead of reading the error.** A transport
   * failure reaches the frontend having lost everything that identified it: the
   * Anthropic SDK reports a rejected `fetch` as `APIConnectionError`, whose
   * default message is the bare string `Connection error.`, and the `cause`
   * chain holding the real code does not survive Theia's RPC error
   * serialization. The backend still has both, so it is asked.
   */
  protected override handleError(request: MutableChatRequestModel, error: Error): void {
    if (!looksLikeTransportFailure(error)) {
      super.handleError(request, error);
      return;
    }
    // Deliberately not awaited: `handleError` is synchronous by contract. The
    // response is not finalized until `super.handleError` calls `.error()`, so
    // appending before that still lands in this answer.
    void this.explainTransportFailure(request, error);
  }

  protected async explainTransportFailure(
    request: MutableChatRequestModel,
    error: Error,
  ): Promise<void> {
    try {
      const check = await this.service.checkAiConnectivity();
      request.response.response.addContent(
        new MarkdownChatResponseContentImpl(explainConnectivity(check)),
      );
    } catch {
      // The diagnosis is a courtesy; failing to obtain one must not replace the
      // original error with a worse one.
    }
    super.handleError(request, error);
  }
}
