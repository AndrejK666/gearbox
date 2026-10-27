// What there is to do when no product is open.
//
// The Home context used to have an empty main area with the catalogue beside it,
// which said "this is an IDE with nothing loaded" rather than "this is a tool for
// building products, and you have not chosen one". STM32CubeMX opens on exactly
// three things -- New, Load, Recent -- and only then shows domain views; this is
// that screen minus the one that is not built.
//
// **Create and clone are here now.** Both go through the same screen: a wizard
// with a live preview on the right (`cpt-gearbox-adr-authoring-ownership-tiers`
// §Consequences: a preview is not optional). Clone reads the source file and
// changes only `id` and `name`; everything else — comments included — survives.
// The two lists are deliberately not merged. `Open Product…` offers one list
// because a picker is a question with one answer; here there is room to say which
// products are *in this workspace* and which were *opened before*, and those are
// different facts about a product.

import { ReactWidget } from "@theia/core/lib/browser";
import { CommandRegistry } from "@theia/core/lib/common";
import { inject, injectable, postConstruct } from "@theia/core/shared/inversify";
import React from "@theia/core/shared/react";

import type { ProductRef } from "../../common/protocol";
import { CatalogueStore } from "../catalogue-store";
import { PendingCreate } from "../create/pending-create";
import { ProductStore } from "../product-store";
import { EngineConnectionService } from "../shell/engine-connection-service";
import { ProductSessionService, type RecentEntry } from "../shell/product-session-service";
import { BROWSE_CATALOGUE, NEW_GEAR, NEW_PRODUCT, OPEN_GEAR, OPEN_PRODUCT } from "../shell/session-command-ids";
import { CONFIGURE_SOURCES } from "../shell/first-run-contribution";

@injectable()
export class StartWidget extends ReactWidget {
  static readonly ID = "gearbox.start";
  static readonly LABEL = "Gearbox Studio";

  @inject(ProductStore) protected readonly products!: ProductStore;
  @inject(ProductSessionService) protected readonly session!: ProductSessionService;
  @inject(CommandRegistry) protected readonly commands!: CommandRegistry;
  @inject(PendingCreate) protected readonly pending!: PendingCreate;
  @inject(EngineConnectionService) protected readonly engine!: EngineConnectionService;
  @inject(CatalogueStore) protected readonly catalogue!: CatalogueStore;

  /**
   * Recent products, read once and after every change.
   *
   * Held rather than read in `render`: `recent()` is asynchronous -- it goes to
   * `StorageService` -- and a render that starts a promise starts one per frame.
   */
  protected recent: readonly RecentEntry[] = [];

  @postConstruct()
  protected init(): void {
    this.id = StartWidget.ID;
    this.title.label = StartWidget.LABEL;
    this.title.caption = "Gearbox Studio";
    // Not closable. Every other view can be closed because the shell still makes
    // sense without it; closing this one leaves the Home context with an empty
    // main area, which is the state it exists to replace.
    this.title.closable = false;
    this.addClass("gbx-widget-start");
    this.toDispose.push(this.products.onChanged(() => this.refresh()));
    this.toDispose.push(this.engine.onDidChange(() => this.update()));
    this.toDispose.push(this.catalogue.onChanged(() => this.update()));
    this.refresh();
  }

  /**
   * Ask what there is to offer, then render.
   *
   * `ensureDiscovered` and not `discover`: it returns immediately when a list is
   * already in hand or a load is in flight, so this can be called from every
   * change event without turning a re-render into a request.
   */
  protected refresh(): void {
    void this.products.ensureDiscovered();
    void this.session.recentEntries().then((recent) => {
      this.recent = recent;
      this.update();
    });
    this.update();
  }

