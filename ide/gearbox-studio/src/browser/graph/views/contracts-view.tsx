// The contract graph: which consumer reaches which provider, and where a process
// boundary can run between them.
//
// A co-location edge is link-time and can never be cut; a contract edge is the
// opposite, and this view is where the difference becomes a picture. Three states,
// and each one is the resolver's verdict rather than this file's opinion:
//
//   * **solid** -- `mode: local`, and the resolver reports no way to sever it.
//     The two gears are in the same binary and the call is a function call.
//   * **dashed** -- `mode: remote`. The resolver *did* cut here: a process
//     boundary runs through this edge and the call crosses it over `transport`.
//   * **red** -- the edge appears in `cuttable_if_declared` with
//     `blocked_by: undeclared-hub-edge`. Note which way that field points:
//     `CutCandidate` is "an edge the resolver would sever, but cannot", so these
//     are forced local *for a reason that is fixable in source*, which is why the
//     plan singles the hub edge out for red. Other blockers are drawn solid with
//     their reason named, because "cannot be cut, and here is why" is a different
//     statement from "should not be cut".
//
// Cut candidates are drawn even when no `ResolvedBinding` exists for the pair.
// An undeclared client lookup produces exactly that shape -- a real runtime edge
// with no declaration behind it -- and leaving it out would hide the one thing
// this view exists to surface (`cpt-gearbox-fr-report-cuttable-if-declared`).
//
// **No `BindingMechanism` literal appears here, and that is a constraint rather
// than a matter of style.** `mechanism` names the code path the runtime takes and
// the resolver derives it from placement; a client that renders one is consuming a
// projected fact, while a client that *branches* on one has taken over a resolver
// decision. So severability is looked up in the engine's own
// `cuttable_if_declared`, never inferred. `prd-studio.spec.ts` enforces this.

import React from "@theia/core/shared/react";

import type { CutCandidate } from "../../../common/generated/CutCandidate";
import type { ResolvedBinding } from "../../../common/generated/ResolvedBinding";
import {
  Edge,
  MARGIN,
  NODE_H,
  NODE_W,
  bandRuleY,
  elbow,
  graphSize,
  layoutLayered,
} from "../layered";

export interface ContractsViewProps {
  readonly bindings: readonly ResolvedBinding[];
  readonly candidates: readonly CutCandidate[];
  readonly profile: string | undefined;
}

/** One drawn edge, after the bindings and the candidates have been merged. */
interface ContractEdge extends Edge {
  readonly contracts: string[];
  readonly remote: boolean;
  readonly transports: string[];
  readonly critical: boolean;
  /** Whether a critical binding actually holds the consumer's readiness: only a
   * remote one resolved at run time does; a static endpoint does not. */
  readonly gating: boolean;
  readonly blockedBy: string | undefined;
}

export function ContractsView({
  bindings,
  candidates,
  profile,
}: ContractsViewProps): React.ReactElement {
  if (bindings.length === 0 && candidates.length === 0) {
    return (
      <div className="gbx-empty">
        This profile resolved no contract edges. A contract edge appears when one
        gear <code>#[toolkit::consumes]</code> a contract another{" "}
        <code>#[toolkit::provides]</code>, so a product whose gears only co-locate
        has none.
      </div>
    );
  }

  const edges = mergeEdges(bindings, candidates);
  const ids = [...new Set(edges.flatMap((e) => [e.from, e.to]))];
  const { placed } = layoutLayered(ids, edges);
  const at = new Map(placed.map((p) => [p.id, p]));
  const { width, height } = graphSize(placed);

  const severed = edges.filter((e) => e.remote).length;
  const hubBlocked = edges.filter((e) => e.blockedBy === "undeclared-hub-edge").length;
  const otherBlocked = edges.filter(
    (e) => e.blockedBy !== undefined && e.blockedBy !== "undeclared-hub-edge",
  ).length;

  return (
    <>
      <div className="gbx-header">
        An arrow is a contract edge, drawn as the resolver left it
        {profile ? (
          <>
            {" "}
            in profile <code>{profile}</code>
          </>
        ) : undefined}
        .{" "}
        <span className="gbx-legend">
          <span className="gbx-legend-item gbx-legend-local">solid</span> forced local,
          the call is a function call;{" "}
          <span className="gbx-legend-item gbx-legend-remote">dashed</span> severed, a
          process boundary runs through it;{" "}
          <span className="gbx-legend-item gbx-legend-bad">red</span> forced local by
          an undeclared hub edge, which source can fix.
        </span>{" "}
        {severed === 0 ? (
          <>
            Nothing is severed here: every edge is a function call inside one binary.
            A profile that splits applications is where the dashed edges appear.
          </>
        ) : (
          <>
            {severed} of {edges.length} edges are severed.
          </>
        )}
        {hubBlocked === 0 && (
          <> No edge is blocked by an undeclared hub edge, so nothing is red.</>
        )}
        {otherBlocked > 0 && (
          <>
            {" "}
            {otherBlocked} more would be severable but for a blocker source cannot
            remove; each is labelled with its reason.
          </>
        )}
      </div>
      <svg width={width} height={height} className="gbx-svg" data-graph="contracts">
        {placed.some((p) => p.isolated) && placed.some((p) => !p.isolated) && (
          <line
            className="gbx-band-rule"
            x1={MARGIN}
            y1={bandRuleY(placed)}
            x2={width - MARGIN}
            y2={bandRuleY(placed)}
          />
        )}

        {edges.map((edge) => {
          const from = at.get(edge.from);
          const to = at.get(edge.to);
          if (!from || !to) return undefined;
          const bad = edge.blockedBy === "undeclared-hub-edge";
          const state = bad
            ? "gbx-edge-bad"
            : edge.remote
              ? "gbx-edge-remote"
              : "gbx-edge-local";
          const marker = bad ? "gbx-arrow-bad" : edge.remote ? "gbx-arrow-cuttable" : "gbx-arrow";
          const x1 = from.x + NODE_W;
          const y1 = from.y + NODE_H / 2;
          const x2 = to.x;
          const y2 = to.y + NODE_H / 2;
          return (
            <g key={`${edge.from}:${edge.to}`}>
              <path
                className={`gbx-edge ${state}`}
                d={elbow(x1, y1, x2, y2)}
                markerEnd={`url(#${marker})`}
                data-from={edge.from}
                data-to={edge.to}
                data-remote={edge.remote ? "true" : "false"}
                data-blocked-by={edge.blockedBy ?? ""}
                data-contract={edge.contracts.join(" ")}
              >
                <title>{describe(edge)}</title>
              </path>
              <text className="gbx-edge-label" x={(x1 + x2) / 2} y={(y1 + y2) / 2 - 4}>
                {label(edge)}
              </text>
            </g>
          );
        })}

        {placed.map((p) => (
          <g
            key={p.id}
            transform={`translate(${p.x},${p.y})`}
            className="gbx-node-g"
            data-gear={p.id}
          >
            <rect width={NODE_W} height={NODE_H} rx={4} className="gbx-node" />
            <text x={8} y={19} className="gbx-node-label">
              {p.id}
            </text>
          </g>
        ))}
      </svg>
    </>
  );
}

