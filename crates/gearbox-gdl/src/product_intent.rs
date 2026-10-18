//! Converting a `product(...)` declaration into typed operator intent.
//!
//! Everything checked here is a question the file can answer about *itself*:
//! are the ids well-formed, does `default_profile` name a declared profile, do
//! two declarations collide on the same profile. None of it needs a catalogue.
//!
//! What is deliberately **not** checked: whether a named gear, contract or
//! provider exists. Those are the resolver's questions, and answering them here
//! would mean a product description could not be edited in an editor until
//! every source it names had been scanned -- which is exactly the latency the
//! RPC surface is meant to avoid.
//!
//! Errors accumulate rather than stopping at the first. An operator fixing a
//! product file wants the whole list, and a single missing profile id should not
//! hide five malformed gear names.

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::{
    ApplicationId, ApplicationPin, BindingIntent, BindingMode, ClusterScopeIntent, ContractId,
    DeploymentProfileDecl, Diagnostic, DiagnosticCode, Diagnostics, Discovery, GearId,
    GearSelection, Location, PluginSelection, Preference, ProductIntent, ProfileId,
    ProviderBinding, SourceDecl, SourceId, Transport,
};

use crate::engine::FileIdentity;
use crate::records::{ClusterProfileRecord, ProfileRecord, ProviderBindingRecord};
use crate::sink::ProductDecl;

/// The declared profiles, which everything else is scoped against.
type Profiles = BTreeMap<ProfileId, DeploymentProfileDecl>;

/// What has already been claimed for one `(subject, key)` pair.
///
/// An unscoped declaration applies to *every* profile, so it cannot be modelled
/// as one more entry beside the scoped ones: the two orders would then disagree.
/// `all` records it as the distinct thing it is, which is what makes
/// scoped-then-unscoped collide exactly as unscoped-then-scoped does.
#[derive(Default)]
struct Claimed {
    /// An unscoped declaration has been seen.
    all: bool,
    /// The profiles scoped declarations have taken.
    profiles: BTreeSet<ProfileId>,
}

/// Claims so far, keyed by `(subject, key)`.
type Claims = BTreeMap<(String, String), Claimed>;

/// A declared string, or `None` when it is absent or blank.
///
/// Blank and absent mean the same thing for a location, and neither is a value
/// worth putting in a lock.
fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `at` rather than `uri`, so every call site states where it points.
///
/// The four records that carry a span -- `source`, the three profile kinds,
/// `use_gear` and `bind` -- pass `Location::or_file(record.declared_at.as_ref(),
/// uri)`. Everything else passes `Location::file(uri.to_owned())`, because there
/// is no span to pass: `templates`, `plugin`, `application`, `cluster_profile`
/// and `prefer` take no `eval` yet, and the id validators are handed a bare
/// string. Spelling the fallback out at the call site is the point -- a shared
/// `uri` parameter hid which diagnostics could be anchored and which could not.
fn invalid(at: Location, message: impl Into<String>, help: impl Into<String>) -> Diagnostic {
    Diagnostic::error(DiagnosticCode::GdlEval, message, help).at(at)
}

fn collision(at: Location, message: impl Into<String>, help: impl Into<String>) -> Diagnostic {
    Diagnostic::error(DiagnosticCode::GdlDuplicateProfileScoped, message, help).at(at)
}

