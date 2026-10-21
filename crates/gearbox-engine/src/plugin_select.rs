//! Resolving which plugin implementation a host gets, per deployment profile.
//!
//! This is the half that needs both halves: the catalogue says which extension
//! points exist and what each plugin implements, the product says which
//! implementations are linked and how they are configured. Neither alone can
//! answer "will this host find a plugin".
//!
//! Two levels, and they must not be conflated:
//!
//! - **Composition-time**, decided here: which implementations are linked into
//!   the process. A plugin absent from the binary registers no GTS instance and
//!   does not exist for the host.
//! - **Runtime**, decided by the host: which linked implementation wins for a
//!   given call. `TOOLKIT_PLUGINS.md` allows selection "by configuration,
//!   tenant, or other context", so several implementations of one point may be
//!   linked deliberately.
//!
//! So a second matching implementation is reported as information, not as an
//! error: it is defined behaviour, and it may be exactly what a multi-tenant
//! product wants.

use gearbox_ir::{
    Catalogue, Diagnostic, DiagnosticCode, Diagnostics, ExtensionPointDecl, GearDescriptor, GearId,
    Location, PluginSelection, ProductIntent, ProfileId,
};

/// What one extension point resolves to in one profile.
#[derive(Debug, Clone)]
pub struct PointResolution {
    pub host: GearId,
    pub point: ExtensionPointDecl,
    pub profile: ProfileId,
    /// Implementations linked in this profile, in catalogue order.
    pub linked: Vec<GearId>,
    /// The one the host's vendor selector would take: lowest priority among
    /// those whose vendor matches. `None` when nothing matches, or when the
    /// lowest priority is shared and the outcome is therefore undefined.
    pub winner: Option<GearId>,
    /// Whether several matching implementations share the lowest priority.
    pub tied: bool,
}

/// Report a present-but-wrong-type `vendor` / `priority` rather than treating
/// it as unset (which would silently change who wins).
fn report_plugin_config_types(intent: &ProductIntent, uri: &str, diagnostics: &mut Diagnostics) {
    for selection in &intent.selected_gears {
        if let Some(value) = selection.config.get("vendor")
            && !value.is_string()
        {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlConfigTypeMismatch,
                    format!("gear `{}`: `vendor` must be a string", selection.gear),
                    "set `vendor` to a string, or drop the key to use the crate default",
                )
                .at(gearbox_ir::Location::or_file(
                    selection.declared_at.as_ref(),
                    uri,
                )),
            );
        }
        for plugin in &selection.plugins {
            if let Err(msg) = plugin.configured_vendor() {
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::GdlConfigTypeMismatch,
                        format!("plugin `{}`: {msg}", plugin.gear),
                        "set `vendor` to a string, or drop the key to use the crate default",
                    )
                    .at(gearbox_ir::Location::or_file(
                        plugin.declared_at.as_ref(),
                        uri,
                    )),
                );
            }
            if let Err(msg) = plugin.configured_priority() {
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::GdlConfigTypeMismatch,
                        format!("plugin `{}`: {msg}", plugin.gear),
                        "set `priority` to an integer, or drop the key to use the crate default",
                    )
                    .at(gearbox_ir::Location::or_file(
                        plugin.declared_at.as_ref(),
                        uri,
                    )),
                );
            }
        }
    }
}

/// The vendor a host will search for.
///
/// Read from the product's config for the host at the key its extension point
/// names -- `idp.vendor` is `config = {"idp": {"vendor": ...}}` -- or the
/// top-level `vendor` when none is named. The default comes from the same
/// field, projected into `vendor_selector`.
fn host_vendor<'a>(gear: &'a GearDescriptor, intent: &'a ProductIntent) -> Option<&'a str> {
    let key = gear
        .extension_points
        .iter()
        .find_map(|p| p.selector.as_deref())
        .unwrap_or("vendor");
    intent
        .selected_gears
        .iter()
        .find(|s| s.gear == gear.id)
        .and_then(|s| {
            let mut segments = key.split('.');
            let first = s.config.get(segments.next()?)?;
            segments.try_fold(first, |value, segment| value.get(segment))
        })
        .and_then(serde_json::Value::as_str)
        .or(gear.vendor_selector.as_deref())
}

