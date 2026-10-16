//! The wire types, and the shape of a staged catalogue load over JSON-RPC.
//!
//! Everything here derives `TS`, because the client must never hand-maintain a
//! mirror of a wire format (`cpt-gearbox-nfr-no-type-drift`). The envelopes join
//! the `gearbox-ir` types already exported, so one `make ts` covers both.
//!
//! No `#[ts(export_to = ...)]` on any of these, and that is not an omission: the
//! output directory belongs to the export test's `Config`, and an `export_to`
//! here is resolved *relative to it*, which silently doubles the path. The IR
//! types carry no such attribute for the same reason.

use gearbox_ir::{
    ConfigValue, DesignGear, Diagnostic, ExplanationGraph, FileAction, FilePlan, GearDescriptor,
    Ownership, PendingGear, ProductIntent, ResolvedProduct,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Method names, in one place so the server and the smoke test cannot drift.
pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const SHUTDOWN: &str = "shutdown";
    pub const CATALOGUE_LOAD: &str = "gearbox/catalogue/load";
    pub const PRODUCT_LOAD: &str = "gearbox/product/load";
    pub const PRODUCT_RESOLVE: &str = "gearbox/product/resolve";
    pub const PRODUCT_RESOLVE_PREVIEW: &str = "gearbox/product/resolvePreview";
    pub const PRODUCT_LOCK: &str = "gearbox/product/lock";
    pub const PRODUCT_ADD_GEAR: &str = "gearbox/product/addGear";
    pub const PRODUCT_REMOVE_GEAR: &str = "gearbox/product/removeGear";
    pub const PRODUCT_SET_CONFIG: &str = "gearbox/product/setConfig";
    pub const PRODUCT_SET_FEATURES: &str = "gearbox/product/setFeatures";
    pub const PRODUCT_ADD_PROFILE: &str = "gearbox/product/addProfile";
    pub const PRODUCT_REMOVE_PROFILE: &str = "gearbox/product/removeProfile";
    pub const PRODUCT_SET_PROFILE_FIELD: &str = "gearbox/product/setProfileField";
    pub const PRODUCT_APPLY_EDITS: &str = "gearbox/product/applyEdits";
    pub const PRODUCT_CREATE: &str = "gearbox/product/create";
    pub const GEAR_SCAFFOLD: &str = "gearbox/gear/scaffold";
    pub const VALIDATE: &str = "gearbox/validate";
    pub const GENERATE_PLAN: &str = "gearbox/generate/plan";
    pub const GENERATE_APPLY: &str = "gearbox/generate/apply";
    pub const GENERATE_FILE: &str = "gearbox/generate/file";

    pub const INITIALIZED: &str = "initialized";
    pub const EXIT: &str = "exit";
    // LSP's own, spelled exactly as LSP spells them: a `.gdl` language client
    // built on anything standard has to find them here. See `crate::lsp`.
    pub const DID_OPEN: &str = "textDocument/didOpen";
    pub const DID_CHANGE: &str = "textDocument/didChange";
    pub const DID_CLOSE: &str = "textDocument/didClose";
    pub const PUBLISH_DIAGNOSTICS: &str = "textDocument/publishDiagnostics";
    pub const COMPLETION: &str = "textDocument/completion";
    pub const HOVER: &str = "textDocument/hover";
    pub const CATALOGUE_CHANGED: &str = "gearbox/catalogueChanged";
    pub const CATALOGUE_DIAGNOSTICS: &str = "gearbox/catalogueDiagnostics";
    pub const PROGRESS: &str = "$/progress";
    pub const LOG: &str = "gearbox/log";
}

