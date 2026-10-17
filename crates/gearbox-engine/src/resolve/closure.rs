//! Step 2: the co-location closure.
//!
//! This is the step that carries the project's most consequential finding. A
//! gear's `deps` are link-time: the gear macro emits a hidden re-export per
//! entry and the registry treats a missing one as a hard `MissingDeps` failure,
//! so a process containing a gear contains everything that gear reaches. The set
//! is therefore a **closure, not a partition** -- two processes may legitimately
//! share gears, and no resolver decision can sever an edge inside one.
//!
//! Breadth-first from the selected gears, popping in sorted order, so the answer
//! is the same on every run and on every machine.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use gearbox_ir::{
    Catalogue, Diagnostic, DiagnosticCode, Diagnostics, GearId, InclusionReason, Location,
    ProductIntent,
};

/// Every gear in the product, with why each is here.
#[derive(Debug, Default)]
pub struct Closure {
    /// Reasons per gear, sorted and deduplicated. A gear can be both selected
    /// directly and pulled in by someone else, and both are worth keeping: the
    /// answer to "why is this here" is "for two reasons", not one of them.
    pub members: BTreeMap<GearId, Vec<InclusionReason>>,
}

impl Closure {
    #[must_use]
    pub fn contains(&self, gear: &GearId) -> bool {
        self.members.contains_key(gear)
    }

    #[must_use]
    pub fn ids(&self) -> BTreeSet<GearId> {
        self.members.keys().cloned().collect()
    }
}

