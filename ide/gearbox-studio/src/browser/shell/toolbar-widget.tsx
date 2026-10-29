// The shell header: what you are working on, and the two verbs that act on it.
//
// This was a perspective switch -- `Catalogue` and `Product` buttons beside the
// menu bar -- and it was the wrong control. The words duplicated two entries in
// the Gearbox menu while meaning something else entirely: switch a whole layout,
// not toggle a view. Navigation does not belong in a header.
//
// What does belong is **current state you need at a glance and can change from
// anywhere**, which is the distinction Arduino IDE draws by putting the selected
// board in both its toolbar and its Tools menu. Here that is the product, its
// profile, and whether it resolved. The profile selector stays in the Product
// panel's own header, where plan §9 put it deliberately -- it has to be reachable
// *while* a resolution is in flight, and a header row rendered from the resolved
// product cannot be. So this shows the profile; it does not offer to change it.
//
// Actions come from `CommandRegistry` -- label, icon, enablement -- so the header
// cannot name a command that does not exist and cannot drift from the menu. The
// only thing duplicated is the list of ids, and that list is what the regression
// test reads.

import { ReactWidget } from "@theia/core/lib/browser";
import { CommandRegistry } from "@theia/core/lib/common";
import { inject, injectable, postConstruct } from "@theia/core/shared/inversify";
import React from "@theia/core/shared/react";

import { errorsIn } from "../diagnostics/diagnostics-list";
import { ProductEditService } from "../product-edit-service";
import { ProductStore } from "../product-store";
import { EngineConnectionService } from "./engine-connection-service";
import { CLOSE_GEAR } from "./session-command-ids";
import { StudioContextService } from "./studio-context-service";
import { SelectionService } from "./selection-service";
import { GearSessionService } from "./gear-session-service";

/**
 * Command ids the header may invoke. Quoted here so the regression test can read
 * them from the source rather than from a running registry.
 */
export const TOOLBAR_COMMAND_IDS = [
  "gearbox.gear.close",
] as const;

/**
 * Which actions each context offers.
 *
 * `Reload Catalogue` used to be here and is not any more: it is catalogue
 * maintenance, not something a person does *to a product*, and a header is the
 * two or three verbs that act on the subject beside it. It stays in the menu and
 * the palette.
 *
 * `home` offers nothing. With no product there is no subject to act on, and a
 * header full of buttons for something that is not open is the shape of interface
 * this rework exists to remove. The Start screen is what belongs there.
 */
const ACTIONS: Readonly<Record<string, readonly string[]>> = {
  home: [],
  // Product actions live in the workspace, including on native Electron menus.
  product: [],
  gear: [CLOSE_GEAR.id],
};

@injectable()
export class ToolbarWidget extends ReactWidget {
  static readonly ID = "gearbox.toolbar";

  @inject(StudioContextService) protected readonly context!: StudioContextService;
  @inject(CommandRegistry) protected readonly commands!: CommandRegistry;
  @inject(ProductStore) protected readonly products!: ProductStore;
  @inject(GearSessionService) protected readonly gears!: GearSessionService;
  @inject(ProductEditService) protected readonly edits!: ProductEditService;
  @inject(EngineConnectionService) protected readonly engine!: EngineConnectionService;
  @inject(SelectionService) protected readonly selection!: SelectionService;

  @postConstruct()
  protected init(): void {
    this.id = ToolbarWidget.ID;
    this.title.closable = false;
    this.addClass("gbx-widget-toolbar");
    this.toDispose.push(this.context.onDidChange(() => this.update()));
    this.toDispose.push(this.commands.onCommandsChanged(() => this.update()));
    // Resolve's enablement depends on a product being open, and its handler does
    // not fire `onDidChangeEnabled`. The store is the fact the handler reads, so a
    // change here is a change of the button -- and of the status text beside it.
    this.toDispose.push(this.products.onChanged(() => this.update()));
    this.toDispose.push(this.gears.onDidChange(() => this.update()));
    this.toDispose.push(this.edits.onDraftChanged(() => this.update()));
    // Same for New Product / Resolve / Generate vs the engine: Theia does not
    // re-query `isEnabled` when `EngineConnectionService` flips.
    this.toDispose.push(this.engine.onDidChange(() => this.update()));
    // Not for anything this bar draws: see `data-has-selection` in `render`.
    this.toDispose.push(this.selection.onDidChange(() => this.update()));
    this.update();
  }

  protected render(): React.ReactNode {
    const context = this.context.current;
    const actions = ACTIONS[context.kind] ?? [];
    return (
      <div
        className={`gbx-toolbar ${context.kind === "product" ? "gbx-toolbar-product-context" : ""}`}
        data-context={context.kind}
        // **Whether anything is selected, published where it does not move.**
        // Nothing on this bar draws from it; it is here because the question is
        // asked from outside and every other answer to it is a widget that may
        // not be attached. The suite used to ask the Inspector -- which is a
        // panel a person can close, and one that is about to stop opening itself
        // -- so "is anything selected" quietly became "is the Inspector on
        // screen", and the helper that asked went on to click a catalogue row
        // and destroy the selection its caller had just made.
        //
        // `gearbox.hasSelection` is the same fact as a context key
        // (`studio-context-service.ts`), but a context key is not in the DOM.
        data-has-selection={this.selection.current !== undefined ? "true" : "false"}
      >
        <div className="gbx-toolbar-subject">
          {context.kind === "product"
            ? this.renderProduct()
            : context.kind === "gear"
              ? this.renderGear()
              : this.renderHome()}
        </div>
        <div className="gbx-toolbar-actions">
          {actions.map((id) => this.actionButton(id))}
        </div>
      </div>
    );
  }

