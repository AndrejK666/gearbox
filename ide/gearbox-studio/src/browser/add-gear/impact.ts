// What adding a gear would do to the product, as the difference between two
// resolutions.
//
// **Arithmetic, not a second solver.** The engine resolves the proposed
// description (`gearbox/product/resolvePreview`, which writes nothing) and this
// subtracts the resolution already on screen from it. Reimplementing closure or
// binding rules in the client would be a second answer to a question that already
// has one, and the two would drift on the first engine change -- the mistake
// `cpt-gearbox-fr-studio: no resolution logic` exists to prevent.

import type { Diagnostic } from "../../common/generated/Diagnostic";
import { whyPresent } from "../product/inclusion";
import type { ResolvedBindingMode } from "../../common/generated/ResolvedBindingMode";
import type { ResolvedProduct } from "../../common/generated/ResolvedProduct";
import type { Transport } from "../../common/generated/Transport";

/** A gear the proposed description would bring in, and what put it there. */
export interface ArrivingGear {
  readonly id: string;
  /** `asked for` when the product names it, otherwise what pulled it in. */
  readonly why: string;
}

/** A binding whose mode or transport is not what it is today. */
export interface ChangedBinding {
  readonly consumer: string;
  /** The process this copy of the consumer runs in: a gear linked into several
   *  has a binding in each, and they can change in opposite directions. */
  readonly application: string;
  readonly contract: string;
  readonly before: string;
  readonly after: string;
}

export interface Impact {
  readonly arriving: readonly ArrivingGear[];
  readonly applicationsAdded: readonly string[];
  readonly applicationsRemoved: readonly string[];
  /** Gears that end up in a different application than they are in now. */
  readonly moved: readonly { readonly gear: string; readonly from: string; readonly to: string }[];
  readonly bindingsAdded: readonly ChangedBinding[];
  readonly bindingsChanged: readonly ChangedBinding[];
  /** Diagnostics the proposed resolution has and the current one does not. */
  readonly newDiagnostics: readonly Diagnostic[];
}

/**
 * How a contract is reached, as one phrase two resolutions can be compared by.
 *
 * The transport is left out when it is `local`, because "local over local" says
 * one thing twice. What a person needs from this line is the sentence "was local,
 * becomes remote over rest" -- the moment a call stops being a function call is
 * the reason this tool exists.
 */
function wiring(binding: { mode: ResolvedBindingMode; transport: Transport }): string {
  return binding.transport === "local"
    ? binding.mode
    : `${binding.mode} over ${binding.transport}`;
}

function bindingKey(binding: {
  consumer: string;
  contract: string;
  consumer_application: string;
}): string {
  // The `{consumer}|{contract}` the explanation graph keys bindings by, plus the
  // process: a gear linked into several has a binding in each, and comparing
  // only the pair would set one copy's wiring against another's.
  return `${binding.consumer}|${binding.contract}|${binding.consumer_application}`;
}

/** Which application each gear ends up in. A gear can appear in several; first wins. */
function placement(product: ResolvedProduct): Map<string, string> {
  const at = new Map<string, string>();
  for (const application of product.applications) {
    for (const gear of application.gears ?? []) {
      if (!at.has(String(gear))) at.set(String(gear), String(application.name));
    }
  }
  return at;
}

/**
 * The difference the proposed description would make.
 *
 * `before` is `undefined` when nothing is resolved yet -- then everything in the
 * proposal is arriving, which is the truthful answer rather than an empty diff.
 */
export function impactOf(
  before: ResolvedProduct | undefined,
  after: ResolvedProduct,
  beforeDiagnostics: readonly Diagnostic[],
  afterDiagnostics: readonly Diagnostic[],
): Impact {
  const had = new Set(Object.keys(before?.gears ?? {}));
  const arriving: ArrivingGear[] = Object.entries(after.gears)
    .filter(([id]) => !had.has(id))
    .map(([id, gear]) => ({ id, why: whyPresent(gear) }))
    .sort((a, b) => a.id.localeCompare(b.id));

  const hadProcesses = new Set((before?.applications ?? []).map((p) => String(p.name)));
  const hasProcesses = new Set(after.applications.map((p) => String(p.name)));
  const applicationsAdded = [...hasProcesses].filter((n) => !hadProcesses.has(n)).sort();
  const applicationsRemoved = [...hadProcesses].filter((n) => !hasProcesses.has(n)).sort();

  const wasAt = before === undefined ? new Map<string, string>() : placement(before);
  const willBe = placement(after);
  const moved = [...willBe.entries()]
    .filter(([gear, to]) => wasAt.has(gear) && wasAt.get(gear) !== to)
    .map(([gear, to]) => ({ gear, from: wasAt.get(gear) ?? "?", to }))
    .sort((a, b) => a.gear.localeCompare(b.gear));

  const wasBound = new Map<string, string>();
  for (const binding of before?.bindings ?? []) {
    wasBound.set(bindingKey(binding), wiring(binding));
  }
  const bindingsAdded: ChangedBinding[] = [];
  const bindingsChanged: ChangedBinding[] = [];
  for (const binding of after.bindings ?? []) {
    const key = bindingKey(binding);
    const now = wiring(binding);
    const then = wasBound.get(key);
    const row = {
      consumer: String(binding.consumer),
      application: String(binding.consumer_application),
      contract: String(binding.contract),
      before: then ?? "",
      after: now,
    };
    if (then === undefined) bindingsAdded.push(row);
    else if (then !== now) bindingsChanged.push(row);
  }

  // By code and message: the same complaint about the same thing is the same
  // diagnostic, and a resolution that merely re-reports it has changed nothing.
  const had_ = new Set(beforeDiagnostics.map((d) => `${d.code}|${d.message}`));
  const newDiagnostics = afterDiagnostics.filter(
    (d) => !had_.has(`${d.code}|${d.message}`),
  );

  return {
    arriving,
    applicationsAdded,
    applicationsRemoved,
    moved,
    bindingsAdded,
    bindingsChanged,
    newDiagnostics,
  };
}
