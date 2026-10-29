// The workspace is a domain object, not something a person has to find.
//
// ADR 0011 lists this among the mechanisms Arduino IDE uses: "Make the workspace
// root a domain object -- `WorkspaceService.toFileStat` override: open a `.ino`,
// get its sketch folder." The same idea, one step earlier: Studio already knows
// which directories it works on, so it opens them rather than showing an empty
// Explorer and waiting.
//
// Three things stop working without a workspace, and none of them look like a
// missing workspace:
//
//   - the Explorer has nothing to browse, so `gear.gdl` and `product.gdl` can
//     only be reached through a Gearbox link;
//   - a generated `product.lock` cannot be opened at all, which left the
//     read-only-lock requirement implemented and unverifiable;
//   - the VS Code git extension finds repositories by walking workspace folders,
//     so Source Control stays empty however well the plugin host works.
//
// **Multi-root is not a preference here, it is forced by the layout.** The two
// repositories are siblings -- `gearbox` and `gears-rust` -- so no single
// folder contains both, and a single-folder workspace would silently cover only
// half the tree a person edits.

import { FrontendApplicationContribution } from "@theia/core/lib/browser";
import { ILogger } from "@theia/core/lib/common/logger";
import { PreferenceScope, PreferenceService } from "@theia/core/lib/common/preferences";
import { URI } from "@theia/core/lib/common/uri";
import { WORKSPACE_TRUST_TRUSTED_FOLDERS } from "@theia/workspace/lib/common/workspace-trust-preferences";
import { WorkspaceService } from "@theia/workspace/lib/browser/workspace-service";
import { inject, injectable } from "@theia/core/shared/inversify";

import { configuredDefaults } from "../../../common/gearbox-preferences";
import { GearboxService } from "../../../common/protocol";

@injectable()
export class DomainWorkspace implements FrontendApplicationContribution {
  @inject(WorkspaceService) protected readonly workspace!: WorkspaceService;
  @inject(PreferenceService) protected readonly preferences!: PreferenceService;
  @inject(GearboxService) protected readonly service!: GearboxService;
  @inject(ILogger) protected readonly logger!: ILogger;

  // **Not awaited by startup.** Adding a root rewrites the workspace, and Theia
  // does that through services still starting while `onStart` runs: awaited
  // here, a configured workspace that was not yet a root held the application
  // on its loading screen for good.
  onStart(): void {
    void this.apply();
  }

  /** Open the derived roots as the workspace, adding any that are missing. */
  async apply(): Promise<void> {
    // `ready` first: `WorkspaceService` resolves whatever was saved during
    // startup, and opening one before it has finished races with that -- the
    // saved workspace would win, arbitrarily.
    await this.workspace.ready;

    let roots: string[];
    try {
      await this.preferences.ready;
      roots = await this.service.workspaceRoots(configuredDefaults(this.preferences));
    } catch (error) {
      // Not fatal, and not silent. A Studio with no workspace still browses the
      // catalogue; it just cannot browse files.
      this.logger.warn(`could not ask the backend for workspace roots: ${String(error)}`);
      return;
    }

    const [first, ...rest] = roots;
    if (first === undefined) return;

    // Trust these folders *before* opening them, and only these.
    //
    // The plugin host turns workspace trust from a formality into a real gate:
    // an untrusted workspace restricts extensions, so the git extension would be
    // present and inert. Opening a workspace without settling trust first puts a
    // modal dialog over the whole application on first launch -- which is how
    // this was found.
    //
    // The blunt fix is `security.workspace.trust.enabled: false`, and it is the
    // wrong one: it would trust every folder anyone opens afterwards, in an
    // application that now runs third-party extension code. This trusts exactly
    // the directories Studio derived for itself and leaves the mechanism in
    // place for everything else.
    await this.trust(roots);

    if (!this.workspace.opened) {
      // `preserveWindow`, or `open` navigates and the first launch flickers
      // through a reload before showing anything.
      this.workspace.open(URI.fromFilePath(first), { preserveWindow: true });
      await this.workspace.ready;
    }

    // Added rather than replaced. Someone who opened their own folder keeps it,
    // and the source roots join it -- which is also what makes this idempotent
    // across restarts, since `addRoot` on an existing root is a no-op.
    const present = new Set(this.workspace.tryGetRoots().map((stat) => stat.resource.path.fsPath()));
    const missing = [first, ...rest]
      .filter((root) => !present.has(root))
      .map((root) => URI.fromFilePath(root));
    if (missing.length > 0) {
      await this.workspace.addRoot(missing);
    }
  }

  /**
   * Add `roots` to the trusted-folder list, keeping whatever is already there.
   *
   * User scope, so it lands in `~/.theia/settings.json`: a trust decision is
   * per-machine and per-person, and writing it into the repository would be one
   * person deciding for everyone.
   *
   * **Stored as `file://` URIs, not as paths.** `WorkspaceTrustService.isUriTrusted`
   * does `new URI(folder).isEqualOrParent(rootUri)`, and `isEqualOrParent`
   * compares schemes -- so a bare `/Users/...` entry has no scheme and never
   * matches a `file:///Users/...` workspace root. The symptom is a trust dialog
   * that reappears every start while `settings.json` plainly lists the folder,
   * and it is the same mistake that once made the `gear.gdl` links dead: handing
   * a path to `new URI(...)` yields a URI nothing claims.
   */
  protected async trust(roots: readonly string[]): Promise<void> {
    const existing = this.preferences.get<string[]>(WORKSPACE_TRUST_TRUSTED_FOLDERS, []) ?? [];
    const wanted = roots.map((root) => URI.fromFilePath(root).toString());
    const merged = [...new Set([...existing, ...wanted])].sort();
    if (merged.length === existing.length) return;
    try {
      await this.preferences.set(WORKSPACE_TRUST_TRUSTED_FOLDERS, merged, PreferenceScope.User);
    } catch (error) {
      // Reported rather than swallowed: the visible consequence is a trust
      // dialog on every start, and "why does it keep asking" is much harder to
      // answer from nothing than from a log line.
      this.logger.warn(`could not record trusted folders: ${String(error)}`);
    }
  }
}