/// Application errors, so a transport failure and a Gearbox failure are never
/// confused.
///
/// Inside LSP's `ServerErrorStart..ServerErrorEnd` window (`-32099..-32000`) but
/// clear of the two values LSP itself defines there -- `-32001`
/// `UnknownErrorCode` and `-32002` `ServerNotInitialized`. Reusing `-32002` for
/// "no workspace" would have been quietly wrong: a client mapping codes to
/// messages would report the wrong cause.
pub mod error_code {
    pub const NOT_INITIALIZED: i32 = -32050;
    pub const WORKSPACE_NOT_OPEN: i32 = -32051;
    /// A catalogue load failed outright.
    ///
    /// Reserved rather than live: the only thing that ever answered with it was
    /// a result that could not be *serialized*, on whatever method had been
    /// called, which told a client its load had failed. That now answers
    /// `InternalError`. A catalogue load that goes wrong today reports its
    /// diagnostics with a successful response, so this is the code for the day
    /// one cannot -- kept because it is part of the wire contract clients map.
    pub const LOAD_FAILED: i32 = -32052;
    /// The product description could not be evaluated.
    ///
    /// Distinct from [`RESOLVE_FAILED`]: this one means the file is not a
    /// product at all, so there is nothing to resolve and no partial answer to
    /// return.
    pub const PRODUCT_LOAD_FAILED: i32 = -32053;
    /// Resolution ran and could not produce a lock.
    ///
    /// Reserved for the case where no `ResolvedProduct` exists at all. A product
    /// that resolves *with errors* is not this: it comes back normally, with its
    /// diagnostics, because a partial graph plus three errors is more useful
    /// than one error and nothing to look at.
    pub const RESOLVE_FAILED: i32 = -32054;
    /// The client never declared write capability, so a mutating method is
    /// refused outright (`cpt-gearbox-fr-rpc-writes-opt-in`). Distinct from a
    /// failed write: nothing was attempted.
    pub const WRITES_NOT_ALLOWED: i32 = -32055;
    /// The path is outside every declared root, or the file could not be edited.
    pub const EDIT_REFUSED: i32 = -32056;
    /// Generation was refused: the output root is not writable, resolution
    /// reported errors, or the engine could not produce a tree.
    pub const GENERATE_REFUSED: i32 = -32057;

    /// Every code above, so the guard that checks them against LSP's reserved
    /// window sees all of them.
    ///
    /// Here rather than in the test, because the test's own array had four of
    /// the nine in it: half the codes had nothing checking them, and a new one
    /// outside the window would have been caught only if whoever added it
    /// remembered to extend a list in another file. A code added here and left
    /// out of this slice is still possible, and it is one line away from the
    /// constant instead of one file away.
    pub const ALL: &[i32] = &[
        NOT_INITIALIZED,
        WORKSPACE_NOT_OPEN,
        LOAD_FAILED,
        PRODUCT_LOAD_FAILED,
        RESOLVE_FAILED,
        WRITES_NOT_ALLOWED,
        EDIT_REFUSED,
        GENERATE_REFUSED,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct InitializeParams {
    /// Source roots to scan. Absolute, or relative to the server's working
    /// directory.
    #[serde(default)]
    pub roots: Vec<String>,

    /// Whether this client may ask the server to change files.
    ///
    /// Declared by the client, defaulting to `false`, and every mutating method
    /// is refused until it is `true`
    /// (`cpt-gearbox-fr-rpc-writes-opt-in`): "The API's first clients are an
    /// editor and an autonomous agent. Read-only by default is the only safe
    /// posture."
    ///
    /// A declaration rather than a negotiation. The server has no way to judge
    /// whether a caller *should* be allowed to write, so it does not pretend to:
    /// it records what was claimed and refuses everything not claimed, which
    /// makes a client that never asks for writes incapable of making one by
    /// accident.
    #[serde(default)]
    pub allow_writes: bool,

    /// The directory writes may touch, beyond the source roots.
    ///
    /// `cpt-gearbox-fr-rpc-writes-opt-in` requires rejecting "any path outside
    /// the declared workspace or source roots", and a product description lives
    /// in neither: it sits beside the products, not inside a *gear* source root.
    /// So the workspace is declared too, by the client that knows where it is.
    ///
    /// Deliberately not the server's working directory, which was the first
    /// attempt: the cwd of a process is not a boundary anybody declared, and
    /// treating it as one means the permitted set changes with how the server
    /// happened to be launched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,

    /// The roots and workspace `gearbox/product/create` is judged against,
    /// whatever this session declares above.
    ///
    /// **Why create needs its own boundary.** ADR
    /// `cpt-gearbox-adr-create-product` says "Start-screen create runs against
    /// the repository workspace the engine already knows from boot ... not
    /// against an open product session". It could not: `writable_out_root`
    /// reads `roots` and `workspace`, so opening a product whose `sources`
    /// contain the place products live made every later create refuse with "is
    /// inside a source root" -- the first create in a session worked and the
    /// next did not.
    ///
    /// **And the server cannot remember it by itself.** Studio disposes and
    /// respawns the engine on every `initialize`, so each process sees exactly
    /// one, and "the first one" is not a boot the process ever witnessed. The
    /// client is the only party that knows its own defaults, so it says them.
    ///
    /// Absent means "judge create by this session", which is what every client
    /// that does not set it gets -- including the CLI, whose behaviour is
    /// therefore unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_boundary: Option<CreationBoundary>,
}

/// Where a client will always permit `create`, independent of its session.
///
/// Both halves are needed because `writable_out_root` applies both tests: a path
/// must be inside the workspace and outside every source root. Judging create by
/// boot roots while still measuring it against an open product's *workspace*
/// would refuse the same paths for the other of the two reasons.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CreationBoundary {
    /// The source roots create is judged against. Empty means "none declared",
    /// which makes every path pass the source-root half.
    #[serde(default)]
    pub roots: Vec<String>,

    /// The workspace create is judged against. Absent falls back to the
    /// session's, because a create with no workspace at all is refused anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// One source root as the server resolved it on this machine.
///
/// Deliberately an RPC fact, not an IR one. `SourceDecl::location` keeps the
/// location *as the operator wrote it* because it goes into `product.lock`, and
/// a lock carrying `/Users/someone/...` would not survive being committed. But a
/// client rendering a clickable path needs a real path, and the RPC server and
/// its client are on the same machine by construction -- so this is the layer
/// where an absolute path is the right answer.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ResolvedRoot {
    /// The source id every `GearDescriptor::source` refers to.
    pub id: String,
    /// Canonical absolute path of the root directory.
    pub path: String,
}

/// A root the server was asked for and could not open.
///
/// Reported rather than dropped. A shorter `roots` list says nothing about
/// *which* root is missing or why, and the alternative -- waiting for the load
/// to fail with `WORKSPACE_NOT_OPEN` -- loses the cause entirely.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct FailedRoot {
    /// The path as the client (or `--root`) spelled it.
    pub path: String,
    /// Why it could not be opened.
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct InitializeResult {
    pub server_info: ServerInfo,
    /// What this build can do. Read by the client to decide which panels are
    /// worth showing, so a panel is disabled rather than empty when the engine
    /// cannot answer it yet.
    pub capabilities: Capabilities,
    /// Where each source root actually is.
    ///
    /// Without this a client cannot open anything the catalogue points at:
    /// `gdl_path` and every docs path are relative to their source root, and the
    /// root is the one thing only the server knows.
    #[serde(default)]
    pub roots: Vec<ResolvedRoot>,

    /// The roots that could not be opened, with the reason for each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_roots: Vec<FailedRoot>,
}

/// Deliberately honest about what is not built.
///
/// `resolve` and `generate` are advertised once the engine can answer them, so
/// the client can hide a panel rather than render an empty one that looks like
/// a bug. Docker and Helm (M7) are still missing, and their absence is a smaller
/// output set rather than a flag: `generate: false` would hide a panel that
/// answers correctly for everything it does cover.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "a set of names cannot say `resolve: false`: absence would be \
              ambiguous between `this server does not have it` and `this server \
              is too old to know about it`, and telling those apart is the whole \
              reason the field exists"
)]
pub struct Capabilities {
    pub catalogue: bool,
    pub staged_catalogue: bool,
    pub resolve: bool,
    pub generate: bool,
    /// Whether *this session* may change files.
    ///
    /// Reflects back what the client declared in `InitializeParams`, not a
    /// property of the build. A client that forgot to ask can therefore see that
    /// it forgot, instead of discovering it from a refusal later.
    pub writes: bool,

