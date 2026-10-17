//! The `gearbox` command-line interface.
//!
//! A thin shell over `gearbox-engine`: parse arguments, call the engine, print
//! the result. It holds no product semantics of its own, which is what lets the
//! same engine back the RPC server and the editor without either becoming the
//! authority.
//!
//! One rule shapes the output: **structured output goes to stdout, everything
//! else to stderr.** `gearbox rpc --stdio` will use stdout as its JSON-RPC
//! channel, and a stray progress line there would corrupt the protocol. Getting
//! into that habit now costs nothing and avoids a class of bug later.

mod generate;
mod lock;
mod pipeline;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand, ValueEnum};
use gearbox_engine::{SourceRoot, check_plugins, load_catalogue, load_product};
use gearbox_ir::{Diagnostic, GearDescriptor, Severity, SourceId};

#[derive(Parser)]
#[command(
    name = "gearbox",
    about = "Compose, resolve and generate Gears products",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan source roots for `gear.gdl` files and print the catalogue.
    Catalogue {
        /// A source root to scan. Repeatable; each is scanned in order.
        #[arg(long, value_name = "DIR", required = true)]
        root: Vec<PathBuf>,

        /// The id to record for the source. Defaults to the root's directory name.
        #[arg(long, value_name = "ID")]
        source_id: Option<String>,

        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },

    /// Serve JSON-RPC over stdio, for the Studio and the `.gdl` language client.
    ///
    /// **Nothing but JSON-RPC goes to stdout while this runs.** Logs go to
    /// stderr and to `gearbox/log` notifications.
    Rpc {
        /// Required for symmetry with LSP servers, which are all started this
        /// way; there is no other transport, so it carries no choice.
        #[arg(long)]
        stdio: bool,

        /// A source root to scan, used when `initialize` names none. Repeatable.
        #[arg(long, value_name = "DIR")]
        root: Vec<PathBuf>,
    },

    /// Show plugin extension points and the implementations available for them.
    ///
    /// Without `--product`, lists what a gear *could* use. With `--product`,
    /// resolves what it *will* use, per deployment profile -- which is a
    /// different question, because the choice is profile-scoped.
    Plugins {
        /// A source root to scan. Repeatable.
        #[arg(long, value_name = "DIR", required = true)]
        root: Vec<PathBuf>,

        /// Limit to one host gear.
        #[arg(long, value_name = "ID")]
        gear: Option<String>,

        /// Resolve against a product description instead of listing.
        #[arg(long, value_name = "FILE")]
        product: Option<PathBuf>,
    },

    /// Resolve a product for one deployment profile and print the lock.
    ///
    /// The mode of every contract binding is *derived* from where the gears end
    /// up, never declared, so the same description gives a different lock per
    /// profile. Nothing is written to disk: this prints, and `--format toml`
    /// prints exactly what a lock file would contain.
    Resolve {
        /// A source root to scan. Repeatable.
        #[arg(long, value_name = "DIR", required = true)]
        root: Vec<PathBuf>,

        /// The id to record for the source. Defaults to the root's directory name.
        #[arg(long, value_name = "ID")]
        source_id: Option<String>,

        /// The product description to resolve.
        #[arg(long, value_name = "FILE")]
        product: PathBuf,

        /// Which deployment profile. Defaults to the product's own default.
        #[arg(long, value_name = "ID")]
        profile: Option<String>,

        #[arg(long, value_enum, default_value_t = ResolveFormat::Text)]
        format: ResolveFormat,
    },

    /// Check everything that can be checked without resolving.
    ///
    /// Runs the catalogue load and reports its diagnostics with an exit code.
    /// With `--product`, also joins the product's selected gears against the
    /// catalogue -- the one check that needs both halves but not the resolver.
    ///
    /// Exits non-zero when any diagnostic is an error.
    Validate {
        /// A source root to scan. Repeatable; each is scanned in order.
        #[arg(long, value_name = "DIR", required = true)]
        root: Vec<PathBuf>,

        /// The id to record for the source. Defaults to the root's directory name.
        #[arg(long, value_name = "ID")]
        source_id: Option<String>,

        /// Also check a product description's gear selections.
        #[arg(long, value_name = "FILE")]
        product: Option<PathBuf>,

        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },

    /// Evaluate a `product.gdl` and print the operator intent it declares.
    ///
    /// Evaluation only: no catalogue is read, so this reports what the file says
    /// and whether it is internally consistent, not whether the gears it names
    /// exist. That is `gearbox resolve`'s question.
    Product {
        /// Path to the product description.
        #[arg(long, value_name = "FILE")]
        file: PathBuf,

        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },

    /// Resolve a product and write its artefacts under `.gearbox/`.
    ///
    /// Resolves rather than reading an existing lock, so a preview can never be
    /// answering about a stale one. Composition output is never written into a
    /// source root: everything lands under `.gearbox/<product>/<profile>/`,
    /// including the `product.lock` the tree was generated from.
    Generate {
        /// A source root to scan. Repeatable.
        #[arg(long, value_name = "DIR", required = true)]
        root: Vec<PathBuf>,

        /// The id to record for the source. Defaults to the root's directory name.
        #[arg(long, value_name = "ID")]
        source_id: Option<String>,

        /// The product description to generate from.
        #[arg(long, value_name = "FILE")]
        product: PathBuf,

        /// Which deployment profile. Defaults to the product's own default.
        #[arg(long, value_name = "ID")]
        profile: Option<String>,

        /// Where to write. Defaults to `.gearbox/<product>/<profile>/`.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,

        /// Report what would be written and write nothing
        /// (`cpt-gearbox-fr-generate-preview`).
        #[arg(long)]
        dry_run: bool,

        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },

    /// Ask questions of a written `product.lock`.
    Lock {
        #[command(subcommand)]
        query: lock::LockQuery,
    },
}

