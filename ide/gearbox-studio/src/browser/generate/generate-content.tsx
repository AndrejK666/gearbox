// Preview and apply the artefact tree for the resolution on screen.
//
// The engine already knows how to generate; this panel is the missing place
// to look at the plan and to say yes. The tree is `FilePlan[]` -- one line
// per file, no bytes -- and a click asks `gearbox/generate/file` for the two
// sides of that one file. Putting every file on the plan would drag
// `Cargo.lock` (~100k) across for a preview nobody asked to read.

import { ConfirmDialog } from "@theia/core/lib/browser";
import URI from "@theia/core/lib/common/uri";
import { CommandService } from "@theia/core/lib/common/command";
import { inject, injectable } from "@theia/core/shared/inversify";
import { DisposableCollection } from "@theia/core/lib/common/disposable";
import React from "@theia/core/shared/react";
import { FileDialogService } from "@theia/filesystem/lib/browser";
import { FileService } from "@theia/filesystem/lib/browser/file-service";
import * as monaco from "@theia/monaco-editor-core";

import type { FileAction } from "../../common/generated/FileAction";
import type { FileKind } from "../../common/generated/FileKind";
import type { FilePlan } from "../../common/generated/FilePlan";
import type { GenerateFileResult } from "../../common/generated/GenerateFileResult";
import type { ProductTemplate } from "../../common/generated/ProductTemplate";
import type { TemplateStatus } from "../../common/generated/TemplateStatus";
import { CatalogueStore } from "../catalogue-store";
import { DiagnosticsList, selectionOf } from "../diagnostics/diagnostics-list";
import { SelectionService } from "../shell/selection-service";
import { ProductStore } from "../product-store";
import { ProductEditService } from "../product-edit-service";
import { RevealService } from "../reveal-service";
import { SHOW_PRODUCT } from "../shell/session-command-ids";
import { GenerateService } from "./generate-service";

const ACTION_ICON: Record<FileAction, string> = {
  create: "new-file",
  update: "edit",
  unchanged: "pass",
  conflict: "warning",
  kept: "lock",
};

const KIND_LANGUAGE: Record<FileKind, string> = {
  rust: "rust",
  toml: "ini",
  yaml: "yaml",
  json: "json",
  dockerfile: "dockerfile",
  shell: "shell",
  text: "plaintext",
  binary: "plaintext", // Binary plans have preview_available = false.
};

/** How each state of a product template reads, and why. */
const TEMPLATE_STATUS: Record<TemplateStatus, { label: string; title: string }> = {
  default: { label: "default", title: "The builtin: a new Gearbox build replaces it with its own" },
  edited: {
    label: "edited",
    title: "Edited in the product: a new builtin is merged into it, and the edit stays",
  },
  behind: {
    label: "behind",
    title:
      "Edited where the builtin changed too, or with no record of its builtin: it renders as it is " +
      "and does not get the builtin's change (GBX0712). Merge by hand, or reset it",
  },
  unknown: { label: "unused", title: "No key renders this file; it is kept and unused" },
};

/** The directory a product's templates are in, from where its copies are. */
function templatesDir(templates: readonly ProductTemplate[]): string | undefined {
  const first = templates.find((t) => t.status !== "unknown") ?? templates[0];
  if (first === undefined) return undefined;
  const suffix = `${first.key}.jinja`;
  return first.path.endsWith(suffix) ? first.path.slice(0, first.path.length - suffix.length) : undefined;
}

/** Pixels per tree level. */
const INDENT = 12;

interface TreeNode {
  readonly name: string;
  readonly plan?: FilePlan;
  readonly children: TreeNode[];
}

function treeOf(plans: readonly FilePlan[]): TreeNode[] {
  const root: TreeNode = { name: "", children: [] };
  for (const plan of plans) {
    const parts = plan.path.split("/");
    let at = root;
    for (let i = 0; i < parts.length; i += 1) {
      const name = parts[i] ?? "";
      const last = i === parts.length - 1;
      let next = at.children.find((child) => child.name === name);
      if (next === undefined) {
        next = { name, children: [], plan: last ? plan : undefined };
        at.children.push(next);
      }
      at = next;
    }
  }
  sortTree(root);
  return root.children;
}

