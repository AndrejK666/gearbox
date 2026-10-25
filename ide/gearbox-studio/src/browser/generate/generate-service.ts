// Policy in front of Apply, and the plan the Generate view renders.
//
// A service rather than a handler in the widget: there are four independent
// reasons to refuse a write, and they have to be named in the UI when they
// fail. "Apply is disabled" with no reason is a dead end. Keeping the checks
// here means the next surface that wants to apply a plan does not reimplement
// them.
//
// The four, in the order a person can do something about them:
//
//   1. `capabilities.generate` -- otherwise the view is not offered at all;
//   2. `capabilities.writes` -- the client must have declared the right;
//   3. no resolution errors -- the engine refuses those on the server too;
//   4. no `FileAction::Conflict` -- GBX0701; a partial apply is forbidden.
//
// The plan is fetched lazily, the way the lock is: the other panels never need
// it, and a plan from the previous profile is worse than none.

import { Emitter, Event } from "@theia/core/lib/common/event";
import { StorageService } from "@theia/core/lib/browser/storage-service";
import { MessageService } from "@theia/core/lib/common/message-service";
import { inject, injectable, postConstruct } from "@theia/core/shared/inversify";

import type { FileAction } from "../../common/generated/FileAction";
import type { FilePlan } from "../../common/generated/FilePlan";
import type { GenerateApplyResult } from "../../common/generated/GenerateApplyResult";
import type { GenerateFileResult } from "../../common/generated/GenerateFileResult";
import type { GeneratePlanResult } from "../../common/generated/GeneratePlanResult";
import { GearboxService } from "../../common/protocol";
import { CatalogueStore } from "../catalogue-store";
import { ProductStore } from "../product-store";
import { EngineConnectionService } from "../shell/engine-connection-service";

export type GenerateStatus = "idle" | "planning" | "ready" | "error";

export interface GenerateState {
  readonly status: GenerateStatus;
  readonly plan: GeneratePlanResult | undefined;
  /**
   * How many files the last apply wrote. `undefined` until one has run for
   * this plan; `0` is a real answer (a second apply of an unchanged tree).
   */
  readonly written: number | undefined;
  readonly error: string | undefined;
  /**
   * What the last apply did about git, when it wrote to a chosen folder:
   * `initialized` with the first commit, or not when the folder was a
   * repository already and was left alone.
   */
  readonly repository?: { readonly initialized: boolean; readonly commit?: string };
}

/** Where the chosen output folders are kept, per product and profile. */
const OUT_KEY = "gearbox.generate.out";

export interface ApplyBlock {
  readonly id: "generate" | "writes" | "resolution" | "conflict" | "nothing";
  readonly reason: string;
}

/**
 * Whether a planned action puts bytes on disk.
 *
 * The mirror of `FileAction::writes()` (`crates/gearbox-ir/src/fileset.rs`),
 * which has existed since the plan did and which nothing on this side used. It
 * is the difference between "the plan is empty of work" and "the plan is empty":
 * `unchanged` and `kept` are answers, not absences.
 */
export function writesBytes(action: FileAction): boolean {
  return action === "create" || action === "update";
}

const EMPTY: GenerateState = {
  status: "idle",
  plan: undefined,
  written: undefined,
  error: undefined,
};

@injectable()
export class GenerateService {
  @inject(GearboxService) protected readonly service!: GearboxService;
  @inject(ProductStore) protected readonly product!: ProductStore;
  @inject(CatalogueStore) protected readonly catalogue!: CatalogueStore;
  @inject(MessageService) protected readonly messages!: MessageService;
  @inject(EngineConnectionService) protected readonly engine!: EngineConnectionService;
  @inject(StorageService) protected readonly storage!: StorageService;

  protected readonly onChangedEmitter = new Emitter<void>();
  readonly onChanged: Event<void> = this.onChangedEmitter.event;