    /// LSP's `ServerCapabilities.textDocumentSync`, in LSP's spelling.
    ///
    /// **This field is why `capabilities` is a superset rather than a second
    /// surface.** An LSP client reads `result.capabilities.textDocumentSync` and
    /// ignores the sibling fields it does not recognise, which is what the
    /// protocol requires of it; a Gearbox client reads the sibling fields and
    /// ignores this one. One object, two readers, no negotiation between them.
    ///
    /// `1` is `Full`: the whole document arrives on every edit. See
    /// `crate::lsp::SYNC_FULL` for why incremental sync is not worth its
    /// bookkeeping here.
    #[serde(rename = "textDocumentSync")]
    pub text_document_sync: u8,

    /// LSP's `ServerCapabilities.completionProvider`, in LSP's spelling.
    ///
    /// A language client that does not see this never sends
    /// `textDocument/completion`, so an unadvertised provider is a silent
    /// no-feature -- the same failure mode `textDocumentSync` has.
    #[serde(rename = "completionProvider")]
    pub completion_provider: CompletionOptions,

    /// LSP's `ServerCapabilities.hoverProvider`.
    #[serde(rename = "hoverProvider")]
    pub hover_provider: bool,
}

/// LSP's `CompletionOptions`.
///
/// No `triggerCharacters`: completion here is asked for explicitly or on an
/// identifier, and Monaco's trigger characters are single characters only. `(`
/// as a trigger would open the list on every call in the file, including the
/// ones a person has finished writing.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CompletionOptions {
    /// Whether a chosen item needs a second round trip to complete. It does not:
    /// every label is already the text to insert.
    #[serde(rename = "resolveProvider")]
    pub resolve_provider: bool,
}