/// The vendor a plugin registers itself under with nothing configured.
fn default_vendor(gear: &GearDescriptor) -> Option<&str> {
    gear.implements
        .as_ref()
        .and_then(|f| f.default_vendor.as_deref())
}

/// Whether a host and one implementation would find each other with no `vendor`
/// set anywhere in the product.
///
/// Public because the CLI's `plugins` listing asks exactly this question and had
/// its own copy of the comparison. The rule is the one [`check`] applies -- the
/// selector against what the plugin registers under -- and two spellings of it
/// can disagree, which for a listing means telling an operator the opposite of
/// what the resolver will do.
#[must_use]
pub fn default_vendors_agree(host: &GearDescriptor, implementation: &GearDescriptor) -> bool {
    host.vendor_selector.as_deref() == default_vendor(implementation)
}

/// The vendor a plugin will register itself under.
fn plugin_vendor<'a>(selection: &'a PluginSelection, catalogue: &'a Catalogue) -> Option<&'a str> {
    selection
        .configured_vendor()
        .ok()
        .flatten()
        .or_else(|| catalogue.gear(&selection.gear).and_then(default_vendor))
}

/// Lower wins. A plugin that states no priority sorts last, so an explicit
/// value always beats an unstated one.
fn plugin_priority(selection: &PluginSelection, catalogue: &Catalogue) -> i64 {
    selection
        .configured_priority()
        .ok()
        .flatten()
        .or_else(|| {
            catalogue
                .gear(&selection.gear)
                .and_then(|g| g.implements.as_ref())
                .and_then(|f| f.default_priority)
        })
        .unwrap_or(i64::MAX)
}

