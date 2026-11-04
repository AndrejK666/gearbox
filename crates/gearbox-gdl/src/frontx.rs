//! The `frontx.gdl` vocabulary (GDL 1.5): a `FrontX` package's passport.
//!
//! One file at a package's root, beside its `package.json`, with one call that
//! says what the package is -- `shell(...)`, `mfe(...)` or `library(...)` --
//! and everything a tool needs to know about it: who it is, how it is built
//! and served, what it reads at start, what it calls. That includes what
//! `package.json` and `mfe.json` already say (name, version, pins, screens):
//! the passport is the one place to read a package from, and generation
//! checks every restated fact against the manifest it restates (GBX0722,
//! GBX0723), so the copies cannot drift unnoticed. Every argument but a
//! shell's `signs_in` is optional; one left out is read from the manifests.
//!
//! The calls are not `frontx_mfe(...)` and `frontx_library(...)`: those are the
//! product's, where they *locate* a package (`package = path(...)`). Here the
//! file is the locator, and the file's name already says `FrontX`, the way
//! `gear.gdl` holds `gear(...)`.

// Same reasoning as `globals.rs`: these fire on `#[starlark_module]` expansion.
// A passport is many optional named arguments, and each is one Rust parameter.
#![allow(
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::too_many_arguments,
    reason = "artifacts of #[starlark_module] expansion, not of hand-written code"
)]

use std::collections::{BTreeMap, BTreeSet};

use either::Either;
use gearbox_ir::GearId;
use gearbox_ir::frontx::{FrontxIdentity, FrontxMfePackage, FrontxPackage, FrontxShell};
use starlark::environment::{Globals, GlobalsBuilder};
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::dict::UnpackDictEntries;
use starlark::values::list::UnpackList;
use starlark::values::none::{NoneOr, NoneType};

use crate::sink::GdlSink;

fn sink<'a>(eval: &'a Evaluator<'_, '_, '_>) -> anyhow::Result<&'a GdlSink> {
    eval.extra
        .and_then(|e| e.downcast_ref::<GdlSink>())
        .ok_or_else(|| anyhow::anyhow!("internal error: no GdlSink installed on the evaluator"))
}

/// A path inside the package: relative, forward slashes, never leaving it.
fn package_relative(field: &str, path: &str) -> anyhow::Result<String> {
    if path.chars().any(char::is_whitespace) {
        return Err(anyhow::anyhow!(
            "{field} = \"{path}\" is not a path inside the package: it has spaces"
        ));
    }
    gearbox_ir::RelPath::new(path)
        .map(|_| path.to_owned())
        .map_err(|e| anyhow::anyhow!("{field} = \"{path}\" is not a path inside the package: {e}"))
}

/// A non-empty, one-line string: it may reach a Dockerfile or a server
/// configuration, where a line break would start a new instruction.
fn one_line(field: &str, value: &str) -> anyhow::Result<String> {
    if value.trim().is_empty() || value.contains(['\n', '\r']) {
        return Err(anyhow::anyhow!(
            "{field} = {value:?} is empty or spans lines; write it on one line"
        ));
    }
    Ok(value.to_owned())
}

/// A word with no spaces: a package name, a version, a key.
fn word(field: &str, value: &str) -> anyhow::Result<String> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(anyhow::anyhow!(
            "{field} = {value:?} is empty or has spaces"
        ));
    }
    Ok(value.to_owned())
}

/// A path on the package's origin, `/` to `/`: `"/"`, `"/cf/"`. Only
/// letters, digits, `-`, `_`, `.` and `/`, since it is written into the
/// server's configuration.
fn url_path(field: &str, value: &str, root_allowed: bool) -> anyhow::Result<String> {
    let safe = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    if !safe || !value.starts_with('/') || !value.ends_with('/') || value.contains("//") {
        return Err(anyhow::anyhow!(
            "{field} = {value:?} is not a path such as \"/cf/\": it starts and ends with `/`, \
             with letters, digits, `-`, `_` and `.` between"
        ));
    }
    if value == "/" && !root_allowed {
        return Err(anyhow::anyhow!(
            "{field} = \"/\" would send every request to the backend; name the prefix, e.g. \"/cf/\""
        ));
    }
    Ok(value.to_owned())
}

