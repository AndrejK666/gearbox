// Frontend wiring: the views, two stores, one proxied service.

import {
  OpenFileDialog,
  OpenFileDialogFactory,
  OpenFileDialogProps,
  SaveFileDialog,
  SaveFileDialogFactory,
  SaveFileDialogProps,
  createOpenFileDialogContainer,
  createSaveFileDialogContainer,
} from "@theia/filesystem/lib/browser";
import { NewFolderOpenFileDialog, NewFolderSaveFileDialog } from "./theia/filesystem/new-folder-file-dialog";
import { Agent, AIVariableContribution, bindToolProvider } from "@theia/ai-core";
import { ChatAgent, DefaultChatAgentId, FallbackChatAgentId } from "@theia/ai-chat";
import {
  FrontendApplicationContribution,
  LabelProviderContribution,
  bindViewContribution,
} from "@theia/core/lib/browser";
import { PerspectiveContribution } from "@theia/core/lib/browser/perspective-service";
import { WebSocketConnectionProvider } from "@theia/core/lib/browser/messaging";
import { CommandContribution } from "@theia/core/lib/common/command";
import { MenuContribution } from "@theia/core/lib/common/menu";
import { ContainerModule } from "@theia/core/shared/inversify";
import { DebugFrontendApplicationContribution } from "@theia/debug/lib/browser/debug-frontend-application-contribution";
import { ProblemContribution } from "@theia/markers/lib/browser/problem/problem-contribution";
import { OutlineViewContribution } from "@theia/outline-view/lib/browser/outline-view-contribution";
import { MonacoEditorProvider } from "@theia/monaco/lib/browser/monaco-editor-provider";
import { TerminalFrontendContribution } from "@theia/terminal/lib/browser/terminal-frontend-contribution";
import { TestViewContribution } from "@theia/test/lib/browser/view/test-view-contribution";
import { WorkspaceTrustService } from "@theia/workspace/lib/browser/workspace-trust-service";
import { LanguageGrammarDefinitionContribution } from "@theia/monaco/lib/browser/textmate/textmate-contribution";

import { GEARBOX_SERVICE_PATH, GearboxClient, GearboxService } from "../common/protocol";
import { CatalogueStore } from "./catalogue-store";
import { GenerateService } from "./generate/generate-service";
import { ChatResponsePartRenderer } from "@theia/ai-chat-ui/lib/browser/chat-response-part-renderer";
import { TitledToolRenderer } from "./ai/titled-tool-renderer";
import { ProductGearTool } from "./ai/product-tools";
import { GearboxChatAgent } from "./ai/gearbox-chat-agent";
import { GearboxContextContribution } from "./ai/gearbox-context";
import { GEARBOX_TOOLS } from "./ai/gearbox-tools";
import { AIChatContribution } from "@theia/ai-chat-ui/lib/browser/ai-chat-ui-contribution";
import { ChatInTheBottomPanel } from "./theia/ai-chat-ui/chat-in-the-bottom-panel";
import { PreferenceLayoutProvider } from "@theia/preferences/lib/browser/util/preference-layout";
import { PreferenceNodeRendererContribution } from "@theia/preferences/lib/browser/views/components/preference-node-renderer-creator";
import { bindGearboxPreferences } from "../common/gearbox-preferences";
import { GearboxPreferenceLayout } from "./theia/preferences/gearbox-preference-layout";
import { ApiKeyService } from "./settings/api-key-service";
import { SettingsContribution } from "./settings/settings-contribution";
import {
  SecretPreferenceRenderer,
  SecretPreferenceRendererContribution,
} from "./settings/secret-preference-renderer";
import {
  GearboxSelectionChip,
  GearboxVariableLabelProvider,
} from "./ai/gearbox-selection-chip";
import { ProductEditService } from "./product-edit-service";
import { ProductStore } from "./product-store";
import { ResolutionMarkers } from "./resolution-markers";
import { bindWidget } from "./contribution";
import { HiddenDebugView } from "./theia/debug/hidden-debug-view";
import { HiddenProblemsView } from "./theia/markers/hidden-problems-view";
import { HiddenOutlineView } from "./theia/outline-view/hidden-outline-view";
import { ShellPolicy } from "./theia/core/shell-policy";
import { ReadOnlyLockEditorProvider } from "./theia/monaco/read-only-lock-editor-provider";
import { DomainWorkspace } from "./theia/workspace/domain-workspace";
import { FirstRunContribution } from "./shell/first-run-contribution";
import { StudioWorkspaceTrustService } from "./theia/workspace/studio-workspace-trust-service";
import { EditorContribution } from "@theia/editor/lib/browser/editor-contribution";
import { MonacoStatusBarContribution } from "@theia/monaco/lib/browser/monaco-status-bar-contribution";