/// Build typed intent, reporting everything wrong with the declaration.
///
/// Returns `None` only when the product has no usable identity -- no profiles,
/// or a `default_profile` naming none of them. Anything else is reported and the
/// entry skipped, so one bad binding does not hide the rest of the file.
pub fn build(
    identity: &FileIdentity,
    decl: &ProductDecl,
    diagnostics: &mut Diagnostics,
) -> Option<ProductIntent> {
    let uri = identity.uri.as_str();

    // Profiles first: everything else is scoped to them, so a `profiles = [...]`
    // naming an undeclared id can only be caught once these are known.
    let profiles = build_profiles(uri, decl, diagnostics);
    if profiles.is_empty() {
        diagnostics.push(invalid(
            Location::file(uri.to_owned()),
            "product declares no deployment profile",
            "add at least one, e.g. `profiles = [embedded(id = \"dev\")]`",
        ));
        return None;
    }

    let default_profile = profile_id(None, uri, &decl.default_profile, diagnostics)?;
    if !profiles.contains_key(&default_profile) {
        diagnostics.push(invalid(
            Location::file(uri.to_owned()),
            format!(
                "`default_profile = \"{default_profile}\"` names no declared profile; \
                 declared: {}",
                joined(profiles.keys().map(ProfileId::as_str)),
            ),
            "point `default_profile` at one of the declared profile ids",
        ));
        return None;
    }

    let sources = build_sources(uri, decl, diagnostics);

    let ui = decl.ui.as_ref().and_then(|ui| {
        let modules: Result<std::collections::BTreeSet<_>, _> = ui
            .modules
            .iter()
            .map(|id| GearId::new(id.clone()))
            .collect();
        match modules {
            Ok(modules) => Some(gearbox_ir::ui::ProductUi {
                shell: ui.shell.clone(),
                sdk: ui.sdk.clone(),
                source: ui.source.clone(),
                modules,
            }),
            Err(error) => {
                diagnostics.push(Diagnostic::error(
                    DiagnosticCode::GdlEval,
                    format!("invalid UI module id: {error}"),
                    "use gear ids in ui_product(modules = [...])",
                ));
                None
            }
        }
    });
    Some(ProductIntent {
        id: decl.id.clone(),
        display_name: decl.display_name.clone(),
        version: decl.version.clone(),
        gdl_path: identity.gdl_path.clone(),
        selected_gears: build_gears(uri, decl, &sources, &profiles, diagnostics),
        bindings: build_bindings(uri, decl, &profiles, diagnostics),
        cluster_scopes: build_cluster_scopes(uri, decl, &profiles, diagnostics),
        application_pins: build_application_pins(uri, decl, &profiles, diagnostics),
        preferences: build_preferences(uri, decl, diagnostics),
        templates: build_templates(uri, decl, diagnostics),
        layout: decl.layout.clone(),
        sources,
        profiles,
        default_profile,
        ui,
    })
}

fn build_profiles(uri: &str, decl: &ProductDecl, diagnostics: &mut Diagnostics) -> Profiles {
    let mut profiles = Profiles::new();
    for record in &decl.profiles {
        let Some(id) = profile_id(record.declared_at.as_ref(), uri, &record.id, diagnostics) else {
            continue;
        };
        let Some(profile) = deployment_profile(uri, record, &id, diagnostics) else {
            continue;
        };
        if profiles.insert(id.clone(), profile).is_some() {
            diagnostics.push(collision(
                Location::or_file(record.declared_at.as_ref(), uri),
                format!("deployment profile `{id}` is declared twice"),
                "give each profile a distinct id; the id is what `--profile` selects",
            ));
        }
    }
    profiles
}

/// The declared template overlay, or `None` for the convention beside the file.
///
/// Refuses `git(...)` and `registry(...)` by name rather than reporting an
/// unknown argument: both are spellable, and a message saying which one was
/// written and why it cannot work is worth more than a parse error. Fetching is
/// the reason -- `generate` is a pure function of the lock, and a template set
/// pulled over the network at generation time would make `--dry-run` a preview
/// of whatever the remote said this morning.
fn build_templates(uri: &str, decl: &ProductDecl, diagnostics: &mut Diagnostics) -> Option<String> {
    let record = decl.templates.as_ref()?;
    match record.kind.as_str() {
        "path" => {
            let at = non_empty(record.at.as_deref());
            if at.is_none() {
                diagnostics.push(invalid(
                    Location::file(uri.to_owned()),
                    "`templates` declares `path()` with no directory".to_owned(),
                    "write `templates = path(\"../../house-templates\")`; the path is relative \
                     to the product description and may point outside it",
                ));
            }
            at
        }
        other => {
            diagnostics.push(invalid(
                Location::file(uri.to_owned()),
                format!("`templates` uses `{other}()`, which generation cannot read"),
                "use `path(\"...\")`; a template set fetched at generation time would make \
                 `--dry-run` a preview of whatever the remote said at the time",
            ));
            None
        }
    }
}

