//! The resolution `resolve` and `generate` both run.
//!
//! One copy because there were two. The open-roots-through-assemble sequence was
//! written out in `resolve_product` and again in `generate::run`, down to
//! identical error strings, and the two had already started to drift: they
//! reached the same `load_product` through different module paths. A resolution
//! sequence that exists twice is one the two commands can answer differently.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use gearbox_engine::{CatalogueScan, SourceRoot};
use gearbox_ir::{Diagnostic, ProductIntent, ProfileId, ResolvedProduct};

use crate::{open_roots, refuse_catalogue_errors, report};

/// A product resolved for one profile, with everything either command goes on
/// to need.
pub struct Resolution {
    pub opened: Vec<SourceRoot>,
    pub scan: CatalogueScan,

    /// Canonicalized, so every diagnostic's `file://` URI points at a path an
    /// editor can open. A relative one renders and does nothing, which is the
    /// failure the Studio's dead links already taught.
    pub product_file: PathBuf,

    pub intent: ProductIntent,
    pub profile: ProfileId,
    pub lock: ResolvedProduct,

    /// The product's own diagnostics followed by the lock's, in that order.
    pub diagnostics: Vec<Diagnostic>,
}

/// Either a resolution, or the exit code the catalogue gate already decided.
pub enum Outcome {
    /// Boxed because a whole catalogue scan and lock travel in it, and the other
    /// variant is an exit code.
    Resolved(Box<Resolution>),

    /// The catalogue failed to load. Its diagnostics are already reported, so
    /// the caller has nothing left to say.
    Refused(ExitCode),
}

/// Open the roots, evaluate the product, and resolve it for one profile.
///
/// # Errors
/// Returns an error when a source root cannot be opened, when the product
/// description cannot be read, or when it does not evaluate.
pub fn resolve(
    roots: &[PathBuf],
    source_id: Option<&str>,
    product_file: &Path,
    profile: Option<&str>,
) -> anyhow::Result<Outcome> {
    let product_file = product_file.canonicalize().with_context(|| {
        format!(
            "cannot read product description `{}`",
            product_file.display()
        )
    })?;
    // Read before the roots are scanned, because the description says what its
    // `path(...)` sources are called and where they are: a root at one of them
    // is scanned under the declared id, and one not given at all is opened from
    // the description. Its failure is still reported after the catalogue's.
    let product_scan = gearbox_engine::load_product(&product_file, None);
    let mut opened = open_roots(roots, source_id)?;
    if let Some(intent) = &product_scan.intent {
        opened = gearbox_engine::with_declared_paths(
            opened,
            &intent.sources,
            &product_file,
            source_id.is_some(),
        );
    }
    let scan = gearbox_engine::load_catalogue(&opened);
    if let Some(code) = refuse_catalogue_errors(&scan.catalogue) {
        return Ok(Outcome::Refused(code));
    }

    let mut diagnostics: Vec<Diagnostic> = product_scan.diagnostics.as_slice().to_vec();
    let Some(intent) = product_scan.intent else {
        report(&diagnostics);
        anyhow::bail!("`{}` could not be evaluated", product_file.display());
    };

    // The product's own default when none is named, so the common invocation is
    // short and the answer still comes from the description rather than from a
    // guess made here.
    let profile = match profile {
        Some(id) => ProfileId::new(id)?,
        None => intent.default_profile.clone(),
    };

    let resolution = gearbox_engine::resolve::resolve_at(
        &scan.catalogue,
        &intent,
        &profile,
        Some(&product_file),
    );
    let sources = gearbox_engine::lock_sources(&opened, &scan.catalogue, &product_file);
    let lock =
        gearbox_engine::resolve::product::assemble(&scan.catalogue, &intent, &resolution, sources);
    diagnostics.extend(lock.diagnostics.as_slice().iter().cloned());

    Ok(Outcome::Resolved(Box::new(Resolution {
        opened,
        scan,
        product_file,
        intent,
        profile,
        lock,
        diagnostics,
    })))
}