  protected state: GenerateState = EMPTY;
  protected epoch = 0;
  protected inFlight = false;
  /**
   * The `(path, profile)` the current plan answers about. A plan for a
   * different resolution is discarded rather than shown.
   */
  protected forKey: string | undefined;
  /**
   * Output folders a person chose, keyed by `path::profile`. Absent means the
   * engine's default tree under the workspace.
   */
  protected outs: Record<string, string> = {};
  /** Whether an apply to a chosen folder makes it a repository. On by default. */
  protected gitInit = true;

  @postConstruct()
  protected init(): void {
    void this.storage.getData<Record<string, string>>(OUT_KEY).then((saved) => {
      if (saved === undefined) return;
      this.outs = saved;
      this.forgetIfStale();
    });
  }

  get current(): GenerateState {
    return this.state;
  }

  /** The folder chosen for what is on screen, or `undefined` for the default. */
  get outFolder(): string | undefined {
    const key = this.outKey();
    return key === undefined ? undefined : this.outs[key];
  }

  /** The panel and chat must plan against the same product/profile destination. */
  planFor(path: string, profile?: string): Promise<GeneratePlanResult> {
    const out = profile === undefined ? undefined : this.outs[`${path}::${profile}`];
    return this.service.planGenerate(path, profile, out);
  }

  get initializesRepository(): boolean {
    return this.gitInit;
  }

  set initializesRepository(on: boolean) {
    this.gitInit = on;
    this.onChangedEmitter.fire();
  }

  /**
   * Generate into `folder` from now on, or back into the default tree.
   *
   * A different folder is a different plan -- the same files can be `create`
   * in one and `unchanged` in the other -- so the plan on screen is dropped and
   * asked for again.
   */
  async chooseOut(folder: string | undefined): Promise<void> {
    const key = this.outKey();
    if (key === undefined) return;
    const next = { ...this.outs };
    if (folder === undefined) delete next[key];
    else next[key] = folder;
    this.outs = next;
    await this.storage.setData(OUT_KEY, next);
    this.forgetIfStale();
  }

  protected outKey(): string | undefined {
    const open = this.product.current.open;
    const profile = this.product.current.profile;
    if (open === undefined || profile === undefined) return undefined;
    return `${open.path}::${profile}`;
  }

  /**
   * Why Apply is off, named so the button is never just grey.
   *
   * Empty means every gate passed. `capabilities.generate` being false is
   * included for completeness; the view itself is not offered in that case.
   */
  get blocks(): readonly ApplyBlock[] {
    const caps = this.catalogue.engineCapabilities;
    const blocks: ApplyBlock[] = [];
    if (caps !== undefined && !caps.generate) {
      blocks.push({
        id: "generate",
        reason: "the engine does not advertise generation",
      });
    }
    if (caps !== undefined && !caps.writes) {
      blocks.push({
        id: "writes",
        reason: "this session has no write capability",
      });
    }
    const diagnostics = this.product.current.resolution?.diagnostics ?? [];
    if (diagnostics.some((d) => d.severity === "error")) {
      blocks.push({
        id: "resolution",
        reason: "resolution reported errors",
      });
    }
    const plans = this.state.plan?.plans ?? [];
    if (plans.some((p) => p.action === "conflict")) {
      blocks.push({
        id: "conflict",
        reason: "the plan has a conflict",
      });
    }
    // **Nothing to write is a state, and Apply used to accept it.** Every gate
    // above is about permission or correctness, and an all-`unchanged` plan
    // passes all of them -- so the button stayed live, the round trip ran, and
    // the engine answered `written: 0`. A control that is enabled for an
    // operation with no effect teaches the reader that the count above it is
    // decoration. Said as a count rather than as "nothing to do", because
    // `12 unchanged` is the reassuring half of the sentence.
    // The product's templates are written by an apply too, so a plan whose tree
    // is up to date still has work while a template waits to be written.
    const templateWrites = (this.state.plan?.templates ?? []).some((t) => writesBytes(t.action));
    if (
      this.state.status === "ready" &&
      plans.length > 0 &&
      !plans.some((p) => writesBytes(p.action)) &&
      !templateWrites
    ) {
      const kept = plans.length;
      blocks.push({
        id: "nothing",
        reason: `generation is up to date — ${kept} file${kept === 1 ? "" : "s"} unchanged`,
      });
    }
    return blocks;
  }