fn build_sources(
    uri: &str,
    decl: &ProductDecl,
    diagnostics: &mut Diagnostics,
) -> BTreeMap<SourceId, SourceDecl> {
    let mut sources = BTreeMap::new();
    for record in &decl.sources {
        let id = match SourceId::new(record.id.clone()) {
            Ok(id) => id,
            Err(e) => {
                diagnostics.push(invalid(
                    Location::or_file(record.declared_at.as_ref(), uri),
                    format!("source id `{}` is not valid: {e}", record.id),
                    "source ids are kebab-case",
                ));
                continue;
            }
        };
        let at = match record.at.kind.as_str() {
            // `unwrap_or_default()` used to stand here, which turned a missing
            // or blank location into `at = ""` -- a source pointing at nothing,
            // recorded in the lock as if it were a real answer.
            "path" => {
                let Some(at) = non_empty(record.at.at.as_deref()) else {
                    diagnostics.push(invalid(
                        Location::or_file(record.declared_at.as_ref(), uri),
                        format!("source `{id}` declares `path()` with no directory"),
                        "write `path(\"../some-repo\")`; the path is relative to the product \
                         description",
                    ));
                    continue;
                };
                let crates = match &record.crates {
                    None => None,
                    Some(crates) if crates.kind == "registry" => non_empty(crates.url.as_deref()),
                    Some(crates) => {
                        diagnostics.push(invalid(
                            Location::or_file(record.declared_at.as_ref(), uri),
                            format!(
                                "source `{id}` takes its crates from `{}()`, which is not a \
                                 registry",
                                crates.kind
                            ),
                            "write `crates = registry(\"crates.io\")`",
                        ));
                        None
                    }
                };
                SourceDecl::Path {
                    at,
                    crates,
                    declared_at: record.declared_at.clone(),
                }
            }
            "git" | "registry" if record.crates.is_some() => {
                diagnostics.push(invalid(
                    Location::or_file(record.declared_at.as_ref(), uri),
                    format!(
                        "source `{id}` declares `crates` on a `{}()` source",
                        record.at.kind
                    ),
                    "`crates = registry(...)` belongs to a `path(...)` checkout: it says where \
                     the build takes the crates whose descriptions the checkout carries",
                ));
                continue;
            }
            "git" => {
                let Some(url) = non_empty(record.at.url.as_deref()) else {
                    diagnostics.push(invalid(
                        Location::or_file(record.declared_at.as_ref(), uri),
                        format!("source `{id}` declares `git()` with no url"),
                        "write `git(url = \"...\", tag = \"...\")`",
                    ));
                    continue;
                };
                SourceDecl::Git {
                    url,
                    tag: record.at.tag.clone(),
                    rev: record.at.rev.clone(),
                    branch: record.at.branch.clone(),
                    declared_at: record.declared_at.clone(),
                }
            }
            "registry" => {
                let Some(url) = non_empty(record.at.url.as_deref()) else {
                    diagnostics.push(invalid(
                        Location::or_file(record.declared_at.as_ref(), uri),
                        format!("source `{id}` declares `registry()` with no registry"),
                        "write `registry(\"crates.io\")`, optionally with \
                         `prefix = \"cf-gears-\"`",
                    ));
                    continue;
                };
                SourceDecl::Registry {
                    url,
                    prefix: non_empty(record.at.prefix.as_deref()),
                    declared_at: record.declared_at.clone(),
                }
            }
            other => {
                diagnostics.push(invalid(
                    Location::or_file(record.declared_at.as_ref(), uri),
                    format!("source `{id}` uses `{other}()`, which is not a source kind"),
                    "use `path(\"...\")`, `git(url = \"...\", tag = \"...\")` or \
                     `registry(\"crates.io\")`",
                ));
                continue;
            }
        };
        if sources.insert(id.clone(), at).is_some() {
            diagnostics.push(invalid(
                Location::or_file(record.declared_at.as_ref(), uri),
                format!("source `{id}` is declared twice"),
                "give each source a distinct id",
            ));
        }
    }
    sources
}