function sortTree(node: TreeNode): void {
  node.children.sort((a, b) => {
    const aDir = a.plan === undefined;
    const bDir = b.plan === undefined;
    if (aDir !== bDir) return aDir ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
  for (const child of node.children) sortTree(child);
}

function countByAction(plans: readonly FilePlan[]): Partial<Record<FileAction, number>> {
  const counts: Partial<Record<FileAction, number>> = {};
  for (const plan of plans) {
    counts[plan.action] = (counts[plan.action] ?? 0) + 1;
  }
  return counts;
}

/** Shared dependencies; the embedded view and legacy adapter use one generation service. */
@injectable()
export class GenerateViewServices {
  @inject(ProductEditService) readonly edits!: ProductEditService;
  @inject(ProductStore) readonly product!: ProductStore;
  @inject(GenerateService) readonly generate!: GenerateService;
  @inject(CatalogueStore) readonly catalogue!: CatalogueStore;
  // For the one way out of a failed plan. See the error branch in `render`.
  @inject(CommandService) readonly commands!: CommandService;
  // For the trip from a blocked generation to the object that blocked it.
  @inject(SelectionService) readonly selection!: SelectionService;
  @inject(FileDialogService) readonly fileDialog!: FileDialogService;
  @inject(FileService) readonly files!: FileService;
  @inject(RevealService) readonly reveals!: RevealService;

}

// Theia resolves React at runtime; the workspace owns its React type declarations.
const Component: typeof import("react").Component = React.Component;
export class GenerateContent extends Component<{ services: GenerateViewServices }> {
  protected readonly toDispose = new DisposableCollection();
  private mounted = false;
  protected get edits() { return this.props.services.edits; }
  protected get product() { return this.props.services.product; }
  protected get generate() { return this.props.services.generate; }
  protected get catalogue() { return this.props.services.catalogue; }
  protected get commands() { return this.props.services.commands; }
  protected get selection() { return this.props.services.selection; }
  protected get fileDialog() { return this.props.services.fileDialog; }
  protected get files() { return this.props.services.files; }
  protected get reveals() { return this.props.services.reveals; }
  protected selected: string | undefined;
  protected preview: GenerateFileResult | undefined;
  protected previewError: string | undefined;
  protected previewing = false;
  protected previewEpoch = 0;
  /** The folder field while it is being typed in; `undefined` shows the choice. */
  protected outDraft: string | undefined;
  /** Template copies an Apply is writing, whose change events are its own. */
  protected applyingTemplates = new Set<string>();
  protected replanTimer: ReturnType<typeof setTimeout> | undefined;

  override componentDidMount(): void {
    this.mounted = true;
    this.init();
    this.requestPlan();
  }

  override componentDidUpdate(): void {
    this.requestPlan();
  }

  protected requestPlan(): void {
    const state = this.product.current;
    if (this.mounted && !this.edits.hasDraft() && state.status === "ready" && !state.stale &&
      this.generate.current.plan === undefined && this.generate.current.status !== "error") {
      void this.generate.ensurePlan();
    }
  }

  override componentWillUnmount(): void {
    this.mounted = false;
    this.toDispose.dispose();
  }

  protected update(): void {
    if (this.mounted) this.forceUpdate();
  }

  protected init(): void {
    this.toDispose.push(
      this.product.onChanged(() => {
        this.generate.forgetIfStale();
        this.update();
      }),
    );
    this.toDispose.push(this.generate.onChanged(() => this.update()));
    this.toDispose.push(this.edits.onDraftChanged(() => this.update()));
    this.toDispose.push(this.catalogue.onChanged(() => this.update()));
    // **A template edited in the editor changes the plan, not the resolution.**
    // The store re-resolves on a description save; nothing re-plans on a
    // template save, so the list would say `default` over a file someone just
    // changed. Plan again once the writes stop -- except for the copies an
    // Apply is writing itself, which would otherwise wipe the count it reports.
    this.toDispose.push(
      this.files.onDidFilesChange((event) => {
        const templates = this.generate.current.plan?.templates ?? [];
        if (templates.length === 0) return;
        const copies = new Set(templates.map((t) => t.path));
        const dir = templatesDir(templates);
        const touched = event.changes
          .map((change) => change.resource.path.fsPath())
          .filter((path) => (copies.has(path) || (dir !== undefined && path.startsWith(dir))))
          .filter((path) => !/[\\/]\.base[\\/]/.test(path) && !this.applyingTemplates.has(path));
        if (touched.length === 0) return;
        if (this.replanTimer !== undefined) clearTimeout(this.replanTimer);
        this.replanTimer = setTimeout(() => {
          this.replanTimer = undefined;
          void this.generate.replan();
        }, 300);
      }),
    );
    this.toDispose.push({
      dispose: () => {
        if (this.replanTimer !== undefined) clearTimeout(this.replanTimer);
      },
    });
    this.update();
  }

  /** Apply, remembering which template copies it writes. */
  protected async apply(): Promise<void> {
    if (this.edits.hasDraft()) return;
    const writes = (this.generate.current.plan?.templates ?? []).filter(
      (t) => t.action === "create" || t.action === "update",
    );
    for (const t of writes) this.applyingTemplates.add(t.path);
    try {
      await this.generate.apply();
    } finally {
      // The file watcher reports a write some time after it lands.
      setTimeout(() => {
        for (const t of writes) this.applyingTemplates.delete(t.path);
      }, 2000);
    }
  }

  /**
   * The profile this plan belongs to, and what its kind does not produce.
   *
   * **The condition is the engine's own, not a table repeated here.**
   * `docker::files` and `helm::files` both return nothing when
   * `lock.kubernetes.is_none()`, and that same field is on the resolution the
   * client already holds -- so this reads the fact rather than re-deriving it
   * from the profile kind. A future profile kind that gains images would light
   * this up without anyone remembering to edit it.
   */
  protected renderProfile(): React.ReactNode {
    const state = this.product.current;
    const profile = state.profile;
    if (profile === undefined) return undefined;
    const decl = state.intent?.profiles[profile];
    const kubernetes = state.resolution?.product?.kubernetes;
    const deploys = kubernetes !== undefined && kubernetes !== null;
    return (
      <div className="gbx-generate-profile" data-generate-profile={profile}>
        <span className="gbx-badge" title="deployment profile">
          {profile}
        </span>
        {decl !== undefined && <span className="gbx-id">{decl.profile}</span>}
        {!deploys && (
          <span className="gbx-generate-profile-note" data-generate-no-deployment>
            no images or chart: those come from a `kubernetes` profile
          </span>
        )}
      </div>
    );
  }

  /**
   * Where Apply writes, and the control to put it somewhere else.
   *
   * The default is the tree under the workspace that the Lock view reads. A
   * chosen folder is for shipping: the repository the product is built and
   * deployed from, which is why the git option sits beside it. Shown in the
   * error state too, because "this folder holds somebody's files" is a refusal
   * whose only answer is this control.
   */
  protected renderOut(outRoot: string | undefined): React.ReactNode {
    const chosen = this.generate.outFolder;
    const repository = this.generate.current.repository;
    // Editable as well as chosen, like New Product's destination: a path is
    // often faster to paste than to navigate to. Empty means the default, which
    // the placeholder names once the engine has said where it is.
    return (
      <div className="gbx-generate-out" data-generate-out={chosen ?? ""}>
        <label className="gbx-generate-out-label" htmlFor="gbx-generate-out-input">
          Output folder
        </label>
        <input
          id="gbx-generate-out-input"
          className="theia-input gbx-generate-out-input"
          data-generate-out-input="true"
          dir="ltr"
          spellCheck={false}
          placeholder={chosen === undefined && outRoot !== undefined ? outRoot : "default: .gearbox/<product>/<profile>"}
          title={outRoot ?? chosen ?? ""}
          value={this.outDraft ?? chosen ?? ""}
          onChange={(event) => {
            this.outDraft = event.currentTarget.value;
            this.update();
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") void this.commitOut();
          }}
          onBlur={() => void this.commitOut()}
        />
        <button
          type="button"
          className="theia-button secondary"
          data-generate-choose-out="true"
          onClick={() => void this.chooseOut()}
        >
          Choose…
        </button>
        {chosen !== undefined && (
          <>
            <button
              type="button"
              className="theia-button secondary"
              data-generate-default-out="true"
              title="Generate into the workspace's .gearbox tree again"
              onClick={() => void this.generate.chooseOut(undefined)}
            >
              Use default
            </button>
            <label className="gbx-generate-out-git">
              <input
                type="checkbox"
                data-generate-git-init="true"
                checked={this.generate.initializesRepository}
                onChange={(event) => {
                  this.generate.initializesRepository = event.currentTarget.checked;
                }}
              />
              Make it a git repository on Apply, if it is not one
            </label>
          </>
        )}
        {repository !== undefined && (
          <span
            className="gbx-badge"
            data-generate-repository={repository.initialized ? "initialized" : "existing"}
          >
            {repository.initialized
              ? `git repository created, first commit ${repository.commit ?? ""}`
              : "already a git repository; nothing committed"}
          </span>
        )}
      </div>
    );
  }

  /** Take what was typed in the folder field, if it changed anything. */
  protected async commitOut(): Promise<void> {
    const draft = this.outDraft;
    this.outDraft = undefined;
    if (draft === undefined) return;
    const folder = draft.trim().replace(/\/+$/, "");
    const next = folder === "" ? undefined : folder;
    if (next === this.generate.outFolder) {
      this.update();
      return;
    }
    this.selected = undefined;
    this.preview = undefined;
    await this.generate.chooseOut(next);
  }

  protected async chooseOut(): Promise<void> {
    const uri = await this.fileDialog.showOpenDialog({
      title: "Folder to generate into",
      canSelectFiles: false,
      canSelectFolders: true,
      canSelectMany: false,
    });
    if (uri === undefined) return;
    const folder = uri.path.fsPath().replace(/\\/g, "/").replace(/\/+$/, "");
    this.selected = undefined;
    this.preview = undefined;
    this.outDraft = undefined;
    await this.generate.chooseOut(folder);
  }

  override render(): React.ReactNode {
    const caps = this.catalogue.engineCapabilities;
    if (caps !== undefined && !caps.generate) {
      return undefined;
    }

    const state = this.product.current;
    if (state.open === undefined || state.status !== "ready" || state.stale !== undefined) {
      return (
        <div className="gbx-generate gbx-empty">
          Generation needs a current resolution. Reconnect the engine or wait for this profile to finish resolving.
        </div>
      );
    }

    if (this.edits.hasDraft()) {
      return <section className="gbx-generate" data-generation-draft-block>
        <h2>Review pending changes first</h2>
        <p>Apply or discard the product's draft before generating files.</p>
        <button type="button" className="gbx-choice" onClick={() => void this.commands.executeCommand(SHOW_PRODUCT.id, "composition")}>Review changes</button>
      </section>;
    }

    const gen = this.generate.current;
    if (gen.status === "error") {
      // **A refusal is not a reason, and this screen used to show only the
      // refusal.** `resolution reported errors; nothing was generated` replaced
      // the entire widget: no profile, no list of which errors, and no control
      // of any kind -- so the correct decision not to generate arrived as a dead
      // end. The errors are already in the store, because the resolution is what
      // this screen draws from, so the reasons cost nothing to show.
      const errors = (state.resolution?.diagnostics ?? []).filter((d) => d.severity === "error");
      return (
        <div className="gbx-generate">
          {this.renderProfile()}
          {this.renderOut(undefined)}
          <div className="gbx-error" role="alert">
            {gen.error}
          </div>
          {errors.length > 0 && (
            <>
              <div className="gbx-group-label">
                {errors.length === 1 ? "the error" : `the ${errors.length} errors`} that stopped it
              </div>
              <DiagnosticsList diagnostics={errors} density="compact" />
            </>
          )}
          {/* A refusal of the chosen folder has no resolution error to fix, and
              its answer is the folder row above; the trip to the product would
              be a trip to nothing. */}
          {(errors.length > 0 || this.generate.outFolder === undefined) && (
          <div className="gbx-generate-actions">
            {/* **To the object, not just to the view.** Landing on the Product
                view left a person to find, among the gears, the one the error is
                about. The first error names its subject, and `selectionOf` is
                the same reader the diagnostics rows use, so the trip ends on the
                thing that has to change with its settings already open.
                The profile is untouched on purpose: it lives in the store, and
                the errors being read are that profile's. */}
            <button
              type="button"
              className="gbx-start-primary"
              data-generate-to-product="true"
              onClick={() => {
                const subject = selectionOf(errors[0]?.subject);
                if (subject !== undefined) this.selection.select(subject);
                void this.commands.executeCommand(SHOW_PRODUCT.id, "checks");
              }}
            >
              {selectionOf(errors[0]?.subject) !== undefined
                ? "Review blocking checks"
                : "Review checks"}
            </button>
            <span className="gbx-waiting">
              The preview refreshes after the errors are fixed. Files are written only when you apply the plan.
            </span>
          </div>
          )}
        </div>
      );
    }
    if (gen.plan === undefined) {
      return <div className="gbx-generate gbx-empty">Planning generation…</div>;
    }

    const plans = gen.plan.plans;
    const counts = countByAction(plans);
    const blocks = this.generate.blocks;
    const selectedPlan = plans.find((p) => p.path === this.selected);

    return (
      <div className="gbx-generate" data-out-root={gen.plan.out_root} data-written={gen.written ?? ""}>
        <div className="gbx-generate-head">
          {/* **Which profile this plan is for.** The screen used to name no
              profile at all: the only identity it carried was `data-out-root`,
              an attribute nothing prints, so a plan for `dev` and a plan for
              `prod` were two file lists with no way to tell them apart. A
              Kubernetes profile writes a Dockerfile per image and a Helm
              umbrella chart and an embedded one writes neither, which makes the
              absence of `docker/` and `helm/` either correct or alarming
              depending on a fact the screen was keeping to itself.

              Named, not offered: the switch lives in the Product view, and the
              shell header follows the same rule -- it "shows the profile; it
              does not offer to change it". */}
          {this.renderProfile()}
          <div className="gbx-generate-counts">
            {(["create", "update", "unchanged", "conflict", "kept"] as const).map((action) =>
              (counts[action] ?? 0) > 0 ? (
                <span key={action} className="gbx-badge gbx-badge-count" data-count={action}>
                  {counts[action]} {action}
                </span>
              ) : undefined,
            )}
            {gen.written !== undefined && (
              <span className="gbx-badge gbx-badge-count" data-written-count={gen.written}>
                {gen.written} written
              </span>
            )}
            {/* An unexpected chart must have a visible cause. The engine has
                always known which builtins a product replaced; the RPC path
                dropped it, so this panel showed a house template and a stock one
                as the same thing. */}
            {(gen.plan.overridden_templates ?? []).length > 0 && (
              <span
                className="gbx-badge"
                data-overridden={(gen.plan.overridden_templates ?? []).join(",")}
                title={`Product templates replace: ${(gen.plan.overridden_templates ?? []).join(", ")}`}
              >
                {(gen.plan.overridden_templates ?? []).length} overridden
              </span>
            )}
          </div>
        </div>
        {/* **Its own row, and the house primary style.** This was a
            `gbx-choice gbx-choice-on` chip inside the head -- the class whose own
            comment in the stylesheet calls it the profile switch -- so the
            central action of the product looked like a small selected toggle. It
            also sat *after* `.gbx-generate-counts`, which is `flex: 1` in a
            `flex-wrap: wrap` row, so a wide enough badge row pushed it onto a
            second line at the left edge: the position of the one thing this
            screen is for depended on how many badges there happened to be.
            A row of its own is what makes that impossible.

            Under the head rather than at the bottom of the panel: the tree and
            the diff below both scroll, and a footer would leave the screen. */}
        {this.renderOut(gen.plan.out_root)}
        <div className="gbx-generate-actions">
          <button
            className="gbx-start-primary"
            type="button"
            disabled={!this.generate.canApply}
            data-apply="true"
            onClick={() => void this.apply()}
          >
            Generate files
          </button>
          {/* Beside the button it explains, not two elements away from it. */}
          {blocks.length > 0 && (
            <ul className="gbx-generate-blocks">
              {blocks.map((block) => (
                <li key={block.id} data-apply-block={block.id}>
                  {block.reason}
                </li>
              ))}
            </ul>
          )}
        </div>
        {/* **What generation itself has to say.** The plan carries the
            generator's diagnostics -- a crate patched in because the checkout
            differs from what was published (GBX0709), a template overridden, a
            stale file left behind -- and this view used to render none of them,
            so a registry-backed product built with local patches and nothing on
            screen said so. Only the generator's own (GBX07xx): the resolution's
            are in Validation already, and repeating them here would bury these. */}
        {(gen.plan.diagnostics ?? []).some((d) => d.code.startsWith("GBX07")) && (
          <div className="gbx-generate-diagnostics" data-generate-diagnostics>
            <DiagnosticsList
              diagnostics={(gen.plan.diagnostics ?? []).filter((d) => d.code.startsWith("GBX07"))}
              density="compact"
            />
          </div>
        )}
        {this.renderTemplates(gen.plan.templates ?? [])}
        <div className="gbx-generate-body">
          <div className="gbx-generate-tree" role="tree">
            {treeOf(plans).map((node) => this.renderNode(node, 0))}
          </div>
          <div className="gbx-generate-preview">
            {this.renderPreview(selectedPlan)}
          </div>
        </div>
      </div>
    );
  }

  /**
   * The product's own templates, which generation writes and renders from.
   *
   * Open, not edited here: a template is a file, and the editor is where files
   * are edited. Reset deletes the copy, and the next plan takes the builtin
   * back -- which is the whole of what "default" means for a template.
   */
  protected renderTemplates(templates: readonly ProductTemplate[]): React.ReactNode {
    if (templates.length === 0) return undefined;
    const changed = templates.filter((t) => t.status !== "default");
    const dir = templatesDir(templates);
    const counts = (["edited", "behind", "unknown"] as const)
      .map((status) => [status, templates.filter((t) => t.status === status).length] as const)
      .filter(([, n]) => n > 0)
      .map(([status, n]) => `${n} ${TEMPLATE_STATUS[status].label}`);
    return (
      <details className="gbx-generate-templates" data-generate-templates open={changed.length > 0}>
        <summary title={dir}>
          Templates: {templates.length}
          {counts.length > 0 ? `, ${counts.join(", ")}` : ", all default"}
          {templates.some((t) => t.action === "create") && " (written on Apply)"}
        </summary>
        <ul>
          {templates.map((template) => (
            <li
              key={template.key}
              data-template={template.key}
              data-template-status={template.status}
              data-template-action={template.action}
              data-template-path={template.path}
            >
              <span className="gbx-row-name" title={template.path}>
                {template.key}
              </span>
              <span className="gbx-badge" title={TEMPLATE_STATUS[template.status].title}>
                {TEMPLATE_STATUS[template.status].label}
              </span>
              {template.action === "update" && (
                <span className="gbx-badge" title="Apply updates this copy to the builtin it follows">
                  updates on Apply
                </span>
              )}
              <button
                type="button"
                className="theia-button secondary"
                data-template-open={template.key}
                disabled={template.action === "create"}
                title={template.action === "create" ? "Written on Apply" : template.path}
                onClick={() => void this.reveals.revealPath(template.path)}
              >
                Open
              </button>
              {template.status !== "default" && (
                <button
                  type="button"
                  className="theia-button secondary"
                  data-template-reset={template.key}
                  title="Delete this copy; the next Apply writes the builtin"
                  onClick={() => void this.resetTemplate(template)}
                >
                  Reset
                </button>
              )}
            </li>
          ))}
        </ul>
      </details>
    );
  }

  /** Delete a product's copy of a template, after asking, and plan again. */
  protected async resetTemplate(template: ProductTemplate): Promise<void> {
    const confirmed = await new ConfirmDialog({
      title: `Reset ${template.key}`,
      msg: `Delete ${template.path}? Its edits are lost; the next Apply writes Gearbox's builtin.`,
      ok: "Reset",
    }).open();
    if (confirmed !== true) return;
    await this.files.delete(URI.fromFilePath(template.path));
    await this.generate.replan();
  }

  protected renderNode(node: TreeNode, depth: number): React.ReactNode {
    const plan = node.plan;
    if (plan === undefined) {
      return (
        // **The indent is per level, not per ancestor.** The folder used to pad
        // itself and render its children inside the padding, and each child
        // padded again -- so the offset grew as a sum, and four levels down
        // `helm/<product>/charts/<app>/templates` pushed every name off a 280px
        // column. Only the label is padded now; children carry their own depth.
        <div className="gbx-generate-dir" key={node.name}>
          <div className="gbx-generate-dir-label" style={{ paddingLeft: 4 + depth * INDENT }}>
            <span className="codicon codicon-folder" />
            {node.name}
          </div>
          {node.children.map((child) => this.renderNode(child, depth + 1))}
        </div>
      );
    }
    const on = this.selected === plan.path;
    return (
      <div
        className={`gbx-row ${on ? "gbx-selected" : ""}`}
        key={plan.path}
        role="treeitem"
        tabIndex={0}
        style={{ paddingLeft: 4 + depth * INDENT }}
        data-plan-path={plan.path}
        data-action={plan.action}
        data-ownership={plan.ownership}
        aria-selected={on}
        onClick={() => void this.select(plan)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            void this.select(plan);
          }
        }}
      >
        <span className={`gbx-leaf-icon codicon codicon-${ACTION_ICON[plan.action]}`} />
        <span className="gbx-row-name" title={plan.path}>{node.name}</span>
        {/* Only when it says something: nearly every file is `generated`, and a
            badge on every row wrapped under the name and read as noise. The
            ones worth a mark are the files that become the operator's. */}
        {plan.ownership !== "generated" && (
          <span className="gbx-badge" data-ownership={plan.ownership}>
            {plan.ownership}
          </span>
        )}
      </div>
    );
  }

  protected renderPreview(plan: FilePlan | undefined): React.ReactNode {
    if (plan === undefined) {
      return <div className="gbx-empty">Select a file to see the proposed content.</div>;
    }
    if (!plan.preview_available) {
      return (
        <div className="gbx-empty" data-preview-unavailable={plan.path}>
          `{plan.path}` is not text, so there is no preview to show. The plan
          still names it; applying will write the bytes.
        </div>
      );
    }
    if (this.previewing && this.selected === plan.path && this.preview === undefined) {
      return <div className="gbx-empty">Loading preview…</div>;
    }
    if (this.previewError !== undefined && this.selected === plan.path) {
      return (
        <div className="gbx-error" role="alert">
          {this.previewError}
        </div>
      );
    }
    if (this.preview === undefined || this.selected !== plan.path) {
      return <div className="gbx-empty">Select a file to see the proposed content.</div>;
    }
    return (
      <GenerateDiff
        original={this.preview.current ?? ""}
        modified={this.preview.proposed ?? ""}
        language={KIND_LANGUAGE[plan.kind]}
      />
    );
  }

  protected async select(plan: FilePlan): Promise<void> {
    const epoch = ++this.previewEpoch;
    this.selected = plan.path;
    this.preview = undefined;
    this.previewError = undefined;
    this.update();
    if (!plan.preview_available) {
      return;
    }
    this.previewing = true;
    try {
      const preview = await this.generate.file(plan);
      if (epoch === this.previewEpoch) this.preview = preview;
    } catch (error) {
      if (epoch === this.previewEpoch) this.previewError = error instanceof Error ? error.message : String(error);
    } finally {
      if (epoch === this.previewEpoch) { this.previewing = false; this.update(); }
    }
  }
}

/**
 * A read-only Monaco diff of one planned file.
 *
 * Created here rather than opened as a Theia editor tab: the preview belongs
 * to this panel, and a tab would steal the Product view. Disposed on every
 * change of the two sides, because Monaco models are not cheap to leak.
 */
function GenerateDiff(props: { original: string; modified: string; language: string }): React.ReactElement {
  const host = React.useRef<HTMLDivElement>(null);
  React.useEffect(() => {
    const el = host.current;
    if (el === null) return undefined;
    const original = monaco.editor.createModel(props.original, props.language);
    const modified = monaco.editor.createModel(props.modified, props.language);
    const editor = monaco.editor.createDiffEditor(el, {
      readOnly: true,
      renderSideBySide: true,
      automaticLayout: true,
      scrollBeyondLastLine: false,
      minimap: { enabled: false },
    });
    editor.setModel({ original, modified });
    return () => {
      editor.dispose();
      original.dispose();
      modified.dispose();
    };
  }, [props.original, props.modified, props.language]);
  return <div className="gbx-generate-diff" ref={host} />;
}
