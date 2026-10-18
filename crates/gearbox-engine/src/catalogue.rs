//! Finding `gear.gdl` files and assembling them into a [`Catalogue`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gearbox_gdl::GdlEngine;
use gearbox_gdl::engine::FileIdentity;
use gearbox_ir::{
    Catalogue, ContractDescriptor, ContractId, DesignGear, Diagnostic, DiagnosticCode, Diagnostics,
    GearDescriptor, GearId, LoadStage, Location, PendingGear, RelPath,
};

use crate::source::SourceRoot;

/// The name a gear's description must have, so discovery is a filename match
/// rather than a heuristic.
/// Re-exported from `gearbox_gdl`, not declared here.
///
/// Two copies of the name meant the engine and the language server could
/// disagree about which files are descriptions: a rename would have given one
/// of them diagnostics and the other silence.
pub use gearbox_gdl::GEAR_FILE;

/// Directory names never worth descending into.
///
/// `target` alone would save most of the time, but a `node_modules` under
/// `ide/` and a `.git` object store are both large enough to matter and can
/// never contain a gear description.
const SKIP_DIRS: &[&str] = &["target", "node_modules", ".git", ".gearbox"];

/// A catalogue plus everything that went wrong building it.
#[derive(Debug)]
pub struct CatalogueScan {
    pub catalogue: Catalogue,
    /// The description files that were read, in the order they were evaluated.
    pub files: Vec<PathBuf>,
    /// How many distinct crates were parsed.
    ///
    /// Reported because the cache is otherwise invisible: the catalogue is
    /// identical with or without it, which is the point, so this is the only
    /// thing a test can hold on to.
    pub crates_scanned: usize,
    /// How many scans were asked for, cache hits included.
    ///
    /// The gap between this and `crates_scanned` is the parsing avoided.
    pub scan_requests: usize,
    /// Gears declared but not projected.
    ///
    /// **Empty when a load runs to completion.** Non-empty only when the event
    /// callback asked to stop, which is what makes a cancelled load degrade to a
    /// smaller catalogue rather than to none. If this is ever non-empty after an
    /// uninterrupted load, gears are going missing silently.
    pub pending: Vec<PendingGear>,
}