fn build_gears(
    uri: &str,
    decl: &ProductDecl,
    sources: &BTreeMap<SourceId, SourceDecl>,
    profiles: &Profiles,
    diagnostics: &mut Diagnostics,
) -> Vec<GearSelection> {
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for record in &decl.gears {
        let Some(gear) = gear_id(
            record.declared_at.as_ref(),
            uri,
            &record.gear,
            "use_gear",
            diagnostics,
        ) else {
            continue;
        };
        let source = match SourceId::new(record.source.clone()) {
            Ok(id) => id,
            Err(e) => {
                diagnostics.push(invalid(
                    Location::or_file(record.declared_at.as_ref(), uri),
                    format!(
                        "use_gear(\"{gear}\", source = \"{}\") is not a valid source id: {e}",
                        record.source
                    ),
                    "source ids are kebab-case",
                ));
                continue;
            }
        };
        if !sources.contains_key(&source) {
            diagnostics.push(invalid(
                Location::or_file(record.declared_at.as_ref(), uri),
                format!("use_gear(\"{gear}\") names source `{source}`, which is not declared"),
                format!(
                    "declared sources: {}",
                    joined(sources.keys().map(SourceId::as_str))
                ),
            ));
            continue;
        }
        if !seen.insert(gear.clone()) {
            diagnostics.push(invalid(
                Location::or_file(record.declared_at.as_ref(), uri),
                format!("gear `{gear}` is selected twice"),
                "list each gear once; per-profile differences belong on `bind`/`process`",
            ));
            continue;
        }
        let plugins = build_plugins(uri, &gear, record, profiles, diagnostics);
        // `version` and `package` only mean something for a registry. Silently
        // dropping them on a path source would let a description carry a
        // requirement nobody honours -- the reader would believe a version was
        // pinned when the directory on disk is whatever it is.
        if !matches!(sources.get(&source), Some(SourceDecl::Registry { .. })) {
            for (field, value) in [
                ("version", record.version.as_ref()),
                ("package", record.package.as_ref()),
            ] {
                if value.is_some() {
                    diagnostics.push(invalid(
                        Location::or_file(record.declared_at.as_ref(), uri),
                        format!("`{gear}` sets `{field}`, but source `{source}` is not a registry"),
                        format!(
                            "remove `{field}`, or declare the source as \
                             `registry(\"crates.io\")`"
                        ),
                    ));
                }
            }
        }
        selected.push(GearSelection {
            gear,
            source,
            version: record.version.clone(),
            package: record.package.clone(),
            features: record.features.clone(),
            config: record.config.iter().cloned().collect(),
            plugins,
            declared_at: record.declared_at.clone(),
        });
    }
    selected
}