  get canApply(): boolean {
    return (
      this.state.status === "ready" &&
      this.state.plan !== undefined &&
      this.blocks.length === 0
    );
  }

  // No output root is sent unless a person chose a folder, and that is the whole
  // of the policy now. A chosen folder is for shipping the product from its own
  // repository (ADR-0010, Amendment 2026-09-29); the default below is the tree
  // the Lock view compares against.
  //
  // Studio used to write `.gearbox/studio/<product>/<profile>/` to keep out of
  // the CLI's tree, because `product.lock` was not client-independent: `digest`
  // was `path:<declared location>`, so the CLI declaring `../gears-rust` and this
  // backend declaring an absolute path produced two locks with different hashes,
  // and one shared tree would have meant each client rewriting the other's lock
  // forever.
  //
  // `digest` is a content digest now (`gearbox_engine::content_digest`) and a
  // lock's `location` is recorded relative to the description, so the two clients
  // produce byte-identical trees -- demonstrated by generating the same product
  // with a relative and an absolute root and diffing the results. The separation
  // has nothing left to protect, so the engine's default
  // `.gearbox/<product>/<profile>/` stands: one tree, and it is the tree §12
  // step 2 builds and runs.

  /**
   * What a cached plan is a plan *of*.
   *
   * **The revision is the part that was missing.** The key used to be path and
   * profile alone, so editing the product and re-resolving produced an identical
   * key -- `forgetIfStale` compared it, found no change, and kept a plan that
   * described the resolution before the edit. `ProductStore.revision` is the
   * epoch that changes on every resolve, which is exactly the question being
   * asked here.
   */
  protected planKey(): string | undefined {
    const open = this.product.current.open;
    const profile = this.product.current.profile;
    if (open === undefined || profile === undefined) return undefined;
    // The folder is part of what the plan is *of*: the same files are `create`
    // in an empty repository and `unchanged` in the default tree.
    return `${open.path}::${profile}::${this.product.revision}::${this.outFolder ?? ""}`;
  }

  /**
   * Refuse to write a plan that is not a plan of what is on screen.
   *
   * `apply()` and `file()` both re-read `this.product.current.open` when they
   * are called, so without this they act on whatever is open *now* using a plan
   * that described something else -- and `canApply` is computed from the cached
   * plan too, so the gates could be evaluated against one product while the
   * write went to another.
   */
  protected planIsCurrent(): boolean {
    const key = this.planKey();
    if (key !== undefined && key === this.forKey) return true;
    this.messages.warn(
      "Nothing was written: this plan describes a resolution that has since changed. " +
        "Plan again.",
    );
    return false;
  }

  /** Fetch the plan for whatever is on screen, once per resolution. */
  async ensurePlan(): Promise<void> {
    if (!this.engine.isConnected) {
      return;
    }
    const open = this.product.current.open;
    const profile = this.product.current.profile;
    if (open === undefined || profile === undefined) {
      return;
    }
    const key = this.planKey();
    if (key === undefined) return;
    if (
      this.inFlight ||
      this.product.current.status !== "ready" ||
      (this.forKey === key && this.state.plan !== undefined)
    ) {
      return;
    }
    this.inFlight = true;
    const epoch = ++this.epoch;
    this.forKey = key;
    this.update({ status: "planning", plan: undefined, written: undefined, error: undefined });
    try {
      const plan = await this.planFor(open.path, profile);
      if (epoch !== this.epoch) return;
      this.update({ status: "ready", plan, error: undefined });
    } catch (error) {
      if (epoch !== this.epoch) return;
      this.update({ status: "error", error: messageOf(error) });
    } finally {
      this.inFlight = false;
    }
  }