/// What happened during a staged load.
///
/// Borrowed rather than owned: an event is handed to the callback while the load
/// still holds the value, so forwarding one over RPC means serializing it, not
/// taking it. That keeps the loader from paying for clones a caller may not want.
#[derive(Debug)]
pub enum LoadEvent<'a> {
    /// Discovery finished. `total` descriptions were found across all roots.
    ///
    /// First and once, so a progress bar has a denominator before any work that
    /// could take a while.
    Discovered { total: usize },

    /// One description was evaluated. Its declared facts are now known.
    Declared(&'a PendingGear),

    /// One description was evaluated and is a design gear, which is complete
    /// as declared: it has no crate, so it is never pending and never
    /// `Projected`. Sent during the first pass, before `DeclarationComplete`.
    Design(&'a DesignGear),

    /// Every description has been evaluated; parsing is about to begin.
    ///
    /// The boundary between the two passes, and a real one rather than a
    /// convenience: it is the moment a registry view has its whole shape and
    /// none of its badges. A consumer that needs to answer "the tree is ready"
    /// would otherwise have to infer it from the first `Projected`, which does
    /// not arrive at all when every gear fails to project.
    ///
    /// Carries the diagnostics raised so far, because a consumer that answers a
    /// request at this boundary has no other chance to report them: the scan
    /// they end up in is only returned when the whole load finishes.
    DeclarationComplete {
        declared: usize,
        diagnostics: &'a [Diagnostic],
    },

    /// One gear finished projection and entered the catalogue.
    Projected(&'a GearDescriptor),

    /// A gear already sent as `Projected` changed once every gear was known.
    ///
    /// A plugin's `implements` names only a spec; the point it joins is its host's
    /// declaration, and the host may be described later in discovery order. So
    /// the join runs after the last projection, and the plugins it completes
    /// are sent again. Not counted as progress: the gear was counted once.
    Joined(&'a GearDescriptor),
}

/// Whether the load should keep going.
///
/// Returned by the event callback, which is what makes `$/cancelRequest`
/// implementable without the loader knowing anything about RPC. Stopping leaves
/// what is already projected valid and the rest in `CatalogueScan::pending`,
/// rather than discarding the work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Continue {
    Yes,
    Stop,
}

/// Find and evaluate every `gear.gdl` under each root, in one catalogue.
///
/// Discovery is sorted before evaluation, so the catalogue does not depend on
/// the order the filesystem happens to hand back directory entries
/// (`cpt-gearbox-nfr-determinism`). Ordered maps do the rest.
///
/// A file that fails to evaluate contributes its diagnostics and is otherwise
/// skipped: one malformed description should not hide the other seven.
#[must_use]
pub fn load_catalogue(roots: &[SourceRoot]) -> CatalogueScan {
    load_catalogue_staged(roots, &mut |_| Continue::Yes)
}

/// The same load, reporting each stage as it completes.
///
/// Two passes, not one per gear, and the order is the point: **every description
/// is evaluated before any crate is parsed.** A registry view can therefore show
/// its whole shape -- names grouped by category -- while the expensive stage is
/// still running, instead of watching rows trickle in one complete gear at a
/// time. See ADR `cpt-gearbox-adr-staged-catalogue-loading` for the measured
/// costs; the second pass is roughly ten times the first on a real tree.
///
/// Synchronous, with callbacks on the calling thread. Source text is read in
/// bounded parallel batches inside the scanner; AST projection and event delivery
/// remain ordered. The engine adds no parallelism dependency.
#[must_use]
pub fn load_catalogue_staged(
    roots: &[SourceRoot],
    on_event: &mut dyn FnMut(LoadEvent<'_>) -> Continue,
) -> CatalogueScan {
    let gdl = GdlEngine::new();
    let mut catalogue = Catalogue::default();
    let mut contracts = ContractMerge::default();
    // What each gear's crate implements, kept aside for the plugin join below:
    // evidence a declared `implements` is checked against, not part of the descriptor.
    let mut implemented: BTreeMap<gearbox_ir::GearId, std::collections::BTreeSet<String>> =
        BTreeMap::new();
    let mut diagnostics = Diagnostics::new();
    // One cache per load: a crate named by several gears is parsed once.
    let mut scans = crate::scans::CrateScans::new();

    for root in roots {
        catalogue
            .sources
            .insert(root.id.clone(), root.to_resolved());
    }

    // ---- S0: discover -------------------------------------------------------
    let discovered = discover_all(roots, &mut diagnostics);

    // The digest of each source, now that discovery knows what is under it.
    //
    // Here rather than in `SourceRoot::open`: a digest is a statement about
    // content, and this is the first moment anything has looked. Doing it at
    // `open` would also walk every root twice -- once to hash, once to discover --
    // and would have to invent an answer for a root whose walk failed.
    //
    // Set even when the load is about to stop below, because a caller that
    // stopped still holds a catalogue and its sources should not claim `unread`
    // for a root that was in fact read.
    for root in roots {
        let under: Vec<PathBuf> = discovered
            .iter()
            .filter(|(r, _)| r.id == root.id)
            .map(|(_, path)| path.clone())
            .collect();
        if let Some(source) = catalogue.sources.get_mut(&root.id) {
            source.digest = crate::source::content_digest(&root.root, &under);
        }
    }

    let files: Vec<PathBuf> = discovered.iter().map(|(_, p)| p.clone()).collect();
    if on_event(LoadEvent::Discovered {
        total: discovered.len(),
    }) == Continue::Stop
    {
        diagnostics.finish();
        catalogue.diagnostics = diagnostics;
        return CatalogueScan {
            catalogue,
            files,
            crates_scanned: 0,
            scan_requests: 0,
            pending: Vec::new(),
        };
    }

    // ---- S1: evaluate every description ------------------------------------
    let mut declared: Vec<(&SourceRoot, FileIdentity, gearbox_gdl::GearDecl)> = Vec::new();
    let mut pending: Vec<PendingGear> = Vec::new();
    let mut stopped = false;

    for (root, path) in &discovered {
        let Some(identity) = identity_for(root, path, &mut diagnostics) else {
            continue;
        };

        let Some(source) = read_description(path, &identity, &mut diagnostics) else {
            continue;
        };

        let outcome = gdl.eval_gear(&identity, &source);
        diagnostics.extend(outcome.diagnostics);
        let Some(decl) = outcome.value else { continue };

        if decl.is_design() {
            let admitted = admit_design(
                root,
                &identity,
                &decl,
                &mut catalogue,
                &mut scans,
                &mut diagnostics,
                on_event,
            );
            if admitted == Continue::Stop {
                stopped = true;
                break;
            }
            continue;
        }

        let entry = PendingGear {
            source: root.id.clone(),
            gdl_path: identity.gdl_path.clone(),
            stage: LoadStage::Declared,
            display_name: decl.name.clone(),
            description: decl.description.clone(),
            category: decl.category.clone(),
        };
        if on_event(LoadEvent::Declared(&entry)) == Continue::Stop {
            stopped = true;
        }
        pending.push(entry);
        declared.push((root, identity, decl));

        if stopped {
            break;
        }
    }

    if !stopped
        && on_event(LoadEvent::DeclarationComplete {
            declared: declared.len(),
            diagnostics: diagnostics.as_slice(),
        }) == Continue::Stop
    {
        stopped = true;
    }

    // ---- S2..S4: project and merge -----------------------------------------
    if !stopped {
        for (root, identity, decl) in &declared {
            // The declared half is in hand; now project the half the Rust
            // attributes own and merge. A projection failure is fatal for this
            // gear -- under ADR `cpt-gearbox-adr-macro-projected-catalogue` the
            // catalogue cannot be assembled without it, which is a deliberate
            // trade recorded in the PRD's risk table.
            let merged = project_and_merge(root, identity, decl, &mut scans, &mut diagnostics);

            // Whether it projected or not, it is no longer in flight: a failure
            // is represented by its diagnostics, not by staying pending forever.
            //
            // Matched on `(source, gdl_path)`, not on the path alone: the path
            // is relative to one source root, so with two roots of the same
            // shape `gears/x/gear.gdl` names an entry in each and the path
            // alone retires the wrong one.
            pending.retain(|p| !(p.source == identity.source && p.gdl_path == identity.gdl_path));

            let Some(merged) = merged else { continue };

            let id = merged.gear.id.clone();
            implemented
                .entry(id.clone())
                .or_insert_with(|| merged.implemented_traits.clone());
            // **The first declaration wins, and the second is an error.**
            //
            // This used to `insert` unconditionally, so the diagnostic below said
            // "declared twice" while the catalogue quietly kept the *second* one.
            // Deterministic -- discovery is sorted -- but arbitrary, and it meant
            // the tree disagreed with its own complaint: the gear a reader was
            // told about was not the gear that resolution would use.
            //
            // Keeping the first is the choice that can be stated in one sentence
            // ("the earliest declared root wins"), and with roots now derived from
            // a product's own `sources` list, "earliest" is an order the person
            // wrote down. See ADR `cpt-gearbox-adr-multiple-source-roots`.
            if let Some(previous) = catalogue.gears.get(&id) {
                diagnostics.push(declared_twice(
                    &id,
                    (&previous.gdl_path, &previous.source),
                    identity,
                ));
            } else {
                catalogue.gears.insert(id.clone(), merged.gear);
            }
            contracts.absorb(merged.provided, merged.consumed, &id);

            // Borrowed from the catalogue, so the callback sees the merged gear
            // rather than a copy made for its benefit.
            if let Some(stored) = catalogue.gears.get(&id)
                && on_event(LoadEvent::Projected(stored)) == Continue::Stop
            {
                break;
            }
        }
    }

    catalogue.contracts = contracts.finish();

    // A gear that has code and a design description of the same id: someone
    // wrote the crate and left the design behind. Both stay listed -- the
    // resolver only ever reads `gears`, so the gear with code is the one used
    // -- and the stale description is reported.
    for (id, design) in &catalogue.designs {
        if let Some(gear) = catalogue.gears.get(id) {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlCardinality,
                    format!(
                        "gear `{id}` has code (`{}` in source `{}`) and is also described at \
                         design maturity (`{}` in source `{}`)",
                        gear.gdl_path, gear.source, design.gdl_path, design.source
                    ),
                    "the gear has outgrown its design description: delete that `gear.gdl`, \
                     moving anything still true of the gear into the one beside its crate",
                )
                .at(Location::or_file(
                    design.declared_at.as_ref(),
                    &roots
                        .iter()
                        .find(|r| r.id == design.source)
                        .map(|r| gearbox_ir::file_uri(&r.root.join(design.gdl_path.as_str())))
                        .unwrap_or_default(),
                )),
            );
        }
    }

    // The whole gear set is known only here. Inside the loop above,
    // `catalogue.gears` holds only what `discover()`'s sorted order has reached,
    // so an owner described later than the description that pulls its contract
    // in would look absent.
    report_unknown_contract_owners(&catalogue, roots, &mut diagnostics);
    // Same reason: a plugin's host may be described later in discovery order.
    // A stopped load has not reached every host, so an unjoined fill is only
    // reported when it had the chance to join.
    let complete = !stopped && pending.is_empty();
    let joined = join_plugin_points(
        &mut catalogue,
        &implemented,
        roots,
        complete,
        &mut diagnostics,
    );
    if complete {
        for id in &joined {
            if let Some(gear) = catalogue.gears.get(id)
                && on_event(LoadEvent::Joined(gear)) == Continue::Stop
            {
                break;
            }
        }
    }

    diagnostics.finish();
    catalogue.diagnostics = diagnostics;
    CatalogueScan {
        catalogue,
        files,
        crates_scanned: scans.distinct_crates(),
        scan_requests: scans.scan_requests(),
        pending,
    }
}

/// "Declared twice", for a gear or a design gear: the first one is kept.
fn declared_twice(
    id: &gearbox_ir::GearId,
    (previous_path, previous_source): (&gearbox_ir::RelPath, &gearbox_ir::SourceId),
    identity: &FileIdentity,
) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::GdlCardinality,
        format!(
            "gear `{id}` is declared twice: `{previous_path}` in source `{previous_source}` and \
             `{}` in source `{}`; the first is the one in the catalogue",
            identity.gdl_path, identity.source
        ),
        "one of the descriptions points at the wrong crate, or names the wrong attribute with \
         `attr = \"...\"`; if both are wanted, they need distinct gear ids",
    )
    .at(Location::file(identity.uri.clone()))
}