/// Convert one gear's `plugins = [...]`.
///
/// The collision checked here is the one the product file can answer on its own:
/// the same implementation named twice for one host in one profile. Whether two
/// *different* plugins collide is a catalogue question -- it depends on which
/// extension point each implements -- and is checked where the catalogue is in scope.
fn build_plugins(
    uri: &str,
    host: &GearId,
    record: &crate::records::UseGearRecord,
    profiles: &Profiles,
    diagnostics: &mut Diagnostics,
) -> Vec<PluginSelection> {
    let mut out = Vec::new();
    let mut claimed = Claims::new();

    for (entry_index, entry) in record.plugins.iter().enumerate() {
        let Some(gear) = gear_id(
            record.declared_at.as_ref(),
            uri,
            &entry.gear,
            "plugin",
            diagnostics,
        ) else {
            continue;
        };
        let scoped = scoped_profiles(
            uri,
            &entry.profiles,
            &format!("plugin(\"{gear}\") on `{host}`"),
            profiles,
            diagnostics,
        );
        if !claim(&mut claimed, host.as_str(), gear.as_str(), &scoped) {
            diagnostics.push(collision(
                // The `plugin(...)` entry, now that it carries its own span:
                // the host's `use_gear(...)` contains it, but the duplicate is
                // the thing at fault.
                Location::or_file(entry.declared_at.as_ref(), uri),
                format!("plugin `{gear}` is selected twice for `{host}` in the same profile"),
                "list each implementation once; per-profile differences belong in \
                 `profiles = [...]`",
            ));
            continue;
        }
        out.push(PluginSelection {
            gear,
            config: entry.config.iter().cloned().collect(),
            profiles: scoped,
            // The position in `record.plugins`, which is the written list: the
            // `continue`s above are why this is not `out.len()`.
            entry_index,
            declared_at: entry.declared_at.clone(),
        });
    }
    out
}

fn build_bindings(
    uri: &str,
    decl: &ProductDecl,
    profiles: &Profiles,
    diagnostics: &mut Diagnostics,
) -> Vec<BindingIntent> {
    let mut bindings = Vec::new();
    let mut claimed = Claims::new();

    for record in &decl.bindings {
        let Some(consumer) = gear_id(
            record.declared_at.as_ref(),
            uri,
            &record.consumer,
            "bind",
            diagnostics,
        ) else {
            continue;
        };
        let contract = match ContractId::new(record.contract.clone()) {
            Ok(id) => id,
            Err(e) => {
                diagnostics.push(invalid(
                    Location::or_file(record.declared_at.as_ref(), uri),
                    format!(
                        "bind names contract `{}`, which is not valid: {e}",
                        record.contract
                    ),
                    "contract ids look like `gear-name/TraitName@v1`",
                ));
                continue;
            }
        };
        let scoped = scoped_profiles(
            uri,
            &record.profiles,
            &format!("bind(consumer = \"{consumer}\", contract = \"{contract}\")"),
            profiles,
            diagnostics,
        );

        if !claim(&mut claimed, consumer.as_str(), contract.as_str(), &scoped) {
            diagnostics.push(collision(
                Location::or_file(record.declared_at.as_ref(), uri),
                format!(
                    "two `bind` declarations cover consumer `{consumer}` and contract \
                     `{contract}` in the same profile"
                ),
                "a single edge cannot be bound two ways at once; narrow one entry's \
                 `profiles = [...]`",
            ));
            continue;
        }

        bindings.push(BindingIntent {
            consumer,
            contract,
            mode: binding_mode(&record.mode),
            transport: record.transport.as_deref().and_then(transport),
            endpoint: record.endpoint.clone(),
            profiles: scoped,
            declared_at: record.declared_at.clone(),
        });
    }
    bindings
}

fn build_cluster_scopes(
    uri: &str,
    decl: &ProductDecl,
    profiles: &Profiles,
    diagnostics: &mut Diagnostics,
) -> Vec<ClusterScopeIntent> {
    let mut out = Vec::new();
    let mut claimed = Claims::new();

    for (written, record) in decl.cluster_profiles.iter().enumerate() {
        let scoped = scoped_profiles(
            uri,
            &record.profiles,
            &format!("cluster_profile(name = \"{}\")", record.scope),
            profiles,
            diagnostics,
        );
        if !claim(&mut claimed, "cluster", &record.scope, &scoped) {
            diagnostics.push(collision(
                Location::or_file(record.declared_at.as_ref(), uri),
                format!(
                    "cluster scope `{}` is bound twice in the same profile",
                    record.scope
                ),
                "one scope resolves to one backend per profile; narrow one entry's \
                 `profiles = [...]`",
            ));
            continue;
        }
        out.push(cluster_scope(record, scoped, written));
    }
    out
}