  protected render(): React.ReactNode {
    const found = this.products.current.products;
    const remembered = this.recent.filter(
      (ref) => !found.some((candidate) => candidate.path === ref.path),
    );
    const connected = this.engine.isConnected;

    return (
      <div className="gbx-start">
        <div className="gbx-start-head">
          <div className="gbx-start-title">Gearbox Studio</div>
          <div className="gbx-start-subtitle">
            Compose a product from gears, resolve it for a profile, and generate what runs.
          </div>
        </div>

        {!connected && (
          <div className="gbx-error" role="alert" data-engine-status="disconnected">
            <div>Engine disconnected: {this.engine.disconnectReason}</div>
            <button
              type="button"
              className="gbx-choice"
              data-engine-retry
              // **The boot session, not whatever was asked for last.** Start is
              // Home: no product is open, so the session to re-establish is the
              // one the application starts with. A bare `load()` re-used the last
              // session, and when that was a product that failed to open, Retry
              // repeated its failure for ever.
              onClick={() => void this.catalogue.resetToBootSession()}
            >
              Retry
            </button>
          </div>
        )}

        {/* A fresh install has no gears checkout yet; say so where the work
            starts, with the way to fix it, rather than an empty catalogue. */}
        {connected && this.catalogue.current.status === "ready" && this.catalogue.rootPaths().length === 0 && (
          <div className="gbx-start-section" data-start-unconfigured>
            <div>No gears source is configured, so the catalogue is empty.</div>
            <button
              type="button"
              className="gbx-choice"
              onClick={() => void this.commands.executeCommand(CONFIGURE_SOURCES.id)}
            >
              Configure Sources…
            </button>
          </div>
        )}

        {this.renderContinue(connected)}

        <div className="gbx-start-actions">
          <button
            type="button"
            className="gbx-start-primary"
            data-start-action="create"
            disabled={!connected}
            onClick={() => void this.commands.executeCommand(NEW_PRODUCT.id)}
          >
            New Product…
          </button>
          <button
            type="button"
            className="gbx-start-primary gbx-start-secondary"
            data-start-action="open"
            onClick={() => void this.commands.executeCommand(OPEN_PRODUCT.id)}
          >
            Open Product…
          </button>
          <button
            type="button"
            className="gbx-start-primary"
            data-start-action="new-gear"
            disabled={!connected}
            onClick={() => void this.commands.executeCommand(NEW_GEAR.id)}
          >
            New Gear…
          </button>
          <button
            type="button"
            className="gbx-start-primary gbx-start-secondary"
            data-start-action="open-gear"
            onClick={() => void this.commands.executeCommand(OPEN_GEAR.id)}
          >
            Open Gear…
          </button>
          <button
            type="button"
            className="gbx-start-link"
            data-start-action="browse-catalogue"
            onClick={() => void this.commands.executeCommand(BROWSE_CATALOGUE.id)}
          >
            Browse Catalogue
          </button>
        </div>

        {this.renderList("In this workspace", "workspace", found, connected)}
        {this.renderList("Recent", "recent", remembered, connected)}

        {found.length === 0 && remembered.length === 0 && (
          <div className="gbx-empty" data-start-empty>
            Nothing in this workspace declares a product. A product is a{" "}
            <code>product.gdl</code> naming the gears it wants and the profiles it deploys under.
          </div>
        )}
      </div>
    );
  }

  /**
   * The way back to the last product, and the reason there is one.
   *
   * Studio used to open the only product it could find whenever the Product
   * widget was constructed, so a reload came back into a product nobody had asked
   * for and Home was unreachable with one in the workspace. Removing that made
   * Home honest and made returning a click longer, which is what this card pays
   * back -- named, timed, and one act rather than a picker.
   */
  protected renderContinue(connected: boolean): React.ReactNode {
    const [last] = this.recent;
    if (last === undefined) return undefined;
    return (
      <div className="gbx-start-section" data-start-continue={last.path}>
        <button
          type="button"
          className="gbx-start-primary gbx-start-continue"
          data-start-action="continue"
          disabled={!connected}
          onClick={() => void this.open("recent", last)}
        >
          <span className="gbx-start-item-name">Continue {last.name ?? last.label}</span>
          <span className="gbx-start-item-path">
            {last.openedAt === undefined ? <bdi dir="ltr">{last.path}</bdi> : `Last opened ${ago(last.openedAt)}`}
          </span>
        </button>
      </div>
    );
  }