/// Every description under every root, each with the root it was found in.
fn discover_all<'r>(
    roots: &'r [SourceRoot],
    diagnostics: &mut Diagnostics,
) -> Vec<(&'r SourceRoot, PathBuf)> {
    let mut discovered = Vec::new();
    for root in roots {
        let (found, failures) = discover(&root.root);
        discovered.extend(found.into_iter().map(|p| (root, p)));
        for failure in failures {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlEval,
                    format!("cannot search `{}` for descriptions: {failure}", root.id),
                    "a directory the application cannot read is indistinguishable from one with no \
                     gears in it; check its permissions",
                )
                .at(Location::file(gearbox_ir::file_uri(&root.root))),
            );
        }
    }
    discovered
}

/// The text of one description, or `None` with the reason reported.
fn read_description(
    path: &std::path::Path,
    identity: &FileIdentity,
    diagnostics: &mut Diagnostics,
) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) => {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlEval,
                    format!("cannot read `{}`: {e}", path.display()),
                    "check the file's permissions",
                )
                .at(Location::file(identity.uri.clone())),
            );
            None
        }
    }
}

/// Put one design gear in the catalogue and tell the caller whether to go on.
///
/// The same rule as for gears with code: the first declaration wins, and a
/// second is reported rather than stored.
fn admit_design(
    root: &SourceRoot,
    identity: &FileIdentity,
    decl: &gearbox_gdl::GearDecl,
    catalogue: &mut Catalogue,
    scans: &mut crate::scans::CrateScans,
    diagnostics: &mut Diagnostics,
    on_event: &mut dyn FnMut(LoadEvent<'_>) -> Continue,
) -> Continue {
    let Some(design) = design_gear(root, identity, decl, scans, diagnostics) else {
        return Continue::Yes;
    };
    let id = design.id.clone();
    if let Some(previous) = catalogue.designs.get(&id) {
        diagnostics.push(declared_twice(
            &id,
            (&previous.gdl_path, &previous.source),
            identity,
        ));
        return Continue::Yes;
    }
    catalogue.designs.insert(id.clone(), design);
    catalogue
        .designs
        .get(&id)
        .map_or(Continue::Yes, |stored| on_event(LoadEvent::Design(stored)))
}

/// What a description locates beside its gear, rather than in its crate.
struct Beside {
    origin: Option<gearbox_ir::GitOrigin>,
    docs: Option<gearbox_ir::GearDocs>,
    tests: Vec<gearbox_ir::Located>,
}

/// The gear's git source, documents and tests.
///
/// Pure filesystem lookup, no parsing, so it costs a few stats. The source
/// comes first because the other two fall back to it for what is not on disk.
fn beside(
    root: &SourceRoot,
    identity: &FileIdentity,
    decl: &gearbox_gdl::GearDecl,
    diagnostics: &mut Diagnostics,
) -> Beside {
    // `.` cannot fail to resolve, so the description's own directory is the one
    // path here with no error to report.
    let gdl_dir = root.root.join(identity.gdl_path.parent().as_str());
    let origin = crate::origin::project(&root.root, identity, decl, diagnostics);
    let docs = crate::docs::project(
        &root.root,
        &gdl_dir,
        identity,
        decl,
        origin.as_ref(),
        diagnostics,
    );
    let tests = crate::docs::tests(
        &root.root,
        &gdl_dir,
        identity,
        decl,
        origin.as_ref(),
        diagnostics,
    );
    Beside {
        origin,
        docs,
        tests,
    }
}

/// A design gear, built from its declaration alone.
///
/// No crate is looked for and nothing is projected. The SDK, when declared,
/// is still checked against its `Cargo.toml` -- it is the one crate a design
/// gear names, and a wrong name there fails the day the gear gets code.
fn design_gear(
    root: &SourceRoot,
    identity: &FileIdentity,
    decl: &gearbox_gdl::GearDecl,
    scans: &mut crate::scans::CrateScans,
    diagnostics: &mut Diagnostics,
) -> Option<DesignGear> {
    // `gear()` refuses a design gear without a valid id, so this cannot fail
    // for a declaration that evaluated.
    let id = gearbox_ir::GearId::new(decl.id.as_deref()?).ok()?;
    crate::manifest_check::check(root, identity, decl, scans, diagnostics);
    let uri = identity.uri.as_str();
    let gdl_dir = identity.gdl_path.parent();
    // A design gear refuses `tests`, so that list is empty.
    let Beside { origin, docs, .. } = beside(root, identity, decl, diagnostics);
    Some(DesignGear {
        display_name: decl.name.clone().unwrap_or_else(|| id.to_string()),
        id,
        description: decl.description.clone(),
        category: decl.category.clone(),
        source: root.id.clone(),
        gdl_path: identity.gdl_path.clone(),
        sdks: crate::merge::sdk_refs(decl, &gdl_dir, uri, diagnostics),
        origin,
        scenarios: crate::merge::scenarios(decl),
        docs,
        declared_at: decl.declared_at.clone(),
    })
}

/// Project the Rust half for one description and merge it with the declared half.
///
/// Returns `None` when the gear attribute cannot be located or parsed. Both are
/// reported: an unlocatable attribute is GBX0211 and lists the candidates plus
/// the `attr` line to add, which is the difference between an actionable error
/// and a dead end.
fn project_and_merge(
    root: &SourceRoot,
    identity: &FileIdentity,
    decl: &gearbox_gdl::GearDecl,
    scans: &mut crate::scans::CrateScans,
    diagnostics: &mut Diagnostics,
) -> Option<crate::merge::MergedGear> {
    let package = decl.package.as_ref()?;

    // Before anything is projected: the declared crate identities are the only
    // facts in a description with an external authority, and a wrong one fails
    // at `cargo build` on a crate the reader did not write (GBX0209).
    crate::manifest_check::check(root, identity, decl, scans, diagnostics);

    let crate_dir =
        match crate::merge::crate_dir(root, &identity.gdl_path, &package.path, &package.crate_name)
        {
            Ok(dir) => dir,
            Err(e) => {
                diagnostics.push(crate::merge::bad_crate_path(
                    &identity.uri,
                    "package",
                    &package.path,
                    &e,
                    package.declared_at.as_ref(),
                ));
                return None;
            }
        };
    let label = format!("{} ({})", package.crate_name, crate_dir.display());

    let files = match scans.get(&crate_dir) {
        Ok(files) => files,
        Err(e) => {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlEval,
                    format!("cannot read the crate `{label}` this description points at: {e}"),
                    "check `package = cargo(..., path = \"...\")`; it is relative to the                      description's own directory",
                )
                .at(Location::file(identity.uri.clone())),
            );
            return None;
        }
    };

    let site = match gearbox_project::locate_gear_attribute(&files, &label, package.attr.as_deref())
    {
        Ok(site) => site,
        Err(e) => {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::ValidateAttributeAmbiguous,
                    e.to_string(),
                    "add `attr = \"src/....rs\"` to the description's `package = cargo(...)`                      to name which attribute this description describes",
                )
                .at(Location::file(identity.uri.clone()))
                .with_evidence("cpt-gearbox-fr-attribute-location"),
            );
            return None;
        }
    };

    let projected = match gearbox_project::project_gear(&site) {
        Ok(p) => p,
        Err(e) => {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlEval,
                    format!("cannot read the gear attribute in `{label}`: {e}"),
                    "the attribute does not parse; the crate would not compile either",
                )
                .at(Location::file(identity.uri.clone())),
            );
            return None;
        }
    };

    // Contracts live in the SDK crates the description names, which is why
    // `sdk` stays declared: it locates them without restating them.
    let mut contracts_by_trait = BTreeMap::new();
    let mut sdk_dirs: Vec<std::path::PathBuf> = decl
        .provides
        .iter()
        .map(|p| &p.sdk)
        .chain(decl.consumes.iter().map(|c| &c.sdk))
        .filter_map(|sdk| {
            crate::merge::crate_dir(root, &identity.gdl_path, &sdk.path, &sdk.crate_name)
                .map_err(|e| {
                    diagnostics.push(crate::merge::bad_crate_path(
                        &identity.uri,
                        "sdk",
                        &sdk.path,
                        &e,
                        sdk.declared_at.as_ref(),
                    ));
                })
                .ok()
        })
        .collect();
    sdk_dirs.sort();
    sdk_dirs.dedup();

    for sdk_dir in sdk_dirs {
        match scans.get(&sdk_dir) {
            Ok(sdk_files) => match gearbox_project::project_contracts(&sdk_files) {
                Ok(contracts) => {
                    for contract in contracts {
                        contracts_by_trait.insert(contract.trait_ident.clone(), contract);
                    }
                }
                // A `#[toolkit::contract]` whose arguments do not parse means
                // the SDK crate does not compile. Reporting it is the whole
                // point: silently omitting the trait would resurface as every
                // `provide`/`consume` naming it being "a trait nobody wrote".
                Err(e) => diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::GdlEval,
                        format!(
                            "the sdk crate `{}` has a `#[toolkit::contract]` whose arguments \
                             do not parse: {e}",
                            sdk_dir.display()
                        ),
                        "fix the attribute; the macro would reject it at compile time too",
                    )
                    .at(Location::file(identity.uri.clone())),
                ),
            },
            Err(e) => diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlEval,
                    format!("cannot read the sdk crate `{}`: {e}", sdk_dir.display()),
                    "check `sdk = cargo(..., path = \"...\")`",
                )
                .at(Location::file(identity.uri.clone())),
            ),
        }
    }

    // Cluster: profiles come free from the crate scan above; providers cost one
    // extra scan per declared plugin crate, which only `cluster` itself declares.
    let cluster = crate::cluster::project(root, identity, decl, &files, scans, diagnostics);
    // Plugin facts cost one SDK scan, and only for gears that declare `sdk`.
    // The SDK crate is scanned once and shared: both the plugin projection and
    // the GTS one read it, and it is the expensive step.
    let sdk_files = match decl.sdk.as_ref() {
        None => std::sync::Arc::from(Vec::new()),
        Some(sdk) => {
            let Ok(sdk_dir) =
                crate::merge::crate_dir(root, &identity.gdl_path, &sdk.path, &sdk.crate_name)
            else {
                // Already reported where the sdk dirs were collected above.
                return None;
            };
            match scans.get(&sdk_dir) {
                Ok(files) => files,
                Err(e) => {
                    diagnostics.push(
                        Diagnostic::error(
                            DiagnosticCode::GdlEval,
                            format!("cannot read the sdk crate `{}`: {e}", sdk_dir.display()),
                            "check `sdk = cargo(..., path = \"...\")`; the path is relative to \
                             the description's own directory",
                        )
                        .at(Location::file(identity.uri.clone())),
                    );
                    std::sync::Arc::from(Vec::new())
                }
            }
        }
    };

    // The manifest is already in the scan cache: `manifest_check` read the same
    // directory a few lines above, so this costs a map lookup. An unreadable
    // manifest is reported there, not twice, and yields no features rather than
    // no gear -- a crate whose `Cargo.toml` cannot be read fails the build,
    // which is a louder answer than this.
    let available_features = scans
        .manifest(&crate_dir)
        .map(|manifest| manifest.features.clone())
        .unwrap_or_default();

    // A crate may declare several gears -- mini-chat declares a host and two
    // plugins -- and each reads only the files it owns, or the host finds three
    // config structs and every plugin sees the others' impls.
    let owned = gearbox_project::files_owned_by(&files, &site.relative);
    let own_files: &[gearbox_project::RustFile] = owned.as_deref().unwrap_or(&files);

    let gts_types = sdk_gts_types(&sdk_files, identity, diagnostics);

    let point_sdks = point_sdks(root, identity, decl, &sdk_files, scans, diagnostics);
    let trait_sdks: Vec<Option<crate::plugin::TraitSdk<'_>>> = point_sdks
        .iter()
        .map(|entry| {
            entry
                .as_ref()
                .map(|(record, files)| crate::plugin::TraitSdk { record, files })
        })
        .collect();

    let plugin = crate::plugin::project(
        identity,
        decl,
        own_files,
        &gts_types,
        &trait_sdks,
        diagnostics,
    );
    let config = crate::config::project(identity, decl, own_files, diagnostics);

    let Beside {
        origin,
        docs,
        tests,
    } = beside(root, identity, decl, diagnostics);

    crate::merge::merge(
        identity,
        decl,
        crate::merge::Projections {
            gear: &projected,
            contracts_by_trait: &contracts_by_trait,
            cluster: &cluster,
            plugin: &plugin,
            config,
            docs,
            origin,
            tests,
            gts_types,
            // The manifest is already in the scan cache: `manifest_check` read
            // the same directory a few lines above, so this costs a map lookup.
            // An unreadable manifest is reported there, not twice, and yields no
            // features rather than no gear -- a crate whose `Cargo.toml` cannot
            // be read fails the build, which is a louder answer than this.
            available_features: available_features.clone(),
            cargo_features: crate::features::project(
                identity,
                decl,
                &available_features,
                diagnostics,
            ),
        },
        diagnostics,
    )
}

