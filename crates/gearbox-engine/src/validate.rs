//! `gearbox validate`: everything checkable without resolving.
//!
//! Deliberately not a new set of checks. Loading a catalogue already reports
//! `GBX01xx` (evaluation), GBX0206, GBX0209, GBX0210 and GBX0211, and validate's
//! job is to run that and give the operator an exit code. What it *adds* is the
//! one join that does not need the resolver: a product's selected gears against
//! the catalogue's keys.
//!
//! That join is worth doing here rather than waiting for M4 because of the
//! numbers. The tree holds 44 `#[toolkit::gear]` attributes and 14 descriptions,
//! so a selected gear that is missing from the catalogue is usually a gear
//! nobody has described yet -- not a typo. Those two want opposite responses:
//! one is "here is the `gear.gdl` to write, and here is the crate it goes
//! beside" (GBX0208), the other is "no crate anywhere declares this"
//! (GBX0301). Reporting them as one code would make the common case unhelpful.

use gearbox_ir::{Diagnostic, DiagnosticCode, Diagnostics, GearId, ProductIntent, Severity};

use crate::catalogue::CatalogueScan;
use crate::source::SourceRoot;

/// The outcome of one validation run.
pub struct ValidateReport {
    /// The catalogue as loaded, so a caller can print what *did* work.
    pub scan: CatalogueScan,
    /// Everything found, the catalogue's own diagnostics included.
    pub diagnostics: Diagnostics,
    /// What each selected host's extension points resolve to, per profile.
    ///
    /// Empty when no product was given. Included because "does every extension
    /// point have an implementation" is checkable without resolving -- it needs
    /// the catalogue and the product, and nothing else -- and leaving it to the
    /// `plugins` subcommand made it the one check a passing `validate` did not
    /// cover.
    pub plugins: Vec<crate::plugin_select::PointResolution>,
}

impl ValidateReport {
    /// Whether anything makes the product unbuildable.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    #[must_use]
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    #[must_use]
    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }
}

/// Load the catalogue and, when a product is given, check its selections.
#[must_use]
pub fn validate(roots: &[SourceRoot], product: Option<&ProductIntent>) -> ValidateReport {
    validate_at(roots, product, None)
}

/// As [`validate`], but told where the product description lives on disk.
///
/// `product_path` only affects the diagnostics' URI, and it matters: a
/// `ProductIntent` carries `gdl_path` relative to its own root, so building
/// `file://` out of it yields `file://product.gdl` -- a URI no editor can open.
#[must_use]
pub fn validate_at(
    roots: &[SourceRoot],
    product: Option<&ProductIntent>,
    product_path: Option<&std::path::Path>,
) -> ValidateReport {
    // **Loaded fresh, every time, and that is the point of the call.**
    //
    // The RPC server keeps a catalogue cache and `product/resolve` answers from
    // it, so this looks like the one read path that forgot. It did not. Validate
    // has a single caller -- the Gear Author's `Validate` button -- whose whole
    // job is to say what is on disk *now*, after an edit the cache may not have
    // seen. Answering from the cache would return what the client already has
    // from `catalogue/load`: the button would stop checking and start
    // restating. And `undescribed::find` below walks the filesystem regardless,
    // so a cached answer would be half fresh and half stale, which is worse than
    // either.
    //
    // The cost is one scan per press, in a session whose roots are usually the
    // single gear being authored. Reported as waste once; recorded here so it is
    // not reported again.
    let scan = crate::catalogue::load_catalogue(roots);

    let mut diagnostics = Diagnostics::new();
    for diagnostic in &scan.catalogue.diagnostics {
        diagnostics.push(diagnostic.clone());
    }

    let plugins = match product {
        None => Vec::new(),
        Some(intent) => {
            let uri = product_uri(intent, product_path);
            check_selections(roots, &scan, intent, &uri, &mut diagnostics);
            // The other join that needs no resolver: a config value against the
            // type its gear declares for that field.
            crate::config_check::check(&scan.catalogue, intent, &uri, &mut diagnostics);
            // And the same join for features. Every declared profile, because
            // `use_gear` takes no `profiles`: a feature is selected for all of
            // them, so one that suits none is wrong wherever it is read.
            let profiles: Vec<&gearbox_ir::ProfileId> = intent.profiles.keys().collect();
            crate::feature_check::check(&scan.catalogue, intent, &profiles, &uri, &mut diagnostics);
            crate::provider_feature_check::check(&scan.catalogue, intent, &uri, &mut diagnostics);
            // An unfilled extension point is a product that builds and then
            // finds nothing at runtime. It needs the catalogue and the product
            // and no resolution at all, so validate is where it belongs.
            crate::plugin_select::check(&scan.catalogue, intent, &uri, &mut diagnostics)
        }
    };

    diagnostics.finish();
    ValidateReport {
        scan,
        diagnostics,
        plugins,
    }
}

/// The `file://` URI diagnostics about the product should point at.
///
/// A `ProductIntent` carries `gdl_path` relative to its own root, so building a
/// URI out of it alone yields `file://product.gdl` -- a link no editor can open.
///
/// Through `gearbox_ir::file_uri` rather than `format!("file://{}", display())`,
/// which is the hand-rolled form that function exists to replace: on Windows it
/// yields backslashes a URI parser rejects and reads `C:` as the authority. The
/// relative fallback is still a lie about where the file is, but it is at least
/// a well-formed URI, and `file_uri` documents that trade.
fn product_uri(intent: &ProductIntent, product_path: Option<&std::path::Path>) -> String {
    match product_path.map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf())) {
        Some(path) => gearbox_ir::file_uri(&path),
        None => gearbox_ir::file_uri(std::path::Path::new(intent.gdl_path.as_str())),
    }
}

