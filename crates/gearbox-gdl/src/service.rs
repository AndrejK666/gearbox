//! The `service.gdl` vocabulary (GDL 1.6): the passport of a service that is
//! not a gear.
//!
//! One file beside the service, with one `service(...)` call. An **own**
//! service (the default kind) is built from its own Dockerfile; an **infra**
//! service (`kind = "infra"`) is a third-party one a product runs beside its
//! gears -- Keycloak, PostgreSQL -- pulled, never built, and chosen by a
//! product with `use_service(...)`.
//!
//! An infra service's `provides` is a dict of capability to the template of
//! its value, resolved at deploy: `{host}`, `{namespace}`, `{public_url}`,
//! `{port.<name>}`, `{config.<KEY>}` and `{secret.<KEY>}`, each name it uses
//! declared by the passport. A gear's configuration reads a capability as an
//! environment variable named after it: `oidc.issuer` is `${OIDC_ISSUER}`.
//!
//! The vocabulary is the one Studio's catalogue reads (its ADR-0061 and
//! ADR-0064), so a passport means the same to both.

// Same reasoning as `globals.rs`: these fire on `#[starlark_module]` expansion.
#![allow(
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::too_many_arguments,
    reason = "artifacts of #[starlark_module] expansion, not of hand-written code"
)]

use std::collections::BTreeMap;

use either::Either;
use gearbox_ir::infra::{ServiceKind, ServicePassport, is_capability};
use starlark::environment::{Globals, GlobalsBuilder};
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;
use starlark::values::dict::UnpackDictEntries;
use starlark::values::list::UnpackList;
use starlark::values::none::{NoneOr, NoneType};

use crate::sink::GdlSink;

fn sink<'a>(eval: &'a Evaluator<'_, '_, '_>) -> anyhow::Result<&'a GdlSink> {
    eval.extra
        .and_then(|e| e.downcast_ref::<GdlSink>())
        .ok_or_else(|| anyhow::anyhow!("internal error: no GdlSink installed on the evaluator"))
}

/// A kebab-case id: `keycloak`, `graph-postgres`.
fn kebab(field: &str, value: &str) -> anyhow::Result<String> {
    gearbox_ir::GearId::new(value.to_owned())
        .map(|_| value.to_owned())
        .map_err(|e| anyhow::anyhow!("{field} = {value:?} is not a kebab-case id: {e}"))
}

/// A non-empty, one-line string.
fn one_line(field: &str, value: &str) -> anyhow::Result<String> {
    if value.trim().is_empty() || value.contains(['\n', '\r']) {
        return Err(anyhow::anyhow!(
            "{field} = {value:?} is empty or spans lines; write it on one line"
        ));
    }
    Ok(value.to_owned())
}

/// A word with no spaces: an image, a version, a URL.
fn word(field: &str, value: &str) -> anyhow::Result<String> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(anyhow::anyhow!(
            "{field} = {value:?} is empty or has spaces"
        ));
    }
    Ok(value.to_owned())
}

/// A path relative to the passport; it may climb with `..`, never start at `/`.
fn relative(field: &str, value: &str) -> anyhow::Result<String> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value.chars().any(char::is_whitespace)
    {
        return Err(anyhow::anyhow!(
            "{field} = {value:?} is not a path relative to this file"
        ));
    }
    Ok(value.to_owned())
}

/// Whether `value` is an environment variable name: `KC_HEALTH_ENABLED`.
#[must_use]
pub fn is_env_name(value: &str) -> bool {
    value.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
        && value
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn env_name(field: &str, value: &str) -> anyhow::Result<String> {
    if is_env_name(value) {
        Ok(value.to_owned())
    } else {
        Err(anyhow::anyhow!(
            "{field}: `{value}` is not a variable name (uppercase letters, digits and `_`)"
        ))
    }
}

/// A volume size: `10Gi`, `512Mi`.
fn volume_size(value: &str) -> anyhow::Result<String> {
    let digits = value.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let unit = &value[digits.len()..];
    if !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && ["Mi", "Gi", "Ti"].contains(&unit)
    {
        Ok(value.to_owned())
    } else {
        Err(anyhow::anyhow!(
            "storage = {value:?} is not a volume size (10Gi, 512Mi)"
        ))
    }
}

/// One name a `provides` template refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateRef {
    /// `{host}`: the service's name in the namespace.
    Host,
    /// `{namespace}`: the namespace it runs in.
    Namespace,
    /// `{public_url}`: where people reach it, as the environment says.
    PublicUrl,
    /// `{port.<name>}`.
    Port(String),
    /// `{config.<KEY>}`.
    Config(String),
    /// `{secret.<KEY>}`.
    Secret(String),
}

