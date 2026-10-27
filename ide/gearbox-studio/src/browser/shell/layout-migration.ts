// Close, once, the panels this application no longer has.
//
// Suppressing a menu entry and a command does not remove a panel that is already
// **in a saved layout**. `Type Hierarchy` and a `zsh` terminal kept coming back at
// the bottom of the shell after the shell stopped offering either -- Theia's
// `ShellLayoutRestorer` faithfully restores what was there before the rules
// changed, which is correct of it and wrong for the reader.
//
// So the rules changing needs a migration, and a migration has to run **once**.
// Closing these on every start would be a different bug: a terminal a person
// deliberately opened would vanish on reload, which is worse than one they did not
// ask for staying. The version below is what makes it once; bump it when the set
// of panels the application has changes again.
//
// Deliberately not `ApplicationShellLayoutMigration`, Theia's own hook for this:
// that one is keyed to the layout *schema* version and exists for rewriting
// serialised layout data. Nothing about the schema changed -- the application's
// inventory did -- so this closes widgets through the shell instead of rewriting
// a blob, which is both smaller and legible in a stack trace.

import { ApplicationShell, FrontendApplicationContribution } from "@theia/core/lib/browser";
import { PerspectiveService } from "@theia/core/lib/browser/perspective-service";
import { FrontendApplicationStateService } from "@theia/core/lib/browser/frontend-application-state";
import { StorageService } from "@theia/core/lib/browser/storage-service";
import { inject, injectable } from "@theia/core/shared/inversify";

/**
 * Widget id prefixes that no longer belong to this application.
 *
 * Prefixes because ids are generated: a terminal is `terminal-0`, `terminal-1`.
 * The families match `ShellPolicy.FORBIDDEN_COMMAND_PREFIXES` in intent, but not
 * in spelling -- a widget id and a command id are different namespaces, and
 * assuming they agreed is how the camelCase mistake in that list happened.
 */
export const CLOSED_ON_MIGRATION: readonly string[] = [
  // Generate now lives inside the product workspace; source editor tabs are preserved.
  "gearbox.generate",
  // **Three of these had a `theia-` prefix nobody had read**, which is the fourth
  // time in this repository that a family was suppressed everywhere except where
  // it actually lives. `TypeHierarchyTreeWidget.WIDGET_ID` is `theia-typehierarchy`,
  // the timeline is `theia-timeline`, and bulk edit is
  // `theia-bulk-edit-container` -- so `typehierarchy`, `timeline` and `bulk-edit`
  // matched nothing at all, and a UX pass duly reported Type Hierarchy still
  // sitting in the bottom panel on Home. `callhierarchy` and `outline-view` are
  // the ids as written. Read out of the packages, not guessed; the claim that
  // reads the rendered panel is what makes the next one of these visible.
  "theia-typehierarchy",
  "typehierarchy",
  "callhierarchy",
  "outline-view",
  "notebook",
  "theia-timeline",
  "timeline",
  "theia-bulk-edit",
  "bulk-edit",
  "debug",
  "test-",
  "testing",
  // The two panels the Inspector replaced. Their widget factories are gone, so a
  // restored layout that still names them logs a construction failure and leaves
  // an empty tab -- which is exactly the shape of thing this migration exists to
  // remove. Version 2 is what makes the sweep run again for a person who already
  // ran version 1.
  "gearbox.detail",
  "gearbox.explain",
  // The `Plugins` view. It never opened itself -- `@theia/plugin-ext` binds the
  // contribution and nothing else -- so anyone seeing it in the left bar is seeing
  // their own saved layout, which is precisely what this sweep is for. Version 3
  // makes it run again for someone who already ran version 2.
  //
  // Safe as a prefix: views contributed *by* extensions are `plugin-view*`, and
  // this matches only `plugins`.
  "plugins",
  // The Inspector, once, because it **moved** rather than went away. A saved
  // layout holds it at the bottom, where `defaultWidgetOptions` has no say -- so
  // a returning person would keep the one-field keyhole the move exists to fix.
  // Closing is safe here in a way it was not for the terminal: this widget is a
  // plain `WidgetFactory` binding, so `WidgetManager` drops its cache entry on
  // dispose (`widget-manager.js:150`) and the next selection builds a fresh one
  // in the right panel. Version 4 makes the sweep run again.
  "gearbox.inspector",
  // Problems: `HiddenProblemsView.initializeLayout` stops a *fresh* profile from
  // auto-opening it, but a returning layout that already restored the empty
  // bottom panel still shows it on Home. Close once; `problems.` stays in
  // `KEPT_COMMAND_PREFIXES` so on-demand open still works. Version 5.
  "problems",
  // The chat moved out of the right panel, for the reason
  // `theia/ai-chat-ui/chat-in-the-bottom-panel.ts` gives: the Inspector shares
  // that panel and takes it on every selection, so the chat was covered by the
  // very act it exists to support. A saved layout keeps it where it was, so it
  // is closed once and rebuilt in the bottom panel on the next open. Safe for
  // the same reason the Inspector was: `bindChatViewWidget` is a plain
  // `WidgetFactory`, and it re-creates the widget when its cached one has been
  // disposed. Version 7.
  "chat-view-widget",
  // Adding a gear became a modal dialog, so the panel and its `WidgetFactory`
  // are gone. A saved layout that still names it has nothing to build from, and
  // Theia leaves the empty tab this list exists to prevent -- the same failure
  // as `gearbox.detail` above, from the same cause. Version 8.
  "gearbox.add-gear",
  // **No `terminal-` here, and that was learned the hard way.** Closing the boot
  // terminal broke the capability: `widget.close()` disposes the widget while
  // `WidgetManager` keeps its entry under the same id, so the next
  // `Terminal: Create New Terminal` returned the disposed instance and nothing
  // appeared -- by command and by keybinding alike, with no error. A terminal that
  // should not be open at startup is handled where it is opened, by
  // `HiddenTerminal.initializeLayout(): NOOP`, which is the mechanism ADR-0011
  // already names for Debug and Test.
];