/**
 * One drawn arrow per consumer/provider pair.
 *
 * Merged rather than one arrow per contract, because the demo product binds
 * `PaymentApi@v1` and `@v2` between the same two gears, and two arrows between one
 * pair of boxes overlap into an unreadable smear. The label carries both contracts.
 *
 * Merging loses nothing, and the reason is in the IR: `mode` is "derived from
 * placement, never configured". Two gears are either in one process or in two, so
 * every binding between the same pair agrees about `mode` -- there is no case where
 * one contract is local and another remote between the same boxes. `remote` is
 * still folded with `||` rather than taken from the first binding, because relying
 * on that invariant silently would make a future change to placement show up as a
 * wrong picture rather than as a failing test.
 */
function mergeEdges(
  bindings: readonly ResolvedBinding[],
  candidates: readonly CutCandidate[],
): ContractEdge[] {
  const merged = new Map<string, ContractEdge>();
  const key = (from: string, to: string): string => `${from} ${to}`;

  for (const binding of bindings) {
    const id = key(binding.consumer, binding.provider);
    const existing = merged.get(id);
    const remote = binding.mode === "remote";
    if (existing) {
      merged.set(id, {
        ...existing,
        contracts: [...existing.contracts, binding.contract].sort(),
        remote: existing.remote || remote,
        transports: [...new Set([...existing.transports, binding.transport])].sort(),
        critical: existing.critical || binding.critical,
        gating: existing.gating || gates(binding),
      });
    } else {
      merged.set(id, {
        from: binding.consumer,
        to: binding.provider,
        contracts: [binding.contract],
        remote,
        transports: [binding.transport],
        critical: binding.critical,
        gating: gates(binding),
        blockedBy: undefined,
      });
    }
  }

  for (const candidate of candidates) {
    const id = key(candidate.consumer, candidate.provider);
    const existing = merged.get(id);
    if (existing) {
      merged.set(id, { ...existing, blockedBy: candidate.blocked_by });
    } else {
      merged.set(id, {
        from: candidate.consumer,
        to: candidate.provider,
        contracts: candidate.contract ? [candidate.contract] : [],
        remote: false,
        transports: [],
        critical: false,
        gating: false,
        blockedBy: candidate.blocked_by,
      });
    }
  }

  return [...merged.values()].sort(
    (a, b) => a.from.localeCompare(b.from) || a.to.localeCompare(b.to),
  );
}

function label(edge: ContractEdge): string {
  const contracts = edge.contracts.map(shortContract);
  const head = contracts.length === 0 ? "undeclared" : contracts.join(", ");
  return edge.remote ? `${head} (${edge.transports.join(", ")})` : head;
}

/** `api-contracts/PaymentApi@v1` becomes `PaymentApi@v1`: the owner is the box it points at. */
function shortContract(contract: string): string {
  const slash = contract.indexOf("/");
  return slash === -1 ? contract : contract.slice(slash + 1);
}

/** The resolver's verdict, printed rather than derived (`cpt-gearbox-fr-studio`). */
function gates(binding: { gates_readiness: boolean }): boolean {
  return binding.gates_readiness;
}

function describe(edge: ContractEdge): string {
  const parts = [
    `${edge.from} to ${edge.to}`,
    edge.contracts.length === 0 ? "no declared contract" : edge.contracts.join(", "),
    edge.remote ? `severed, over ${edge.transports.join(", ")}` : "local: a function call",
  ];
  if (edge.gating) parts.push("critical: the consumer is not ready until it is reachable");
  else if (edge.critical && edge.remote)
    parts.push("critical, but a static endpoint does not hold readiness");
  if (edge.blockedBy) parts.push(`would be severable but for: ${edge.blockedBy}`);
  return parts.join("\n");
}