/// The names `template` refers to, in order; `Err` names the first one this
/// does not know, or a brace left open.
///
/// # Errors
/// A name outside the template vocabulary, or an unbalanced brace.
pub fn template_refs(template: &str) -> Result<Vec<TemplateRef>, String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        if rest[..open].contains('}') {
            return Err(format!("closes a `}}` it never opened in {template:?}"));
        }
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .ok_or_else(|| format!("leaves `{{` open in {template:?}"))?;
        let name = &after[..close];
        out.push(match name.split_once('.') {
            None if name == "host" => TemplateRef::Host,
            None if name == "namespace" => TemplateRef::Namespace,
            None if name == "public_url" => TemplateRef::PublicUrl,
            Some(("port", p)) if gearbox_ir::GearId::new(p.to_owned()).is_ok() => {
                TemplateRef::Port(p.to_owned())
            }
            Some(("config", k)) if is_env_name(k) => TemplateRef::Config(k.to_owned()),
            Some(("secret", k)) if is_env_name(k) => TemplateRef::Secret(k.to_owned()),
            _ => {
                return Err(format!(
                    "refers to `{{{name}}}`; a template knows {{host}}, {{namespace}}, \
                     {{public_url}}, {{port.<name>}}, {{config.<KEY>}} and {{secret.<KEY>}}"
                ));
            }
        });
        rest = &after[close + 1..];
    }
    if rest.contains('}') {
        return Err(format!("closes a `}}` it never opened in {template:?}"));
    }
    Ok(out)
}

/// `ports = {"http": 8080}`: each a kebab name to 1..65535 (a digit string is
/// read too).
fn ports_of(
    entries: UnpackDictEntries<String, Value<'_>>,
) -> anyhow::Result<BTreeMap<String, u16>> {
    let mut ports = BTreeMap::new();
    for (name, value) in entries.entries {
        kebab("ports key", &name)?;
        let port = if let Some(n) = value.unpack_i32() {
            u16::try_from(n).ok()
        } else {
            value.unpack_str().and_then(|s| s.parse::<u16>().ok())
        };
        let Some(port) = port.filter(|p| *p > 0) else {
            return Err(anyhow::anyhow!(
                "ports[{name:?}] is not a port (1 to 65535)"
            ));
        };
        ports.insert(name, port);
    }
    Ok(ports)
}

/// A dict of variable names to one-line strings.
fn variables(
    field: &str,
    entries: UnpackDictEntries<String, String>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for (key, value) in entries.entries {
        env_name(&format!("{field} key"), &key)?;
        if value.contains(['\n', '\r']) {
            return Err(anyhow::anyhow!("{field}[{key:?}] spans lines"));
        }
        map.insert(key, value);
    }
    Ok(map)
}

/// `provides`: a dict of capability to template, or a list of keys.
type Provides = Either<UnpackDictEntries<String, String>, UnpackList<String>>;

/// The arguments only an infra service takes.
struct InfraArgs<'a> {
    args: Option<UnpackList<String>>,
    config: Option<UnpackDictEntries<String, String>>,
    secrets: Option<UnpackList<String>>,
    stateful: Option<bool>,
    storage: Option<&'a str>,
    data_path: Option<&'a str>,
    chart: Option<&'a str>,
    chart_version: Option<&'a str>,
}