/// Check every selected host's extension points, in every profile.
///
/// Per profile rather than once: the canonical product runs a static plugin in
/// dev and a real one in prod, so "is this point implemented" only has an answer once
/// a profile is fixed.
pub fn check(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Vec<PointResolution> {
    // Each profile on the configuration it runs: a `profile_config` can change
    // a host's or a plugin's `vendor`, the join key, so the raw intent would
    // answer for no profile in particular. What deriving it reports is the
    // resolver's and `validate`'s to say, not this check's.
    let mut out = Vec::new();
    for profile in intent.profiles.keys() {
        let effective =
            crate::resolve::profile::effective(intent, profile, uri, &mut Diagnostics::new());
        out.extend(check_profiles(
            catalogue,
            &effective,
            &[profile],
            uri,
            diagnostics,
        ));
    }
    out
}

/// As [`check`], for the named profiles only.
///
/// **What the resolver runs, and why it has to.** This check lived only in
/// `validate`, and neither the Studio nor `generate` validates: both resolve. So
/// a host with no plugin for its point -- GBX0511, an error the runtime meets as
/// `PluginNotFound` at its first request -- was reported by the CLI's `validate`
/// and by nothing a person building the product would see. Measured in the
/// Studio: "errors 0" beside a description `validate` refused three times over,
/// and a generated tree for it. A resolution is for one profile, so it asks about
/// that one; `validate` still asks about all of them.
///
/// A profile the description does not declare is skipped: the resolver reports
/// it on its own, and "no implementation in a profile that does not exist" would
/// be a second, wrong, account of the same mistake.
pub fn check_profiles(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    profiles: &[&ProfileId],
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Vec<PointResolution> {
    let mut out = Vec::new();
    report_plugin_config_types(intent, uri, diagnostics);

    for profile in profiles
        .iter()
        .copied()
        .filter(|p| intent.profiles.contains_key(*p))
    {
        for selection in &intent.selected_gears {
            let Some(host) = catalogue.gear(&selection.gear) else {
                continue;
            };
            if host.extension_points.is_empty() {
                continue;
            }
            let wanted = host_vendor(host, intent);

            for point in &host.extension_points {
                out.push(resolve_point(
                    catalogue,
                    host,
                    point,
                    profile,
                    wanted,
                    selection,
                    uri,
                    diagnostics,
                ));
            }
        }
    }

    report_orphan_plugins(catalogue, intent, uri, diagnostics);
    report_misplaced_plugins(catalogue, intent, uri, diagnostics);
    out
}

/// A plugin listed under a host that does not declare its point.
///
/// The gap `report_orphan_plugins` leaves. That one asks whether *some* selected
/// gear expects the plugin's point, which is the right question for a plugin
/// selected as an ordinary gear and the wrong one for a plugin written into a
/// specific host's `plugins = [...]`: with `authn-resolver` also in the product,
/// listing `oidc-authn-plugin` under `types-registry` passed both checks while
/// meaning nothing at all -- the host looks for no implementation and the plugin
/// registers a trait nobody queries.
///
/// Reported per host entry rather than per profile: the list is profile-scoped
/// but the mismatch is not, and one sentence per wrong pair is the useful count.
fn report_misplaced_plugins(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for selection in &intent.selected_gears {
        let Some(host) = catalogue.gear(&selection.gear) else {
            continue;
        };
        for plugin in &selection.plugins {
            let Some(implements) = catalogue
                .gear(&plugin.gear)
                .and_then(|g| g.implements.as_ref())
            else {
                // Not a plugin at all, or absent from the catalogue. Both are
                // other codes' business (GBX0301, GBX0516), and naming them here
                // would report one fault twice.
                continue;
            };
            if host.declares_point(&implements.spec) {
                continue;
            }
            let declares = if host.extension_points.is_empty() {
                "declares no extension point".to_owned()
            } else {
                format!(
                    "declares {}",
                    host.extension_points
                        .iter()
                        .map(ExtensionPointDecl::qualified)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::PluginPointNotDeclared,
                    format!(
                        "gear `{}` lists plugin `{}`, which implements `{}`, but `{}` {declares}",
                        selection.gear,
                        plugin.gear,
                        implements.describe(),
                        selection.gear
                    ),
                    format!(
                        "list `{}` under the gear that declares `{}`, or drop it",
                        plugin.gear,
                        implements.describe()
                    ),
                )
                .at(gearbox_ir::Location::or_file(
                    plugin.declared_at.as_ref(),
                    uri,
                )),
            );
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one call site; bundling these into a struct would only move the list"
)]
fn resolve_point(
    catalogue: &Catalogue,
    host: &GearDescriptor,
    point: &ExtensionPointDecl,
    profile: &ProfileId,
    wanted: Option<&str>,
    selection: &gearbox_ir::GearSelection,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> PointResolution {
    // Implementations of *this* point, linked in *this* profile.
    let linked: Vec<&PluginSelection> = selection
        .plugins
        .iter()
        .filter(|p| ProductIntent::applies(&p.profiles, profile))
        .filter(|p| {
            catalogue
                .gear(&p.gear)
                .and_then(|g| g.implements.as_ref())
                .is_some_and(|f| f.spec == point.spec)
        })
        .collect();

    let mut resolution = PointResolution {
        host: host.id.clone(),
        point: point.clone(),
        profile: profile.clone(),
        linked: linked.iter().map(|p| p.gear.clone()).collect(),
        winner: None,
        tied: false,
    };

    if linked.is_empty() {
        let available = catalogue.implementations_of(point);
        let hint = if available.is_empty() {
            "no gear in the catalogue implements it".to_owned()
        } else {
            format!(
                "available: {}",
                available
                    .iter()
                    .map(|g| g.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::PluginPointUnfilled,
                format!(
                    "in profile `{profile}`, gear `{}` has no implementation for extension \
                     point `{}`; {hint}",
                    host.id,
                    point.qualified()
                ),
                "add `plugins = [plugin(\"...\")]` to this gear's `use_gear`, scoping it with \
                 `profiles = [...]` if it differs per profile",
            )
            .at(Location::file(uri.to_owned())),
        );
        return resolution;
    }

    // Of those linked, which the host's selector will actually find.
    let mut matching: Vec<&PluginSelection> = linked
        .iter()
        .copied()
        .filter(|p| plugin_vendor(p, catalogue) == wanted)
        .collect();

    if matching.is_empty() {
        let offered = linked
            .iter()
            .map(|p| {
                format!(
                    "{} offers `{}`",
                    p.gear,
                    plugin_vendor(p, catalogue).unwrap_or("<none>")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::PluginVendorMismatch,
                format!(
                    "in profile `{profile}`, gear `{}` selects vendor `{}` but no linked \
                     implementation of `{}` registers under it: {offered}",
                    host.id,
                    wanted.unwrap_or("<none>"),
                    point.qualified()
                ),
                "set the same `vendor` on both sides, or drop both overrides and let the \
                 compiled-in defaults agree",
            )
            .at(Location::file(uri.to_owned())),
        );
        return resolution;
    }

    matching.sort_by_key(|p| plugin_priority(p, catalogue));

    // A tie on priority is genuinely undecided. The host takes the lowest
    // priority from whatever types-registry returns, and nothing orders equal
    // priorities -- so naming a winner here would claim more than the runtime
    // guarantees. Report the tie instead.
    let tied = matching.len() > 1
        && plugin_priority(matching[0], catalogue) == plugin_priority(matching[1], catalogue);

    if matching.len() > 1 {
        let order = matching
            .iter()
            .map(|p| format!("{} (priority {})", p.gear, plugin_priority(p, catalogue)))
            .collect::<Vec<_>>()
            .join(", ");
        let (verdict, help) = if tied {
            (
                format!(
                    "and share priority {}, so which one the host resolves is undefined",
                    plugin_priority(matching[0], catalogue)
                ),
                "give them distinct priorities, distinct vendors, or scope them to different \
                 profiles -- as written, the winner depends on registry order",
            )
        } else {
            (
                format!("and `{}` wins on priority", matching[0].gear),
                "this is defined behaviour and may be intended -- the host may also select \
                 per tenant at runtime; give them distinct vendors if it is not",
            )
        };
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::PluginVendorAmbiguous,
                format!(
                    "in profile `{profile}`, {} implementations of `{}` share vendor \
                     `{}` {verdict}. Order: {order}",
                    matching.len(),
                    point.qualified(),
                    wanted.unwrap_or("<none>"),
                ),
            )
            .at(Location::file(uri.to_owned()))
            .with_help(help),
        );
    }

    // Only claim a winner when the priorities actually decide one.
    resolution.winner = (!tied).then(|| matching[0].gear.clone());
    resolution.tied = tied;
    resolution
}

/// A plugin selected as an ordinary gear, with nothing expecting its point.
///
/// Linking a plugin whose host is absent is inert at best: it registers a GTS
/// instance nobody queries. Worth naming, because it usually means the host was
/// forgotten rather than the plugin being spare.
fn report_orphan_plugins(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for selection in &intent.selected_gears {
        let Some(implements) = catalogue
            .gear(&selection.gear)
            .and_then(|g| g.implements.as_ref())
        else {
            continue;
        };
        let has_host = intent.selected_gears.iter().any(|other| {
            catalogue
                .gear(&other.gear)
                .is_some_and(|g| g.declares_point(&implements.spec))
        });
        if !has_host {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::PluginHostNotSelected,
                    format!(
                        "gear `{}` implements extension point `{}`, but no selected gear \
                         expects it",
                        selection.gear,
                        implements.describe()
                    ),
                    "select the host gear and list this one under its `plugins = [...]`, or \
                     drop it",
                )
                .at(gearbox_ir::Location::or_file(
                    selection.declared_at.as_ref(),
                    uri,
                )),
            );
        }
    }
}