/// Every selected gear must be in the catalogue.
fn check_selections(
    roots: &[SourceRoot],
    scan: &CatalogueScan,
    intent: &ProductIntent,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    // What it selects below `stable`. The selections only: validate does not
    // resolve, so what they pull in is not known here (resolve says that).
    // A set, because two profiles may each name the same gear.
    let selected: std::collections::BTreeSet<&gearbox_ir::GearId> =
        intent.selected_gears.iter().map(|s| &s.gear).collect();
    let selected = selected.into_iter().map(|g| {
        (
            g,
            std::slice::from_ref(&gearbox_ir::InclusionReason::Selected),
        )
    });
    crate::resolve::closure::maturity_diagnostics(
        &scan.catalogue,
        selected,
        intent,
        uri,
        diagnostics,
    );

    for selection in &intent.selected_gears {
        if scan.catalogue.gears.contains_key(&selection.gear) {
            continue;
        }
        let at = gearbox_ir::Location::or_file(selection.declared_at.as_ref(), uri);
        if let Some(design) =
            crate::resolve::closure::at_design(&scan.catalogue, &selection.gear, &at)
        {
            diagnostics.push(design);
            continue;
        }

        match crate::undescribed::find(roots, &selection.gear) {
            Some(found) => diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::ValidateMissingDescription,
                    format!(
                        "`use_gear(\"{}\")` names a gear that exists in Rust but has no \
                         `gear.gdl`: its `#[toolkit::gear]` is in `{}`",
                        selection.gear.as_str(),
                        found.crate_dir
                    ),
                    skeleton(&found),
                )
                .at(gearbox_ir::Location::or_file(
                    selection.declared_at.as_ref(),
                    uri,
                ))
                // `src/` is put back: `RustFile::relative` is keyed relative to
                // the crate's `src/`, so joining it straight onto the crate
                // directory yields a path that does not exist -- and evidence
                // that cannot be opened is worse than none.
                .with_evidence(format!(
                    "{}/src/{}:{}",
                    found.crate_dir,
                    found.attr_file.display(),
                    found.attr_line
                )),
            ),
            None => diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::TopologyUnknownGear,
                    format!(
                        "`use_gear(\"{}\")` names a gear no source declares",
                        selection.gear.as_str()
                    ),
                    nearest_hint(scan.catalogue.gears.keys(), selection.gear.as_str()),
                )
                .at(gearbox_ir::Location::or_file(
                    selection.declared_at.as_ref(),
                    uri,
                )),
            ),
        }
    }
}

/// The `gear.gdl` to write, with the two fields that cannot be guessed.
///
/// `crate_name` and `lib` come from the manifest rather than from the directory,
/// because that is exactly where the mistake GBX0209 catches gets made: a crate
/// with no `[lib]` section links as its package name with underscores, and
/// `cf-api-contracts` is `cf_api_contracts`, not `api_contracts`.
fn skeleton(found: &crate::undescribed::UndescribedGear) -> String {
    let Some(manifest) = found.manifest.as_ref() else {
        return format!(
            "add a `gear.gdl` beside `{}/Cargo.toml`; its manifest could not be read, \
             so fill in `crate_name` and `lib` by hand",
            found.crate_dir
        );
    };
    format!(
        "add `{}/gear.gdl` with `package = cargo(crate_name = \"{}\", lib = \"{}\", \
         path = \".\")` and a `maturity` (\"experimental\", \"preview\", \"stable\" or \
         \"deprecated\"); capabilities and dependencies are projected from the attribute, \
         so do not restate them",
        found.crate_dir, manifest.package_name, manifest.lib_ident
    )
}

/// Suggest a catalogue gear whose id is close, when one is.
///
/// Cheap and only on this path. Reported as part of the help rather than as a
/// separate diagnostic: a near miss is a guess, and a guess belongs in advice.
///
/// Takes the ids rather than a [`CatalogueScan`] because the contract-owner
/// check in `catalogue.rs` wants the same hint and runs *before* a scan exists
/// -- the scan is assembled after the load finishes. It only ever read
/// `scan.catalogue.gears.keys()` anyway.
pub(crate) fn nearest_hint<'a>(
    known: impl IntoIterator<Item = &'a GearId>,
    wanted: &str,
) -> String {
    let nearest = known
        .into_iter()
        .map(|id| (distance(wanted, id.as_str()), id.as_str()))
        .filter(|(d, _)| *d <= 3)
        .min();
    match nearest {
        Some((_, candidate)) => format!(
            "check the spelling -- the catalogue has `{candidate}`; \
             `gearbox catalogue` lists every id"
        ),
        None => "run `gearbox catalogue` to list the ids the sources actually declare".to_owned(),
    }
}

/// Levenshtein distance, iterative with one row.
///
/// Written out rather than pulled in: it is used once, on a handful of short
/// strings, and a dependency for it would be the largest thing in the crate
/// graph doing the least.
fn distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b_chars.len()).collect();

    for (i, ca) in a.chars().enumerate() {
        let mut previous_diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b_chars.iter().enumerate() {
            let previous_above = row[j + 1];
            row[j + 1] = if ca == *cb {
                previous_diagonal
            } else {
                1 + previous_above.min(row[j]).min(previous_diagonal)
            };
            previous_diagonal = previous_above;
        }
    }
    row[b_chars.len()]
}