/// Expand the selected gears over `colocated_deps`.
///
/// Unknown gears are reported and skipped rather than aborting: a product naming
/// one gear that does not exist should still resolve the rest, so the operator
/// sees every problem at once instead of one per run.
///
/// `uri` is the product description's URI, passed in rather than derived here:
/// `intent.gdl_path` is relative to its root, and a relative path in a `file://`
/// URI reads its first segment as the host. `resolve_at` is the one place that
/// knows the real path.
pub fn expand(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    profile: &gearbox_ir::ProfileId,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Closure {
    let mut closure = Closure::default();
    let mut queue: VecDeque<GearId> = VecDeque::new();

    // Seed from the selection, in sorted order so the walk is reproducible.
    let selected: BTreeSet<GearId> = intent
        .selected_gears
        .iter()
        .map(|s| s.gear.clone())
        .collect();
    for gear in &selected {
        if !catalogue.gears.contains_key(gear) {
            // `selected` is a deduped set of ids, so the selection is looked up
            // again here. The *first* `use_gear` naming this gear, when two
            // profiles each name it: both are equally at fault, and underlining
            // one of them beats underlining the file.
            let declared_at = intent
                .selected_gears
                .iter()
                .find(|s| &s.gear == gear)
                .and_then(|s| s.declared_at.clone());
            let at = gearbox_ir::Location::or_file(declared_at.as_ref(), uri);
            diagnostics
                .push(at_design(catalogue, gear, &at).unwrap_or_else(|| unknown(gear, None, at)));
            continue;
        }
        closure
            .members
            .entry(gear.clone())
            .or_default()
            .push(InclusionReason::Selected);
        queue.push_back(gear.clone());
    }

    // Plugins are seeds too, and they are the only thing that makes the seed set
    // depend on the profile: a `plugin(..., profiles = [...])` selection puts a
    // crate in one profile's binary and not another's. Every selected plugin is
    // seeded, not only the one that wins the vendor match -- the registry links
    // them all and the host chooses at runtime, so a losing plugin is still in
    // the binary and still pulls its own co-location closure in with it.
    for selection in &intent.selected_gears {
        for plugin in &selection.plugins {
            if !applies(&plugin.profiles, profile) {
                continue;
            }
            if !catalogue.gears.contains_key(&plugin.gear) {
                let at = gearbox_ir::Location::or_file(selection.declared_at.as_ref(), uri);
                diagnostics.push(
                    at_design(catalogue, &plugin.gear, &at)
                        .unwrap_or_else(|| unknown_plugin(&plugin.gear, &selection.gear, at)),
                );
                continue;
            }
            let reason = InclusionReason::PluginOf {
                host: selection.gear.clone(),
                profile: profile.clone(),
            };
            let entry = closure.members.entry(plugin.gear.clone()).or_default();
            let first_visit = entry.is_empty();
            if !entry.contains(&reason) {
                entry.push(reason);
            }
            if first_visit {
                queue.push_back(plugin.gear.clone());
            }
        }
    }

    while let Some(current) = queue.pop_front() {
        let Some(descriptor) = catalogue.gears.get(&current) else {
            continue;
        };
        for dep in &descriptor.colocated_deps {
            if !catalogue.gears.contains_key(dep) {
                let at = Location::file(uri.to_owned());
                diagnostics.push(
                    at_design(catalogue, dep, &at)
                        .unwrap_or_else(|| unknown(dep, Some(&current), at)),
                );
                continue;
            }
            let reason = InclusionReason::ColocatedBy {
                gear: current.clone(),
            };
            let entry = closure.members.entry(dep.clone()).or_default();
            let first_visit = entry.is_empty();
            if !entry.contains(&reason) {
                entry.push(reason);
            }
            // Enqueue on first visit only. A second reason for a gear already in
            // the closure adds provenance, not new frontier.
            if first_visit {
                queue.push_back(dep.clone());
            }
        }
    }

    for reasons in closure.members.values_mut() {
        reasons.sort();
        reasons.dedup();
    }

    detect_cycle(catalogue, &closure, uri, diagnostics);
    closure
}

/// Every gear the description names for this profile, plugins included.
///
/// Shared with the partition rather than computed twice. The two disagreeing is
/// exactly what produced an orphaned plugin: the closure knew about it and the
/// process seeds did not, so it was in the product and in no binary.
#[must_use]
pub fn seeds(intent: &ProductIntent, profile: &gearbox_ir::ProfileId) -> BTreeSet<GearId> {
    let mut out: BTreeSet<GearId> = BTreeSet::new();
    for selection in &intent.selected_gears {
        out.insert(selection.gear.clone());
        for plugin in &selection.plugins {
            if applies(&plugin.profiles, profile) {
                out.insert(plugin.gear.clone());
            }
        }
    }
    out
}

/// Whether a `profiles = [...]` list admits this profile. Empty means all.
fn applies(
    profiles: &std::collections::BTreeSet<gearbox_ir::ProfileId>,
    profile: &gearbox_ir::ProfileId,
) -> bool {
    profiles.is_empty() || profiles.contains(profile)
}

/// GBX0321, when a gear absent from `gears` is a design gear.
///
/// Asked before "unknown gear" at every place a gear id fails to resolve:
/// GBX0301 says to look for a typo or a closed source root, and for a design
/// gear neither is the answer. Shared with `validate`, which checks
/// selections without resolving.
pub(crate) fn at_design(
    catalogue: &Catalogue,
    gear: &GearId,
    at: &gearbox_ir::Location,
) -> Option<Diagnostic> {
    let design = catalogue.designs.get(gear)?;
    Some(
        Diagnostic::error(
            DiagnosticCode::TopologyDesignGear,
            format!(
                "`{gear}` is at design maturity: it is described, and there is no crate to \
                 build yet"
            ),
            format!(
                "`{}` in source `{}` says `implementation = \"design\"`; leave the gear out of the \
                 product until it has code{}",
                design.gdl_path,
                design.source,
                design
                    .docs
                    .as_ref()
                    .and_then(|d| d.prd.as_ref().or(d.design.as_ref()))
                    .map(|doc| format!(", and see `{doc}` for what is planned"))
                    .unwrap_or_default()
            ),
        )
        .at(at.clone()),
    )
}

/// GBX0322-0324: one diagnostic per gear in `members` below `stable`.
///
/// Every member, not only the selected ones: a co-located dependency or a
/// plugin is linked into the same binary, and "nobody chose it" is not a
/// reason it promises more. Each is anchored where a reader can act -- the
/// `use_gear(...)` that selected it, or that selected its host -- and the
/// file otherwise, with the reason it is there in the message.
///
/// Shared with `validate`, which passes only the selections because it does
/// not resolve.
pub(crate) fn maturity_diagnostics<'m>(
    catalogue: &Catalogue,
    members: impl IntoIterator<Item = (&'m GearId, &'m [InclusionReason])>,
    intent: &ProductIntent,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let selected_at = |gear: &GearId| {
        intent
            .selected_gears
            .iter()
            .find(|s| &s.gear == gear)
            .and_then(|s| s.declared_at.clone())
    };
    for (gear, reasons) in members {
        let Some(descriptor) = catalogue.gears.get(gear) else {
            continue;
        };
        let (code, what, help) = match descriptor.maturity {
            gearbox_ir::Maturity::Stable => continue,
            gearbox_ir::Maturity::Experimental => (
                DiagnosticCode::TopologyExperimentalGear,
                "experimental: its API and behaviour may change freely",
                "pin the gear's version, or wait for its owners to raise it to `preview`",
            ),
            gearbox_ir::Maturity::Preview => (
                DiagnosticCode::TopologyPreviewGear,
                "at preview: usable, but not declared stable",
                "nothing to do for now; this says what the product depends on",
            ),
            gearbox_ir::Maturity::Deprecated => (
                DiagnosticCode::TopologyDeprecatedGear,
                "deprecated: still available, not for new products",
                "see the gear's documents for what replaces it",
            ),
        };
        let mut why = Vec::new();
        let mut at = None;
        for reason in reasons {
            match reason {
                InclusionReason::Selected => at = at.or_else(|| selected_at(gear)),
                InclusionReason::ColocatedBy { gear: by } => {
                    why.push(format!("co-located by `{by}`"));
                }
                InclusionReason::PluginOf { host, .. } => {
                    why.push(format!("a plugin of `{host}`"));
                    at = at.or_else(|| selected_at(host));
                }
            }
        }
        let message = if why.is_empty() {
            format!("`{gear}` is {what}")
        } else {
            format!("`{gear}` ({}) is {what}", why.join(", "))
        };
        diagnostics.push(
            Diagnostic::new(code, message)
                .with_help(help)
                .at(gearbox_ir::Location::or_file(at.as_ref(), uri)),
        );
    }
}