/// `gearbox/product/load` -- evaluate a `product.gdl` and return what it says.
///
/// Evaluation only. Whether the gears it names exist is
/// `gearbox/validate`'s question, and what topology they produce is
/// `gearbox/product/resolve`'s.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ProductLoadParams {
    /// Absolute path to the description. Absolute because the server's working
    /// directory is not the client's, and a relative path here has produced a
    /// `file://` URI that renders as a link and opens nothing.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ProductLoadResult {
    /// Exact document snapshot used to evaluate intent.
    pub source: String,
    pub intent: ProductIntent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

/// `gearbox/product/resolve` -- one profile's topology.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ResolveParams {
    pub path: String,
    /// Which deployment profile. `None` uses the product's own default, so the
    /// common call is short and the answer still comes from the description
    /// rather than from a guess made here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// A gear a preview proposes adding, before anything is written.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PreviewAddGear {
    pub gear: String,
    /// The source id to record, exactly as `addGear` would.
    pub source: String,
}

/// `gearbox/product/resolvePreview` -- resolve a description that is not on disk.
///
/// The question this answers is the one a configurator has to answer before it
/// writes: *what would this product become*. The edits are applied to the
/// description's text in memory and the result is resolved; nothing is written,
/// so there is no write gate and no lock to disturb.
///
/// `add` and `edits` are applied in that order, which is the order
/// `commitAddGear` uses -- a gear enters the list, then its features, config and
/// plugins are set on the entry. A preview built from a different order would be
/// answering about a product nobody is going to write.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ResolvePreviewParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The gear to add first, if this preview is about adding one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add: Option<PreviewAddGear>,
    /// Edits applied after the addition. `config` and `features` do not change a
    /// resolution, but `plugins` do -- a chosen plugin enters the closure itself --
    /// so a preview that ignored them would understate what it is for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<ProductEdit>,
}

/// `gearbox/product/lock` -- the canonical lock text for one profile.
///
/// The same shape as [`ResolveParams`], and deliberately a separate method
/// rather than another field on [`ResolveResult`]: serializing the lock costs
/// work and bytes that the panels reading a resolution do not need, and the
/// client asks for the text only when something is going to show it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct LockParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,

    /// The output tree to compare against, when it is not the default
    /// `.gearbox/<product>/<profile>/`.
    ///
    /// The Studio sends none: there is one generated tree now that a lock no
    /// longer depends on which client wrote it. It exists so a test can put a
    /// deliberately stale lock somewhere of its own instead of doctoring the tree
    /// the plan's section 12 step 2 builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
}

/// The lock as it would be written.
///
/// `canonical` comes from `gearbox_lock::write_canonical`, the one function that
/// decides the lock's bytes. A client must never render its own TOML: byte
/// identity across runs is the property the lock exists for
/// (`cpt-gearbox-nfr-determinism`), and a second serializer is a second answer.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct LockResult {
    pub canonical: String,
    /// Repeated here so a caller can label the text without parsing it.
    pub lock_hash: String,
    pub profile: String,

    /// Where a lock for this profile lives, whether or not one is there.
    ///
    /// Reported even when absent, because "there is no lock yet" and "I did not
    /// look" are different answers and a client cannot tell them apart from a
    /// missing field.
    pub lock_path: String,

    /// The lock already on disk, when there is one.
    ///
    /// Carried with the text rather than fetched by a second method, for the same
    /// reason `ResolveResult` carries its explanation: the two have to be about
    /// one resolution, and a separate call cannot promise that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_disk: Option<LockOnDisk>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

/// The lock found on disk, and how it differs from the one just resolved.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct LockOnDisk {
    /// The bytes as they are on disk, so a client can show a text diff without
    /// reading the file itself.
    pub canonical: String,

    /// The hash the file carries. Empty when the file could not be parsed.
    pub lock_hash: String,

    /// What differs, in the engine's own words: `LockDiff::summary()`, whose doc
    /// comment names this widget as its consumer. `+` added, `-` removed, `~`
    /// changed.
    ///
    /// **Empty means the two are the same**, which is why it is not
    /// `skip_serializing_if`: an absent list and an empty one would read alike,
    /// and "no differences" is the answer most worth being sure of.
    ///
    /// Sent rather than computed by the client. A second implementation of "what
    /// changed" is a second answer, and the whole point of a lock is that there
    /// is one.
    pub changes: Vec<String>,

    /// Why the file on disk is not a lock, when it is not one.
    ///
    /// A file that exists and does not parse is neither "current" nor "stale",
    /// and saying so beats reporting an empty diff for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
}