fn build_application_pins(
    uri: &str,
    decl: &ProductDecl,
    profiles: &Profiles,
    diagnostics: &mut Diagnostics,
) -> Vec<ApplicationPin> {
    let mut pins = Vec::new();
    for record in &decl.applications {
        let name = match ApplicationId::new(record.name.clone()) {
            Ok(id) => id,
            Err(e) => {
                diagnostics.push(invalid(
                    Location::or_file(record.declared_at.as_ref(), uri),
                    format!("application name `{}` is not valid: {e}", record.name),
                    "application names are kebab-case",
                ));
                continue;
            }
        };
        let Some(anchor) = gear_id(
            record.declared_at.as_ref(),
            uri,
            &record.anchor,
            "application",
            diagnostics,
        ) else {
            continue;
        };
        let scoped = scoped_profiles(
            uri,
            &record.profiles,
            &format!("application(\"{name}\")"),
            profiles,
            diagnostics,
        );
        pins.push(ApplicationPin {
            declared_at: record.declared_at.clone(),
            name,
            anchor,
            include: record
                .include
                .iter()
                .filter_map(|id| {
                    gear_id(
                        record.declared_at.as_ref(),
                        uri,
                        id,
                        "application.include",
                        diagnostics,
                    )
                })
                .collect(),
            // Not checked here: whether the anchor declares this role is a
            // question for the catalogue, which this layer does not have.
            role: record.role.clone(),
            replicas: record.replicas,
            profiles: scoped,
        });
    }
    pins
}

fn build_preferences(
    uri: &str,
    decl: &ProductDecl,
    diagnostics: &mut Diagnostics,
) -> Vec<Preference> {
    let mut preferences = Vec::new();
    for record in &decl.preferences {
        let preference = match record.kind.as_str() {
            "existing-infrastructure" => Preference::ExistingInfrastructure,
            "fewer-applications" => Preference::FewerApplications,
            "isolate" => {
                let raw = record.gear.clone().unwrap_or_default();
                match gear_id(None, uri, &raw, "prefer.isolate", diagnostics) {
                    Some(gear) => Preference::Isolate { gear },
                    None => continue,
                }
            }
            other => {
                diagnostics.push(invalid(
                    Location::file(uri.to_owned()),
                    format!("unknown preference `{other}`"),
                    "use `prefer.existing_infrastructure()`, `prefer.fewer_applications()` or \
                     `prefer.isolate(gear = \"...\")`",
                ));
                continue;
            }
        };
        // A repeated preference is a no-op, not an error: it orders candidates,
        // and ordering twice by the same key changes nothing.
        if !preferences.contains(&preference) {
            preferences.push(preference);
        }
    }
    preferences
}

/// Resolve a `profiles = [...]` list against the declared profiles.
///
/// An empty list means "every profile", which is why the caller distinguishes it
/// from an explicit list rather than treating it as none.
fn scoped_profiles(
    uri: &str,
    names: &[String],
    what: &str,
    profiles: &Profiles,
    diagnostics: &mut Diagnostics,
) -> BTreeSet<ProfileId> {
    let mut out = BTreeSet::new();
    for name in names {
        match ProfileId::new(name.clone()) {
            Ok(id) if profiles.contains_key(&id) => {
                out.insert(id);
            }
            Ok(id) => diagnostics.push(invalid(
                Location::file(uri.to_owned()),
                format!("{what} is scoped to profile `{id}`, which is not declared"),
                format!(
                    "declared profiles: {}",
                    joined(profiles.keys().map(ProfileId::as_str))
                ),
            )),
            Err(e) => diagnostics.push(invalid(
                Location::file(uri.to_owned()),
                format!("{what} is scoped to `{name}`, which is not a valid profile id: {e}"),
                "profile ids are kebab-case",
            )),
        }
    }
    out
}

