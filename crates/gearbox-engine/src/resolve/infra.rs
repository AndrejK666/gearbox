//! A product's infra services (GDL 1.6), against the passports the catalogue
//! found: each `use_service(...)` names an infra passport, sets only keys of
//! its `config`, and no two provide one capability. What the gears need is
//! bound to the services that meet it, for tools to show; an unmet need is
//! not an error, since it may be met outside the product.

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::infra::{
    InfraNeed, LockedImage, NEED_BINDINGS, ProvidedCapability, ResolvedInfra, ResolvedService,
    ServiceKind, ServiceSelection, capability_variable,
};
use gearbox_ir::{
    Catalogue, Diagnostic, DiagnosticCode, Diagnostics, GearId, Location, ProductIntent, ProfileId,
    RuntimeCap,
};

/// The infra section of the lock, or `None` for a product that declares no
/// infra service: its lock stays what it was.
pub(super) fn resolve(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    profile: &ProfileId,
    gears: &BTreeSet<GearId>,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Option<ResolvedInfra> {
    if intent.infra.is_empty() {
        return None;
    }
    let mut infra = ResolvedInfra::default();
    for selection in &intent.infra {
        if let Some(service) = service(catalogue, selection, profile, uri, diagnostics) {
            infra.services.push(service);
        }
    }
    infra.capabilities = capabilities(&infra.services, &intent.infra, uri, diagnostics);
    infra.needs = needs(catalogue, intent, profile, gears, &infra.capabilities);
    Some(infra)
}

/// Every capability of the declared services, each from one service; a second
/// provider is GBX0128.
fn capabilities(
    services: &[ResolvedService],
    selections: &[ServiceSelection],
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> BTreeMap<String, ProvidedCapability> {
    let mut out: BTreeMap<String, ProvidedCapability> = BTreeMap::new();
    for service in services {
        for (capability, template) in &service.passport.provides {
            if let Some(first) = out.get(capability) {
                let at = selections
                    .iter()
                    .find(|s| s.name == service.name)
                    .and_then(|s| s.declared_at.as_ref());
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::GdlInfraCapabilityConflict,
                        format!(
                            "infra services `{}` and `{}` both provide `{capability}`, which the \
                             gears read as `${{{}}}`",
                            first.service,
                            service.name,
                            capability_variable(capability)
                        ),
                        "keep one of them in `infra`; a capability reaches the gears as one \
                         variable",
                    )
                    .at(Location::or_file(at, uri)),
                );
                continue;
            }
            out.insert(
                capability.clone(),
                ProvidedCapability {
                    service: service.name.clone(),
                    env: capability_variable(capability),
                    template: template.clone(),
                },
            );
        }
    }
    out
}