/// `gearbox/product/addGear` and `gearbox/product/removeGear`.
///
/// One envelope for both, because they differ only in direction, and a caller
/// that can express one can express the other without learning a second shape.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct EditGearParams {
    /// The product description to edit.
    pub path: String,
    /// The gear to add or remove.
    pub gear: String,
    /// Which declared source the gear comes from. Ignored when removing.
    #[serde(default)]
    pub source: Option<String>,
    /// Report what would change and write nothing.
    ///
    /// ADR `cpt-gearbox-adr-authoring-ownership-tiers`: "A preview is not
    /// optional. Every surveyed tool has `--dry-run`." The flag rather than a
    /// second method, for the same reason `gearbox generate` has one.
    #[serde(default)]
    pub dry_run: bool,
}

/// `gearbox/product/setConfig` -- one key in a gear's `config = {...}`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SetConfigParams {
    pub path: String,
    pub gear: String,
    pub key: String,
    /// `None` removes the key.
    #[serde(default)]
    pub value: Option<ConfigValue>,
    #[serde(default)]
    pub dry_run: bool,
}

/// `gearbox/product/setFeatures`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SetFeaturesParams {
    pub path: String,
    pub gear: String,
    pub features: Vec<String>,
    #[serde(default)]
    pub dry_run: bool,
}

/// `gearbox/product/addProfile`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct AddProfileParams {
    pub path: String,
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub fields: Vec<ProfileFieldEntry>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ProfileFieldEntry {
    pub name: String,
    pub value: String,
}

/// `gearbox/product/removeProfile`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct RemoveProfileParams {
    pub path: String,
    pub id: String,
    #[serde(default)]
    pub dry_run: bool,
}

/// `gearbox/product/setProfileField`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SetProfileFieldParams {
    pub path: String,
    pub id: String,
    pub field: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

/// One edit in a `gearbox/product/applyEdits` batch.
///
/// Applied in order against the same file text, so a draft of several fields
/// becomes one dry-run, one confirmation, and one write.
///
/// **`AddGear` is here so that adding a gear *and* configuring it is one batch.**
/// `gearbox/product/addGear` still exists and still does one thing; what could
/// not be expressed before was the Add Gear panel's actual proposal, which is a
/// gear plus the features, config and plugins staged beside it. Those had to be
/// a second call, because `applyEdits` reads the file and the gear is not in it
/// yet -- so the panel's "What will be written" could only ever show the
/// `use_gear` line, and the commit wrote twice with a window in between where
/// the description named a gear nobody had configured.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProductEdit {
    /// Insert one `use_gear(...)` entry. Idempotent: naming a gear the
    /// description already has is `Unchanged`, as `add_gear` has always been.
    AddGear {
        gear: String,
        source: String,
    },
    /// Remove every `use_gear` naming this gear.
    RemoveGear {
        gear: String,
    },
    /// Declare a directory as a source the product reads gears from.
    ///
    /// Here for one flow: a gear scaffolded from inside a product lands outside
    /// every declared source, because `writable_out_root` refuses to write into
    /// one -- so "create a gear and add it to this product" is two edits that
    /// have to be one batch, or the description spends a moment naming a gear
    /// from a source it does not have.
    AddSource {
        id: String,
        at: String,
    },
    /// Remove a `source(...)` no gear reads from any more.
    ///
    /// The inverse of `AddSource`, offered when removing a gear leaves the source
    /// it came from unused. The engine refuses a source a `use_gear` still names.
    RemoveSource {
        id: String,
    },
    SetConfig {
        gear: String,
        key: String,
        /// `None` removes the key. A scalar, because a control writes scalars
        /// and a nested literal has no control to render it.
        #[serde(default)]
        value: Option<ConfigValue>,
    },
    SetFeatures {
        gear: String,
        features: Vec<String>,
    },
    /// Set, change or remove one option on a cluster `provider(...)`.
    ///
    /// **Addressed by where it is written**, like a plugin connection: a product
    /// may hold two `cluster_profile(...)` entries under one name for disjoint
    /// deployment profiles, so the name alone picks one of two. `entry_index` is
    /// the written position, and `scope` is checked against what is found there
    /// -- an address computed against text that has since changed is refused
    /// rather than applied to whatever now sits at that position.
    SetProviderOption {
        scope: String,
        /// The written position of the `cluster_profile(...)` entry.
        entry_index: usize,
        /// `cache`, `leader_election` or `lock`.
        primitive: String,
        key: String,
        /// `None` removes the option.
        #[serde(default)]
        value: Option<ConfigValue>,
    },
    /// Attach one plugin to a host gear, leaving its other plugins alone.
    ///
    /// **Not `SetPlugins` with one more entry, and the difference is data.**
    /// `set_gear_plugins` rewrites the list as bare `plugin("id")` entries -- its
    /// own documentation says profiles and per-plugin config stay manual -- so
    /// using it to attach a plugin to `payments-demo`'s `authn-resolver` drops
    /// `profiles` and `config` from the two entries already there. A visual
    /// authoring tool cannot own an edit that destroys what it did not write, so
    /// attaching is its own operation and appends.
    AddPlugin {
        gear: String,
        plugin: String,
    },
    AddPluginSelection {
        gear: String,
        plugin: String,
        profiles: Vec<String>,
    },
    RemovePlugin {
        target: PluginTarget,
    },
    SetPluginConfig {
        target: PluginTarget,
        key: String,
        value: Option<ConfigValue>,
    },
    SetPluginProfiles {
        target: PluginTarget,
        profiles: Vec<String>,
    },
    SetPlugins {
        gear: String,
        plugins: Vec<String>,
    },
    SetProfileField {
        profile: String,
        field: String,
        #[serde(default)]
        value: Option<String>,
    },
}

