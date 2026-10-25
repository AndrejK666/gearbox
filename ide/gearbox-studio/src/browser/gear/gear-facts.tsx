// What a gear is, for whichever surface is showing one.
//
// **Extracted rather than copied, and the docs row is the reason.** `GearDocs`
// drags `adrLabel` with it -- the rule that a nine-ADR gear is listed as
// `ADR 001` rather than as a filename that wraps to three lines -- and a second
// copy of that rule in another file is how the two come to disagree about the
// same gear.
//
// **Only the parts two surfaces want.** The Inspector's capabilities, extension
// points, `provides` and GTS types stay in the Inspector: those are facts about
// the gear in the *catalogue*, and the settings pane is answering about one
// object in one product. What both need is the same three things -- what it is
// called, what it is for, and where to read more -- plus, for the pane, why it
// is here at all.

import React from "@theia/core/shared/react";

import type { GearDescriptor } from "../../common/generated/GearDescriptor";
import type { Implementation } from "../../common/generated/Implementation";
import type { Maturity } from "../../common/generated/Maturity";
import type { InclusionReason } from "../../common/generated/InclusionReason";
import { describeInclusion } from "../product/inclusion";
import { RevealLink } from "../reveal-link";
import type { RevealService } from "../reveal-service";

/**
 * The display name with the id beside it.
 *
 * The markup the Inspector's detail block has always emitted, so that a claim
 * reading `.gbx-detail-title` keeps matching wherever a gear is named.
 */
/** What each level below `stable` says, as the tooltip and the screen-reader text. */
const MATURITY_MEANS: Record<Exclude<Maturity, "stable">, string> = {
  experimental: "experimental: its API and behaviour may change freely",
  preview: "preview: usable, but not declared stable",
  deprecated: "deprecated: still available, not for new products",
};

/**
 * The gear's maturity, shown only below `stable`.
 *
 * Nothing for `stable`: a badge on every settled gear would be the noise that
 * hides the ones that are not. One component for the catalogue row, the Add
 * Gear list and the heading, so the three cannot say it three ways.
 */
export function MaturityBadge({ maturity }: { readonly maturity: Maturity }): React.ReactElement | null {
  if (maturity === "stable") return null;
  return (
    <span
      className={`gbx-badge gbx-maturity gbx-maturity-${maturity}`}
      data-maturity={maturity}
      title={MATURITY_MEANS[maturity]}
    >
      {maturity}
    </span>
  );
}

/** What each value says, as the tooltip. `design` never reaches a gear with code. */
const IMPLEMENTATION_MEANS: Record<Exclude<Implementation, "design">, string> = {
  "in-progress": "in progress: code exists and work on it continues",
  done: "done: everything the gear sets out to do is written",
};

/**
 * How much of the gear is written, beside what it promises.
 *
 * The other axis from maturity, so a separate badge: a `stable` gear can be in
 * progress, and an `experimental` one done. Nothing when the description does
 * not say -- absent is not a claim either way.
 */
export function ImplementationBadge({
  implementation,
}: {
  readonly implementation?: Implementation | null;
}): React.ReactElement | null {
  if (implementation === undefined || implementation === null || implementation === "design") {
    return null;
  }
  return (
    <span
      className={`gbx-badge gbx-implementation gbx-implementation-${implementation}`}
      data-implementation={implementation}
      title={IMPLEMENTATION_MEANS[implementation]}
    >
      {implementation}
    </span>
  );
}

export function GearHeading({
  id,
  descriptor,
}: {
  readonly id: string;
  readonly descriptor?: GearDescriptor;
}): React.ReactElement {
  return (
    <div className="gbx-detail-title">
      {descriptor?.display_name || id} <span className="gbx-id">{id}</span>
      {descriptor !== undefined && <MaturityBadge maturity={descriptor.maturity} />}
      {descriptor !== undefined && <ImplementationBadge implementation={descriptor.implementation} />}
    </div>
  );
}