/// Claim `(subject, key)` for each profile in `scoped`, or report a collision.
///
/// An unscoped declaration applies to every profile, so it collides with any
/// other declaration for the same subject regardless of which came first --
/// including one scoped to a single profile.
fn claim(claimed: &mut Claims, subject: &str, key: &str, scoped: &BTreeSet<ProfileId>) -> bool {
    let entry = claimed
        .entry((subject.to_owned(), key.to_owned()))
        .or_default();

    if scoped.is_empty() {
        // Claims every profile, so anything already claimed collides.
        let ok = !entry.all && entry.profiles.is_empty();
        entry.all = true;
        return ok;
    }

    // An earlier unscoped entry already claimed every profile.
    let mut ok = !entry.all;
    for profile in scoped {
        if !entry.profiles.insert(profile.clone()) {
            ok = false;
        }
    }
    ok
}

fn cluster_scope(
    record: &ClusterProfileRecord,
    profiles: BTreeSet<ProfileId>,
    entry_index: usize,
) -> ClusterScopeIntent {
    ClusterScopeIntent {
        scope: record.scope.clone(),
        cache: provider_binding(&record.cache),
        leader_election: record.leader_election.as_ref().map(provider_binding),
        lock: record.lock.as_ref().map(provider_binding),
        profiles,
        declared_at: record.declared_at.clone(),
        entry_index,
    }
}

fn provider_binding(record: &ProviderBindingRecord) -> ProviderBinding {
    ProviderBinding {
        provider: record.provider.clone(),
        options: record.options.iter().cloned().collect(),
        secret_ref: record.secret_ref.clone(),
        declared_at: record.declared_at.clone(),
    }
}

/// Map a `transport.*` variant by comparing against the IR's own spelling, so
/// the two cannot drift.
fn transport(variant: &str) -> Option<Transport> {
    Transport::ALL
        .iter()
        .copied()
        .find(|t| t.as_str() == variant)
}

/// Map a `binding_mode.*` variant. Already validated at the call site, so an
/// unknown value here would be an internal inconsistency, not user input.
fn binding_mode(variant: &str) -> BindingMode {
    match variant {
        "local" => BindingMode::Local,
        "remote" => BindingMode::Remote,
        _ => BindingMode::Auto,
    }
}

/// `declared_at` is the declaration that names the id, when there is one.
///
/// `default_profile = "..."` is written on the `product(...)` call, which records
/// no span, so that caller passes `None` and gets the file.
fn profile_id(
    declared_at: Option<&Location>,
    uri: &str,
    raw: &str,
    diagnostics: &mut Diagnostics,
) -> Option<ProfileId> {
    match ProfileId::new(raw.to_owned()) {
        Ok(id) => Some(id),
        Err(e) => {
            diagnostics.push(invalid(
                Location::or_file(declared_at, uri),
                format!("profile id `{raw}` is not valid: {e}"),
                "profile ids are kebab-case",
            ));
            None
        }
    }
}

/// `declared_at` is the declaration that names the gear, when there is one.
///
/// `application(anchor = ...)` and `prefer.isolate(...)` record no span yet, so
/// those callers pass `None`.
fn gear_id(
    declared_at: Option<&Location>,
    uri: &str,
    raw: &str,
    what: &str,
    diagnostics: &mut Diagnostics,
) -> Option<GearId> {
    match GearId::new(raw.to_owned()) {
        Ok(id) => Some(id),
        Err(e) => {
            diagnostics.push(invalid(
                Location::or_file(declared_at, uri),
                format!("{what} names gear `{raw}`, which is not a valid gear id: {e}"),
                "gear ids are kebab-case, exactly as `#[toolkit::gear(name = \"...\")]` \
                 spells them",
            ));
            None
        }
    }
}