/// One `plugin(...)` entry, addressed inside its host.
///
/// **A position, deliberately, and not a key.** GDL gives a `plugin(...)` call
/// no identity of its own, so two entries naming the same implementation for
/// disjoint profiles are distinguishable only by where they are written. The
/// pair `(gear, entry_index)` is that address, and `plugin` is carried beside it
/// as a cheap assertion about what is expected to be found there.
///
/// **`entry_index` counts written entries, not evaluated ones.** It is
/// [`PluginSelection::entry_index`](gearbox_ir::PluginSelection), which records
/// the position in the host's `plugins = [...]` list including the entries
/// evaluation dropped. Counting the survivors instead would address the wrong
/// entry whenever a malformed sibling existed.
///
/// The document this position refers to is pinned once for the whole batch by
/// [`ApplyEditsParams::expected_before`], not per target: one snapshot cannot
/// disagree with itself.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PluginTarget {
    pub gear: String,
    pub plugin: String,
    pub entry_index: usize,
}

/// `gearbox/product/applyEdits` -- several description edits in one pass.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ApplyEditsParams {
    /// The document the client previewed against, refused if it no longer matches.
    ///
    /// The one staleness guard for the batch. Every `PluginTarget` in `edits`
    /// addresses an entry by position, and a position only means something
    /// against a known text.
    #[serde(default)]
    pub expected_before: Option<String>,
    pub path: String,
    #[serde(default)]
    pub dry_run: bool,
    pub edits: Vec<ProductEdit>,
}

/// A source entry written into a new product description.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CreateSourceEntry {
    pub id: String,
    pub at: String,
}

/// `gearbox/product/create` -- new file from a template or a clone.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CreateProductParams {
    pub path: String,
    pub id: String,
    pub name: String,
    #[serde(default = "default_product_version")]
    pub version: String,
    #[serde(default)]
    pub sources: Vec<CreateSourceEntry>,
    #[serde(default = "default_profile_kind")]
    pub profile_kind: String,
    #[serde(default = "default_profile_id")]
    pub profile_id: String,
    #[serde(default)]
    pub clone_from: Option<String>,
    /// Re-base the clone's relative paths onto its new folder.
    ///
    /// **Asked for, not inferred, because only the client knows what the source
    /// file's folder means.** A local clone reads a description where it lives,
    /// so its `path("../..")` means something from there and re-basing keeps it.
    /// A git clone reads a temporary checkout whose location means nothing: its
    /// paths were written for the repository's own layout, and re-basing them
    /// from the checkout named directories that do not exist. Off by default, so
    /// a client that does not know the field keeps the behaviour it had.
    #[serde(default)]
    pub rebase_relative_paths: bool,
    #[serde(default)]
    pub dry_run: bool,
}

/// What `gearbox/gear/scaffold` answers.
///
/// `GeneratePlanResult` plus the description's own text, and the addition is what
/// makes the shape choice visible: the three files and their paths are identical
/// for all three kinds, so a preview of paths alone showed `Service` and `Plugin`
/// as the same answer -- which a UX pass duly read as the choice doing nothing.
///
/// One field, not a payload per file. `FilePlan` is a preview line and stays one;
/// `gear.gdl` is the only file whose *content* is the decision being previewed,
/// and the dry run has already built and evaluated it, so carrying it costs
/// nothing.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ScaffoldGearResult {
    #[serde(flatten)]
    pub plan: GeneratePlanResult,
    /// The `gear.gdl` this scaffold would write.
    pub gear_gdl: String,
}