/// What `resolve` prints.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum ResolveFormat {
    /// The canonical lock, byte-for-byte what would be written to disk.
    Toml,
    /// Machine-readable. The stable contract for tooling.
    Json,
    /// Human-readable summary.
    Text,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub(crate) enum Format {
    /// Machine-readable. The stable contract for tooling.
    Json,
    /// Human-readable summary.
    Text,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Command::Catalogue {
            root,
            source_id,
            format,
        } => catalogue(&root, source_id.as_deref(), format),
        Command::Resolve {
            root,
            source_id,
            product: product_file,
            profile,
            format,
        } => resolve_product(
            &root,
            source_id.as_deref(),
            &product_file,
            profile.as_deref(),
            format,
        ),
        Command::Validate {
            root,
            source_id,
            product: product_file,
            format,
        } => validate(&root, source_id.as_deref(), product_file.as_deref(), format),
        Command::Product { file, format } => product(&file, format),
        Command::Rpc { stdio, root } => {
            if !stdio {
                anyhow::bail!("only `--stdio` is supported");
            }
            gearbox_rpc::serve_stdio(&root)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Plugins {
            root,
            gear,
            product,
        } => plugins(&root, gear.as_deref(), product.as_deref()),
        Command::Generate {
            root,
            source_id,
            product,
            profile,
            out,
            dry_run,
            format,
        } => generate::run(
            &root,
            source_id.as_deref(),
            &product,
            profile.as_deref(),
            out.as_deref(),
            dry_run,
            format,
        ),
        Command::Lock { query } => lock::run(&query),
    }
}

