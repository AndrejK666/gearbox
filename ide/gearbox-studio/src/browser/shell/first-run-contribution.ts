// First-run setup: where this machine's gears checkout and workspace are.
//
// The desktop app is installed with no paths in it -- a path baked in at build
// time is the build machine's, and on any other machine it names nothing. So
// the first start asks, once, and keeps the answers in Studio's settings
// (`gearbox.sourceRoots`, `gearbox.workspace`), where the settings editor shows
// them and **Gearbox: Configure Sources** asks again.
//
// It asks only when nothing answers already: a development checkout has its
// repository and `../gears-rust` (or `GEARBOX_ROOT`), and `workspaceRoots()`
// reports them, so running from source is unchanged.

import { AbstractDialog, DialogProps, FrontendApplicationContribution } from "@theia/core/lib/browser";
import { CommandContribution, CommandRegistry } from "@theia/core/lib/common/command";
import { MessageService } from "@theia/core/lib/common/message-service";
import { PreferenceScope, PreferenceService } from "@theia/core/lib/common/preferences";
import { inject, injectable } from "@theia/core/shared/inversify";
import { FileDialogService } from "@theia/filesystem/lib/browser";
import { FrontendApplicationStateService } from "@theia/core/lib/browser/frontend-application-state";

import {
  configuredDefaults,
  GEARBOX_SOURCE_REPOSITORY,
  GEARBOX_SOURCE_ROOTS,
  GEARBOX_WORKSPACE,
} from "../../common/gearbox-preferences";
import { GearboxService } from "../../common/protocol";
import { CatalogueStore } from "../catalogue-store";
import { ProductStore } from "../product-store";
import { DomainWorkspace } from "../theia/workspace/domain-workspace";

export const CONFIGURE_SOURCES = {
  id: "gearbox.sources.configure",
  label: "Gearbox: Configure Sources",
};

type SourceChoice = "choose" | "clone";

/** Two ways to a gears checkout, and a way out. */
class SourceChoiceDialog extends AbstractDialog<SourceChoice | undefined> {
  protected choice: SourceChoice | undefined;

  constructor(repository: string) {
    super({ title: "Set up Gearbox" } satisfies DialogProps);
    const text = document.createElement("div");
    text.className = "gbx-first-run";
    const lead = document.createElement("p");
    lead.textContent =
      "Gearbox reads its gear catalogue from a local checkout of a gears repository. " +
      "Choose the checkout you have, or clone one.";
    const repo = document.createElement("p");
    repo.textContent = `Repository: ${repository}`;
    repo.className = "gbx-first-run-repository";
    text.append(lead, repo);
    this.contentNode.appendChild(text);

    this.appendCloseButton("Later");
    const clone = this.createButton("Clone…");
    clone.classList.add("secondary");
    this.controlPanel.appendChild(clone);
    this.addEventListener(clone, "click", () => this.pick("clone"));
    const choose = this.appendAcceptButton("Choose checkout…");
    this.addEventListener(choose, "click", () => this.pick("choose"));
  }

  protected pick(choice: SourceChoice): void {
    this.choice = choice;
    void this.accept();
  }

  get value(): SourceChoice | undefined {
    return this.choice;
  }
}

@injectable()
export class FirstRunContribution implements FrontendApplicationContribution, CommandContribution {
  @inject(PreferenceService) protected readonly preferences!: PreferenceService;
  @inject(GearboxService) protected readonly service!: GearboxService;
  @inject(FileDialogService) protected readonly fileDialog!: FileDialogService;
  @inject(MessageService) protected readonly messages!: MessageService;
  @inject(CatalogueStore) protected readonly catalogue!: CatalogueStore;
  @inject(ProductStore) protected readonly products!: ProductStore;
  @inject(DomainWorkspace) protected readonly domainWorkspace!: DomainWorkspace;
  @inject(FrontendApplicationStateService) protected readonly state!: FrontendApplicationStateService;

  registerCommands(commands: CommandRegistry): void {
    commands.registerCommand(CONFIGURE_SOURCES, { execute: () => this.runSetup() });
  }

  // **After the shell is up, never inside `onStart`.** Theia awaits every
  // `onStart` before it replaces the loading screen, so a dialog awaited there
  // opened behind the spinner and held the whole application until answered.
  onStart(): void {
    void this.state.reachedState("ready").then(() => this.offerSetup());
  }

  protected async offerSetup(): Promise<void> {
    await this.preferences.ready;
    if (configuredDefaults(this.preferences) !== undefined) return;
    // A checkout to run from answers already; only an install has nothing.
    const found = await this.service.workspaceRoots().catch(() => []);
    if (found.length > 0) return;
    await this.runSetup();
  }

  /**
   * Ask for a gears checkout and a workspace, save them, and load from them.
   *
   * Not `configure`: that name is a `FrontendApplicationContribution` hook,
   * which Theia calls -- and awaits -- before the shell is shown.
   */
  async runSetup(): Promise<void> {
    await this.preferences.ready;
    const repository = this.preferences.get<string>(GEARBOX_SOURCE_REPOSITORY, "");
    const choice = await new SourceChoiceDialog(repository).open();
    if (choice === undefined) return;

    const root = choice === "choose" ? await this.chooseCheckout() : await this.clone(repository);
    if (root === undefined) return;

    const workspace = await this.pickFolder(
      "Choose the Gearbox workspace: products are listed from and created in its products/ folder",
    );
    if (workspace === undefined) return;

    await this.preferences.set(GEARBOX_SOURCE_ROOTS, [root], PreferenceScope.User);
    await this.preferences.set(GEARBOX_WORKSPACE, workspace, PreferenceScope.User);

    await this.catalogue.resetToBootSession();
    await this.products.discover();
    await this.domainWorkspace.apply();
  }

  protected chooseCheckout(): Promise<string | undefined> {
    return this.pickFolder("Choose the gears checkout");
  }

  protected async clone(repository: string): Promise<string | undefined> {
    const parent = await this.pickFolder("Clone into: the checkout becomes a new folder inside this one");
    if (parent === undefined) return undefined;
    const progress = await this.messages.showProgress({ text: `Cloning ${repository}…` });
    try {
      return await this.service.cloneSourceRepository(repository, parent);
    } catch (error) {
      void this.messages.error(error instanceof Error ? error.message : String(error));
      return undefined;
    } finally {
      progress.cancel();
    }
  }

  protected async pickFolder(title: string): Promise<string | undefined> {
    const uri = await this.fileDialog.showOpenDialog({
      title,
      openLabel: "Select Folder",
      canSelectFiles: false,
      canSelectFolders: true,
      canSelectMany: false,
    });
    return uri === undefined ? undefined : uri.path.fsPath();
  }
}
