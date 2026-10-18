//! Turning a `product.lock` into files.
//!
//! [`generate`] is a pure function from a [`ResolvedProduct`] to a [`FileSet`]:
//! same lock in, same bytes out, no clock, no environment, no filesystem. That
//! is what makes `--dry-run` trustworthy -- the preview is produced by the same
//! code that produces the artefact, not by a second implementation that
//! describes it.
//!
//! **Two functions here touch the filesystem, and neither is reachable from
//! `generate`.** [`apply`] writes, which is its whole job.
//! [`templates::TemplateSet::load_for_product`] reads a product's template
//! overlay, and it is called by the CLI and the RPC *before* generation, so the
//! overlay arrives as data in [`GenerateInput`]. Reading it inside would mean a
//! `--dry-run` answering about a tree it had gone and looked at, which is the
//! one thing the preview must not do.
//!
//! **Why this lives in `gearbox-engine` and not in a `gearbox-gen` crate.** The
//! plan names a separate crate. Two things argue against it here, and the second
//! is decisive. First, precedent: this repository has twice folded a planned
//! crate into an existing one when the boundary bought nothing -- `gearbox-verify`
//! became `gearbox-project`, `gearbox-resolve` became `gearbox-engine::resolve`.
//! Second, [`apply`] must live in `gearbox-engine` regardless, because that is
//! the crate permitted to touch the filesystem; a separate crate would therefore
//! split the writer from the thing it writes, and every generator would be one
//! `use` away from a caller that forgot which half it was holding. Purity is a
//! property of these functions, and it is asserted by testing them from literals
//! rather than by a manifest that cannot see inside them.
//!
//! **What is here and what is not.** Every process in the lock is generated,
//! host or worker. Dockerfiles and a Helm chart are emitted for a Kubernetes
//! profile. There used to be a `skipped` list
//! naming the workers this could not produce, on the argument that a generator
//! emitting four files out of six and saying nothing is indistinguishable from
//! a finished one. That argument was right and the list is gone anyway: the
//! dispatch on [`ApplicationKind`] is exhaustive, so a kind this cannot generate is
//! now a compile error at the `match` rather than a value at run time. The list
//! should come back the moment something can genuinely be skipped, with a
//! producer -- an always-empty field is a report nobody can ever read.

mod apply;
mod config;
mod docker;
mod helm;
mod json;
mod manifest;
mod merge3;
mod paths;
mod rust;
mod templates;
mod ui;
mod workspace;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gearbox_ir::{
    ApplicationKind, Catalogue, Diagnostic, DiagnosticCode, Diagnostics, FileSet, ResolvedProduct,
    SourceId,
};

pub use apply::{
    ApplyOutcome, OUTPUT_DIR, apply_generate, base_root_for, default_out_root, plan,
    standalone_base_root, summarize,
};
pub use templates::TemplateSet;
// Re-exported for the one caller outside generation that needs the same
// arithmetic: a cloned description re-bases its relative paths, and a second
// implementation of "relative, or absolute when there is none" would be a
// second place to get the fallback wrong.
pub use manifest::{TOOLKIT_ALIAS, TOOLKIT_PACKAGE, TOOLKIT_SUBDIR, locate_toolkit};
pub use paths::{normalize, relative, to_slash};

/// Numeric uid the image and the chart agree on.
///
/// Distroless/nonroot convention. The Dockerfile `USER`s this and the
/// chart's `runAsUser` / `fsGroup` match it, so a volume the pod mounts
/// is writable by the process that runs.
pub(crate) const NONROOT_UID: u32 = 65532;

/// Where a Kubernetes process keeps `server.home_dir`.
///
/// `~` is unwritable under `readOnlyRootFilesystem`; the chart mounts an
/// emptyDir at this path.
pub(crate) const K8S_HOME_DIR: &str = "/var/lib/gearbox";