/// Report every contract whose owner names nothing the catalogue holds.
///
/// `#[toolkit::contract(gear = "...")]` is a free string, and the two places
/// that turn it into an id both guard a *malformed* one -- `build_provider`
/// falls back to the declaring gear, `build_consumer` diagnoses and gives up.
/// Neither asks whether the parsed id names anything, because neither can: the
/// catalogue is still being built around them.
fn report_unknown_contract_owners(
    catalogue: &Catalogue,
    roots: &[SourceRoot],
    diagnostics: &mut Diagnostics,
) {
    let registered = registered_names(catalogue);
    for (id, contract) in &catalogue.contracts {
        if registered.contains(contract.owner.as_str()) {
            continue;
        }
        let mut diagnostic = Diagnostic::error(
            DiagnosticCode::TopologyUnknownContractOwner,
            format!(
                "contract `{id}` names owner `{}`, which is neither a gear in the catalogue \
                 nor a declared role's directory name",
                contract.owner
            ),
            format!(
                "the owner comes from `#[toolkit::contract(gear = \"{}\")]` in `{}`; write a \
                 `gear.gdl` for it, open the source root that holds one, declare it as a \
                 `role(directory_name = \"{}\")` on the gear that serves it, or fix the \
                 attribute. {}",
                contract.owner,
                contract.sdk.crate_name,
                contract.owner,
                crate::validate::nearest_hint(catalogue.gears.keys(), contract.owner.as_str()),
            ),
        );
        if let Some(uri) = first_referrer(catalogue, id).and_then(|g| description_uri(roots, g)) {
            diagnostic = diagnostic.at(Location::file(uri));
        }
        diagnostics.push(diagnostic);
    }
}

