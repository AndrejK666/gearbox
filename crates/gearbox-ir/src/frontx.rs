//! Optional `FrontX` frontend: a shell from a `FrontX` template with micro-frontends
//! mounted in it. Unlike [`crate::ui`], packages keep `FrontX`'s own contract
//! (npm workspaces, `mfe.json`, module federation). Each package may carry a
//! passport, its `frontx.gdl` ([`FrontxPackage`]), which restates what those
//! manifests say and adds what they cannot; generation checks the two agree.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{GearId, SourceId};

/// One micro-frontend a product mounts in its shell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FrontxMfe {
    /// The package directory, relative to `source`'s root, or to the product
    /// description without one.
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
    /// Backend gears its screens call. They must be in the product; naming
    /// them neither adds them nor links anything.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub requires: BTreeSet<GearId>,
}

/// A package the micro-frontends share that is not one itself (no `mfe.json`),
/// such as a library of common components. It is mounted beside them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FrontxLibrary {
    /// The package directory, relative to `source`'s root, or to the product
    /// description without one.
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
}

/// A product's `FrontX` frontend, as the description chose it. The lock carries
/// the same record once resolution has checked the backends it requires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProductFrontx {
    /// The shell's directory (a `FrontX` template such as `template-shell`, or a
    /// product's own fork of it), relative to `source`'s root or the product.
    pub shell: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
    /// In the order the product lists them.
    pub mfes: Vec<FrontxMfe>,
    /// Packages the micro-frontends share, mounted beside them. One the shell
    /// already has under the same directory is replaced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub libraries: Vec<FrontxLibrary>,
    /// Named same-origin paths published by deployment adapters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entry_points: BTreeMap<String, String>,
}

/// What a `FrontX` package says about itself in its own `frontx.gdl`: its
/// passport.
///
/// One file holding what a tool needs to know about the package, including
/// what its `package.json` and `mfe.json` already say. Every field but a
/// shell's `signs_in` is optional, and a missing one falls back to the
/// manifests. Where a field restates a manifest fact, generation checks that
/// the two agree (GBX0722 for identity, pins and screens; GBX0723 for the
/// softer facts), so the second copy cannot drift silently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrontxPackage {
    /// `shell(...)`: a workspace that mounts micro-frontends.
    Shell(FrontxShell),
    /// `mfe(...)`: a micro-frontend.
    Mfe(FrontxMfePackage),
    /// `library(...)`: a package micro-frontends share that is not one itself.
    Library(FrontxIdentity),
}

impl FrontxPackage {
    /// Who the package says it is.
    #[must_use]
    pub fn identity(&self) -> &FrontxIdentity {
        match self {
            Self::Shell(shell) => &shell.identity,
            Self::Mfe(mfe) => &mfe.identity,
            Self::Library(identity) => identity,
        }
    }
}

/// Who a package is: the same three facts its `package.json` states, each
/// optional. Restated, they must agree with it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontxIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A shell's passport: what it does for a running product beyond mounting
/// screens, and how it is built and served.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontxShell {
    #[serde(flatten)]
    pub identity: FrontxIdentity,
    /// Whether it signs users in before it mounts any screen. A shell that
    /// does not, on a product whose backend wants signed-in callers, mounts
    /// screens whose calls are refused.
    pub signs_in: bool,
    /// Where the identity provider sends a signed-in user back to, paths on
    /// the shell's own origin. Empty when it does not sign in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sign_in_redirect: Vec<String>,
    /// The file, relative to the shell, it reads the product's addresses from
    /// at start (`public/env.js`), so one image serves every environment.
    /// `None` when it takes them at build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_env: Option<String>,
    /// The keys it reads from its runtime environment, each to what fills it
    /// (`"apiBase": "gateway.url"`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Variables it needs at build, when its addresses are fixed there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_env: Vec<String>,
    /// The Node.js it builds with, as `engines.node` says it (`">=24"`, `"24"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// `npm`, `pnpm` or `yarn`: the one whose lock file it keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_manager: Option<String>,
    /// The command that builds it, run at the package root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// The directory, relative to the shell, its build writes the site to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// The path it is served under (`/`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The path on its own origin its screens call the backend under
    /// (`/cf/`): the one the server proxies to the API Gateway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_prefix: Option<String>,
    /// The `FrontX` packages it gives what it mounts, name to version. `None`
    /// when not declared: then it is only read from the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carries: Option<BTreeMap<String, String>>,
}

impl FrontxShell {
    /// A shell that says only whether it signs in and where it reads its
    /// addresses: the minimal description.
    #[must_use]
    pub fn new(signs_in: bool, runtime_env: Option<String>) -> Self {
        Self {
            identity: FrontxIdentity::default(),
            signs_in,
            sign_in_redirect: Vec::new(),
            runtime_env,
            env: BTreeMap::new(),
            build_env: Vec::new(),
            node: None,
            package_manager: None,
            build: None,
            output: None,
            base: None,
            api_prefix: None,
            carries: None,
        }
    }
}

/// A micro-frontend's passport.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontxMfePackage {
    #[serde(flatten)]
    pub identity: FrontxIdentity,
    /// Backend gears its screens call. Added to what the product's
    /// `frontx_mfe(requires = ...)` names for it.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub requires: BTreeSet<GearId>,
    /// The labels of the screens it brings: its `mfe.json` extensions'
    /// `presentation.label`, in that order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screens: Option<Vec<String>>,
    /// The routes its extensions mount at: its `mfe.json` extensions'
    /// `route`, in that order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounts: Option<Vec<String>>,
    /// The `FrontX` it is written against: its exact `@gears-frontx/react` pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontx: Option<String>,
    /// Permissions its screens need, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
    /// Routes worth a screenshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview: Vec<String>,
    /// Mock data for those screenshots, a file inside the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixtures: Option<String>,
}