/// One `use_service(...)` against its passport.
fn service(
    catalogue: &Catalogue,
    selection: &ServiceSelection,
    profile: &ProfileId,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Option<ResolvedService> {
    let at = Location::or_file(selection.declared_at.as_ref(), uri);
    let Some(descriptor) = catalogue.service(&selection.source, &selection.name) else {
        let known: Vec<&str> = catalogue
            .services
            .iter()
            .filter(|s| s.source == selection.source && s.passport.kind == ServiceKind::Infra)
            .map(|s| s.passport.name.as_str())
            .collect();
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::GdlInfraServiceUnknown,
                format!(
                    "use_service(\"{}\"): source `{}` has no `service.gdl` named `{}`",
                    selection.name, selection.source, selection.name
                ),
                if known.is_empty() {
                    format!(
                        "source `{}` describes no infra service; add a `service.gdl` with \
                         kind = \"infra\" to it, or name the source that has one",
                        selection.source
                    )
                } else {
                    format!("its infra services: {}", known.join(", "))
                },
            )
            .at(at),
        );
        return None;
    };
    if descriptor.passport.kind != ServiceKind::Infra {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::GdlInfraServiceUnknown,
                format!(
                    "use_service(\"{}\"): `{}` in source `{}` is an own service, not \
                     kind = \"infra\"",
                    selection.name, descriptor.gdl_path, selection.source
                ),
                "an own service is built from its Dockerfile; a product runs beside its gears \
                 only a service whose passport says kind = \"infra\"",
            )
            .at(at),
        );
        return None;
    }
    let passport = &descriptor.passport;

    let mut config = passport.config.clone();
    let mut overridden = BTreeSet::new();
    let profile_values = selection.profile_config.get(profile);
    for (key, value) in selection
        .config
        .iter()
        .chain(profile_values.into_iter().flatten())
    {
        if let Some(problem) = config_problem(passport, key, value) {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlInfraConfigInvalid,
                    format!(
                        "use_service(\"{}\") config `{key}`: {problem}",
                        selection.name
                    ),
                    format!(
                        "set only keys of the passport's `config` ({}), each to a literal; its \
                         secrets are the deployment's to make",
                        if passport.config.is_empty() {
                            "it declares none".to_owned()
                        } else {
                            passport
                                .config
                                .keys()
                                .map(String::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        }
                    ),
                )
                .at(at.clone()),
            );
            continue;
        }
        config.insert(key.clone(), value.clone());
        overridden.insert(key.clone());
    }

    Some(ResolvedService {
        name: selection.name.clone(),
        source: selection.source.clone(),
        gdl_path: descriptor.gdl_path.clone(),
        passport_hash: descriptor.passport_hash.clone(),
        passport: passport.clone(),
        image: selection
            .image
            .as_deref()
            .or(passport.image.as_deref())
            .map(LockedImage::new),
        chart: selection.chart.clone().or_else(|| passport.chart.clone()),
        chart_version: if selection.chart.is_some() {
            selection.chart_version.clone()
        } else {
            selection
                .chart_version
                .clone()
                .or_else(|| passport.chart_version.clone())
        },
        config,
        overridden,
    })
}

/// Why `key = value` may not be set on this passport's config, if it may not.
fn config_problem(
    passport: &gearbox_ir::infra::ServicePassport,
    key: &str,
    value: &str,
) -> Option<String> {
    if passport.secrets.iter().any(|s| s == key) {
        return Some(
            "is one of the passport's `secrets`; its value is the deployment's to make and keep, \
             never the description's"
                .to_owned(),
        );
    }
    if !passport.config.contains_key(key) {
        return Some("is not a key of the passport's `config`".to_owned());
    }
    if value.contains("${") {
        return Some(
            "is a `${...}` reference; an infra service's config is literal, and what it reads \
             from the environment is a `secret`"
                .to_owned(),
        );
    }
    if value.contains(['\n', '\r']) {
        return Some("spans lines".to_owned());
    }
    None
}

/// What the product's gears need running beside them, each with the declared
/// service that meets it.
fn needs(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    profile: &ProfileId,
    gears: &BTreeSet<GearId>,
    capabilities: &BTreeMap<String, ProvidedCapability>,
) -> Vec<InfraNeed> {
    let sqlite = intent
        .profiles
        .get(profile)
        .map(|d| {
            d.sqlite_databases()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    for (need, capability, suffix) in NEED_BINDINGS {
        let needing: BTreeSet<GearId> = gears
            .iter()
            .filter(|id| {
                if suffix.is_empty() {
                    !sqlite.contains(*id)
                        && catalogue
                            .gear(id)
                            .is_some_and(|g| g.runtime_caps.contains(&RuntimeCap::Db))
                } else {
                    id.as_str().ends_with(suffix)
                }
            })
            .cloned()
            .collect();
        if needing.is_empty() {
            continue;
        }
        out.push(InfraNeed {
            need: (*need).to_owned(),
            capability: (*capability).to_owned(),
            gears: needing,
            met_by: capabilities.get(*capability).map(|c| c.service.clone()),
        });
    }
    out
}