fn deployment_profile(
    uri: &str,
    record: &ProfileRecord,
    id: &ProfileId,
    diagnostics: &mut Diagnostics,
) -> Option<DeploymentProfileDecl> {
    match record.kind.as_str() {
        "embedded" => Some(DeploymentProfileDecl::Embedded {
            id: id.clone(),
            database: record.database.as_ref().map(|db| db.settings.clone()),
            sqlite_databases: record.sqlite_databases.clone(),
            declared_at: record.declared_at.clone(),
        }),
        "self-hosted" => {
            let raw = record.host.clone().unwrap_or_default();
            let host = match ApplicationId::new(raw.clone()) {
                Ok(host) => host,
                Err(e) => {
                    diagnostics.push(invalid(
                        Location::or_file(record.declared_at.as_ref(), uri),
                        format!("profile `{id}` names host `{raw}`, which is not valid: {e}"),
                        "the host is an application name, kebab-case",
                    ));
                    return None;
                }
            };
            let cargo_profile = match record.cargo_profile.as_deref() {
                None => None,
                Some(raw) if is_cargo_profile_name(raw) => Some(raw.to_owned()),
                Some(raw) => {
                    diagnostics.push(invalid(
                        Location::or_file(record.declared_at.as_ref(), uri),
                        format!("profile `{id}` names cargo profile `{raw}`"),
                        "use a single path segment (`dev`, `release`, or a custom Cargo profile \
                         name); `dev` writes under `target/debug`",
                    ));
                    return None;
                }
            };
            Some(DeploymentProfileDecl::SelfHosted {
                id: id.clone(),
                database: record.database.as_ref().map(|db| db.settings.clone()),
                sqlite_databases: record.sqlite_databases.clone(),
                host,
                discovery: discovery(
                    uri,
                    id,
                    record.discovery.as_deref(),
                    record.declared_at.as_ref(),
                    diagnostics,
                )?,
                target_dir: record.target_dir.clone(),
                cargo_profile,
                declared_at: record.declared_at.clone(),
            })
        }
        "kubernetes" => Some(DeploymentProfileDecl::Kubernetes {
            id: id.clone(),
            database: record.database.as_ref().map(|db| db.settings.clone()),
            sqlite_databases: record.sqlite_databases.clone(),
            discovery: discovery(
                uri,
                id,
                record.discovery.as_deref(),
                record.declared_at.as_ref(),
                diagnostics,
            )?,
            namespace: record.namespace.clone(),
            image_registry: record.image_registry.clone(),
            declared_at: record.declared_at.clone(),
        }),
        other => {
            diagnostics.push(invalid(
                Location::or_file(record.declared_at.as_ref(), uri),
                format!("profile `{id}` has unknown kind `{other}`"),
                "use `embedded(...)`, `self_hosted(...)` or `kubernetes(...)`",
            ));
            None
        }
    }
}

fn is_cargo_profile_name(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn discovery(
    uri: &str,
    id: &ProfileId,
    raw: Option<&str>,
    declared_at: Option<&Location>,
    diagnostics: &mut Diagnostics,
) -> Option<Discovery> {
    match raw.unwrap_or("static") {
        "static" => Some(Discovery::Static),
        "directory" => Some(Discovery::Directory),
        other => {
            diagnostics.push(invalid(
                Location::or_file(declared_at, uri),
                format!("profile `{id}` asks for discovery `{other}`"),
                "use `\"static\"` or `\"directory\"`",
            ));
            None
        }
    }
}

fn joined<'a>(items: impl Iterator<Item = &'a str>) -> String {
    let mut v: Vec<&str> = items.collect();
    v.sort_unstable();
    v.join(", ")
}