  /**
   * No product: say so, rather than showing an empty bar.
   *
   * An empty header reads as a rendering failure. Naming the state is what makes
   * the Start screen's absence a decision rather than a gap.
   */
  protected renderHome(): React.ReactNode {
    return <span className="gbx-toolbar-empty">No product open</span>;
  }

  protected renderGear(): React.ReactNode {
    const gear = this.gears.current;
    return (
      <span className="gbx-toolbar-name" data-gear={gear?.label ?? ""}>
        {gear?.label ?? "Gear"}
      </span>
    );
  }

  protected renderProduct(): React.ReactNode {
    const state = this.products.current;
    const product = state.resolution?.product?.product;
    // **The product's name, not the reference it was opened by.** `ProductRef.label`
    // is documented as a repository-relative *path* ("which is what a person
    // recognises" -- `protocol.ts`), and that is what discovery puts there. So
    // this header read `UX Audit Clone` right after Create, because the create
    // path passed a display name as the label, and
    // `products/ux-audit-clone-20260916/product.gdl` after closing and reopening
    // the same product -- while Overview two lines down said the name both
    // times. One entity, two names, side by side. The expression is the one
    // `createGearForProduct` already uses.
    // `||`, not `??`: an empty `display_name` is a name nobody wrote, and it used
    // to win over everything after it and leave the header blank.
    const name = state.intent?.display_name || state.intent?.id || state.open?.name || state.open?.label || "";
    return (
      <>
        <span className="gbx-toolbar-name" data-product={name}>
          {name}
        </span>
        {state.profile !== undefined && (
          // `data-header-profile`, not `data-profile`: the Product view's own
          // switch already owns that attribute and means "a profile you may
          // select", and `openProduct` clicks it. Reusing the name made one
          // locator match two elements -- the fifth time in this shell that a
          // shared attribute has crossed two widgets. One name, one meaning.
          <span
            className="gbx-badge"
            data-header-profile={state.profile}
            title="deployment profile"
          >
            {state.profile}
          </span>
        )}
        {this.renderStatus(state.status, product?.lock_hash !== undefined)}
      </>
    );
  }

  /**
   * Resolved, resolving, or the count of errors -- in the product's own terms.
   *
   * Errors are counted rather than listed: a header is read at a glance, and the
   * list belongs where a person can act on each one -- which is the Conflicts
   * screen, and the count opens it.
   */
  protected renderStatus(status: string, resolved: boolean): React.ReactNode {
    // **`state.diagnostics`, not `resolution.diagnostics`.** Every other reader
    // uses the former, and the difference is only visible in the case this badge
    // exists for: a refused load leaves `status: "error"` with the diagnostics
    // kept and `resolution` unset (`ProductStore`), so reading through the
    // resolution counted zero errors exactly when there were some, and the
    // header went quiet at the moment it had something to say.
    const errors = errorsIn(this.products.current.diagnostics);

    if (status === "loading" || status === "resolving") {
      return (
        <span className="gbx-toolbar-status" data-status="working">
          resolving…
        </span>
      );
    }
    if (errors > 0) {
      // A button, not a label. The count is only useful if it leads somewhere, and
      // the place it leads to -- the Conflicts screen -- is where each one can be
      // read and acted on. A number with nowhere to go is a number that trains
      // people to ignore it.
      return (
        <button
          type="button"
          className="gbx-badge gbx-downgraded gbx-toolbar-status"
          data-status="conflicts"
          data-conflicts={errors}
          title="Show the conflicts"
          onClick={() => void this.commands.executeCommand("gearbox.conflicts.toggle")}
        >
          {errors} {errors === 1 ? "conflict" : "conflicts"}
        </button>
      );
    }
    if (resolved) {
      return (
        <span className="gbx-badge gbx-toolbar-status" data-status="resolved">
          resolved
        </span>
      );
    }
    return undefined;
  }

  protected actionButton(id: string): React.ReactNode {
    const command = this.commands.getCommand(id);
    if (command === undefined) return undefined;
    const enabled = this.commands.isEnabled(id);
    // `shortTitle` and nothing else. This was briefly a mapping from command id to
    // caption right here, which is precisely the drift this file's first paragraph
    // rules out: the header would have said `Generate` while the View menu and the
    // palette still said `Toggle Gearbox Generate`. Every command the header names
    // carries its own short title now -- `RESOLVE_PRODUCT`, `SWITCH_PRODUCT`,
    // `CLOSE_PRODUCT` always did, and `GenerateViewContribution` registers its
    // toggle with one (ADR-0011 amendment 2026-09-02).
    const title = command.shortTitle ?? command.label ?? id;
    return (
      <button
        key={id}
        type="button"
        className="gbx-choice"
        data-command={id}
        disabled={!enabled}
        title={command.label}
        onClick={() => void this.commands.executeCommand(id)}
      >
        {command.iconClass !== undefined && <span className={command.iconClass} />}
        {title}
      </button>
    );
  }
}