  /**
   * Plan again for what is on screen, even though the resolution did not move.
   *
   * A product's templates are files the plan reads but the resolution does not:
   * deleting one to take the builtin back changes the plan and nothing else.
   */
  async replan(): Promise<void> {
    this.epoch += 1;
    this.forKey = undefined;
    this.inFlight = false;
    this.update({ plan: undefined, written: undefined });
    await this.ensurePlan();
  }

  /** Write the planned tree. No-op when a gate is closed. */
  async apply(): Promise<GenerateApplyResult | undefined> {
    if (!this.engine.isConnected) {
      this.messages.warn(this.engine.disconnectReason);
      return undefined;
    }
    if (!this.canApply) {
      this.messages.warn(this.blocks.map((b) => b.reason).join("; "));
      return undefined;
    }
    if (!this.planIsCurrent()) return undefined;
    const open = this.product.current.open;
    const profile = this.product.current.profile;
    if (open === undefined || profile === undefined) return undefined;
    try {
      const out = this.outFolder;
      const outcome = await this.service.applyGenerate(open.path, profile, out);
      this.update({
        plan: {
          plans: outcome.plans,
          diagnostics: outcome.diagnostics,
          out_root: this.state.plan?.out_root ?? "",
          // The apply recomputes this, so take its answer rather than the plan's:
          // a product whose `templates = path(...)` changed between the two would
          // otherwise keep reporting the overlay it no longer uses.
          overridden_templates: outcome.overridden_templates,
          // What the apply left in the product's templates directory.
          templates: outcome.templates,
        },
        written: outcome.written,
        repository: undefined,
      });
      if (out !== undefined && this.gitInit) {
        try {
          const repository = await this.service.initRepository(this.state.plan?.out_root ?? out);
          this.update({ repository });
        } catch (error) {
          // The files are written either way; the repository is a convenience
          // on top, and failing it must not read as a failed generation.
          this.messages.warn(`Generated, but git init failed: ${messageOf(error)}`);
        }
      }
      // An apply rewrites `product.lock` in the output tree, so the Lock view's
      // comparison against disk is now about the previous file. Nothing watches
      // `.gearbox/**` -- it is excluded from the file watcher on purpose -- so the
      // write has to say so itself.
      void this.product.refreshLock();
      return outcome;
    } catch (error) {
      this.messages.error(messageOf(error));
      return undefined;
    }
  }

  /** The two sides of one planned file. */
  async file(plan: FilePlan): Promise<GenerateFileResult> {
    const open = this.product.current.open;
    const profile = this.product.current.profile;
    if (open === undefined || profile === undefined) {
      throw new Error("open a product before asking for a generated file");
    }
    // The same check as `apply`, because the same mistake is available: a
    // `FilePlan` comes from `this.state.plan`, and resolving one against a
    // different product would show two sides of a file neither of them plans.
    if (this.planKey() !== this.forKey) {
      throw new Error("this plan describes a resolution that has since changed");
    }
    return this.service.generateFile(open.path, plan.path, profile, this.outFolder);
  }

  /**
   * Drop the plan when the resolution it describes is no longer on screen.
   *
   * Called from the widget on every store change. A plan for `dev` shown
   * against a `prod` resolution would be a lie.
   */
  forgetIfStale(): void {
    const key = this.planKey();
    if (key !== this.forKey) {
      this.epoch += 1;
      this.forKey = undefined;
      this.inFlight = false;
      this.state = EMPTY;
      this.onChangedEmitter.fire();
    }
  }

  protected update(patch: Partial<GenerateState>): void {
    this.state = { ...this.state, ...patch };
    this.onChangedEmitter.fire();
  }
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