fn plugins(
    roots: &[PathBuf],
    gear: Option<&str>,
    product_file: Option<&std::path::Path>,
) -> anyhow::Result<ExitCode> {
    let opened = open_roots(roots, None)?;
    let scan = load_catalogue(&opened);

    // The catalogue's own diagnostics used to be dropped on the floor here, so
    // `plugins` listed extension points out of a half-loaded catalogue and
    // exited 0. `catalogue` and `validate` both report them; there is no reason
    // this command should not.
    if let Some(code) = refuse_catalogue_errors(&scan.catalogue) {
        return Ok(code);
    }

    if let Some(file) = product_file {
        return resolve_plugins(&scan.catalogue, file, gear);
    }
    if list_plugins(&scan.catalogue, gear) {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Which gears fill each extension point, in one pass over the catalogue.
///
/// `Catalogue::implementations_of` scans every gear and allocates, so calling it
/// per extension point inside the per-host loop cost one full scan per point --
/// quadratic in catalogue size for a command whose answer is a single join.
fn implementations_by_point(
    catalogue: &gearbox_ir::Catalogue,
) -> BTreeMap<&str, Vec<&GearDescriptor>> {
    let mut by_point: BTreeMap<&str, Vec<&GearDescriptor>> = BTreeMap::new();
    for gear in catalogue.gears.values() {
        if let Some(fill) = gear.implements.as_ref() {
            by_point.entry(fill.spec.as_str()).or_default().push(gear);
        }
    }
    by_point
}

/// What each host *could* use. The answer to "which implementations exist".
fn list_plugins(catalogue: &gearbox_ir::Catalogue, only: Option<&str>) -> bool {
    let by_point = implementations_by_point(catalogue);
    let mut any = false;
    for host in catalogue.gears.values() {
        if host.extension_points.is_empty() {
            continue;
        }
        if only.is_some_and(|id| host.id.as_str() != id) {
            continue;
        }
        any = true;

        let selector = host.vendor_selector.as_deref().unwrap_or("<none>");
        println!("\n{}   selector: vendor = \"{selector}\"", host.id);

        for point in &host.extension_points {
            println!("\n  extension point  {}", point.qualified());
            let Some(impls) = by_point.get(point.spec.as_str()) else {
                println!("    (no implementation in the catalogue)");
                continue;
            };
            for gear in impls {
                let fill = gear.implements.as_ref();
                let vendor = fill
                    .and_then(|f| f.default_vendor.as_deref())
                    .unwrap_or("<none>");
                let priority = fill
                    .and_then(|f| f.default_priority)
                    .map_or_else(|| "-".to_owned(), |p| p.to_string());
                // Asked of the engine, not decided here. `check_plugins` owns
                // vendor selection -- it is where the `linked, but no vendor
                // match` arm below comes from -- and a second copy of the rule
                // in the CLI could tell an operator the opposite of what the
                // resolver will do. A mismatch is not fatal, since the product
                // can set `vendor` on either side, but it is what silently
                // fails if nobody does.
                let mark = if gearbox_engine::plugin_select::default_vendors_agree(host, gear) {
                    "matches"
                } else {
                    "NEEDS vendor override"
                };
                println!(
                    "    {:<26} vendor={vendor:<20} priority={priority:<6} {mark}",
                    gear.id
                );
                println!("      {} · {}", gear.package.crate_name, gear.gdl_path);
            }
        }
    }

    if !any {
        if let Some(id) = only {
            eprintln!("no gear named `{id}` declares a plugin extension point");
            return false;
        }
        println!("no gear declares a plugin extension point");
        println!("(a host declares `sdk = cargo(...)`; the points are read from that crate)");
    }
    true
}

/// What each host *will* use, per profile.
fn resolve_plugins(
    catalogue: &gearbox_ir::Catalogue,
    file: &std::path::Path,
    only: Option<&str>,
) -> anyhow::Result<ExitCode> {
    // Canonicalized before anything is loaded, and the failure propagated. A
    // relative path went through `file_uri` as `file:///product.gdl` -- an
    // absolute-looking URI pointing at the filesystem root -- so every
    // diagnostic this command reports named a file that does not exist. That is
    // the dead link `resolve_product` refuses outright, and the same mistyped
    // `--product` must not be a hard error there and a wrong answer here.
    let file = &file
        .canonicalize()
        .with_context(|| format!("cannot read product description `{}`", file.display()))?;
    let scan = load_product(file, None);
    // Reported whether or not the product evaluated. A warning that arrives
    // *with* a usable intent used to be dropped, so plugin resolution could exit
    // 0 on a product description the same file's `product` subcommand complains
    // about.
    report(scan.diagnostics.as_slice());
    let product_failed = scan.diagnostics.has_errors();
    let Some(intent) = scan.intent else {
        return Ok(ExitCode::FAILURE);
    };

    let mut diagnostics = gearbox_ir::Diagnostics::new();
    let uri = gearbox_ir::file_uri(file);
    let resolutions = check_plugins(catalogue, &intent, &uri, &mut diagnostics);
    diagnostics.finish();
    let resolutions: Vec<_> = resolutions
        .into_iter()
        .filter(|r| only.is_none_or(|id| r.host.as_str() == id))
        .collect();
    if let Some(id) = only
        && resolutions.is_empty()
    {
        eprintln!("no gear named `{id}` declares a plugin extension point");
        report(diagnostics.as_slice());
        return Ok(ExitCode::FAILURE);
    }

    let mut last: Option<(String, String)> = None;
    for r in &resolutions {
        let key = (r.host.to_string(), r.point.qualified());
        if last.as_ref() != Some(&key) {
            println!("\n{} / {}", key.0, key.1);
            last = Some(key);
        }
        let outcome = match (&r.winner, r.tied, r.linked.len()) {
            (Some(w), _, 1) => w.to_string(),
            (Some(w), _, n) => format!("{w}  (wins over {} other linked)", n - 1),
            (None, true, n) => format!("undefined - {n} tied on priority"),
            (None, _, 0) => "- nothing selected".to_owned(),
            (None, _, _) => "- linked, but no vendor match".to_owned(),
        };
        println!("  {:<8} -> {outcome}", r.profile.as_str());
    }

    if resolutions.is_empty() {
        println!("no selected gear declares a plugin extension point");
    }

    report(diagnostics.as_slice());
    Ok(if product_failed || diagnostics.has_errors() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn product(file: &std::path::Path, format: Format) -> anyhow::Result<ExitCode> {
    let scan = load_product(file, None);

    if let Some(intent) = &scan.intent {
        match format {
            Format::Json => println!("{}", serde_json::to_string_pretty(intent)?),
            Format::Text => print_intent(intent),
        }
    }

    report(scan.diagnostics.as_slice());

    Ok(if scan.diagnostics.has_errors() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn print_intent(intent: &gearbox_ir::ProductIntent) {
    println!(
        "{} {} ({}), default profile `{}`",
        intent.id, intent.version, intent.display_name, intent.default_profile
    );

    println!("\n  sources");
    for (id, source) in &intent.sources {
        let pinned = if source.is_immutable() {
            ""
        } else {
            "  [not immutable: repeatable, not reproducible]"
        };
        println!("    {id}: {}{pinned}", describe_source(source));
    }

    println!("\n  profiles");
    for (id, profile) in &intent.profiles {
        let default = if *id == intent.default_profile {
            " (default)"
        } else {
            ""
        };
        println!("    {id}: {}{default}", profile.kind());
    }

    println!("\n  gears");
    for selection in &intent.selected_gears {
        println!("    {} from {}", selection.gear, selection.source);
        // Which extension point each implements is a catalogue fact, so it is not
        // shown here: this command evaluates the product alone. `gearbox
        // plugins --product` resolves them against the catalogue.
        for plugin in &selection.plugins {
            let scope = if plugin.profiles.is_empty() {
                "all profiles".to_owned()
            } else {
                plugin
                    .profiles
                    .iter()
                    .map(gearbox_ir::ProfileId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            println!("      plugin {} [{scope}]", plugin.gear);
        }
    }

    // Profile-scoped declarations are printed per profile, because that is the
    // only way to see what a given `--profile` will actually resolve.
    for id in intent.profiles.keys() {
        let bindings = intent.bindings_for(id);
        let scopes = intent.cluster_scopes_for(id);
        let pins = intent.application_pins_for(id);
        if bindings.is_empty() && scopes.is_empty() && pins.is_empty() {
            continue;
        }
        println!("\n  profile `{id}`");
        for binding in bindings {
            let transport = binding
                .transport
                .map(|t| format!(" over {t}"))
                .unwrap_or_default();
            println!(
                "    bind {} -> {} as {}{transport}",
                binding.consumer,
                binding.contract,
                describe_mode(binding.mode)
            );
        }
        for scope in scopes {
            println!(
                "    cluster `{}` cache = {}{}",
                scope.scope,
                scope.cache.provider,
                if scope.cache.options.is_empty() {
                    String::new()
                } else {
                    format!(" ({} option(s))", scope.cache.options.len())
                }
            );
        }
        for pin in pins {
            println!(
                "    application `{}` anchored on {} x{}",
                pin.name, pin.anchor, pin.replicas
            );
        }
    }

    if !intent.preferences.is_empty() {
        println!("\n  preferences");
        for preference in &intent.preferences {
            println!("    {}", describe_preference(preference));
        }
    }
}

fn catalogue(
    roots: &[PathBuf],
    source_id: Option<&str>,
    format: Format,
) -> anyhow::Result<ExitCode> {
    let opened = open_source_roots(roots, source_id)?;

    let scan = load_catalogue(&opened);

    match format {
        Format::Json => {
            // stdout: the machine-readable contract.
            println!("{}", serde_json::to_string_pretty(&scan.catalogue)?);
        }
        Format::Text => print_summary(&scan),
    }

    // Diagnostics always go to stderr, so `| jq` works regardless.
    report(scan.catalogue.diagnostics.as_slice());

    Ok(if scan.catalogue.diagnostics.has_errors() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// A source id derived from the root's directory name.
///
/// Falls back to `local` when the path has no usable final component (`/`, or a
/// path ending in `..`), which is rare but should not be a hard error.
/// Open the roots a subcommand was given.
///
/// Shared with `catalogue` because opening them differently would make the two
/// commands disagree about what they are looking at.
pub(crate) fn open_roots(
    roots: &[PathBuf],
    source_id: Option<&str>,
) -> anyhow::Result<Vec<SourceRoot>> {
    open_source_roots(roots, source_id)
}

/// Open every root, naming the ones that were not named explicitly.
///
/// One function because there were two copies of this loop and they had the same
/// two faults. `--source-id` with several roots gave every one of them the same
/// id, and without it the directory-name default gave the same id to any two
/// roots whose directories happen to share a name -- and an id is an identity, so
/// either way the second root's gears quietly replaced the first's. Refusing the
/// first case and disambiguating the second are different answers because the
/// two are different mistakes: one is a person saying something impossible, the
/// other is a layout that is perfectly reasonable.
fn open_source_roots(
    roots: &[PathBuf],
    source_id: Option<&str>,
) -> anyhow::Result<Vec<SourceRoot>> {
    if let Some(id) = source_id {
        anyhow::ensure!(
            roots.len() <= 1,
            "`--source-id {id}` names one source, but {} roots were given; \
             drop it and each root is named after its own directory",
            roots.len(),
        );
        // The directory-name default is what a reader would guess; an explicit
        // `--source-id` overrides it, and for a single root there is nothing to
        // disambiguate against.
        let Some(path) = roots.first() else {
            return Ok(Vec::new());
        };
        return Ok(vec![SourceRoot::open(SourceId::new(id)?, path)?]);
    }
    // Named as a set, because the rule that keeps them distinct cannot be
    // applied one path at a time.
    roots
        .iter()
        .zip(gearbox_engine::default_source_ids(roots))
        .map(|(path, id)| Ok(SourceRoot::open(SourceId::new(id)?, path)?))
        .collect()
}

fn resolve_product(
    roots: &[PathBuf],
    source_id: Option<&str>,
    product_file: &std::path::Path,
    profile: Option<&str>,
    format: ResolveFormat,
) -> anyhow::Result<ExitCode> {
    let outcome = match pipeline::resolve(roots, source_id, product_file, profile)? {
        pipeline::Outcome::Refused(code) => return Ok(code),
        pipeline::Outcome::Resolved(resolved) => resolved,
    };
    let pipeline::Resolution {
        lock: resolved,
        mut diagnostics,
        scan,
        product_file,
        ..
    } = *outcome;

    // Redaction has to cover every path that renders a lock, not just the one
    // that writes it. `generate` and the RPC `lock` handler both go through
    // `redact_product`; without it here, `resolve --format toml|json` prints a
    // literal credential from `gears[].config` straight to stdout, which in CI
    // means into the build log.
    let mut replaced = gearbox_ir::Diagnostics::new();
    let resolved = gearbox_engine::secrets::redact_product(
        &resolved,
        Some(&scan.catalogue),
        product_file.parent(),
        &mut replaced,
    );
    diagnostics.extend(replaced.iter().cloned());

    // A lock is an artifact, and only a writable one is a valid artifact.
    // `write_canonical` stamps a fresh valid hash, so a resolution that reported
    // errors came out on stdout as a well formed lock: redirected into a file it
    // passes `gearbox_lock::read`, and `gearbox lock gears` then answers from a
    // topology the resolver could not finish deciding. `generate` refuses at
    // this point and so does the RPC path; only the exit code said so here.
    if format == ResolveFormat::Toml && !resolved.is_writable() {
        report(&diagnostics);
        anyhow::bail!("resolution reported errors; no lock was printed");
    }

    match format {
        // stdout carries the artifact; diagnostics go to stderr, so a pipe into
        // a file or `jq` gets exactly the artifact and nothing else.
        ResolveFormat::Toml => print!("{}", gearbox_lock::write_canonical(&resolved)?),
        ResolveFormat::Json => println!("{}", serde_json::to_string_pretty(&resolved)?),
        ResolveFormat::Text => print_resolution(&resolved),
    }

    report(&diagnostics);

    Ok(if diagnostics.iter().any(|d| d.severity.is_error()) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn print_resolution(resolved: &gearbox_ir::ResolvedProduct) {
    println!(
        "{} {} for profile `{}` ({})",
        resolved.product.id,
        resolved.product.version,
        resolved.product.profile,
        resolved.product.profile_kind
    );
    println!("  {}", resolved.product.lock_hash);
    println!(
        "  {} gear(s) in {} application(s)",
        resolved.gears.len(),
        resolved.applications.len()
    );
    for application in &resolved.applications {
        println!(
            "    {} [{}] x{} -- {}",
            application.name,
            describe_kind(application.kind),
            application.replicas,
            application
                .gears
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !resolved.bindings.is_empty() {
        println!("  bindings:");
        for b in &resolved.bindings {
            println!(
                "    {} -> {} : {} over {} via {}",
                b.consumer,
                b.provider,
                b.contract,
                b.transport.as_str(),
                describe_mechanism(b.mechanism)
            );
        }
    }
    if !resolved.cluster.is_empty() {
        println!("  cluster:");
        for c in &resolved.cluster {
            println!(
                "    {}/{} -> {}",
                c.scope,
                c.primitive.slug(),
                c.resolved.effective_provider().unwrap_or("unsatisfied")
            );
        }
    }
}

/// Written out rather than derived from `Debug`, which the workspace forbids in
/// output a person reads.
const fn describe_kind(kind: gearbox_ir::ApplicationKind) -> &'static str {
    match kind {
        gearbox_ir::ApplicationKind::Host => "host",
        gearbox_ir::ApplicationKind::Worker => "worker",
    }
}

const fn describe_mechanism(mechanism: gearbox_ir::BindingMechanism) -> &'static str {
    match mechanism {
        gearbox_ir::BindingMechanism::ColocatedLocal => "co-located local",
        gearbox_ir::BindingMechanism::ConsumesStatic => "static endpoint",
        gearbox_ir::BindingMechanism::ConsumesDirectory => "directory",
        gearbox_ir::BindingMechanism::ProvidesClientWiring => "provider client wiring",
    }
}

fn validate(
    roots: &[PathBuf],
    source_id: Option<&str>,
    product_file: Option<&std::path::Path>,
    format: Format,
) -> anyhow::Result<ExitCode> {
    let opened = open_roots(roots, source_id)?;

    // The product is evaluated first so its own GBX01xx are reported even when
    // it cannot be used for the join. A product that does not evaluate is a
    // different failure from a product that names a gear nobody described, and
    // collapsing them would hide the first behind the second.
    let mut intent = None;
    let mut product_diagnostics = Vec::new();
    if let Some(path) = product_file {
        let scan = gearbox_engine::product::load_product(path, None);
        product_diagnostics.extend(scan.diagnostics.as_slice().iter().cloned());
        intent = scan.intent;
    }

    let checked = gearbox_engine::validate::validate_at(&opened, intent.as_ref(), product_file);

    let mut all: Vec<Diagnostic> = product_diagnostics;
    all.extend(checked.diagnostics.as_slice().iter().cloned());

    match format {
        Format::Json => {
            // stdout: the machine-readable contract.
            println!("{}", serde_json::to_string_pretty(&all)?);
        }
        Format::Text => {
            let errors = all.iter().filter(|d| d.severity.is_error()).count();
            let warnings = all
                .iter()
                .filter(|d| d.severity == gearbox_ir::Severity::Warning)
                .count();
            println!(
                "{} gear(s) checked from {} description file(s): {errors} error(s), \
                 {warnings} warning(s)",
                checked.scan.catalogue.gears.len(),
                checked.scan.files.len()
            );
            if let Some(path) = product_file {
                let selected = intent.as_ref().map_or(0, |i| i.selected_gears.len());
                println!("  product {}: {selected} selected gear(s)", path.display());
                // Per profile, because "is this point implemented" only has an
                // answer once a profile is fixed.
                let filled = checked
                    .plugins
                    .iter()
                    .filter(|r| r.winner.is_some())
                    .count();
                if !checked.plugins.is_empty() {
                    println!(
                        "  plugins: {filled} of {} extension point/profile pair(s) resolve to an \
                         implementation",
                        checked.plugins.len()
                    );
                }
            }
        }
    }

    report(&all);

    Ok(if all.iter().any(|d| d.severity.is_error()) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn print_summary(scan: &gearbox_engine::CatalogueScan) {
    let catalogue = &scan.catalogue;
    println!(
        "{} gear(s) from {} description file(s), {} contract(s)",
        catalogue.gears.len(),
        scan.files.len(),
        catalogue.contracts.len()
    );
    if !catalogue.designs.is_empty() {
        println!(
            "  plus {} gear(s) at design maturity, listed last",
            catalogue.designs.len()
        );
    }
    // What the load actually cost. Worth showing because it is the number the
    // incremental-loading work is about: crates parsed is the expensive stage,
    // and the gap against requests is the sharing a single load already gets.
    println!(
        "  parsed {} crate(s) for {} request(s)",
        scan.crates_scanned, scan.scan_requests
    );

    for gear in catalogue.gears.values() {
        let caps: Vec<&str> = gear.runtime_caps.iter().map(|c| c.as_str()).collect();
        // The level only below `stable`, where it is news.
        let maturity = match gear.maturity {
            gearbox_ir::Maturity::Stable => String::new(),
            level => format!(" ({})", level.as_str()),
        };
        println!("\n  {}{maturity} [{}]", gear.id, caps.join(", "));
        println!("    {}", gear.gdl_path);
        if !gear.colocated_deps.is_empty() {
            let deps: Vec<&str> = gear
                .colocated_deps
                .iter()
                .map(gearbox_ir::GearId::as_str)
                .collect();
            // Named "co-located with", not "depends on": these edges are
            // link-time and the resolver can never sever them.
            println!("    co-located with: {}", deps.join(", "));
        }
        for provider in &gear.provides {
            let transports: Vec<&str> = provider.transports.iter().map(|t| t.as_str()).collect();
            println!(
                "    provides {} over [{}]",
                provider.contract,
                transports.join(", ")
            );
        }
        for requirement in &gear.consumes {
            if let (Some(contract), Some(from)) =
                (requirement.contract(), requirement.declared_provider())
            {
                println!("    consumes {contract} from {from}");
            }
        }
        for requirement in &gear.requires {
            if let gearbox_ir::RequirementKind::Cluster { primitive, scope } = &requirement.kind {
                let caps: Vec<&str> = requirement
                    .capabilities
                    .iter()
                    .map(gearbox_ir::CapabilityId::as_str)
                    .collect();
                println!(
                    "    requires cluster.{primitive} in `{scope}` [{}]",
                    caps.join(", ")
                );
            }
        }
    }

    // Apart from the gears above, because none of them can be used yet: a
    // product naming one is refused with GBX0321.
    if !catalogue.designs.is_empty() {
        println!("\n  design (described, no code yet):");
        for design in catalogue.designs.values() {
            println!("    {}  {}", design.id, design.gdl_path);
        }
    }
}

/// Catalogue load errors used to be ignored by `resolve` and `generate`, so a
/// duplicate gear still produced a lock. `plugins` already fails closed; these
/// commands share that gate.
pub(crate) fn refuse_catalogue_errors(catalogue: &gearbox_ir::Catalogue) -> Option<ExitCode> {
    report(catalogue.diagnostics.as_slice());
    catalogue
        .diagnostics
        .has_errors()
        .then_some(ExitCode::FAILURE)
}

/// Print diagnostics to stderr, most severe first.
pub(crate) fn report(diagnostics: &[Diagnostic]) {
    if diagnostics.is_empty() {
        return;
    }

    let mut sorted: Vec<&Diagnostic> = diagnostics.iter().collect();
    sorted.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.code.cmp(&b.code))
    });

    eprintln!();
    for d in sorted {
        let where_ = d
            .location
            .as_ref()
            .map(|l| format!(" {}:{}", l.uri, l.range.start.line + 1))
            .unwrap_or_default();
        eprintln!("{} [{}]{where_}: {}", label(d.severity), d.code, d.message);
        if let Some(help) = &d.help {
            eprintln!("    help: {help}");
        }
        // The citation is the whole point of a runtime-gap diagnostic: it lets a
        // reader confirm the claim instead of taking the tool's word for it.
        if let Some(evidence) = &d.evidence {
            eprintln!("    evidence: {evidence}");
        }
    }
}

const fn label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
        Severity::Hint => "hint",
    }
}

/// Describe a source for a human.
///
/// Spelled out here rather than as a `Display` impl on the IR type: this is one
/// presentation, and the IR should not own a phrasing only the CLI uses.
fn describe_source(source: &gearbox_ir::SourceDecl) -> String {
    match source {
        gearbox_ir::SourceDecl::Path { at, .. } => format!("path {at}"),
        gearbox_ir::SourceDecl::Registry { url, prefix, .. } => {
            let url = without_userinfo(url);
            match prefix {
                Some(prefix) => format!("registry {url} (packages named {prefix}<gear>)"),
                None => format!("registry {url}"),
            }
        }
        gearbox_ir::SourceDecl::Git {
            url,
            tag,
            rev,
            branch,
            ..
        } => {
            let pin = tag
                .as_ref()
                .map(|t| format!("tag {t}"))
                .or_else(|| rev.as_ref().map(|r| format!("rev {r}")))
                .or_else(|| branch.as_ref().map(|b| format!("branch {b}")))
                .unwrap_or_else(|| "unpinned".to_owned());
            format!("git {} @ {pin}", without_userinfo(url))
        }
    }
}

/// `url` with its userinfo component replaced.
///
/// `https://user:TOKEN@host/repo.git` is an ordinary way to write a git or
/// alternate-registry source, and `gearbox product` prints what it finds to
/// stdout -- which under CI is the build log. The component is replaced rather
/// than dropped so the operator can still see that the description carries a
/// credential at all, and only inside the authority, so a path or query
/// containing `@` is left alone. An scp-style `git@host:org/repo` has no `://`
/// and no password field, so it is not touched.
fn without_userinfo(url: &str) -> std::borrow::Cow<'_, str> {
    let Some(scheme_end) = url.find("://") else {
        return std::borrow::Cow::Borrowed(url);
    };
    let authority_start = scheme_end + "://".len();
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| authority_start + i);
    let Some(at) = url[authority_start..authority_end].find('@') else {
        return std::borrow::Cow::Borrowed(url);
    };
    std::borrow::Cow::Owned(format!(
        "{}<credential>{}",
        &url[..authority_start],
        &url[authority_start + at..]
    ))
}

fn describe_mode(mode: gearbox_ir::BindingMode) -> &'static str {
    match mode {
        gearbox_ir::BindingMode::Auto => "auto",
        gearbox_ir::BindingMode::Local => "local",
        gearbox_ir::BindingMode::Remote => "remote",
    }
}

fn describe_preference(preference: &gearbox_ir::Preference) -> String {
    match preference {
        gearbox_ir::Preference::ExistingInfrastructure => "existing-infrastructure".to_owned(),
        gearbox_ir::Preference::FewerApplications => "fewer-applications".to_owned(),
        gearbox_ir::Preference::Isolate { gear } => format!("isolate {gear}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use gearbox_ir::{
        CargoRef, Catalogue, Diagnostic, DiagnosticCode, ExtensionPointDecl, GearDescriptor,
        GearId, PluginImpl, RelPath, SourceId, Visibility,
    };

    use super::{
        describe_source, list_plugins, open_source_roots, refuse_catalogue_errors, without_userinfo,
    };

    fn gear(id: &str) -> GearDescriptor {
        GearDescriptor {
            id: GearId::new(id).unwrap(),
            display_name: id.to_owned(),
            description: None,
            category: None,
            visibility: Visibility::Internal,
            maturity: gearbox_ir::Maturity::Stable,
            source: SourceId::new("gears-rust").unwrap(),
            gdl_path: RelPath::new(format!("gears/{id}/gear.gdl")).unwrap(),
            sdks: Vec::new(),
            implementation: None,
            scenarios: Vec::new(),
            origin: None,
            tests: Vec::new(),
            package: CargoRef::new(
                format!("cf-gears-{id}"),
                id.replace('-', "_"),
                RelPath::here(),
            ),
            runtime_caps: BTreeSet::new(),
            colocated_deps: BTreeSet::new(),
            lifecycle: None,
            provides: Vec::new(),
            consumes: Vec::new(),
            requires: Vec::new(),
            serves: Vec::new(),
            client_trait: None,
            cluster_providers: Vec::new(),
            extension_points: Vec::new(),
            implements: None,
            vendor_selector: None,
            declared_roles: Vec::new(),
            one_per_installation: false,
            available_features: BTreeSet::new(),
            cargo_features: None,
            config_schema: None,
            docs: None,
            gts_types: Vec::new(),
            declared_at: None,
        }
    }

    fn point() -> ExtensionPointDecl {
        ExtensionPointDecl {
            spec: "cf.toolkit.plugins.plugin.v1~cf.core.authn_resolver.plugin.v1~".to_owned(),
            trait_ident: "AuthNResolverPluginClient".to_owned(),
            sdk_lib: "authn_resolver_sdk".to_owned(),
            sdk: CargoRef::new(
                "cf-gears-authn-resolver-sdk",
                "authn_resolver_sdk",
                RelPath::here(),
            ),
            selector: None,
        }
    }

    /// A host declaring one point, and one plugin filling it.
    ///
    /// Both name filters below need a non-empty catalogue: with
    /// `Catalogue::default()` the filter never runs at all, so an inverted
    /// comparison in it kept every test passing.
    fn with_a_host() -> Catalogue {
        let mut host = gear("authn-resolver");
        host.extension_points = vec![point()];
        host.vendor_selector = Some("constructorfabric".to_owned());

        let mut plugin = gear("static-authn-plugin");
        plugin.implements = Some(PluginImpl {
            spec: point().spec,
            point: Some(point()),
            default_vendor: Some("constructorfabric".to_owned()),
            default_priority: Some(10),
        });

        let mut catalogue = Catalogue::default();
        for g in [host, plugin] {
            catalogue.gears.insert(g.id.clone(), g);
        }
        catalogue
    }

    #[test]
    fn a_named_gear_miss_is_failure() {
        assert!(!list_plugins(&Catalogue::default(), Some("no-such-gear")));
    }

    #[test]
    fn listing_with_no_hosts_is_still_success() {
        assert!(list_plugins(&Catalogue::default(), None));
    }

    #[test]
    fn the_gear_filter_finds_the_host_it_names() {
        assert!(list_plugins(&with_a_host(), Some("authn-resolver")));
    }

    #[test]
    fn the_gear_filter_refuses_a_host_the_catalogue_does_not_hold() {
        let catalogue = with_a_host();
        assert!(
            !list_plugins(&catalogue, Some("types-registry")),
            "a name that matches nothing is a miss even when the catalogue has hosts"
        );
        assert!(
            !list_plugins(&catalogue, Some("static-authn-plugin")),
            "a plugin is not a host: it declares no extension point"
        );
    }

    #[test]
    fn an_explicit_source_id_refuses_several_roots() {
        // No filesystem: the refusal fires before any root is opened, which is
        // the point of it -- `--source-id x --root a --root b` gave both roots
        // one identity and the second root's gears replaced the first's.
        let err = open_source_roots(
            &[
                PathBuf::from("/nonexistent/a"),
                PathBuf::from("/nonexistent/b"),
            ],
            Some("shared"),
        )
        .expect_err("two roots cannot share one declared identity");
        assert!(
            err.to_string().contains("2 roots were given"),
            "the message should say how many roots it saw: {err}"
        );
    }

    #[test]
    fn a_clean_catalogue_passes_the_gate() {
        assert!(refuse_catalogue_errors(&Catalogue::default()).is_none());
    }

    #[test]
    fn a_catalogue_error_closes_the_gate() {
        let mut catalogue = Catalogue::default();
        catalogue.diagnostics.push(Diagnostic::error(
            DiagnosticCode::GdlCardinality,
            "gear `api-gateway` is declared twice",
            "give one of the descriptions a different id",
        ));
        assert!(
            refuse_catalogue_errors(&catalogue).is_some(),
            "resolve and generate used to build a lock from a catalogue that failed to load"
        );
    }

    #[test]
    fn a_credential_in_a_source_url_is_not_printed() {
        let git = gearbox_ir::SourceDecl::Git {
            url: "https://ci-bot:s3cr3t-token@git.example.com/org/gears.git".to_owned(),
            tag: Some("v1.2.3".to_owned()),
            rev: None,
            branch: None,
            declared_at: None,
        };
        let rendered = describe_source(&git);
        assert!(
            !rendered.contains("s3cr3t-token") && !rendered.contains("ci-bot"),
            "the userinfo component reaches stdout, and under CI the build log: {rendered}"
        );
        assert!(rendered.contains("git.example.com/org/gears.git"));
        assert!(rendered.contains("tag v1.2.3"));

        let registry = gearbox_ir::SourceDecl::Registry {
            url: "https://token@registry.example.com/index".to_owned(),
            prefix: Some("cf-gears-".to_owned()),
            declared_at: None,
        };
        let rendered = describe_source(&registry);
        assert!(!rendered.contains("token@"), "{rendered}");
    }

    #[test]
    fn only_the_authority_s_userinfo_is_replaced() {
        assert_eq!(
            without_userinfo("https://git.example.com/org/repo.git"),
            "https://git.example.com/org/repo.git"
        );
        assert_eq!(
            without_userinfo("https://git.example.com/org/repo.git?from=a@b"),
            "https://git.example.com/org/repo.git?from=a@b",
            "an `@` past the authority is part of the path or query, not a credential"
        );
        // scp syntax has no `://` and no password field, so there is nothing to
        // replace and dropping `git@` would only lose information.
        assert_eq!(
            without_userinfo("git@github.com:org/repo.git"),
            "git@github.com:org/repo.git"
        );
    }
}
