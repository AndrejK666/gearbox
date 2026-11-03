//! What is wrong with one description, given the text rather than the file.
//!
//! The language server's whole engine-side surface. One function rather than
//! two because the rule deciding which evaluator runs is the file's *name*, and
//! those two names are `gearbox_gdl::PRODUCT_FILE` and `GEAR_FILE` -- one copy,
//! in the crate that owns the language, consumed by this and by
//! `gearbox_gdl::assist::vocabulary_for`. A caller applying the rule itself
//! would be a third place for it to live, and the editor's language
//! registration would be a fourth.
//!
//! Reads nothing. The text belongs to the client's buffer, which is the point:
//! what is on disk is the question `load_product` and `load_catalogue` answer,
//! and an editor asking about a file it has not saved must not be told about
//! the version it is replacing.

use std::path::Path;

use gearbox_gdl::GdlEngine;
use gearbox_gdl::engine::{FileIdentity, LoadPaths};
use gearbox_ir::{Diagnostics, RelPath, SourceId};

use crate::catalogue::GEAR_FILE;
use crate::product::{PRODUCT_FILE, eval_product_text};

/// Evaluate `source` as the kind of description `path` names.
///
/// `None` means the name is not `product.gdl`, `gear.gdl` or `frontx.gdl`, so there is
/// nothing this can say about the file -- distinct from `Some` with no
/// diagnostics, which says the description is clean.
///
/// `source_root` is the gear source root this file was found in, when it was
/// found in one. **It is deliberately ignored for a product**, which is never
/// inside a source root: `load_product` passes `None` at every call site, so a
/// product handed a wider `load()` boundary than the real loader gives it could
/// come back clean here and fail when it is loaded from disk. Answering the same
/// question two ways depending on who asked is the one thing this must not do.
///
/// **Cheap on purpose.** This is called per keystroke, so it evaluates the one
/// description and nothing else. `gearbox/validate` answers a superficially
/// similar question by rescanning the whole corpus, which is right for a button
/// and wrong here by three orders of magnitude.
#[must_use]
pub fn check_description(
    path: &Path,
    source_root: Option<&Path>,
    source: &str,
) -> Option<Diagnostics> {
    match path.file_name()?.to_str()? {
        PRODUCT_FILE => Some(eval_product_text(path, None, source).diagnostics),
        GEAR_FILE => Some(eval_gear_text(path, source_root, source)),
        gearbox_gdl::FRONTX_FILE => Some(eval_frontx_text(path, source)),
        _ => None,
    }
}

/// Evaluate a `gear.gdl` that is not (yet) what is on disk.
///
/// The gear-side twin of `eval_product_text`, and shaped like it for the same
/// reason: where the description lives decides how `load()` resolves, so the
/// path is still required even though the bytes come from the caller.
///
/// Returns only diagnostics. The declaration a gear evaluates to is of no use
/// without the crate scan that turns it into a `GearDescriptor`, and that is
/// `load_catalogue`'s job; handing back a half-built value here would invite a
/// caller to treat it as one.
fn eval_gear_text(path: &Path, root: Option<&Path>, source: &str) -> Diagnostics {
    let dir = path.parent().unwrap_or(Path::new("."));
    let root = root.unwrap_or(dir);

    let gdl_path = path
        .strip_prefix(root)
        .ok()
        .map(|r| {
            r.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        })
        .and_then(|text| RelPath::new(text).ok())
        .unwrap_or_else(RelPath::here);

    let identity = FileIdentity {
        uri: gearbox_ir::file_uri(path),
        // Which source root this file belongs to is a catalogue question, and an
        // editor need not have opened one. The id reaches no diagnostic -- those
        // carry `uri` -- so a placeholder here is honest, where guessing a real
        // source id would not be.
        source: SourceId::new("document").unwrap_or_else(|_| unreachable!("`document` is kebab")),
        gdl_path,
        load_paths: Some(LoadPaths {
            base: dir.to_path_buf(),
            root: root.to_path_buf(),
        }),
    };

    let mut diagnostics = Diagnostics::new();
    diagnostics.extend(GdlEngine::new().eval_gear(&identity, source).diagnostics);
    diagnostics.finish();
    diagnostics
}

/// Evaluate a `frontx.gdl` (GDL 1.5). It has no `load()`: a package describes
/// itself in one file, so there is no boundary to choose.
fn eval_frontx_text(path: &Path, source: &str) -> Diagnostics {
    let identity = FileIdentity {
        uri: gearbox_ir::file_uri(path),
        source: SourceId::new("document").unwrap_or_else(|_| unreachable!("`document` is kebab")),
        gdl_path: RelPath::new(gearbox_gdl::FRONTX_FILE)
            .unwrap_or_else(|_| unreachable!("`frontx.gdl` is relative")),
        load_paths: None,
    };
    let mut diagnostics = Diagnostics::new();
    diagnostics.extend(GdlEngine::new().eval_frontx(&identity, source).diagnostics);
    diagnostics.finish();
    diagnostics
}