/// Why generation could not produce a usable tree.
///
/// Every variant is a condition under which the output would not compile or
/// would not be readable, so none of them is recoverable by carrying on: a
/// half-correct crate whose `registered_gears.rs` names a dependency the
/// manifest does not declare wastes a two-minute `cargo build` to say what this
/// says immediately.
#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error("GBX0720: invalid UI composition: {0}")]
    Ui(String),
    #[error("invalid database configuration: {0}")]
    Database(String),
    #[error("gear `{gear}` is in application `{application}` but not in the lock's gear table")]
    UnknownGear { application: String, gear: String },

    #[error("gear `{gear}` names source `{id}`, which the generator was not given a path for")]
    UnknownSource { gear: String, id: String },

    #[error(
        "gear `{gear}` links `{ident}`, whose crate root `{root}` is not a library identifier the \
         application depends on; the generated `registered_gears.rs` would not compile"
    )]
    UnlinkableIdent {
        gear: String,
        ident: String,
        root: String,
    },

    #[error(
        "`{first}` and `{second}` both link as `{ident}`, so one of them would be dropped from \
         the generated manifest"
    )]
    LibIdentCollision {
        ident: String,
        first: String,
        second: String,
    },

    #[error("`{path}` is not a usable relative path inside the output root")]
    BadPath { path: String },

    #[error("two generators both claim `{path}`")]
    DuplicatePath { path: String },

    #[error("could not render the {what} template")]
    Template {
        what: &'static str,
        #[source]
        source: Box<minijinja::Error>,
    },

    #[error(
        "no template named `{key}`; the override contract is the path under templates/ without .jinja"
    )]
    UnknownTemplate { key: String },

    #[error(
        "the product declares `templates = path(\"{declared}\")`, but `{at}` is not a directory"
    )]
    MissingTemplateDir { declared: String, at: String },

    #[error(
        "the output tree `{out_root}` and the source roots share no relative path, so there is no docker build context a COPY could name"
    )]
    UnreachableDockerContext { out_root: String },

    #[error("could not serialize {what}")]
    Toml {
        what: &'static str,
        #[source]
        source: toml::ser::Error,
    },

    #[error("could not serialize {what} as YAML")]
    Yaml {
        what: &'static str,
        #[source]
        source: serde_saphyr::ser::Error,
    },

    #[error("could not serialize {what} as JSON")]
    Json {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("could not render the lock")]
    Lock(#[from] gearbox_lock::LockError),

    #[error("generated Helm would interpolate `{value}` as template text in {at}")]
    UnsafeHelm { at: &'static str, value: String },

    #[error("config key `{key}` cannot nest under an existing non-object value")]
    ConfigShape { key: String },

    #[error("{what} `{}`", .path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Everything generation needs, and nothing it does not.
///
/// The source roots are absolute and canonical: the generated manifests carry
/// **relative** path dependencies into `gears-rust`, and a relative path can
/// only be computed between two absolute ones. Passing them in rather than
/// reading `ResolvedSource::location` is deliberate -- that field records the
/// location as the operator typed it on the command line, which is relative to
/// whatever directory they were standing in.
pub struct GenerateInput<'a> {
    pub lock: &'a ResolvedProduct,

    /// Absolute, canonicalized root directory per source id.
    pub source_roots: &'a BTreeMap<SourceId, PathBuf>,

    /// Absolute output root, conventionally `.gearbox/<product>/<profile>/`.
    pub out_root: &'a Path,

    /// Template sources, builtins plus any product overlay.
    ///
    /// Loaded by the caller: this function must not consult the filesystem,
    /// and a `--dry-run` that read `templates/` itself would be answering
    /// about a different tree than the one `apply` writes from.
    pub templates: TemplateSet,

    /// The directory holding `product.gdl`, absolute.
    ///
    /// The one base a description-relative path can be resolved against, and the
    /// generator is the only place that has both it and `out_root`. The lock
    /// keeps `target_dir` as the description spelled it -- putting a
    /// machine-specific absolute path in a committed file would be worse than
    /// the bug this fixes.
    ///
    /// Absent when the caller has no description on disk, which is every test
    /// that builds a lock by hand; a shared target directory is then simply not
    /// expressible and the tree uses Cargo's default.
    pub product_dir: Option<&'a Path>,

    /// Catalogue used to honour `ConfigField.secret` at generate time.
    ///
    /// Absent in tests that do not exercise secret fields. Callers that have
    /// already loaded a catalogue (CLI, RPC) pass it so a literal credential
    /// cannot land in a `ConfigMap` or image.
    pub catalogue: Option<&'a Catalogue>,

    /// Crates the build takes from a registry rather than the checkout, decided
    /// by the caller with `gearbox_engine::published::plan`. `None` is a path
    /// dependency on every crate, which is what a product without
    /// `crates = registry(...)` on any source gets.
    pub registry: Option<&'a crate::published::RegistryPlan>,
}

impl GenerateInput<'_> {
    /// The directory the application crates go under, as the lock records it.
    ///
    /// One accessor rather than six call sites reaching into the header: the
    /// segment appears in the crate path, the workspace members and the plan
    /// the Studio renders, and two of those agreeing while the third does not
    /// is precisely the unbuildable tree this exists to prevent.
    #[must_use]
    pub fn layout(&self) -> &str {
        &self.lock.product.layout
    }
}

/// What one generation run produced.
pub struct Generated {
    pub files: FileSet,

    /// What generation had to say about the lock it was given.
    ///
    /// Empty in the ordinary case. **Both callers must merge this into their own
    /// list** -- `overridden_templates` was computed and dropped on the RPC path
    /// for a milestone, which made a house template indistinguishable from a
    /// builtin in Studio. A credential silently replaced would be worse.
    pub diagnostics: Diagnostics,

    /// Template keys the product overlaid, in sorted order.
    ///
    /// Empty in the ordinary case. Reported so an unexpected Dockerfile or
    /// chart has a visible cause rather than looking like a generator change.
    pub overridden_templates: Vec<String>,
}