/** What the gear is for, in the author's own words. */
export function GearBlurb({
  descriptor,
}: {
  readonly descriptor?: GearDescriptor;
}): React.ReactElement {
  return (
    <div className="gbx-kv">
      <span>description</span>
      <span>{descriptor?.description ?? "—"}</span>
    </div>
  );
}

/**
 * The gear's own documents, as links the opener handles.
 *
 * Renders nothing at all when the descriptor declares none: an empty `docs` row
 * is a row saying "this gear has no documentation", which is a different claim
 * from "none was declared here".
 */
export function GearDocs({
  descriptor,
  reveals,
}: {
  // Only where the documents are, so a design gear -- which has documents
  // and nothing projected -- renders through the same component.
  readonly descriptor?: Pick<GearDescriptor, "docs" | "source">;
  readonly reveals: RevealService;
}): React.ReactElement | null {
  const docs = descriptor?.docs;
  if (!descriptor || !docs) return null;
  const adrs = docs.adr ?? [];
  const urls = docs.urls ?? {};
  const remoteAdrs = urls.adr ?? [];
  const local = docs.prd || docs.design || adrs.length > 0;
  const remote = urls.prd || urls.design || urls.openapi || remoteAdrs.length > 0;
  if (!local && !remote) return null;
  const link = (label: string, target: string | null | undefined): React.ReactNode =>
    target === null || target === undefined ? undefined : (
      <RevealLink
        key={target}
        reveals={reveals}
        source={descriptor.source}
        target={target}
        label={label}
      />
    );
  return (
    <div className="gbx-kv">
      <span>docs</span>
      <span className="gbx-links">
        {link("PRD", docs.prd)}
        {link("DESIGN", docs.design)}
        {adrs.map((adr) => link(adrLabel(adr), adr))}
        <WebLink label="PRD" url={urls.prd} />
        <WebLink label="DESIGN" url={urls.design} />
        <WebLink label="OpenAPI" url={urls.openapi} />
        {remoteAdrs.map((adr) => (
          <WebLink key={adr} label={adrLabel(adr)} url={adr} />
        ))}
      </span>
    </div>
  );
}

/**
 * A document or test that is not on disk here: a URL the description wrote, or
 * the same path in the gear's git source. Opened by the browser, not the editor.
 */
export function WebLink({
  label,
  url,
}: {
  readonly label: string;
  readonly url?: string | null;
}): React.ReactElement | null {
  if (url === undefined || url === null) return null;
  return (
    <a className="gbx-web-link" href={url} target="_blank" rel="noreferrer" title={url}>
      {label} ↗
    </a>
  );
}

/**
 * Where the gear is and what comes with it: its git source, its SDKs in every
 * language, its tests, and its scenarios. Each row only when the description
 * says; a design gear has no tests.
 */
