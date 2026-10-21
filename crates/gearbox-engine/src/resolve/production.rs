//! The production rules a profile is held to (gears-rust ADR-0011 sections 1 and 3,
//! Gearbox ADR-0026 section 6; test case P4).
//!
//! Every profile is a production target unless the description marks it
//! `development = True`. Production: each broken rule is an error, reported
//! before anything is generated. Development: the same findings as warnings, so
//! a demo still says what it would take to ship.
//!
//! The rules are the ones a description decides and this tool can see. What the
//! generator writes for a production profile -- the internal listener, the
//! exposed-only public listener, an unbound embedded hub -- is correct by
//! construction; these catch a description that overrides it, or that leaves
//! out what only it can supply (the platform credential).

use gearbox_ir::{
    DeploymentProfileDecl, Diagnostic, DiagnosticCode, Diagnostics, GearId, Location,
    ProductIntent, ProfileId, Severity,
};
use serde_json::Value;

const API_GATEWAY: &str = "api-gateway";
const GRPC_HUB: &str = "grpc-hub";
const STATIC_AUTHN: &str = "static-authn-plugin";
const DIRECTORY: &str = "service-discovery";

/// Check `profile` of `intent` against the production rules.
pub fn check(
    intent: &ProductIntent,
    profile: &ProfileId,
    declaration: &DeploymentProfileDecl,
    multi_process: bool,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let config = |gear: &str| -> Option<&std::collections::BTreeMap<String, Value>> {
        let id = GearId::new(gear).ok()?;
        intent
            .selected_gears
            .iter()
            .find(|selection| selection.gear == id)
            .map(|selection| &selection.config)
    };
    let mut broken: Vec<(String, &'static str)> = Vec::new();

    if let Some(gateway) = config(API_GATEWAY) {
        // (e) The public listener serves only exposed operations.
        if gateway
            .get("public")
            .and_then(|p| p.get("exposed_only"))
            .and_then(Value::as_bool)
            == Some(false)
        {
            broken.push((
                "`api-gateway` sets `public.exposed_only = False`: the public listener would serve \
                 internal operations"
                    .to_owned(),
                "ADR-0011 section 1",
            ));
        }
        if gateway.get("auth_disabled").and_then(Value::as_bool) == Some(true) {
            broken.push((
                "`api-gateway` sets `auth_disabled = True`: requests reach operations without \
                 tenant authentication"
                    .to_owned(),
                "ADR-0011 section 3",
            ));
        }
        // (a, b) Platform auth on every listener a worker or another unit calls.
        let provider = gateway
            .get("internal_auth")
            .and_then(|a| a.get("provider"))
            .and_then(Value::as_str);
        let wanted = match declaration {
            DeploymentProfileDecl::Kubernetes { .. } => Some("kube"),
            DeploymentProfileDecl::SelfHosted { .. } => Some("shared_secret"),
            DeploymentProfileDecl::Embedded { .. } => None,
        };
        if let Some(wanted) = wanted
            && multi_process
            && provider != Some(wanted)
        {
            broken.push((
                format!(
                    "a {} profile needs platform auth `{wanted}` (`platform_auth = \"{wanted}\"` \
                     on the profile), and its `api-gateway` {}",
                    declaration.kind(),
                    provider.map_or_else(|| "has none".to_owned(), |p| format!("has `{p}`"))
                ),
                "ADR-0011 section 3",
            ));
        }
    }

    if let Some(hub) = config(GRPC_HUB) {
        let listen = hub.get("listen_addr").and_then(Value::as_str);
        // (d) The all-in-one binary binds no hub.
        if matches!(declaration, DeploymentProfileDecl::Embedded { .. })
            && listen.is_some_and(|addr| addr != "none" && !addr.starts_with("uds://"))
        {
            broken.push((
                format!(
                    "an embedded profile's gRPC hub listens on TCP `{}`: one process has no \
                     caller for it",
                    listen.unwrap_or_default()
                ),
                "ADR-0011 section 1",
            ));
        }
        // (b) A single-node self-hosted hub is a socket in a private directory,
        // not TCP: no other machine has a reason to reach it.
        if matches!(declaration, DeploymentProfileDecl::SelfHosted { .. })
            && listen.is_some_and(|addr| addr != "none" && !addr.starts_with("uds://"))
        {
            broken.push((
                format!(
                    "a self-hosted profile's gRPC hub listens on TCP `{}`; it is a socket in a \
                     private directory (`uds://...`)",
                    listen.unwrap_or_default()
                ),
                "ADR-0011 section 1",
            ));
        }
        // (f) A UDS hub lives in the service's own private directory.
        if let Some(path) = listen.and_then(|addr| addr.strip_prefix("uds://"))
            && (path.starts_with("/tmp/") || path.starts_with("/var/tmp/"))
        {
            broken.push((
                format!(
                    "the gRPC hub's socket `{path}` is in a shared temporary directory, not a \
                     0700 directory the service owns"
                ),
                "ADR-0011 section 3",
            ));
        }
        // (e) A hub reachable over TCP enforces platform auth.
        if hub
            .get("internal_auth_enforcement")
            .and_then(Value::as_str)
            .is_some_and(|e| e != "required")
        {
            broken.push((
                "the gRPC hub's `internal_auth_enforcement` is not `required`".to_owned(),
                "ADR-0011 section 3",
            ));
        }
    }

    // The directory admits a registrar by ServiceAccount name and namespace;
    // an empty allowlist turns the namespace check off.
    if matches!(declaration, DeploymentProfileDecl::Kubernetes { .. })
        && config(DIRECTORY)
            .and_then(|c| c.get("platform_namespaces"))
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        broken.push((
            "`service-discovery` sets `platform_namespaces = []`: a ServiceAccount named like a \
             gear in any namespace could register it"
                .to_owned(),
            "ADR-0011 section 3",
        ));
    }

    // Tenant AuthN that accepts anyone is a development switch. The static
    // plugin's mode defaults to `accept_all` when the description omits it.
    let accept_all = intent.selected_gears.iter().any(|selection| {
        selection.plugins.iter().any(|plugin| {
            plugin.gear.as_str() == STATIC_AUTHN
                && (plugin.profiles.is_empty() || plugin.profiles.contains(profile))
                && plugin
                    .config
                    .get("mode")
                    .and_then(Value::as_str)
                    .is_none_or(|mode| mode == "accept_all")
        })
    });
    if accept_all {
        broken.push((
            "`static-authn-plugin` in `accept_all` mode authenticates every request".to_owned(),
            "ADR-0011 section 3",
        ));
    }

    let development = declaration.development();
    for (what, rule) in broken {
        let diagnostic = Diagnostic::error(
            DiagnosticCode::TopologyNotProductionReady,
            format!("profile `{profile}` breaks a production rule: {what} ({rule})"),
            if development {
                "allowed because the profile is `development = True`; a production profile \
                 refuses it"
            } else {
                "fix the configuration, or mark the profile `development = True` if it is not a \
                 production target"
            },
        )
        .at(Location::or_file(declaration.declared_at(), uri));
        diagnostics.push(if development {
            diagnostic.with_severity(Severity::Warning)
        } else {
            diagnostic
        });
    }
}