/// The GTS types a gear exposes: the ones its own SDK declares.
///
/// A type in the main crate is internal, and a `gts_id!` reference is not a
/// declaration. A plugin declares no `sdk` -- its host's SDK is the host's --
/// so a spec type is attributed once, to the gear that owns it.
fn sdk_gts_types(
    sdk_files: &[gearbox_project::RustFile],
    identity: &FileIdentity,
    diagnostics: &mut Diagnostics,
) -> Vec<gearbox_ir::GtsTypeDecl> {
    match gearbox_project::project_gts_types(sdk_files) {
        Ok(types) => types
            .into_iter()
            .map(|t| gearbox_ir::GtsTypeDecl {
                type_id: t.type_id,
                description: t.description,
                relative: t.relative,
            })
            .collect(),
        Err(e) => {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlEval,
                    format!("cannot project GTS types: {e}"),
                    "the declaration shape is not modelled; see gearbox-project's gts module",
                )
                .at(Location::file(identity.uri.clone())),
            );
            Vec::new()
        }
    }
}

/// One extension point's trait crate: its locator and its scanned files.
type PointSdk<'d> = Option<(
    &'d gearbox_gdl::records::CargoRecord,
    std::sync::Arc<[gearbox_project::RustFile]>,
)>;

/// Where each of a gear's extension points finds its trait: the point's own
/// `sdk` when it names one (bss-rate-provider's sources implement a trait from
/// the ledger's SDK), the gear's otherwise. Scanned through the same cache as
/// every other crate, and aligned by index with `decl.extension_points`; `None`
/// where the crate could not be read, which is reported here.
fn point_sdks<'d>(
    root: &SourceRoot,
    identity: &FileIdentity,
    decl: &'d gearbox_gdl::GearDecl,
    sdk_files: &std::sync::Arc<[gearbox_project::RustFile]>,
    scans: &mut crate::scans::CrateScans,
    diagnostics: &mut Diagnostics,
) -> Vec<PointSdk<'d>> {
    decl.extension_points
        .iter()
        .map(|point| match point.sdk.as_ref() {
            None => decl.sdk.as_ref().map(|sdk| (sdk, sdk_files.clone())),
            Some(sdk) => {
                match crate::merge::crate_dir(root, &identity.gdl_path, &sdk.path, &sdk.crate_name)
                {
                    Err(e) => {
                        diagnostics.push(crate::merge::bad_crate_path(
                            &identity.uri,
                            "extension_point.sdk",
                            &sdk.path,
                            &e,
                            sdk.declared_at.as_ref(),
                        ));
                        None
                    }
                    Ok(dir) => match scans.get(&dir) {
                        Ok(files) => Some((sdk, files)),
                        Err(e) => {
                            diagnostics.push(
                                Diagnostic::error(
                                    DiagnosticCode::GdlEval,
                                    format!(
                                        "cannot read the extension point's sdk crate `{}`: {e}",
                                        dir.display()
                                    ),
                                    "check `extension_point(..., sdk = cargo(..., path = \"...\"))`",
                                )
                                .at(Location::or_file(
                                    point.declared_at.as_ref(),
                                    &identity.uri,
                                )),
                            );
                            None
                        }
                    },
                }
            }
        })
        .collect()
}

