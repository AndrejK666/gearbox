//! Contracts a gear's code resolves without declaring them (GBX0215; test case
//! G16 of the service-deployment roadmap).
//!
//! The catalogue knows what a gear *declares* -- `#[toolkit::consumes]`,
//! `consume(...)`, `deps` -- and, from `gearbox_project::project_client_uses`,
//! which contract traits its code resolves with `hub.get::<dyn Trait>()`. This
//! is where the two are compared, once every contract in the catalogue is known.

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::{
    Catalogue, ContractDescriptor, Diagnostic, DiagnosticCode, Diagnostics, GearDescriptor, GearId,
    Location,
};

/// Report a contract a gear's code resolves from the `ClientHub` without
/// declaring it (GBX0215).
///
/// A call names a trait; the catalogue maps it to a contract only when exactly
/// one contract has that trait name -- a plugin's interface or a framework
/// client (`DirectoryClient`) is no contract, and an ambiguous name is not
/// guessed at. The call is declared when the gear consumes or provides that
/// contract, or links one of its providers through `deps` (an in-process edge
/// that never crosses a process boundary). Calls are read per crate, so a gear
/// that shares its crate with another (mini-chat's plugins live in mini-chat)
/// is answered for by any gear of that crate.
pub fn check(
    catalogue: &Catalogue,
    client_uses: &BTreeMap<GearId, BTreeSet<String>>,
    uri: impl Fn(&GearDescriptor) -> Option<String>,
    diagnostics: &mut Diagnostics,
) {
    let mut by_trait: BTreeMap<&str, Vec<&ContractDescriptor>> = BTreeMap::new();
    for contract in catalogue.contracts.values() {
        by_trait
            .entry(contract.trait_ident())
            .or_default()
            .push(contract);
    }
    for (id, traits) in client_uses {
        let Some(gear) = catalogue.gears.get(id) else {
            continue;
        };
        for trait_ident in traits {
            let Some([contract]) = by_trait.get(trait_ident.as_str()).map(Vec::as_slice) else {
                continue;
            };
            let declares = |g: &GearDescriptor| {
                g.consumes
                    .iter()
                    .any(|r| r.contract() == Some(&contract.id))
                    || g.provides.iter().any(|p| p.contract == contract.id)
                    || catalogue.gears.values().any(|other| {
                        g.colocated_deps.contains(&other.id)
                            && other.provides.iter().any(|p| p.contract == contract.id)
                    })
            };
            let same_crate = |g: &&GearDescriptor| {
                g.source == gear.source && g.package.path == gear.package.path
            };
            if catalogue.gears.values().filter(same_crate).any(declares) {
                continue;
            }
            let mut diagnostic = Diagnostic::new(
                DiagnosticCode::ValidateUndeclaredClientUse,
                format!(
                    "`{id}` resolves `dyn {trait_ident}` ({}) from the ClientHub, but neither \
                     consumes nor provides it, and links no provider of it through `deps`",
                    contract.id
                ),
            )
            .with_help(format!(
                "declare `#[toolkit::consumes(contract = ..., from = \"...\")]` on the gear and \
                 `consume(contract = \"{}\")` in its gear.gdl, or link the provider with `deps` if \
                 the edge must stay in-process",
                contract.base_name
            ));
            if let Some(uri) = uri(gear) {
                diagnostic = diagnostic.at(Location::file(uri));
            }
            diagnostics.push(diagnostic);
        }
    }
}