/// A route on the package's origin: `/accounts`.
fn route(field: &str, value: &str) -> anyhow::Result<String> {
    if !value.starts_with('/') || value.chars().any(char::is_whitespace) {
        return Err(anyhow::anyhow!(
            "{field}: {value:?} is not a route; it starts with `/` and has no spaces"
        ));
    }
    Ok(value.to_owned())
}

/// An environment variable name: `VITE_API_BASE`.
fn env_name(value: &str) -> anyhow::Result<String> {
    let valid = value.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
        && value
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(anyhow::anyhow!(
            "build_env: `{value}` is not a variable name (uppercase letters, digits and `_`)"
        ));
    }
    Ok(value.to_owned())
}

fn identity(
    name: Option<&str>,
    version: Option<&str>,
    description: Option<&str>,
) -> anyhow::Result<FrontxIdentity> {
    Ok(FrontxIdentity {
        name: name.map(|n| word("name", n)).transpose()?,
        version: version.map(|v| word("version", v)).transpose()?,
        description: description
            .map(|d| one_line("description", d))
            .transpose()?,
    })
}

/// Each item of an optional list, checked; `None` when it was not given.
fn each(
    list: Option<UnpackList<String>>,
    check: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<Option<Vec<String>>> {
    list.map(|l| l.items.iter().map(|s| check(s)).collect())
        .transpose()
}

/// A dict of strings, each key and value checked, every key once.
fn string_map(
    field: &str,
    entries: UnpackDictEntries<String, String>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for (key, value) in entries.entries {
        word(&format!("{field} key"), &key)?;
        one_line(&format!("{field}[{key:?}]"), &value)?;
        if map.insert(key.clone(), value).is_some() {
            return Err(anyhow::anyhow!("{field}: `{key}` is given twice"));
        }
    }
    Ok(map)
}

#[starlark_module]
fn gdl_frontx_vocabulary(builder: &mut GlobalsBuilder) {
    /// `shell(...)` -- this package is a `FrontX` shell: a workspace that mounts
    /// micro-frontends from `src-app/mfe_packages/*` (GDL 1.5). Its passport.
    ///
    /// Every argument but `signs_in` is optional; one left out is read from the
    /// shell's own `package.json` and workspace, as without a passport. One that
    /// restates them is checked against them at generation.
    ///
    /// # Arguments
    /// * `name`: The npm name, as its `package.json` says.
    /// * `version`: The version, as its `package.json` says.
    /// * `description`: One line on what it is, as its `package.json` says.
    /// * `signs_in`: Whether it signs users in before it mounts a screen. A product
    ///   whose backend signs users in needs a shell that does.
    /// * `sign_in_redirect`: Where the identity provider sends a signed-in user back
    ///   to: a path on the shell's origin, or a list of them. Only with `signs_in`.
    /// * `runtime_env`: The file, relative to this package, it reads the product's
    ///   addresses from at start (`"public/env.js"`), so one image serves every
    ///   environment. `None`, the default, when it takes them at build.
    /// * `env`: The keys it reads from that file, each to what fills it
    ///   (`{"apiBase": "gateway.url"}`).
    /// * `build_env`: The variables it needs at build, when its addresses are fixed
    ///   there (`["VITE_API_BASE"]`).
    /// * `node`: The Node.js it builds with, as `engines.node` says it (`">=24"`). An
    ///   exact or major version (`"24"`, `"24.21.0"`) picks the build image.
    /// * `package_manager`: `"npm"`, `"pnpm"` or `"yarn"`: the one whose lock it keeps.
    /// * `build`: The command that builds it at its root. The default is
    ///   `npm install --no-audit --no-fund && npm run build`.
    /// * `output`: The directory its build writes the site to; `"dist"` by default.
    /// * `base`: The path it is served under; `"/"` by default.
    /// * `api_prefix`: The path its screens call the backend under, which the server
    ///   proxies to the API Gateway; `"/cf/"` by default.
    /// * `carries`: The `FrontX` packages it gives what it mounts, name to exact
    ///   version, as its workspace has them.
    fn shell(
        #[starlark(require = named)] signs_in: bool,
        #[starlark(require = named)] name: Option<&str>,
        #[starlark(require = named)] version: Option<&str>,
        #[starlark(require = named)] description: Option<&str>,
        #[starlark(require = named)] sign_in_redirect: Option<
            NoneOr<Either<String, UnpackList<String>>>,
        >,
        #[starlark(require = named)] runtime_env: Option<NoneOr<&str>>,
        #[starlark(require = named)] env: Option<UnpackDictEntries<String, String>>,
        #[starlark(require = named)] build_env: Option<UnpackList<String>>,
        #[starlark(require = named)] node: Option<&str>,
        #[starlark(require = named)] package_manager: Option<&str>,
        #[starlark(require = named)] build: Option<&str>,
        #[starlark(require = named)] output: Option<&str>,
        #[starlark(require = named)] base: Option<&str>,
        #[starlark(require = named)] api_prefix: Option<&str>,
        #[starlark(require = named)] carries: Option<UnpackDictEntries<String, String>>,
        eval: &mut Evaluator<'_, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let runtime_env = match runtime_env.and_then(NoneOr::into_option) {
            Some(path) => Some(package_relative("runtime_env", path)?),
            None => None,
        };
        let redirects = match sign_in_redirect.and_then(NoneOr::into_option) {
            None => Vec::new(),
            Some(Either::Left(one)) => vec![one],
            Some(Either::Right(many)) => many.items,
        };
        let sign_in_redirect = redirects
            .iter()
            .map(|r| route("sign_in_redirect", r))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if !signs_in && !sign_in_redirect.is_empty() {
            return Err(anyhow::anyhow!(
                "sign_in_redirect is where a signed-in user comes back to; a shell with \
                 `signs_in = False` has none"
            ));
        }
        let package_manager = match package_manager {
            None => None,
            Some(pm @ ("npm" | "pnpm" | "yarn")) => Some(pm.to_owned()),
            Some(other) => {
                return Err(anyhow::anyhow!(
                    "package_manager = {other:?}; it is \"npm\", \"pnpm\" or \"yarn\""
                ));
            }
        };
        let carries = carries.map(|c| string_map("carries", c)).transpose()?;
        if let Some((package, at)) = carries
            .iter()
            .flatten()
            .find(|(_, at)| at.chars().any(char::is_whitespace))
        {
            return Err(anyhow::anyhow!(
                "carries[{package:?}] = {at:?} is not a version"
            ));
        }
        sink(eval)?.set_frontx(FrontxPackage::Shell(FrontxShell {
            identity: identity(name, version, description)?,
            signs_in,
            sign_in_redirect,
            runtime_env,
            env: env
                .map(|e| string_map("env", e))
                .transpose()?
                .unwrap_or_default(),
            build_env: each(build_env, env_name)?.unwrap_or_default(),
            node: node.map(|n| word("node", n)).transpose()?,
            package_manager,
            build: build.map(|b| one_line("build", b)).transpose()?,
            output: output.map(|o| package_relative("output", o)).transpose()?,
            base: base.map(|b| url_path("base", b, true)).transpose()?,
            api_prefix: api_prefix
                .map(|p| url_path("api_prefix", p, false))
                .transpose()?,
            carries,
        }));
        Ok(NoneType)
    }

    /// `mfe(...)` -- this package is a `FrontX` micro-frontend (GDL 1.5). Its
    /// passport; every argument is optional, and one that restates its
    /// `package.json` or `mfe.json` is checked against it at generation.
    ///
    /// # Arguments
    /// * `name`: The npm name, as its `package.json` says.
    /// * `version`: The version, as its `package.json` says.
    /// * `description`: One line on what it is, as its `package.json` says.
    /// * `requires`: The backend gears its screens call, by id. A product that mounts
    ///   it must have each; `[]`, the default, says it calls none.
    /// * `screens`: The labels of the screens it brings, as its `mfe.json`
    ///   extensions' `presentation.label` say, in order.
    /// * `mounts`: The routes its extensions mount at, as its `mfe.json` extensions'
    ///   `route` say, in order.
    /// * `frontx`: The `FrontX` it is written against: its `@gears-frontx/react` pin.
    /// * `roles`: The permissions its screens need, when known.
    /// * `preview`: Routes worth a screenshot, among its `mounts`.
    /// * `fixtures`: The file, inside the package, with mock data for those screenshots.
    fn mfe(
        #[starlark(require = named)] name: Option<&str>,
        #[starlark(require = named)] version: Option<&str>,
        #[starlark(require = named)] description: Option<&str>,
        #[starlark(require = named)] requires: Option<UnpackList<String>>,
        #[starlark(require = named)] screens: Option<UnpackList<String>>,
        #[starlark(require = named)] mounts: Option<UnpackList<String>>,
        #[starlark(require = named)] frontx: Option<&str>,
        #[starlark(require = named)] roles: Option<UnpackList<String>>,
        #[starlark(require = named)] preview: Option<UnpackList<String>>,
        #[starlark(require = named)] fixtures: Option<&str>,
        eval: &mut Evaluator<'_, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let requires = requires
            .map(|r| r.items)
            .unwrap_or_default()
            .into_iter()
            .map(|id| {
                GearId::new(id.clone())
                    .map_err(|e| anyhow::anyhow!("requires: `{id}` is not a gear id: {e}"))
            })
            .collect::<anyhow::Result<BTreeSet<_>>>()?;
        sink(eval)?.set_frontx(FrontxPackage::Mfe(FrontxMfePackage {
            identity: identity(name, version, description)?,
            requires,
            screens: each(screens, |s| one_line("screens", s))?,
            mounts: each(mounts, |r| route("mounts", r))?,
            frontx: frontx.map(|f| word("frontx", f)).transpose()?,
            roles: each(roles, |r| word("roles", r))?,
            preview: each(preview, |r| route("preview", r))?.unwrap_or_default(),
            fixtures: fixtures
                .map(|f| package_relative("fixtures", f))
                .transpose()?,
        }));
        Ok(NoneType)
    }

    /// `library(...)` -- this package is shared by micro-frontends and is not one
    /// itself (GDL 1.5). Its passport says who it is; every argument is optional.
    ///
    /// # Arguments
    /// * `name`: The npm name, as its `package.json` says.
    /// * `version`: The version, as its `package.json` says.
    /// * `description`: One line on what it is, as its `package.json` says.
    fn library(
        #[starlark(require = named)] name: Option<&str>,
        #[starlark(require = named)] version: Option<&str>,
        #[starlark(require = named)] description: Option<&str>,
        eval: &mut Evaluator<'_, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        sink(eval)?.set_frontx(FrontxPackage::Library(identity(
            name,
            version,
            description,
        )?));
        Ok(NoneType)
    }
}

/// The globals a `frontx.gdl` is evaluated against: the standard set and the
/// three declarations, nothing of a gear's or a product's.
#[must_use]
pub(crate) fn frontx_globals() -> Globals {
    GlobalsBuilder::standard()
        .with(gdl_frontx_vocabulary)
        .build()
}

/// The `frontx.gdl` vocabulary alone, for the editor; see
/// `globals::gear_vocabulary` on why it is built up rather than subtracted.
#[must_use]
pub fn frontx_vocabulary() -> Globals {
    GlobalsBuilder::new().with(gdl_frontx_vocabulary).build()
}
