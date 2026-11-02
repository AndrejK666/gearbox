//! Optional `FrontX` frontend: a shell from a `FrontX` template with micro-frontends
//! mounted in it. Unlike [`crate::ui`], packages keep `FrontX`'s own contract
//! (npm workspaces, `mfe.json`, module federation); package identity, versions
//! and extensions live in each `package.json` and `mfe.json`.
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
    /// Named same-origin paths published by deployment adapters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entry_points: BTreeMap<String, String>,
}