/// Join every plugin's declared `implements` to the host that declares its spec.
///
/// Run once the whole gear set is known, because discovery order says nothing
/// about hosts coming first. A plugin names only the spec; the trait and SDK are
/// the host's to state, so `implements.point` is copied from the host's declaration.
///
/// - no described gear declares the spec: GBX0519;
/// - two gears declare it: which one a plugin implements has no answer, so the
///   second is refused (`GdlCardinality`) and the first is used;
/// - the plugin's crate implements none of the point's trait: GBX0526, a
///   warning -- the impl is evidence, and it can hide behind a wrapper this
///   reader does not follow.
///
/// Returns the plugins whose `implements.point` it set, so a streaming consumer that
/// saw them before the join can be sent them again.
fn join_plugin_points(
    catalogue: &mut Catalogue,
    implemented: &BTreeMap<gearbox_ir::GearId, std::collections::BTreeSet<String>>,
    roots: &[SourceRoot],
    complete: bool,
    diagnostics: &mut Diagnostics,
) -> Vec<gearbox_ir::GearId> {
    let mut joined = Vec::new();
    let mut declared: BTreeMap<String, (gearbox_ir::GearId, gearbox_ir::ExtensionPointDecl)> =
        BTreeMap::new();
    for (id, gear) in &catalogue.gears {
        for point in &gear.extension_points {
            if let Some((first, _)) = declared.get(&point.spec) {
                let mut diagnostic = Diagnostic::error(
                    DiagnosticCode::GdlCardinality,
                    format!(
                        "extension point `{}` is declared by both `{first}` and `{id}`; a plugin \
                         that implements it would have two hosts",
                        point.spec
                    ),
                    "one host declares a spec; remove the other `extension_point(...)`",
                );
                if let Some(uri) = description_uri(roots, gear) {
                    diagnostic = diagnostic.at(Location::or_file(gear.declared_at.as_ref(), &uri));
                }
                diagnostics.push(diagnostic);
                continue;
            }
            declared.insert(point.spec.clone(), (id.clone(), point.clone()));
        }
    }

    for (id, gear) in &mut catalogue.gears {
        let Some(fill) = gear.implements.as_mut() else {
            continue;
        };
        let at = |gear: &GearDescriptor| {
            description_uri(roots, gear)
                .map(|uri| Location::or_file(gear.declared_at.as_ref(), &uri))
        };
        match declared.get(&fill.spec) {
            None if !complete => {}
            None => {
                let own = fill
                    .spec
                    .strip_prefix(crate::plugin::PLUGIN_BASE)
                    .unwrap_or(&fill.spec)
                    .to_owned();
                let mut diagnostic = Diagnostic::error(
                    DiagnosticCode::PluginSpecUndeclared,
                    format!(
                        "`{id}` implements `{own}`, which no described gear declares as an extension \
                         point"
                    ),
                    "the host declares `extension_points = [extension_point(\"...\", trait = \
                     \"...\")]`; describe it, or fix the spec in `implements`",
                );
                if let Some(location) = at(gear) {
                    diagnostic = diagnostic.at(location);
                }
                diagnostics.push(diagnostic);
            }
            Some((host, point)) => {
                fill.point = Some(point.clone());
                joined.push(id.clone());
                let implements = implemented
                    .get(id)
                    .is_some_and(|traits| traits.contains(&point.trait_ident));
                if !implements {
                    let mut diagnostic = Diagnostic::new(
                        DiagnosticCode::PluginImplMissing,
                        format!(
                            "`{id}` implements `{host}`'s point, and its crate implements no `{}`",
                            point.qualified()
                        ),
                    )
                    .with_help(
                        "check the spec in `implements`; if the impl is behind a wrapper this is \
                         only a note",
                    );
                    if let Some(location) = at(gear) {
                        diagnostic = diagnostic.at(location);
                    }
                    diagnostics.push(diagnostic);
                }
            }
        }
    }
    joined
}