  protected renderList(
    label: string,
    kind: string,
    refs: readonly ProductRef[],
    connected: boolean,
  ): React.ReactNode {
    if (refs.length === 0) return undefined;
    return (
      <div className="gbx-start-section" data-start-list={kind}>
        <div className="gbx-start-label">{label}</div>
        <ul className="gbx-start-items">
          {refs.map((ref) => (
            <li key={ref.path}>
              <button
                type="button"
                className="gbx-start-item"
                data-start-product={ref.path}
                onClick={() => void this.open(kind, ref)}
              >
                <span className="gbx-start-item-name">{ref.name ?? ref.label}</span>
                {/* `bdi`: the cell is right-to-left so that a long path loses its
                    start rather than its end, and in a right-to-left run the
                    leading `/` -- a neutral character -- was drawn at the end,
                    so every path read `…/product.gdl/`. Isolating the text keeps
                    the ellipsis where it is and the slash where it belongs. */}
                <span className="gbx-start-item-path"><bdi dir="ltr">{ref.path}</bdi></span>
              </button>
              {kind === "workspace" && (
                <button
                  type="button"
                  className="gbx-start-clone"
                  data-start-action="clone"
                  data-clone-from={ref.path}
                  disabled={!connected}
                  onClick={() => this.openClone(ref)}
                >
                  Clone
                </button>
              )}
            </li>
          ))}
        </ul>
      </div>
    );
  }

  /**
   * Prefill the wizard to clone this product.
   *
   * **From the folder name, not from the label.** `ProductRef.label` is a
   * repository-relative *path* by that field's own contract, and this built the
   * id out of it by replacing spaces -- so cloning `payments-demo` from Home
   * offered the id `products/payments-demo/product.gdl-copy`, the name
   * `products/payments-demo/product.gdl copy`, and a destination of
   * `.../products/products/payments-demo/product.gdl-copy/product.gdl`. Three
   * wrong defaults from one field read as though it were a name.
   *
   * The ref carries no id or name -- it is `{path, label}` and nothing else --
   * so the folder the description sits in is the best identity available here,
   * and it is the one discovery itself uses to find products. Sanitised to the
   * id rule the wizard and the engine both apply, and left for the person to
   * correct: the wizard now refuses to create on a bad id rather than stamping
   * it.
   */
  protected openClone(ref: ProductRef): void {
    const folder = ref.path
      .replace(/\\/g, "/")
      .replace(/\/product\.gdl$/i, "")
      .split("/")
      .filter((part) => part !== "")
      .pop();
    const base = (folder ?? "product")
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "");
    const id = base === "" ? "product" : base;
    this.pending.state = {
      cloneFrom: ref.path,
      id: `${id}-copy`,
      name: `${id} copy`,
    };
    void this.commands.executeCommand(NEW_PRODUCT.id);
  }

  /**
   * Open it, through the session rather than the store.
   *
   * A remembered path is the one place a path is expected to have rotted, and
   * `openRecent` is what forgets it and says so; a discovered one was listed by
   * the engine a moment ago, so `open` is the honest call for it.
   */
  protected async open(kind: string, ref: ProductRef): Promise<void> {
    if (kind === "recent") {
      await this.session.openRecent({ path: ref.path, label: ref.label });
      // The list may be one shorter now, if the path had rotted.
      this.refresh();
      return;
    }
    await this.session.open(ref);
  }
}

/**
 * How long ago, in the coarsest unit that is still true.
 *
 * Coarse on purpose: the card answers "is this the thing I was just doing?", and
 * a count of seconds invites the reader to care about a number that changes while
 * they look at it. No `Intl.RelativeTimeFormat`, because the shell is not
 * localised and one English sentence beats a locale-shaped guess.
 */
function ago(at: number): string {
  const seconds = Math.max(0, Math.round((Date.now() - at) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} minute${minutes === 1 ? "" : "s"} ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.round(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}