const MIGRATION_KEY = "gearbox.layoutMigration";

/** Bump when `CLOSED_ON_MIGRATION` changes, so the sweep runs again -- once. */
const MIGRATION_VERSION = 9;

/**
 * Prefixes detached on **every** perspective switch, not once.
 *
 * A subset of `CLOSED_ON_MIGRATION`: only the families a saved *snapshot* can
 * resurrect, and only ones this application has withdrawn outright rather than
 * moved. The Inspector is deliberately absent -- it moved to the right panel, so
 * finding it in a snapshot is not a fault -- and so are the two retired Gearbox
 * panels, whose widget factories are gone, which means `healLayoutData` cannot
 * recreate them anyway. Outline is here because it is withdrawn: a one-shot
 * migration closes a stored layout, but a perspective snapshot can put it back.
 */
const SWEPT_ON_EVERY_SWITCH: readonly string[] = [
  "terminal-",
  // Both spellings, for the reason `CLOSED_ON_MIGRATION` gives above.
  "theia-typehierarchy",
  "typehierarchy",
  "callhierarchy",
  "outline-view",
  "notebook",
  "theia-timeline",
  "timeline",
  "theia-bulk-edit",
  "bulk-edit",
  "plugins",
];

@injectable()
export class LayoutMigration implements FrontendApplicationContribution {
  @inject(ApplicationShell) protected readonly shell!: ApplicationShell;
  @inject(StorageService) protected readonly storage!: StorageService;
  @inject(FrontendApplicationStateService)
  protected readonly appState!: FrontendApplicationStateService;
  @inject(PerspectiveService) protected readonly perspectives!: PerspectiveService;

  onStart(): void {
    // After `ready`, because the restorer has to have restored before there is
    // anything to close. `onStart` alone runs while the shell is still being
    // assembled, and closing a widget that has not been attached yet does nothing
    // at all -- silently, which is the worst version of not working.
    void this.appState.reachedState("ready").then(() => {
      void this.migrate();
      // And once at `ready`, not only on a switch. A person who lands on Home and
      // stays there never causes a perspective change, so a snapshot-resurrected
      // panel sat in the bottom bar untouched -- which is how Type Hierarchy was
      // still on screen after both the migration and the sweep were supposed to
      // have dealt with it. (The prefix was also wrong; both halves were needed.)
      this.sweepForbidden();
    });
    // And again on every perspective switch, for the widgets a *snapshot* brings
    // back -- see `sweepForbidden`.
    this.perspectives.onDidChangePerspective(() => this.sweepForbidden());
  }

  /**
   * Detach the forbidden widgets a restored snapshot brought back.
   *
   * The one-shot migration above is about a person's *stored* layout and runs
   * once by design. This is about a different mechanism with the same symptom:
   * `PerspectiveService` snapshots the layout on every switch and restores it on
   * the way back, and `healLayoutData` **recreates** widgets in that snapshot
   * whose instances were disposed. So a terminal that a migration closed, or that
   * a person opened before it was withdrawn, reappears on the second visit to a
   * context -- observed by a UX pass as "the terminal activated itself after I
   * closed the product".
   *
   * **Detached, not closed.** `widget.parent = null` takes it out of the layout
   * without disposing it -- the technique Theia's own `detachStrayWidgets` uses,
   * for the reason its comment gives: another perspective may still need the
   * instance. Closing here would also repeat the mistake `CLOSED_ON_MIGRATION`
   * records about `terminal-`, which is the one id this sweep is most likely to
   * find.
   */
  protected sweepForbidden(): void {
    for (const widget of this.shell.widgets) {
      if (!SWEPT_ON_EVERY_SWITCH.some((prefix) => widget.id.startsWith(prefix))) continue;
      if (widget.parent === null) continue;
      // eslint-disable-next-line no-null/no-null
      widget.parent = null;
    }
  }

  protected async migrate(): Promise<void> {
    const done = await this.storage.getData<number>(MIGRATION_KEY);
    if (done !== undefined && done >= MIGRATION_VERSION) {
      return;
    }

    const closed: string[] = [];
    for (const widget of this.shell.widgets) {
      if (CLOSED_ON_MIGRATION.some((prefix) => widget.id.startsWith(prefix))) {
        widget.close();
        closed.push(widget.id);
      }
    }
    await this.storage.setData(MIGRATION_KEY, MIGRATION_VERSION);

    if (closed.length > 0) {
      // Logged rather than silent: a panel disappearing on one particular start
      // is the kind of thing a person reasonably wants an explanation for, and
      // this is the only record that it was deliberate.
      // eslint-disable-next-line no-console
      console.info(
        `Gearbox: closed ${closed.join(", ")} -- panels this application no longer offers. ` +
          `This runs once per layout migration.`,
      );
    }
  }
}