/// The whole file set for one resolved product.
///
/// # Errors
/// Returns [`GenerateError`] when the lock describes something the generator
/// cannot turn into a compilable tree -- see that type; every variant means the
/// output would be wrong rather than merely incomplete.
pub fn generate(input: &GenerateInput<'_>) -> Result<Generated, GenerateError> {
    input
        .lock
        .validate_database()
        .map_err(GenerateError::Database)?;
    let mut files = FileSet::new();
    let mut diagnostics = Diagnostics::default();

    // Before anything reads the lock, and once. A credential in it is replaced
    // here rather than inside the configuration generator, so the `ConfigMap`,
    // the image and the `product.lock` this run writes cannot disagree about what
    // the value is. `GBX0116` means a lock resolved by this build never has one;
    // a lock from an earlier build can, and `GBX0705` says so out loud.
    let redacted = crate::secrets::redact_product(
        input.lock,
        input.catalogue,
        input.product_dir,
        &mut diagnostics,
    );
    // A fresh input rather than a mutation: `templates` is owned, so the struct
    // cannot be spread over a shared reference. The clone is of a map that is
    // empty for every product without a template overlay.
    let input = &GenerateInput {
        lock: &redacted,
        source_roots: input.source_roots,
        out_root: input.out_root,
        templates: input.templates.clone(),
        product_dir: input.product_dir,
        catalogue: input.catalogue,
        registry: input.registry,
    };

    // Every process is a workspace member, host or worker. A generated crate
    // sitting inside the workspace root without being a member is the failure
    // Cargo reports as "believes it's in a workspace when it's not".
    let applications: Vec<_> = input.lock.applications.iter().collect();

    insert(
        &mut files,
        workspace::workspace_manifest(&applications, input.layout(), input)?,
    )?;
    insert(&mut files, workspace::toolchain()?)?;
    insert(&mut files, workspace::lock_file(input.lock)?)?;
    if let Some(cargo_config) = workspace::cargo_config(input)? {
        insert(&mut files, cargo_config)?;
    }

    for application in applications {
        insert(
            &mut files,
            manifest::application_manifest(input, application)?,
        )?;
        // The entry point is the only file that differs between the two kinds.
        // Everything else -- the manifest, the link file, the configuration --
        // is a function of the process, not of how it is started.
        let entry = match application.kind {
            ApplicationKind::Host => rust::host_main(input, application)?,
            ApplicationKind::Worker => rust::worker_main(input, application)?,
        };
        insert(&mut files, entry)?;
        insert(&mut files, rust::registered_gears(input, application)?)?;
        insert(&mut files, config::app_config(input, application)?)?;
    }

    for entry in ui::files(input)? {
        insert(&mut files, entry)?;
    }
    for entry in docker::files(input)? {
        insert(&mut files, entry)?;
    }
    for entry in helm::files(input, &files)? {
        insert(&mut files, entry)?;
    }

    // One per key rather than one line listing them all: the text output
    // already prints the summary, and a diagnostic is what carries the fact to
    // `--format json` and to every RPC client, which saw nothing at all.
    let overridden_templates = input.templates.overridden();
    for key in &overridden_templates {
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::GenTemplateOverridden,
            format!(
                "template `{key}` came from the product's own overlay rather than from this \
                 build's builtin"
            ),
        ));
    }

    Ok(Generated {
        files,
        diagnostics,
        overridden_templates,
    })
}

/// Add one entry, refusing rather than overwriting.
///
/// Two generators claiming a path is a bug in this module, and the difference
/// between finding it here and finding it in the output tree is the difference
/// between a message and an afternoon.
fn insert(files: &mut FileSet, entry: gearbox_ir::FileEntry) -> Result<(), GenerateError> {
    let path = entry.path.clone();
    if files.insert(entry).is_some() {
        return Err(GenerateError::DuplicatePath {
            path: path.as_str().to_owned(),
        });
    }
    Ok(())
}

/// The header every generated file whose format has comments carries.
///
/// `DO NOT EDIT` is not decoration. `Generated` files are overwritten without
/// asking, so the header is the only warning an operator gets before losing an
/// edit -- and the Go toolchain's convention proves a machine-readable form of
/// it is worth having.
fn header(comment: &str) -> String {
    format!(
        "{comment} GENERATED by gearbox {} -- do not edit. \
         Run `gearbox generate` to regenerate.\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// The header an [`Ownership::OperatorOwned`] file carries instead.
///
/// **Not [`header`], and the difference is the whole point of the class.** That
/// one says "do not edit" because a `Generated` file is overwritten without
/// asking, so the warning is the only protection an editor can offer. An
/// operator-owned file is the opposite: editing it is what it is for, and
/// `cpt-gearbox-fr-preserve-operator-values` promises those edits survive. A
/// file that invites an edit while forbidding one teaches the wrong thing, and
/// an operator who obeys the wrong header defeats the requirement without ever
/// seeing a diagnostic.
fn operator_header(comment: &str, generated_twin: &str) -> String {
    format!(
        "{comment} Yours to edit. `gearbox generate` merges its changes into this file and keeps\n\
         {comment} yours; an overlap it cannot reconcile is reported as GBX0701 and leaves the\n\
         {comment} file untouched. See `{generated_twin}` for what the lock decided, verbatim.\n"
    )
}
