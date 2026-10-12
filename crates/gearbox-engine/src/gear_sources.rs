//! Which source each gear a product uses comes from.
//!
//! Several open sources may declare one gear id: the platform's gear and an
//! organization's fork of it keep the same id on purpose, because everything
//! that names the gear -- a contract owner, a binding, a plugin host -- names
//! it by id. The catalogue lists every copy (`alternatives`) and a catalogue
//! loaded for a product puts the copy the product chose in `gears`
//! ([`crate::load_catalogue_pinned`]). What is left is to say when the product
//! did not choose and had to: that is this module.
//!
//! Two answers, both errors:
//!
//! * **GBX0325** -- the product uses a gear several open sources declare and
//!   names none of them, either in its `use_gear` or, for a gear the closure
//!   pulled in, with a `use_gear` at all. Taking the first root would make the
//!   build depend on the order a caller passed its roots in.
//! * **GBX0326** -- `use_gear(id, source = X)` names an open source that does
//!   not declare the gear while another one does.
//!
//! "Open" is the catalogue's own `sources`, which is every root the catalogue
//! was loaded from. A `source = ` naming a source no root carries counts as
//! unnamed: a product written for a checkout the caller did not open still
//! resolves against the one it did, as long as only one copy is in reach.

use std::collections::BTreeSet;

use gearbox_ir::{
    Catalogue, Diagnostic, DiagnosticCode, Diagnostics, GearId, Location, ProductIntent, SourceId,
};

/// Report every gear whose source the product leaves undecided, or decides
/// wrongly.
///
/// `members` are the gears in play beyond the product's own selections -- the
/// closure, when resolving. Validate passes none: it does not know what the
/// selections pull in.
pub fn check<'a>(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    members: impl IntoIterator<Item = &'a GearId>,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let mut selected: BTreeSet<&GearId> = BTreeSet::new();
    for selection in &intent.selected_gears {
        selected.insert(&selection.gear);
        let declaring = catalogue.sources_declaring(&selection.gear);
        let at = Location::or_file(selection.declared_at.as_ref(), uri);
        let named = selection
            .source
            .as_ref()
            .filter(|s| catalogue.sources.contains_key(*s));

        match named {
            Some(source) => {
                let used = catalogue.gear(&selection.gear).map(|g| &g.source);
                // Absent from `gears` altogether and declared nowhere is
                // GBX0301's to report; a projection failure has its own.
                if used == Some(source) || declaring.is_empty() {
                    continue;
                }
                // The source does declare it, and the catalogue holds another
                // copy: it was loaded without this product's pins. A caller
                // fault rather than the description's, said as such rather than
                // as "not declared", which would send the reader to a file
                // that is right.
                if declaring.contains(&source) {
                    diagnostics.push(
                        Diagnostic::error(
                            DiagnosticCode::TopologyGearNotInSource,
                            format!(
                                "`use_gear(\"{}\", source = \"{source}\")`: the catalogue in use \
                                 holds the copy from `{}`, not this product's choice",
                                selection.gear,
                                used.map_or("no source", SourceId::as_str),
                            ),
                            "the catalogue was loaded without the product's source choices; \
                             reload it for this product",
                        )
                        .at(at),
                    );
                    continue;
                }
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::TopologyGearNotInSource,
                        format!(
                            "`use_gear(\"{}\", source = \"{source}\")`: source `{source}` does not \
                             declare `{}`{}",
                            selection.gear,
                            selection.gear,
                            declared_in(&declaring),
                        ),
                        format!(
                            "name the source that declares it{}, or check that `{source}` is \
                             the checkout you meant",
                            suggestion(&selection.gear, &declaring)
                        ),
                    )
                    .at(at),
                );
            }
            None if declaring.len() > 1 => {
                let written = match &selection.source {
                    Some(source) => format!(
                        "`use_gear(\"{}\", source = \"{source}\")` names a source no open root \
                         carries",
                        selection.gear
                    ),
                    None => format!("`use_gear(\"{}\")` names no source", selection.gear),
                };
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::TopologyAmbiguousGearSource,
                        format!(
                            "gear `{}` is declared by {} sources ({}), and {written}",
                            selection.gear,
                            declaring.len(),
                            listed(&declaring),
                        ),
                        format!(
                            "say which one is used{}",
                            suggestion(&selection.gear, &declaring)
                        ),
                    )
                    .at(at),
                );
            }
            None => {}
        }
    }

    for id in members {
        if selected.contains(id) {
            continue;
        }
        let declaring = catalogue.sources_declaring(id);
        if declaring.len() > 1 {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::TopologyAmbiguousGearSource,
                    format!(
                        "gear `{id}` is pulled into the product and declared by {} sources ({}), \
                         and no `use_gear` says which one is used",
                        declaring.len(),
                        listed(&declaring),
                    ),
                    format!("add{}", suggestion(id, &declaring)),
                )
                .at(Location::file(uri.to_owned())),
            );
        }
    }
}

fn listed(sources: &[&SourceId]) -> String {
    sources
        .iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn declared_in(sources: &[&SourceId]) -> String {
    if sources.is_empty() {
        String::new()
    } else {
        format!("; it is declared in {}", listed(sources))
    }
}

fn suggestion(id: &GearId, sources: &[&SourceId]) -> String {
    match sources.first() {
        Some(first) => format!(": `use_gear(\"{id}\", source = \"{first}\")`"),
        None => String::new(),
    }
}