/// A plugin named under a host that the catalogue does not have.
///
/// Anchored on the host's `use_gear(...)`, which is the call the `plugins = [...]`
/// entry sits inside.
fn unknown_plugin(plugin: &GearId, host: &GearId, at: gearbox_ir::Location) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::TopologyUnknownGear,
        format!("`{host}` selects the plugin `{plugin}`, which is not in the catalogue"),
        "a plugin is an ordinary gear with its own `gear.gdl`; either it has none yet, or its \
         source root is not open. `gearbox validate --product ...` says which",
    )
    .at(at)
}

/// `at` is decided by the caller, because the two cases this serves are about
/// different files. A `use_gear("x")` naming nothing is about the description and
/// gets its call span; a co-location dependency naming nothing is projected from
/// `#[toolkit::gear(deps = [...])]` in Rust, so no `.gdl` span is true of it and
/// the caller passes the file.
fn unknown(gear: &GearId, pulled_by: Option<&GearId>, at: gearbox_ir::Location) -> Diagnostic {
    let message = match pulled_by {
        Some(by) => format!(
            "`{by}` declares a co-location dependency on `{gear}`, which is not in the catalogue"
        ),
        None => format!("`use_gear(\"{gear}\")` names a gear that is not in the catalogue"),
    };
    let help = match pulled_by {
        Some(_) => "the dependency is projected from `#[toolkit::gear(deps = [...])]`, so either \
                    the named gear has no `gear.gdl` or its source root is not open"
            .to_owned(),
        None => "run `gearbox catalogue` to list the ids the open sources declare, or \
                 `gearbox validate --product ...`, which distinguishes a typo from a gear \
                 nobody has described yet"
            .to_owned(),
    };
    Diagnostic::error(DiagnosticCode::TopologyUnknownGear, message, help).at(at)
}

/// Report a cycle among co-location edges.
///
/// The runtime's own topological sort rejects one at startup, so a cycle here
/// means the product cannot boot. Detecting it during the walk would be cheaper,
/// but a separate pass can name the whole cycle rather than the edge that closed
/// it -- and the cycle is what a reader has to break.
fn detect_cycle(
    catalogue: &Catalogue,
    closure: &Closure,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }

    /// Iterative depth-first search: the recursion depth would otherwise follow
    /// the tree's depth, and a malformed catalogue is exactly when that is
    /// unbounded.
    enum Step {
        Enter(GearId),
        Leave(GearId),
    }

    let mut marks: BTreeMap<GearId, Mark> = BTreeMap::new();
    let mut path: Vec<GearId> = Vec::new();
    let mut reported: BTreeSet<Vec<GearId>> = BTreeSet::new();

    for root in closure.members.keys() {
        if marks.get(root) == Some(&Mark::Done) {
            continue;
        }
        let mut stack = vec![Step::Enter(root.clone())];
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(gear) => {
                    match marks.get(&gear) {
                        Some(Mark::Done) => continue,
                        Some(Mark::Open) => {
                            // Found the back edge; the cycle is the path from the
                            // earlier occurrence to here.
                            if let Some(start) = path.iter().position(|g| *g == gear) {
                                let mut cycle: Vec<GearId> = path[start..].to_vec();
                                cycle.push(gear.clone());
                                if reported.insert(cycle.clone()) {
                                    diagnostics.push(cycle_diagnostic(&cycle, uri));
                                }
                            }
                            continue;
                        }
                        None => {}
                    }
                    marks.insert(gear.clone(), Mark::Open);
                    path.push(gear.clone());
                    stack.push(Step::Leave(gear.clone()));
                    if let Some(descriptor) = catalogue.gears.get(&gear) {
                        // Reversed so the sorted order is preserved once popped.
                        for dep in descriptor.colocated_deps.iter().rev() {
                            if closure.contains(dep) {
                                stack.push(Step::Enter(dep.clone()));
                            }
                        }
                    }
                }
                Step::Leave(gear) => {
                    marks.insert(gear.clone(), Mark::Done);
                    path.pop();
                }
            }
        }
    }
}

fn cycle_diagnostic(cycle: &[GearId], uri: &str) -> Diagnostic {
    let rendered = cycle
        .iter()
        .map(GearId::to_string)
        .collect::<Vec<_>>()
        .join(" -> ");
    Diagnostic::error(
        DiagnosticCode::TopologyDepsCycle,
        format!("co-location dependencies form a cycle: {rendered}"),
        "the runtime's registry sorts gears topologically at startup and refuses a cycle, so \
         this product cannot boot; break the loop by removing one `deps` entry, which means \
         the gear that no longer depends must reach the other through a contract instead",
    )
    .at(Location::file(uri.to_owned()))
}
