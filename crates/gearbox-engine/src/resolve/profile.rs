//! Step 1: narrowing the intent to one deployment profile.
//!
//! A product description declares every profile it supports and scopes the
//! declarations that differ with `profiles = [...]`. GDL has no `if`, so this
//! list *is* the conditional -- which means resolving for a profile begins by
//! evaluating it.
//!
//! An empty list means "all profiles". That is not a default filled in for
//! convenience: most declarations are the same everywhere, and requiring each to
//! enumerate the profiles would make the common case noisy and the exceptional
//! one invisible.

use std::borrow::Cow;
use std::collections::BTreeSet;

use gearbox_ir::{
    ApplicationPin, BindingIntent, ClusterScopeIntent, Diagnostic, DiagnosticCode, Diagnostics,
    Location, PlatformAuth, ProductIntent, ProfileId,
};
use serde_json::{Value, json};

const API_GATEWAY: &str = "api-gateway";
const DIRECTORY: &str = "service-discovery";

/// The intent as `profile` sees it: each gear's `profile_config` for it merged
/// over `config`, and the profile's `platform_auth` written into `api-gateway`'s
/// `internal_auth` -- which the generator spreads to the hubs and workers.
///
/// Everything after this reads one configuration per gear, so the production
/// rules, the config check and the lock all see what this profile will run.
/// Borrowed when the profile changes nothing.
pub fn effective<'a>(
    intent: &'a ProductIntent,
    profile: &ProfileId,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Cow<'a, ProductIntent> {
    let declaration = intent.profiles.get(profile);
    let auth = declaration.and_then(gearbox_ir::DeploymentProfileDecl::platform_auth);
    let overridden = intent
        .selected_gears
        .iter()
        .any(|s| s.profile_config.contains_key(profile));
    if auth.is_none() && !overridden {
        return Cow::Borrowed(intent);
    }
    let mut out = intent.clone();
    for selection in &mut out.selected_gears {
        if let Some(over) = selection.profile_config.get(profile) {
            for (key, value) in over {
                match selection.config.get_mut(key) {
                    Some(base) => merge(base, value),
                    None => {
                        selection.config.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }
    if let (Some(auth), Some(declaration)) = (auth, declaration) {
        apply_platform_auth(&mut out, auth, declaration, uri, diagnostics);
    }
    Cow::Owned(out)
}

/// `over` merged into `base`: maps key by key, any other value replaced.
fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(key) {
                    Some(slot) => merge(slot, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, over) => *base = over.clone(),
    }
}

/// What `platform_auth` means in the runtime's `internal_auth`, filled in under
/// any field the description set itself (a provider it names must agree).
fn apply_platform_auth(
    intent: &mut ProductIntent,
    auth: PlatformAuth,
    declaration: &gearbox_ir::DeploymentProfileDecl,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let at = Location::or_file(declaration.declared_at(), uri);
    let profile = declaration.id();
    let label = format!("{}-platform", intent.id);
    let Some(gateway) = intent
        .selected_gears
        .iter_mut()
        .find(|s| s.gear.as_str() == API_GATEWAY)
    else {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::TopologyPlatformAuthUnapplied,
                format!(
                    "profile `{profile}` sets `platform_auth = \"{}\"`, but the product selects no \
                     `api-gateway` to carry it to the hubs and workers",
                    auth.as_str()
                ),
            )
            .with_help("select `api-gateway`, or remove `platform_auth`")
            .at(at),
        );
        return;
    };
    let mut wanted = match auth {
        // The projected token the generated chart mounts for every unit; its
        // path follows the audience, filled in below.
        PlatformAuth::Kube => json!({
            "provider": "kube",
            "audiences": ["toolkit-internal"],
        }),
        // One secret, one identity: every holder authenticates under the
        // product's label. The value comes from the environment (`APP__...`
        // overrides); an empty one fails startup rather than authenticating.
        PlatformAuth::SharedSecret => json!({
            "provider": "shared_secret",
            "secret": "",
            "peer_name": label,
        }),
    };
    if let Some(written) = gateway.config.get("internal_auth") {
        let provider = written.get("provider").and_then(Value::as_str);
        if provider.is_some_and(|p| p != auth.as_str()) {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::TopologyPlatformAuthUnapplied,
                    format!(
                        "profile `{profile}` sets `platform_auth = \"{}\"`, but `api-gateway`'s \
                         `internal_auth.provider` is `{}` for it",
                        auth.as_str(),
                        provider.unwrap_or_default()
                    ),
                )
                .with_help(
                    "keep one: remove `internal_auth.provider` from the gear's configuration \
                     (fields such as `audiences` may stay), or change the profile's \
                     `platform_auth`",
                )
                .at(Location::or_file(gateway.declared_at.as_ref(), uri)),
            );
            return;
        }
        merge(&mut wanted, written);
    }
    if auth == PlatformAuth::Kube
        && let Value::Object(fields) = &mut wanted
        && !fields.contains_key("token_path")
    {
        let audience = fields
            .get("audiences")
            .and_then(|a| a.get(0))
            .and_then(Value::as_str)
            .unwrap_or("toolkit-internal")
            .to_owned();
        fields.insert(
            "token_path".to_owned(),
            Value::String(format!("/var/run/secrets/tokens/{audience}")),
        );
    }
    gateway
        .config
        .insert("internal_auth".to_owned(), wanted.clone());

    // A shared-secret peer may register only the gear named like its label
    // unless the label is a trusted registrar, and every worker holds the same
    // label.
    if auth == PlatformAuth::SharedSecret
        && let Some(directory) = intent
            .selected_gears
            .iter_mut()
            .find(|s| s.gear.as_str() == DIRECTORY)
    {
        let peer = wanted
            .get("peer_name")
            .cloned()
            .unwrap_or(Value::String(label));
        let registrars = directory
            .config
            .entry("trusted_registrars".to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(list) = registrars
            && !list.contains(&peer)
        {
            list.push(peer);
        }
    }
}