/// Every name the catalogue answers to: a gear's id, and each declared role's
/// directory name.
///
/// The role half reads `directory_name` verbatim rather than deriving
/// `<gear-id>-<name>`, which is what keeps this from being rewritten when ADR
/// `cpt-gearbox-adr-role-qualified-names` gives that field a default. A default
/// belongs where the role is declared; this goes on reading one field either
/// way.
fn registered_names(catalogue: &Catalogue) -> std::collections::BTreeSet<&str> {
    let mut names: std::collections::BTreeSet<&str> =
        catalogue.gears.keys().map(GearId::as_str).collect();
    for gear in catalogue.gears.values() {
        names.extend(
            gear.declared_roles
                .iter()
                .map(|role| role.directory_name.as_str()),
        );
    }
    names
}

/// A description that referenced this contract, for a location.
///
/// First in map order, so the choice is deterministic. Several gears may
/// reference one contract and one diagnostic per contract is the right
/// cardinality: the mistake is in the attribute, not in each `gear.gdl`.
fn first_referrer<'a>(catalogue: &'a Catalogue, id: &ContractId) -> Option<&'a GearDescriptor> {
    catalogue.gears.values().find(|gear| {
        gear.provides.iter().any(|p| p.contract == *id)
            || gear
                .consumes
                .iter()
                .chain(&gear.requires)
                .any(|r| r.contract() == Some(id))
    })
}