import {
  QuietEditorContribution,
  QuietMonacoStatusBarContribution,
} from "./theia/editor/quiet-status-bar";
import { HiddenTerminal } from "./theia/terminal/hidden-terminal";
import { HiddenTestView } from "./theia/test/hidden-test-view";
import { RevealService } from "./reveal-service";
import { CataloguePicker } from "./catalogue/catalogue-picker";
import { CatalogueWidget } from "./catalogue/catalogue-widget";
import { ConflictsWidget } from "./conflicts/conflicts-widget";
import { CatalogueDiagnosticsWidget } from "./diagnostics/catalogue-diagnostics-widget";
import { CreateProductWidget } from "./create/create-product-widget";
import { CreateGearWidget } from "./create/create-gear-widget";
import { PendingCreate } from "./create/pending-create";
import { PendingCreateGear } from "./create/pending-create-gear";
import {
  AddGearViewContribution,
  CatalogueViewContribution,
  ConflictsViewContribution,
  CatalogueDiagnosticsViewContribution,
  CreateGearViewContribution,
  CreateProductViewContribution,
  GearAuthorViewContribution,
  GenerateViewContribution,
  GraphViewContribution,
  InspectorViewContribution,
  LockViewContribution,
  ProductViewContribution,
  StartViewContribution,
} from "./view-contributions";
import { GraphWidget } from "./graph/graph-widget";
import { InspectorWidget } from "./inspector/inspector-widget";
import { GenerateWidget } from "./generate/generate-widget";
import { GenerateViewServices } from "./generate/generate-content";
import { LockWidget } from "./lock/lock-widget";
import { ProductWidget } from "./product/product-widget";
import { GearAuthorWidget } from "./gear/gear-author-widget";
import { StartWidget } from "./start/start-widget";
import { DescriptionMarkers } from "./gdl/description-markers";
import { GdlAssistContribution } from "./gdl/gdl-assist-contribution";
import { GdlLanguageContribution } from "./gdl/gdl-language-contribution";
import { FabricThemeContribution } from "./theme/fabric-theme-contribution";
import { GearboxPerspectives } from "./shell/gearbox-perspectives";
import { LayoutMigration } from "./shell/layout-migration";
import { GearSessionService } from "./shell/gear-session-service";
import { ProductSessionService } from "./shell/product-session-service";
import { EngineConnectionService } from "./shell/engine-connection-service";
import { SelectionService } from "./shell/selection-service";
import { SessionCommands } from "./shell/session-commands";
import { FocusModeService } from "./shell/focus-mode-service";
import { DescriptionWatchService } from "./shell/description-watch-service";
import { ScreenScopeService } from "./shell/screen-scope-service";
import { StudioContextService } from "./shell/studio-context-service";
import { ToolbarContribution } from "./shell/toolbar-contribution";
import { ToolbarWidget } from "./shell/toolbar-widget";

import "../../src/browser/style/index.css";
import "../../src/browser/theme/fabric-fonts.css";

