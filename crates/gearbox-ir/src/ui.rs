//! Optional build-time UI composition. Package identity lives in package.json.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{GearId, RelPath, SourceId};

/// A gear's UI package, relative to its source root after GDL lowering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GearUi {
    pub package: RelPath,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub requires: BTreeSet<GearId>,
}

/// A product explicitly chooses its shell, SDK and included gear modules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProductUi {
    /// Package directories relative to the selected source, or product description.
    pub shell: String,
    pub sdk: String,
    pub modules: BTreeSet<GearId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedUiModule {
    pub source: SourceId,
    pub module: GearUi,
}

/// Locators are part of product.lock. Generation projects package.json without
/// executing JavaScript and records identities, exports and hashes in ui.lock.json.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedUi {
    pub shell: String,
    pub sdk: String,
    pub modules: BTreeMap<GearId, ResolvedUiModule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
}