/// What kind of gear is being scaffolded.
///
/// **Three shapes, and the corpus is what decided there are three.** Of the
/// fourteen described gears, seven are plugins -- they fill an extension point
/// declared by an SDK crate -- and the rest are gears that do something on their
/// own. A scaffold that ignored that difference wrote the same file for both and
/// left a plugin author to find out what else a plugin needs.
///
/// What differs is **which declarations `gear.gdl` offers**. The Rust skeleton
/// is the same `#[toolkit::gear]` struct for every kind when the engine finds the
/// toolkit in its roots (a plugin's trait `impl` offered commented), and the
/// previous comment-only form when it does not. In the description the
/// difference is the next declaration each shape needs, written where it
/// goes -- and for a plugin with no host chosen, written as a *comment*,
/// because an `implements` naming a spec no described gear declares is refused
/// (GBX0519). A scaffold must not produce a description that is already wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum GearKind {
    /// A crate and a name. What this method has always written.
    #[default]
    Minimal,
    /// A gear that does something on its own: the declared fields a service
    /// carries, and the configuration hint.
    Service,
    /// A gear that implements another gear's extension point.
    Plugin,
}

/// The point a scaffolded plugin implements, and the crate its trait lives in.
///
/// Chosen from a host gear the catalogue has already loaded, so every field
/// here is something the engine told the client earlier.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PluginScaffold {
    /// The spec's own GTS segment, e.g. `cf.core.authn_resolver.plugin.v1~`.
    /// Written as `implements = "..."`: the declaration that makes this a plugin.
    pub spec: String,
    /// The trait the plugin implements, e.g. `AuthNResolverPluginClient`.
    pub trait_ident: String,
    /// The package name of the crate that trait lives in, e.g.
    /// `cf-gears-authn-resolver-sdk` -- a dependency the plugin's Cargo.toml needs.
    pub crate_name: String,
    /// Its library identifier, e.g. `authn_resolver_sdk`. Never derived from the
    /// package name -- a crate with an explicit `[lib]` differs.
    pub lib_ident: String,
    /// Where that crate lives, relative to the gear being scaffolded.
    pub path: String,
}

/// `gearbox/gear/scaffold` -- tier-0 gear crate under a writable destination.
///
/// Writes `{destination_dir}/{id}/gear.gdl`, `Cargo.toml`, and `src/lib.rs`.
/// Ownership is `GeneratedOnce`: refuse when the gear directory already exists.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ScaffoldGearParams {
    pub id: String,
    pub name: String,
    #[serde(default = "default_product_version")]
    pub version: String,
    /// Which shape to write.
    ///
    /// Absent means [`GearKind::Minimal`], which is what this method wrote
    /// before the field existed -- so an older client keeps its behaviour.
    #[serde(default)]
    pub kind: GearKind,
    /// What this plugin implements, when the kind is [`GearKind::Plugin`].
    ///
    /// **Absent keeps the commented shape, and that shape exists for a reason.**
    /// An `implements` naming a spec no described gear declares is refused (GBX0519),
    /// so with no host chosen a scaffold writes the declaration as a comment
    /// rather than produce a description that is already wrong.
    ///
    /// Present means the client picked a host out of a loaded catalogue, so the
    /// spec is a fact rather than a guess and can be written live. That is
    /// also what makes the kind visible in the preview: it is the same three
    /// files either way, and only the text differs.
    #[serde(default)]
    pub plugin: Option<PluginScaffold>,
    /// Parent directory; the gear lands in `{destination_dir}/{id}/`.
    pub destination_dir: String,
    /// Preview only, like every other write method on this protocol.
    ///
    /// One spelling, `dry_run`. An alias for `plan_only` used to sit here "for
    /// clients that prefer that name" -- there is one client, it is generated
    /// from this file, and ts-rs could not parse the alias anyway, so it bought a
    /// build warning and nothing else.
    #[serde(default)]
    pub dry_run: bool,
}

fn default_product_version() -> String {
    "0.1.0".to_owned()
}

fn default_profile_kind() -> String {
    "embedded".to_owned()
}

fn default_profile_id() -> String {
    "dev".to_owned()
}

/// What an edit would do, or did.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct EditGearResult {
    /// Whether the file needed changing at all. `false` is the idempotent case:
    /// the description already said this.
    pub changed: bool,
    /// Whether the change reached the disk. Always `false` for a dry run.
    pub written: bool,
    /// The file as it is now, for a preview to diff against.
    pub before: String,
    /// The file as it would be, or as it now is. Equal to `before` when
    /// `changed` is false.
    pub after: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

/// The resolved product, plus everything said while producing it.
///
/// `product` is `None` only when the description did not evaluate. A product
/// that resolved *with errors* is present: the UI renders a partial graph and
/// the diagnostics beside it, which is the whole reason errors do not abort
/// resolution.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ResolveResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<ResolvedProduct>,
    /// The explanation graph for this resolution, so "why" needs no second call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<ExplanationGraph>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

/// `gearbox/validate` -- everything checkable without resolving.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ValidateParams {
    /// Also check a product's gear selections. Without it, only the catalogue
    /// is validated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ValidateResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    pub errors: u32,
    pub warnings: u32,
}

