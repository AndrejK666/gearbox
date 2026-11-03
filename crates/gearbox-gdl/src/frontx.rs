//! The `frontx.gdl` vocabulary (GDL 1.5): a `FrontX` package describing itself.
//!
//! One file at a package's root, beside its `package.json`, with one call that
//! says what the package is: `shell(...)`, `mfe(...)` or `library()`. It states
//! only what the package's own manifests cannot: a package's name, version and
//! pins stay in `package.json`, a micro-frontend's screens and extensions in
//! `mfe.json`, and what a shell carries is still read from its workspace.
//! Restating any of those here would be a second copy to drift.
//!
//! The calls are not `frontx_mfe(...)` and `frontx_library(...)`: those are the
//! product's, where they *locate* a package (`package = path(...)`). Here the
//! file is the locator, and the file's name already says `FrontX`, the way
//! `gear.gdl` holds `gear(...)`.

// Same reasoning as `globals.rs`: these fire on `#[starlark_module]` expansion.
#![allow(
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    reason = "artifacts of #[starlark_module] expansion, not of hand-written code"
)]

use std::collections::BTreeSet;

use gearbox_ir::GearId;
use gearbox_ir::frontx::{FrontxPackage, FrontxShell};
use starlark::environment::{Globals, GlobalsBuilder};
use starlark::eval::Evaluator;
use starlark::starlark_module;
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
    gearbox_ir::RelPath::new(path)
        .map(|_| path.to_owned())
        .map_err(|e| anyhow::anyhow!("{field} = \"{path}\" is not a path inside the package: {e}"))
}

#[starlark_module]
fn gdl_frontx_vocabulary(builder: &mut GlobalsBuilder) {
    /// `shell(...)` -- this package is a `FrontX` shell: a workspace that mounts
    /// micro-frontends from `src-app/mfe_packages/*` (GDL 1.5).
    ///
    /// What it carries is not declared: it is read from its `packages/*`, its
    /// exact `@gears-frontx/*` dependencies and its own `package.json`.
    ///
    /// # Arguments
    /// * `signs_in`: Whether it signs users in before it mounts a screen. A product
    ///   whose backend signs users in needs a shell that does.
    /// * `runtime_env`: The file, relative to this package, it reads the product's
    ///   addresses from at start (`"public/env.js"`), so one image serves every
    ///   environment. `None`, the default, when it takes them at build.
    fn shell(
        #[starlark(require = named)] signs_in: bool,
        #[starlark(require = named)] runtime_env: Option<NoneOr<&str>>,
        eval: &mut Evaluator<'_, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let runtime_env = match runtime_env.and_then(NoneOr::into_option) {
            Some(path) => Some(package_relative("runtime_env", path)?),
            None => None,
        };
        sink(eval)?.set_frontx(FrontxPackage::Shell(FrontxShell {
            signs_in,
            runtime_env,
        }));
        Ok(NoneType)
    }

    /// `mfe(...)` -- this package is a `FrontX` micro-frontend (GDL 1.5). Its screens
    /// and extensions stay in its `mfe.json`.
    ///
    /// # Arguments
    /// * `requires`: The backend gears its screens call, by id. A product that mounts
    ///   it must have each; `[]`, the default, says it calls none.
    fn mfe(
        #[starlark(require = named)] requires: Option<UnpackList<String>>,
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
        sink(eval)?.set_frontx(FrontxPackage::Mfe { requires });
        Ok(NoneType)
    }

    /// `library()` -- this package is shared by micro-frontends and is not one
    /// itself (GDL 1.5). It has nothing more to declare.
    fn library(eval: &mut Evaluator<'_, '_, '_>) -> anyhow::Result<NoneType> {
        sink(eval)?.set_frontx(FrontxPackage::Library);
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