export default new ContainerModule((bind, _unbind, _isBound, rebind) => {
  // Without this, `.gdl` opens as plaintext: nothing else in the app registers
  // the language with Monaco.
  bind(LanguageGrammarDefinitionContribution)
    .to(GdlLanguageContribution)
    .inSingletonScope();

  // Why: Gearbox Studio is a Constructor Fabric product. The color themes, fonts
  // and favicon come from constructorfabric.org tokens, registered natively
  // (not as a VS Code theme extension) so they ship with the extension and do
  // not depend on `download:plugins`. Two themes: the light one is the default,
  // the dark one is the same palette read the other way round.
  bind(FabricThemeContribution).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(FabricThemeContribution);

  // The first rebind in this application, so the discipline ADR 0011 asks for
  // starts here: every rebind says why in place. Arduino IDE's frontend module is
  // a thousand lines of these and is readable only because each one is justified
  // where it sits.
  //
  // Why: `product.lock` must open read-only (`cpt-gearbox-fr-lock-read-only`),
  // and `MonacoEditorProvider.createMonacoEditorOptions` is the only hook that
  // can say so for one filename rather than for a whole URI scheme. The subclass
  // explains the two alternatives it rejected.
  rebind(MonacoEditorProvider).to(ReadOnlyLockEditorProvider).inSingletonScope();

  // Why: `@theia/debug` and `@theia/test` arrive with `@theia/plugin-ext`, which
  // needs them for the VS Code debug and testing APIs, and both open a panel in
  // the left bar on first run. A product resolves; it does not execute, and there
  // is no test explorer for gears. `initializeLayout(): NOOP` keeps the package,
  // the command and the keybinding and changes only the default layout, so a
  // saved layout is still respected and the view is one command away.
  rebind(DebugFrontendApplicationContribution).to(HiddenDebugView).inSingletonScope();
  rebind(TestViewContribution).to(HiddenTestView).inSingletonScope();

  // Why: Theia's file dialog cannot make a folder, and every folder Studio asks
  // for -- a new product's, a new gear's, Generate's output -- is often one that
  // does not exist yet. The factories are rebound, not the dialogs, because each
  // dialog is built in a child container of its own; see
  // `theia/filesystem/new-folder-file-dialog.ts`.
  rebind(OpenFileDialogFactory).toFactory((ctx) => (props: OpenFileDialogProps) => {
    const child = createOpenFileDialogContainer(ctx.container, props);
    child.rebind(OpenFileDialog).to(NewFolderOpenFileDialog);
    return child.get(OpenFileDialog);
  });
  rebind(SaveFileDialogFactory).toFactory((ctx) => (props: SaveFileDialogProps) => {
    const child = createSaveFileDialogContainer(ctx.container, props);
    child.rebind(SaveFileDialog).to(NewFolderSaveFileDialog);
    return child.get(SaveFileDialog);
  });

  // Why: the same mechanism for two panels this application does choose the
  // package of and does not choose the panel of. Markers are load-bearing --
  // `ResolutionMarkers` writes every resolve diagnostic into `ProblemManager` --
  // but Conflicts is the domain screen for that array, and an outline of a
  // `product.gdl` is a shorter version of the Product tree. Both were opening
  // themselves on a first run and taking slots beside the Start screen.
  rebind(ProblemContribution).to(HiddenProblemsView).inSingletonScope();
  rebind(OutlineViewContribution).to(HiddenOutlineView).inSingletonScope();

  // Why: the same trade, one package further. A shell is a tool this application
  // offers, not one of the two things it is about, so it appears when asked for
  // rather than at startup. Closing it afterwards was tried and broke creation --
  // see `HiddenTerminal`.
  rebind(TerminalFrontendContribution).to(HiddenTerminal).inSingletonScope();

  // Why: opening a multi-root workspace makes Theia generate
  // `~/.theia/workspaces/Untitled-NN.theia-workspace`, and its own trust check
  // then demands that *that* file be inside a trusted folder -- so the
  // application starts behind a modal dialog that no trusted-folders setting can
  // dismiss. The subclass drops Theia's own bookkeeping file from the set and
  // leaves the folder rules alone.
  rebind(WorkspaceTrustService).to(StudioWorkspaceTrustService).inSingletonScope();

  // The editor's status bar, cut to the language indicator. `Ln`, `Col`, `UTF-8`,
  // `LF` and `Spaces` are the controls of a tool for fixing files; this editor
  // exists to read what a resolution points at. Both classes are bound
  // `toSelf().inSingletonScope()` upstream and reached through `toService`, so
  // rebinding the class is enough to reach every interface they are bound as.
  rebind(EditorContribution).to(QuietEditorContribution).inSingletonScope();
  rebind(MonacoStatusBarContribution).to(QuietMonacoStatusBarContribution).inSingletonScope();

  // Bound before both stores because both inject it. One selection, not one per
  // store: a gear chosen in the catalogue and the same gear chosen in the product
  // tree used to be two selections, which is why the panel answering "why" was
  // empty in the ordinary case.
  bind(SelectionService).toSelf().inSingletonScope();
  bind(EngineConnectionService).toSelf().inSingletonScope();

  bind(CatalogueStore).toSelf().inSingletonScope();
  // Its own store, not a slice of the catalogue's: the two objects of work do not
  // subordinate one another, and their lifecycles differ -- the catalogue loads
  // once and streams, a product is re-resolved on every profile switch.
  bind(ProductStore).toSelf().inSingletonScope();
  // The policy in front of a description edit: a product must be open, its file
  // must have no unsaved changes, the engine must agree, and the person must
  // confirm. A service rather than a widget handler, so the next widget that
  // wants to edit does not reimplement the four checks.
  bind(ProductEditService).toSelf().inSingletonScope();
  // For `onWillStop`: a reload with an unapplied draft asks first.
  bind(FrontendApplicationContribution).toService(ProductEditService);
  bind(PendingCreate).toSelf().inSingletonScope();
  bind(PendingCreateGear).toSelf().inSingletonScope();
  // The policy in front of Apply: generate advertised, writes declared, no
  // resolution errors, no conflicts. Named in the UI when they fail.
  bind(GenerateService).toSelf().inSingletonScope();
  bind(RevealService).toSelf().inSingletonScope();
  bind(GearboxClient).toService(CatalogueStore);

  // `@theia/markers` has been a declared dependency and unused since the shell
  // was built. This is what finally uses it: every resolution replaces the
  // Gearbox markers in the Problems view, which is where
  // `cpt-gearbox-fr-editor-diagnostics` says a resolution diagnostic has to end
  // up. Bound as an application contribution because it needs constructing --
  // nothing injects it -- and `onStart` is where it subscribes.
  bind(ResolutionMarkers).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(ResolutionMarkers);

  // The requirement's other half: the description being typed, marked from the
  // engine's `textDocument/*` surface. A singleton because the forwarder above
  // reaches it by `container.get`, and a contribution because `onStart` is where
  // it starts watching for editors -- the ones a reload restores arrive after
  // it, through `onDidCreate`, since the layout is restored once the
  // contributions have started.
  bind(DescriptionMarkers).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(DescriptionMarkers);

  // Completion and hover for `.gdl`, answered by the engine. A contribution
  // because the providers are registered in `onStart`; nothing injects it.
  bind(GdlAssistContribution).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(GdlAssistContribution);

  // Opens the directories Studio already knows it works on. Without a workspace
  // the Explorer is empty, a generated `product.lock` cannot be opened at all,
  // and the VS Code git extension finds no repositories -- three failures that
  // none of them look like "no workspace".
  bind(DomainWorkspace).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(DomainWorkspace);
  bind(FirstRunContribution).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(FirstRunContribution);
  bind(CommandContribution).toService(FirstRunContribution);

  // Why: what Studio is working on drives the shell, and it has to be derived
  // from what is actually open rather than from a restored layout -- a
  // perspective can come back from a saved snapshot with no product behind it,
  // and a Product menu keyed off that would offer actions with nothing to act
  // on. `StudioContextService` owns the context; perspectives only arrange
  // panels. ADR-0011's own revisit clause asked for this collapse.
  // Why: opening a product decides where the engine looks and where it may
  // write. The backend used to fix both, which made "open a product" mean "open
  // one of the products in this checkout" -- readable anywhere, editable nowhere
  // else, because both write gates measure from the declared workspace.
  bind(ProductSessionService).toSelf().inSingletonScope();
  bind(GearSessionService).toSelf().inSingletonScope();

  // Why: `File` is where a person looks for "what am I working on", so Open,
  // New and Close live there rather than under `Gearbox`, which holds the verbs
  // that act on what is already open.
  bind(SessionCommands).toSelf().inSingletonScope();
  bind(CommandContribution).toService(SessionCommands);
  bind(MenuContribution).toService(SessionCommands);

  // Why: suppressing a menu and a command does not close a panel already in a
  // saved layout. `Type Hierarchy` and a `zsh` terminal kept returning at the
  // bottom after the shell stopped offering either, because the restorer restores
  // what was there before the rules changed. Once, not every start: a terminal
  // someone opened on purpose must survive a reload.
  bind(LayoutMigration).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(LayoutMigration);

  bind(StudioContextService).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(StudioContextService);

  // Nothing listened to the filesystem before this, so every panel showed the
  // resolution from before the person's last save. See the service for why the
  // catalogue's own descriptions are deliberately left out.
  bind(DescriptionWatchService).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(DescriptionWatchService);

  bind(GearboxPerspectives).toSelf().inSingletonScope();
  bind(PerspectiveContribution).toService(GearboxPerspectives);

  // Why: the context says what is being worked on; this says which screens may
  // therefore exist, and closes the ones that may not. Bound after
  // `StudioContextService` because it waits on that service's `settled()` --
  // closing a widget while a perspective switch is applying its layout is the
  // race that poisoned the Home snapshot three times. It is also the **only**
  // imperative activation in the application: `StartViewContribution` and the
  // perspectives' `onActivate` both used to open their own view, before the
  // layout had settled, which is what this replaces.
  // Why a service of its own: `ScreenScopeService` reaches the view
  // contributions to put a context's screen in front, and the view contributions
  // reach this to fold the panels before opening a wizard. Importing both ways
  // closed a module cycle that stopped the frontend booting outright, so the
  // piece both sides need sits below both of them.
  bind(FocusModeService).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(FocusModeService);

  bind(ScreenScopeService).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(ScreenScopeService);

  // Why: the switch and the two domain actions belong on the shell, not on a
  // view. `@theia/toolbar` is a user-configurable bar with its own JSON, which
  // is the opposite of the narrowing ADR-0011 is for. A widget in `top` is
  // what `BrowserMenuBarContribution` already does for the menu.
  bind(ToolbarWidget).toSelf().inSingletonScope();
  bind(ToolbarContribution).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(ToolbarContribution);

  bind(GearboxService)
    .toDynamicValue(({ container }) => {
      const provider = container.get(WebSocketConnectionProvider);
      // The proxy is two-way: the backend calls the client back with each
      // projection, which is what makes the tree fill in rather than reload.
      //
      // The client is reached through a forwarder rather than resolved here,
      // because the store injects the service and the service needs a client --
      // a cycle inversify reports as "circular dependency in one of the
      // toDynamicValue bindings". One of the two edges has to be deferred, and
      // this is the safe one: a notification cannot arrive before the store has
      // asked for the service and started a load.
      const forwarder: GearboxClient = {
        onCatalogueChanged: (event) => container.get(CatalogueStore).onCatalogueChanged(event),
        onCatalogueDiagnostics: (event) =>
          container.get(CatalogueStore).onCatalogueDiagnostics(event),
        onProgress: (event) => container.get(CatalogueStore).onProgress(event),
        onLog: (message) => container.get(CatalogueStore).onLog(message),
        onEngineExit: (reason) => container.get(CatalogueStore).onEngineExit(reason),
        // The one callback that is not the catalogue's. It is about a single
        // open editor, so it goes to the contribution that owns that file's
        // markers -- see `DescriptionMarkers`.
        onDocumentDiagnostics: (params) =>
          container.get(DescriptionMarkers).onDocumentDiagnostics(params),
      };
      return provider.createProxy<GearboxService>(GEARBOX_SERVICE_PATH, forwarder);
    })
    .inSingletonScope();

  // Bound before the view contributions so it runs last among menu
  // contributions: pruning only works on a tree the contributors have already
  // filled. It also re-prunes on `onDidChange`, because plugin contributions
  // arrive after startup and a one-shot prune is correct only until the first
  // extension activates.
  bind(ShellPolicy).toSelf().inSingletonScope();
  bind(MenuContribution).toService(ShellPolicy);

  // The catalogue, called rather than browsed: `Find Gear…` selects into the
  // Inspector, which is how the catalogue serves the product context now that its
  // panel is collapsed there. Adding a gear opens the Add Gear configurator.
  bind(CataloguePicker).toSelf().inSingletonScope();
  bind(CommandContribution).toService(CataloguePicker);
  bind(MenuContribution).toService(CataloguePicker);

  bindWidget(bind, CatalogueWidget);
  bindWidget(bind, GraphWidget);
  bindWidget(bind, ProductWidget);
  bindWidget(bind, InspectorWidget);
  bindWidget(bind, ConflictsWidget);
  bindWidget(bind, CatalogueDiagnosticsWidget);
  bindWidget(bind, StartWidget);
  bindWidget(bind, CreateProductWidget);
  bindWidget(bind, CreateGearWidget);
  bindWidget(bind, GearAuthorWidget);
  bindWidget(bind, LockWidget);
  bind(GenerateViewServices).toSelf().inSingletonScope();
  bindWidget(bind, GenerateWidget);

  // `bindViewContribution` already binds `CommandContribution`,
  // `KeybindingContribution` *and* `MenuContribution` -- see
  // `@theia/core/lib/browser/shell/view-contribution.js`. Binding any of them
  // again runs the contribution twice, and `MenuModelRegistry.registerMenuAction`
  // does not deduplicate: it makes a node and appends it. That is what put every
  // Gearbox entry, and every toggle under View > Views, in the menu twice.
  //
  // The command side failed more quietly. `CommandRegistry.registerCommand` on an
  // existing id logs "A command ... is already registered." and returns a no-op
  // disposable, so the *second* handler is discarded. Behaviour survived only
  // because the first registration wins.
  //
  // What still needs binding by hand is `FrontendApplicationContribution`, which
  // `bindViewContribution` does not touch -- and only for the views that
  // implement something from it.
  bindViewContribution(bind, CatalogueViewContribution);
  bind(FrontendApplicationContribution).toService(CatalogueViewContribution);

  bindViewContribution(bind, InspectorViewContribution);
  bind(FrontendApplicationContribution).toService(InspectorViewContribution);

  bindViewContribution(bind, GraphViewContribution);

  // `FrontendApplicationContribution` here too, since 2026-09-07: the Product
  // view opens itself from the session's own signal. Relying on the Product
  // perspective's `onActivate` meant relying on a switch that is skipped when the
  // shell already believes that perspective is active -- see the view's header.
  bindViewContribution(bind, ProductViewContribution);
  bind(FrontendApplicationContribution).toService(ProductViewContribution);
  // No `FrontendApplicationContribution`: Start no longer opens itself. It was
  // subscribing to the context and opening on every change to Home, which fired
  // before the perspective switch had applied its layout -- `ScreenScopeService`
  // owns that now, and one owner is the point.
  bindViewContribution(bind, StartViewContribution);
  bindViewContribution(bind, CreateProductViewContribution);
  bindViewContribution(bind, CreateGearViewContribution);
  bindViewContribution(bind, GearAuthorViewContribution);
  bindViewContribution(bind, AddGearViewContribution);
  bindViewContribution(bind, ConflictsViewContribution);
  bindViewContribution(bind, CatalogueDiagnosticsViewContribution);
  bindViewContribution(bind, LockViewContribution);
  bindViewContribution(bind, GenerateViewContribution);

  // What the chat may do to a product. `bindToolProvider` is Theia AI's own
  // helper, and the tool it registers holds no policy: it calls the same
  // `ProductEditService.toggle` the catalogue's control calls, so the preview
  // and the refusal on unsaved changes are the ones already in place.
  // Why: the chat asked for the right panel, where the Inspector already lives
  // and takes the panel on every selection -- so clicking a gear hid the chat.
  // The override moves it to the bottom, the one area nothing opens
  // automatically.
  // **Studio's own settings, in Theia's own editor.** The Anthropic key already
  // exists as a preference and is unreachable: `@theia/ai-core` stamps every
  // `ai-features.*` key `hidden: true`, because upstream expects the AI
  // Configuration view -- which ships in the `@theia/ai-ide` this application
  // does not install -- to replace the settings editor for them. Declaring
  // `gearbox.*` sidesteps that filter, which matches by literal prefix, and
  // costs no widget: the editor renders whatever a schema declares.
  bindGearboxPreferences(bind);
  bind(SettingsContribution).toSelf().inSingletonScope();
  bind(CommandContribution).toService(SettingsContribution);
  bind(MenuContribution).toService(SettingsContribution);
  bind(ApiKeyService).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(ApiKeyService);

  // Why: without a category of its own, `gearbox.*` files under *Extensions* --
  // `PreferenceTreeGenerator` falls back there for any namespace the layout does
  // not know. There is no contribution point for sections, so this is ADR-0011's
  // own mechanism, `rebind(TheiaX).to(MyX)`.
  rebind(PreferenceLayoutProvider).to(GearboxPreferenceLayout).inSingletonScope();

  // Why: Theia's settings editor renders every string preference with
  // `input.type = "text"` and has no schema flag for a secret. The renderer
  // registry does have a score-based override, which is what this uses.
  bind(SecretPreferenceRenderer).toSelf();
  bind(PreferenceNodeRendererContribution)
    .to(SecretPreferenceRendererContribution)
    .inSingletonScope();

  rebind(AIChatContribution).to(ChatInTheBottomPanel).inSingletonScope();

  bindToolProvider(ProductGearTool, bind);
  bind(ChatResponsePartRenderer).to(TitledToolRenderer).inSingletonScope();

  // **The agent, without which none of the above was reachable.** `@theia/ai-chat`
  // registers no user-facing agent -- Theia's own live in `@theia/ai-ide`, which
  // this application does not install -- so until now every chat request came
  // back as "No agent was found to handle this request", and the tool above had
  // never been offered to a model: a tool reaches one only through an agent's
  // `functions`.
  bind(GearboxChatAgent).toSelf().inSingletonScope();
  bind(Agent).toService(GearboxChatAgent);
  bind(ChatAgent).toService(GearboxChatAgent);

  // **And it is the default, which registering does not make it.** A request
  // that names no agent goes to `getDefaultAgent`, then `getFallbackAgent`, and
  // both read bound ids rather than "the only agent there is" -- so with the
  // agent registered but neither id bound, typing a plain sentence still failed
  // with "No agent was found to handle this request", while `@Gearbox`
  // succeeded. Both are bound because they answer different questions: the
  // default is for a fresh session, the fallback for a request whose named
  // agent has gone (an id from a restored session, say).
  bind(DefaultChatAgentId).toConstantValue({ id: GearboxChatAgent.ID });
  bind(FallbackChatAgentId).toConstantValue({ id: GearboxChatAgent.ID });

  // The context the chat gets without being told: selection, product,
  // diagnostics, topology and the selected gear's effective configuration. Read
  // from the same stores the panels render from, never from the panels.
  bind(GearboxContextContribution).toSelf().inSingletonScope();
  bind(AIVariableContribution).toService(GearboxContextContribution);

  // Theia renders a context chip's title through the label provider, and ships
  // no contribution that handles a bare variable request -- so without this the
  // chips above appear as empty pills.
  bind(GearboxVariableLabelProvider).toSelf().inSingletonScope();
  bind(LabelProviderContribution).toService(GearboxVariableLabelProvider);

  // Keeps one selection chip attached to the chat input and current. It never
  // creates the chat widget: opening a panel as a side effect of clicking a
  // catalogue row is not something anybody asked for.
  bind(GearboxSelectionChip).toSelf().inSingletonScope();
  bind(FrontendApplicationContribution).toService(GearboxSelectionChip);

  for (const tool of GEARBOX_TOOLS) bindToolProvider(tool, bind);
});