export function GearLocations({
  descriptor,
  reveals,
}: {
  readonly descriptor: Pick<GearDescriptor, "sdks" | "origin" | "scenarios" | "source"> &
    Partial<Pick<GearDescriptor, "tests">>;
  readonly reveals: RevealService;
}): React.ReactElement {
  const sdks = descriptor.sdks ?? [];
  const tests = descriptor.tests ?? [];
  const scenarios = descriptor.scenarios ?? [];
  const origin = descriptor.origin;
  const done = scenarios.filter((scenario) => scenario.done).length;
  return (
    <>
      {origin !== undefined && origin !== null && (
        <div className="gbx-kv">
          <span>source</span>
          <span>
            <WebLink label={origin.url} url={webBase(origin.url)} /> <code>{origin.path}</code>
            {origin.reference !== undefined && origin.reference !== null && (
              <> @ <code>{origin.reference}</code></>
            )}
          </span>
        </div>
      )}
      {sdks.length > 0 && (
        <div className="gbx-kv">
          <span>sdks</span>
          <span>
            {sdks.map((sdk) => (
              <div key={`${sdk.ecosystem}:${sdk.name}`} data-ecosystem={sdk.ecosystem}>
                {sdk.ecosystem} <code>{sdk.name}</code>
              </div>
            ))}
          </span>
        </div>
      )}
      {tests.length > 0 && (
        <div className="gbx-kv">
          <span>tests</span>
          <span className="gbx-links">
            {tests.map((test, index) =>
              test.path !== undefined && test.path !== null ? (
                <RevealLink
                  key={test.path}
                  reveals={reveals}
                  source={descriptor.source}
                  target={test.path}
                  label={basename(test.path)}
                />
              ) : (
                <WebLink key={index} label={basename(test.url ?? "")} url={test.url} />
              ),
            )}
          </span>
        </div>
      )}
      {scenarios.length > 0 && (
        <div className="gbx-kv">
          <span>scenarios</span>
          <span>
            <div className="gbx-scenarios-summary">
              {done} of {scenarios.length} done
            </div>
            {scenarios.map((scenario, index) => (
              <div key={index} className="gbx-scenario" data-done={scenario.done}>
                {scenario.done ? "✓" : "○"}{" "}
                {scenario.priority !== undefined && scenario.priority !== null && (
                  <code>{scenario.priority}</code>
                )}{" "}
                {scenario.text}
              </div>
            ))}
          </span>
        </div>
      )}
    </>
  );
}

/**
 * `git@host:org/repo.git` as `https://host/org/repo`, for a browser.
 *
 * The same rule the engine uses for its links (`GitOrigin::web_url`); only the
 * base is needed here, to open the repository.
 */
export function webBase(url: string): string {
  let trimmed = url.trim().replace(/\/+$/, "").replace(/\.git$/, "");
  const scheme = /^([a-z+]+):\/\/(?:[^@/]+@)?(.*)$/.exec(trimmed);
  if (scheme) {
    return `${scheme[1]!.startsWith("http") ? scheme[1] : "https"}://${scheme[2]}`;
  }
  trimmed = trimmed.replace(/^[^@]+@/, "");
  return `https://${trimmed.replace(":", "/")}`;
}

/**
 * Every reason this gear is in the product, as sentences.
 *
 * A co-located reason additionally *goes* to the gear it names -- saying which
 * gear requires this one is only half an answer if reaching it means finding it
 * again in the tree. The other two kinds are statements, so they are text.
 */
export function InclusionReasons({
  reasons,
  onSelectGear,
}: {
  readonly reasons: readonly InclusionReason[];
  readonly onSelectGear?: (id: string) => void;
}): React.ReactElement | null {
  if (reasons.length === 0) return null;
  return (
    <div className="gbx-kv">
      <span>included because</span>
      <span>
        {reasons.map((reason, index) => (
          <span className="gbx-leaf-why" key={index} data-inclusion={reason.reason}>
            {reason.reason === "colocated_by" && onSelectGear !== undefined ? (
              <button
                type="button"
                className="gbx-choice"
                data-inclusion-gear={reason.gear}
                onClick={() => onSelectGear(reason.gear)}
              >
                {describeInclusion(reason)}
              </button>
            ) : (
              describeInclusion(reason)
            )}
          </span>
        ))}
      </span>
    </div>
  );
}

/**
 * `ADR 001` rather than `001-provider-compatibility-and-performance.md`.
 *
 * `cluster` has nine ADRs and `types-registry` fifteen, with names long enough
 * that the full filenames wrapped to three lines and read as a paragraph rather
 * than as a list. The number is the part anyone actually cites; the filename
 * stays in the link's tooltip.
 */
export function adrLabel(target: string): string {
  const file = basename(target);
  const numbered = /^(\d+)/.exec(file);
  return numbered ? `ADR ${numbered[1]}` : file.replace(/\.md$/, "");
}

export function basename(target: string): string {
  const parts = target.split("/");
  return parts[parts.length - 1] ?? target;
}