/// The `file://` URI of a gear's own description.
///
/// `gdl_path` is relative to its source root, so the root has to be looked back
/// up: a URI built from the relative path alone is one no editor can open.
fn description_uri(roots: &[SourceRoot], gear: &GearDescriptor) -> Option<String> {
    let root = roots.iter().find(|r| r.id == gear.source)?;
    Some(gearbox_ir::file_uri(
        &root.root.join(gear.gdl_path.as_str()),
    ))
}

/// How authoritative one copy of a contract descriptor is.
///
/// A contract is described twice on purpose: the provider declares it fully,
/// and each consumer restates its Rust path so the consumer's generated client
/// needs nothing from the provider's file. The provider's copy is the complete
/// one -- it carries the transport projections -- so it wins, and a consumer's
/// copy only fills a gap when the provider is out of scope.
///
/// **Three ranks rather than two**, and the top one is what keeps this inert
/// for every tree with no roles in it. When two gears provide one contract, the
/// gear whose id equals the owner wins today; collapsing that into a plain
/// "provided beats consumed" would hand the win to whichever gear the walk
/// reached first instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ContractAuthority {
    /// From a `consume(...)`: identity and sdk, no transport projections.
    Consumed,
    /// From a `provide(...)` in a gear the attribute does not name as owner.
    Provided,
    /// From a `provide(...)` in the gear the attribute names as owner.
    OwnerProvided,
}

/// The contract table under construction, with each entry's provenance.
///
/// The provenance is carried from [`crate::merge::MergedGear`] rather than
/// re-derived from `owner == declared_by`. That comparison is false for every
/// file once a contract answers to a role rather than to a gear, which turned
/// the rule into "whoever was walked first" and let a consumer's stub -- whose
/// `rest` and `grpc` are deliberately absent -- beat the provider's complete
/// descriptor. Silently.
#[derive(Default)]
struct ContractMerge {
    descriptors: BTreeMap<ContractId, ContractDescriptor>,
    authority: BTreeMap<ContractId, ContractAuthority>,
}

impl ContractMerge {
    /// Absorb one gear's contracts.
    ///
    /// A strictly more authoritative copy replaces the incumbent; an equally
    /// authoritative one does not, so the first declared wins -- the same rule
    /// the gear map itself uses.
    fn absorb(
        &mut self,
        provided: Vec<ContractDescriptor>,
        consumed: Vec<ContractDescriptor>,
        declared_by: &GearId,
    ) {
        let ranked = provided
            .into_iter()
            .map(|c| {
                let rank = if c.owner == *declared_by {
                    ContractAuthority::OwnerProvided
                } else {
                    ContractAuthority::Provided
                };
                (rank, c)
            })
            .chain(
                consumed
                    .into_iter()
                    .map(|c| (ContractAuthority::Consumed, c)),
            );

        for (rank, contract) in ranked {
            let beaten = self
                .authority
                .get(&contract.id)
                .is_none_or(|held| rank > *held);
            if beaten {
                self.authority.insert(contract.id.clone(), rank);
                self.descriptors.insert(contract.id.clone(), contract);
            }
        }
    }

    fn finish(self) -> BTreeMap<ContractId, ContractDescriptor> {
        self.descriptors
    }
}

/// Build the identity to record for a discovered file.
fn identity_for(
    root: &SourceRoot,
    path: &Path,
    diagnostics: &mut Diagnostics,
) -> Option<FileIdentity> {
    let uri = gearbox_ir::file_uri(path);

    let relative = path.strip_prefix(&root.root).ok().and_then(|r| {
        // Forward slashes regardless of platform: the path goes into a lock
        // that must be byte-identical everywhere.
        let text = r
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        RelPath::new(text).ok()
    });

    let Some(gdl_path) = relative else {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::GdlEval,
                format!(
                    "`{}` is not inside its source root `{}`",
                    path.display(),
                    root.root.display()
                ),
                "move the description inside the declared source root",
            )
            .at(Location::file(uri)),
        );
        return None;
    };

    Some(FileIdentity {
        uri,
        source: root.id.clone(),
        gdl_path,
        // A `load()` resolves from the description's own directory and may not
        // climb above the source root.
        load_paths: Some(gearbox_gdl::engine::LoadPaths {
            base: path.parent().unwrap_or(&root.root).to_path_buf(),
            root: root.root.clone(),
        }),
    })
}

/// Every `gear.gdl` under `root`, sorted, plus the directories that could not
/// be walked.
///
/// The failures are returned rather than dropped. A directory the process
/// cannot read looks exactly like a directory with no gears in it, and "this
/// tree declares nothing" is not a conclusion a permission error should be
/// allowed to reach.
fn discover(root: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            // Keep files; prune only uninteresting directories.
            !entry.file_type().is_dir()
                || entry
                    .file_name()
                    .to_str()
                    .is_none_or(|name| !SKIP_DIRS.contains(&name))
        })
    {
        match entry {
            Ok(entry) => {
                if entry.file_type().is_file() && entry.file_name() == GEAR_FILE {
                    found.push(entry.into_path());
                }
            }
            Err(e) => {
                let at = e.path().unwrap_or(root).display().to_string();
                failures.push(format!("{at}: {e}"));
            }
        }
    }

    found.sort();
    failures.sort();
    (found, failures)
}

#[cfg(test)]
#[path = "catalogue_tests.rs"]
mod catalogue_tests;