/// The response to `gearbox/catalogue/load`.
///
/// Returned at the boundary between the two passes: every description has been
/// evaluated, no crate has been parsed. So `pending` is the whole tree, `gears`
/// is empty, and the rest arrives as `gearbox/catalogueChanged`.
///
/// A client must not read an absent field on a `PendingGear` as an absent fact.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CatalogueLoadResult {
    /// How many descriptions were discovered, so a progress bar has a
    /// denominator immediately.
    pub total: u32,
    pub pending: Vec<PendingGear>,
    /// Gears at design maturity. Complete as declared, so they arrive here
    /// and never as `gearbox/catalogueChanged`: there is nothing to project.
    pub designs: Vec<DesignGear>,
    /// Diagnostics raised while evaluating descriptions. Projection diagnostics
    /// arrive later, with the gears they belong to.
    pub diagnostics: Vec<Diagnostic>,
}

/// Diagnostics raised after the load already answered.
///
/// Everything the second pass produces -- projection failures, manifest
/// mismatches, merge errors -- arrives after the `catalogue/load` response has
/// gone out, so there is no response left to carry it. Without this the client
/// sees a tree that silently omits the gears that failed.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CatalogueDiagnostics {
    /// Only the ones not already sent in the load response.
    pub diagnostics: Vec<Diagnostic>,
}

/// One gear finished projecting.
///
/// Carries the gear alone rather than the catalogue again: a registry of a
/// thousand gears re-sent per completion is the obvious way to make staged
/// loading slower than the blocking load it replaces.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CatalogueChanged {
    /// The gear that moved out of `pending`.
    pub gear: GearDescriptor,
    /// Its `gdl_path`, so the client can drop the matching pending row without
    /// having to know that `gdl_path` was its key.
    ///
    /// The key is `(gear.source, replaces)`, not `replaces` alone: a `gdl_path`
    /// is relative to one source root and the server accepts several, so two
    /// roots of the same shape both hold `gears/x/gear.gdl`.
    pub replaces: String,
}

/// `$/progress`, in the shape the staged load actually produces.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ProgressParams {
    pub token: String,
    pub completed: u32,
    pub total: u32,
    /// Set once, on the last notification, so a client can retire the indicator
    /// without comparing counters.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub done: bool,
}

/// Sent when the load finishes, carrying what only the end knows.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct LogParams {
    pub message: String,
}

/// `gearbox/generate/plan` and `gearbox/generate/apply`.
///
/// The same envelope for both, because they differ only in whether anything is
/// written. `out` is the CLI's `--out`: a test (and a Studio run that must not
/// collide with a developer's tree) can send the artefacts to a separate root.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct GenerateParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Absolute output root. Omitted, the server uses
    /// `<workspace>/.gearbox/<product>/<profile>/`, the same layout as the CLI.
    ///
    /// Outside the workspace it is accepted when the folder is new, holds only
    /// dotfiles, or holds a `product.lock` -- a folder chosen for the product,
    /// such as the repository it ships from. That tree keeps its merge base in
    /// `<out>/.gearbox/base/` and is offered a `.gitignore` once. Never inside a
    /// source root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
}

/// One line per file, and nothing else: `FilePlan` is a preview line, not a
/// payload. File contents arrive on `gearbox/generate/file`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct GeneratePlanResult {
    pub plans: Vec<FilePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    pub out_root: String,
    /// Template keys whose builtin this product replaced.
    ///
    /// An unexpected chart or Dockerfile must have a visible cause. The CLI has
    /// always printed this line; the RPC path computed it and threw it away, so
    /// Studio showed a plan with no way to tell a house template from a builtin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overridden_templates: Vec<String>,
}

/// What an apply did.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct GenerateApplyResult {
    pub plans: Vec<FilePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    pub written: u32,
    /// Template keys whose builtin this product replaced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overridden_templates: Vec<String>,
}

/// `gearbox/generate/file` -- the two sides of one planned file.
///
/// Re-runs generation and picks one entry. Stateless on purpose: a cached plan
/// the client later applies would need a staleness check, and we do not have
/// one yet. `Cargo.lock` is ~100k; putting every file on the plan would make
/// the preview the expensive call.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct GenerateFileParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    /// Path relative to `out_root`, as `FilePlan.path` spelled it.
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct GenerateFileResult {
    /// The bytes generation proposes, as text. Absent when they are not UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed: Option<String>,
    /// What is on disk today. Absent when the file does not exist or is not
    /// UTF-8 -- the same distinction `preview_available` makes on the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    pub action: FileAction,
    pub ownership: Ownership,
}