/// The intent with everything that does not apply to this profile removed.
///
/// Borrowed rather than cloned: the resolver reads these and never mutates them,
/// and the intent outlives the resolution.
#[derive(Debug)]
pub struct ProfileScoped<'a> {
    pub bindings: Vec<&'a BindingIntent>,
    pub cluster_scopes: Vec<&'a ClusterScopeIntent>,
    pub application_pins: Vec<&'a ApplicationPin>,
}

/// Whether a `profiles = [...]` list admits this profile.
fn applies(profiles: &BTreeSet<ProfileId>, profile: &ProfileId) -> bool {
    profiles.is_empty() || profiles.contains(profile)
}

/// Filter the intent to `profile`, reporting keys that collide once narrowed.
///
/// The collision check is the reason this is a step rather than three `filter`
/// calls. Two declarations that name the same key are legal in the description as
/// long as they apply to different profiles -- that is how a product says "REST
/// here, gRPC there". They become a contradiction only after narrowing, so this
/// is the one place the check can be made.
///
/// `uri` is the product description's URI, passed in rather than derived here:
/// `intent.gdl_path` is relative to its root, and a relative path in a `file://`
/// URI reads its first segment as the host. `resolve_at` is the one place that
/// knows the real path.
pub fn scope<'a>(
    intent: &'a ProductIntent,
    profile: &ProfileId,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> ProfileScoped<'a> {
    let bindings: Vec<&BindingIntent> = intent
        .bindings
        .iter()
        .filter(|b| applies(&b.profiles, profile))
        .collect();
    report_duplicates(
        bindings
            .iter()
            .map(|b| format!("{}/{}", b.consumer, b.contract)),
        "bind",
        profile,
        uri,
        diagnostics,
    );

    let cluster_scopes: Vec<&ClusterScopeIntent> = intent
        .cluster_scopes
        .iter()
        .filter(|s| applies(&s.profiles, profile))
        .collect();
    report_duplicates(
        cluster_scopes.iter().map(|s| s.scope.clone()),
        "cluster_profile",
        profile,
        uri,
        diagnostics,
    );

    let application_pins: Vec<&ApplicationPin> = intent
        .application_pins
        .iter()
        .filter(|p| applies(&p.profiles, profile))
        .collect();
    report_duplicates(
        application_pins.iter().map(|p| p.name.to_string()),
        "application",
        profile,
        uri,
        diagnostics,
    );
    // Keyed on the anchor *and* the role, because both halves are the pin's
    // identity and the anchor is the load-bearing half: it decides the
    // application's contents. Two pins naming one anchor and no role describe
    // one process twice, and the name check above cannot see it -- they differ
    // by name, which is exactly what makes them look distinct. Differing by
    // role is the one way two pins on one anchor are not a contradiction.
    report_duplicates(
        application_pins
            .iter()
            .map(|p| format!("{}/{}", p.anchor, p.role.as_deref().unwrap_or(""))),
        "application anchor",
        profile,
        uri,
        diagnostics,
    );

    ProfileScoped {
        bindings,
        cluster_scopes,
        application_pins,
    }
}

/// Report each key that appears more than once after narrowing.
///
/// Reports the key, not the pair, and once per key however many times it repeats:
/// three declarations of the same binding are one mistake, not two.
fn report_duplicates(
    keys: impl Iterator<Item = String>,
    what: &str,
    profile: &ProfileId,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let mut seen = BTreeSet::new();
    let mut duplicated = BTreeSet::new();
    for key in keys {
        if !seen.insert(key.clone()) {
            duplicated.insert(key);
        }
    }
    for key in duplicated {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::GdlDuplicateProfileScoped,
                format!("two `{what}` declarations for `{key}` both apply to profile `{profile}`"),
                "scope them to different profiles with `profiles = [...]`, or delete one; \
                 a declaration with no `profiles` applies to every profile, so it collides with \
                 any scoped one for the same key",
            )
            .at(Location::file(uri.to_owned())),
        );
    }
}