impl InfraArgs<'_> {
    /// The first one given, for an own service's refusal.
    fn first_given(&self) -> Option<&'static str> {
        [
            ("args", self.args.is_some()),
            ("config", self.config.is_some()),
            ("secrets", self.secrets.is_some()),
            ("stateful", self.stateful.is_some()),
            ("storage", self.storage.is_some()),
            ("data_path", self.data_path.is_some()),
            ("chart", self.chart.is_some()),
            ("chart_version", self.chart_version.is_some()),
        ]
        .into_iter()
        .find_map(|(name, given)| given.then_some(name))
    }
}

/// An infra service's own arguments, checked against each other and against
/// what its `provides` templates refer to.
fn infra(
    passport: &mut ServicePassport,
    args: InfraArgs<'_>,
    provides: Option<Provides>,
) -> anyhow::Result<()> {
    if passport.dockerfile.is_some() || passport.context.is_some() {
        return Err(anyhow::anyhow!(
            "an infra service is pulled, never built: it has no `dockerfile` or `context`"
        ));
    }
    if !passport.env.is_empty() {
        return Err(anyhow::anyhow!(
            "an infra service's variables are `config` (literal) and `secrets`, not `env`"
        ));
    }
    let chart = args.chart.map(|c| word("chart", c)).transpose()?;
    if let Some(chart) = &chart
        && !(chart.starts_with("oci://") || chart.starts_with("https://"))
    {
        return Err(anyhow::anyhow!(
            "chart = {chart:?} is not where a chart is published (oci://... or https://...)"
        ));
    }
    if passport.image.is_none() && chart.is_none() {
        return Err(anyhow::anyhow!(
            "an infra service names its `image`, or a ready `chart`, or both"
        ));
    }
    let chart_version = args
        .chart_version
        .map(|v| word("chart_version", v))
        .transpose()?;
    if chart_version.is_some() && chart.is_none() {
        return Err(anyhow::anyhow!(
            "`chart_version` goes with the `chart` it is a version of"
        ));
    }
    let config = args
        .config
        .map(|c| variables("config", c))
        .transpose()?
        .unwrap_or_default();
    let mut secrets = args
        .secrets
        .map(|s| {
            s.items
                .iter()
                .map(|n| env_name("secrets", n))
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    secrets.sort();
    secrets.dedup();
    if let Some(both) = secrets.iter().find(|s| config.contains_key(*s)) {
        return Err(anyhow::anyhow!(
            "{both} is both in `config` and in `secrets`; a secret is the deployment's to make"
        ));
    }
    let stateful = args.stateful.unwrap_or(false);
    let storage = args.storage.map(volume_size).transpose()?;
    let data_path = match args.data_path {
        Some(p) if p.starts_with('/') && !p.chars().any(char::is_whitespace) => Some(p.to_owned()),
        Some(p) => {
            return Err(anyhow::anyhow!(
                "data_path = {p:?} is not an absolute path in the container"
            ));
        }
        None => None,
    };
    if (storage.is_some() || data_path.is_some()) && !stateful {
        return Err(anyhow::anyhow!(
            "`storage` and `data_path` go with stateful = True"
        ));
    }
    if stateful && data_path.is_none() {
        return Err(anyhow::anyhow!(
            "a stateful service says where its volume is mounted: `data_path`"
        ));
    }

    let mut capabilities = BTreeMap::new();
    match provides {
        None => {}
        Some(Either::Left(entries)) => {
            for (key, template) in entries.entries {
                if !is_capability(&key) {
                    return Err(anyhow::anyhow!(
                        "provides: {key:?} is not a capability key (lowercase, dotted: oidc.issuer)"
                    ));
                }
                for r in template_refs(&template)
                    .map_err(|e| anyhow::anyhow!("provides: {key:?} {e}"))?
                {
                    let (known, named) = match &r {
                        TemplateRef::Port(p) => {
                            (passport.ports.contains_key(p), format!("{{port.{p}}}"))
                        }
                        TemplateRef::Config(c) => {
                            (config.contains_key(c), format!("{{config.{c}}}"))
                        }
                        TemplateRef::Secret(s) => (secrets.contains(s), format!("{{secret.{s}}}")),
                        TemplateRef::PublicUrl => {
                            (!passport.ports.is_empty(), "{public_url}".to_owned())
                        }
                        TemplateRef::Host | TemplateRef::Namespace => (true, String::new()),
                    };
                    if !known {
                        return Err(anyhow::anyhow!(
                            "provides: {key:?} refers to {named}, which the passport does not declare"
                        ));
                    }
                }
                capabilities.insert(key, template);
            }
        }
        // Keys without values: the shared place's, or the project's.
        Some(Either::Right(keys)) => {
            for key in keys.items {
                if !is_capability(&key) {
                    return Err(anyhow::anyhow!(
                        "provides: {key:?} is not a capability key (lowercase, dotted: oidc.issuer)"
                    ));
                }
                capabilities.insert(key, String::new());
            }
        }
    }

    passport.args = args
        .args
        .map(|a| {
            a.items
                .iter()
                .map(|x| one_line("args", x))
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    passport.config = config;
    passport.secrets = secrets;
    passport.provides = capabilities;
    passport.stateful = stateful;
    passport.storage = storage;
    passport.data_path = data_path;
    passport.chart = chart;
    passport.chart_version = chart_version;
    Ok(())
}

#[starlark_module]
fn gdl_service_vocabulary(builder: &mut GlobalsBuilder) {
    /// `service(...)` -- a service that is not a gear (GDL 1.6). Its passport.
    ///
    /// Only `name` is required. An own service (the default) is built from its
    /// Dockerfile; an infra service (`kind = "infra"`) is a third-party one a
    /// product runs beside its gears, chosen with `use_service(...)`.
    ///
    /// # Arguments
    /// * `name`: Its id, kebab-case.
    /// * `kind`: `"own"` (the default) or `"infra"`.
    /// * `version`: Its own version.
    /// * `description`: One line on what it is.
    /// * `image`: An own service's image, without a tag; an infra service's, as its
    ///   publisher names it, with a tag or a digest.
    /// * `dockerfile`: Own only: its Dockerfile, relative to this file.
    /// * `context`: Own only: its build context, relative to this file.
    /// * `ports`: Each port it listens on, by name: `{"http": 8080}`.
    /// * `health`: The route that answers when it is healthy.
    /// * `env`: Own only: the variables it reads, each to what fills it.
    /// * `requires`: The platform gears it calls.
    /// * `provides`: An own service's capability keys, as a list; an infra service's
    ///   dict of capability to the template of its value
    ///   (`{"oidc.issuer": "{public_url}/realms/product"}`).
    /// * `docs`: Its documents, relative to this file.
    /// * `args`: Infra only: the container's arguments.
    /// * `config`: Infra only: variables with literal values.
    /// * `secrets`: Infra only: variables whose values the deployment makes and keeps.
    /// * `stateful`: Infra only: it keeps data, on a volume of its own.
    /// * `storage`: Infra only: the volume's size (`10Gi`).
    /// * `data_path`: Infra only: where the volume is mounted.
    /// * `chart`: Infra only: a ready chart it may be installed from (`oci://...`).
    /// * `chart_version`: Infra only: that chart's version.
    fn service<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] kind: Option<&str>,
        #[starlark(require = named)] version: Option<&str>,
        #[starlark(require = named)] description: Option<&str>,
        #[starlark(require = named)] image: Option<&str>,
        #[starlark(require = named)] dockerfile: Option<&str>,
        #[starlark(require = named)] context: Option<&str>,
        #[starlark(require = named)] ports: Option<UnpackDictEntries<String, Value<'v>>>,
        #[starlark(require = named)] health: Option<&str>,
        #[starlark(require = named)] env: Option<UnpackDictEntries<String, String>>,
        #[starlark(require = named)] requires: Option<UnpackList<String>>,
        #[starlark(require = named)] provides: Option<Provides>,
        #[starlark(require = named)] docs: Option<UnpackList<String>>,
        #[starlark(require = named)] args: Option<UnpackList<String>>,
        #[starlark(require = named)] config: Option<UnpackDictEntries<String, String>>,
        #[starlark(require = named)] secrets: Option<UnpackList<String>>,
        #[starlark(require = named)] stateful: Option<NoneOr<bool>>,
        #[starlark(require = named)] storage: Option<&str>,
        #[starlark(require = named)] data_path: Option<&str>,
        #[starlark(require = named)] chart: Option<&str>,
        #[starlark(require = named)] chart_version: Option<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let kind = match kind {
            None | Some("own") => ServiceKind::Own,
            Some("infra") => ServiceKind::Infra,
            Some(other) => {
                return Err(anyhow::anyhow!(
                    "kind = {other:?} is not a kind; a service is \"own\" or \"infra\""
                ));
            }
        };
        let image = image.map(|i| word("image", i)).transpose()?;
        if kind == ServiceKind::Own
            && let Some(image) = &image
            && (image.contains('@') || image.rsplit('/').next().is_some_and(|l| l.contains(':')))
        {
            return Err(anyhow::anyhow!(
                "image = {image:?} names a tag or a digest; an own service's image is named \
                 without one, a build tags it"
            ));
        }
        let mut requires = requires
            .map(|r| {
                r.items
                    .iter()
                    .map(|g| kebab("requires", g))
                    .collect::<anyhow::Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        requires.sort();
        requires.dedup();
        let mut passport = ServicePassport {
            name: kebab("name", name)?,
            kind,
            version: version.map(|v| word("version", v)).transpose()?,
            description: description
                .map(|d| one_line("description", d))
                .transpose()?,
            image,
            dockerfile: dockerfile.map(|d| relative("dockerfile", d)).transpose()?,
            context: context.map(|c| relative("context", c)).transpose()?,
            ports: ports.map(ports_of).transpose()?.unwrap_or_default(),
            health: match health {
                Some(h) if h.starts_with('/') && !h.chars().any(char::is_whitespace) => {
                    Some(h.to_owned())
                }
                Some(h) => {
                    return Err(anyhow::anyhow!(
                        "health = {h:?} is not a route; it starts with `/` and has no spaces"
                    ));
                }
                None => None,
            },
            env: env
                .map(|e| variables("env", e))
                .transpose()?
                .unwrap_or_default(),
            requires,
            provides: BTreeMap::new(),
            docs: docs
                .map(|d| {
                    d.items
                        .iter()
                        .map(|p| relative("docs", p))
                        .collect::<anyhow::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            ..ServicePassport::default()
        };
        let infra_args = InfraArgs {
            args,
            config,
            secrets,
            stateful: stateful.and_then(NoneOr::into_option),
            storage,
            data_path,
            chart,
            chart_version,
        };
        match kind {
            ServiceKind::Infra => infra(&mut passport, infra_args, provides)?,
            ServiceKind::Own => {
                if let Some(arg) = infra_args.first_given() {
                    return Err(anyhow::anyhow!(
                        "`{arg}` goes with kind = \"infra\": an own service is built from its \
                         Dockerfile and filled through `env`"
                    ));
                }
                match provides {
                    None => {}
                    Some(Either::Left(_)) => {
                        return Err(anyhow::anyhow!(
                            "`provides` is a list of capability keys; a dict of values goes \
                             with kind = \"infra\""
                        ));
                    }
                    Some(Either::Right(keys)) => {
                        for key in keys.items {
                            word("provides", &key)?;
                            passport.provides.insert(key, String::new());
                        }
                    }
                }
            }
        }
        sink(eval)?.set_service(passport);
        Ok(NoneType)
    }
}

/// The globals a `service.gdl` is evaluated against: the standard set and the
/// one declaration.
#[must_use]
pub(crate) fn service_globals() -> Globals {
    GlobalsBuilder::standard()
        .with(gdl_service_vocabulary)
        .build()
}

/// The `service.gdl` vocabulary alone, for the editor.
#[must_use]
pub fn service_vocabulary() -> Globals {
    GlobalsBuilder::new().with(gdl_service_vocabulary).build()
}
