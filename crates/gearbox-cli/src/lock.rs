//! `gearbox lock` -- questions answered from a written `product.lock`.
//!
//! Separate from `gearbox resolve`, which prints a lock it has just computed.
//! This reads one off disk, and reading is the point: the answers here are the
//! reference side of the verification oracles, so they have to come from the
//! same bytes the generated crate was built from rather than from a fresh
//! resolution that might disagree.
//!
//! `gearbox_lock::read` verifies the recorded hash, so a hand-edited lock fails
//! to load here rather than quietly supplying a wrong reference.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Subcommand, ValueEnum};
use gearbox_ir::{ApplicationId, GearId, ResolvedProduct};

use crate::Format;

/// The default lock path, which is where `gearbox generate` puts it.
const DEFAULT_LOCK: &str = "product.lock";

#[derive(Subcommand)]
pub enum LockQuery {
    /// List the gears composed into one application.
    ///
    /// The reference side of the `--list-registered-gears` oracle: the binary
    /// reports what the linker and `inventory` actually produced, this reports
    /// what the lock said they should.
    Gears {
        /// The lock to read. Defaults to `product.lock` in the working
        /// directory, which is what `gearbox generate` writes.
        #[arg(long, value_name = "FILE")]
        lock: Option<PathBuf>,

        /// Which application.
        #[arg(long, value_name = "ID")]
        application: String,

        #[arg(long, value_enum, default_value_t = Order::Topo)]
        order: Order,

        /// Append a tab and the gear's co-location dependencies, comma
        /// separated and sorted.
        #[arg(long)]
        with_deps: bool,
    },

    /// List the applications the lock resolved to.
    ///
    /// `processes` stays as an alias: it was the published spelling before
    /// ADR-0016 renamed the level, and a command name is an interface.
    #[command(alias = "processes")]
    Applications {
        #[arg(long, value_name = "FILE")]
        lock: Option<PathBuf>,

        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
}

/// How to order the gears of a process.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum Order {
    /// Dependency order, as the lock recorded it.
    ///
    /// A caveat that matters for the oracle: the running binary's own order is
    /// *a* topological order but not a canonical one. `GearRegistry` seeds
    /// Kahn's algorithm from `HashMap::keys()`
    /// (`gears-rust/libs/toolkit/src/registry.rs:553`), whose iteration order is
    /// randomized per process, so two runs of one binary legitimately differ
    /// wherever two gears do not depend on each other. Compare against `name`,
    /// not against this.
    Topo,
    /// Sorted by name -- the order to compare a binary's output against.
    Name,
}

/// Answer one lock query.
///
/// # Errors
/// Returns an error when the lock cannot be read or fails its hash check, or
/// when it names no such process.
pub fn run(query: &LockQuery) -> anyhow::Result<ExitCode> {
    match query {
        LockQuery::Gears {
            lock,
            application,
            order,
            with_deps,
        } => gears(lock.as_deref(), application, *order, *with_deps),
        LockQuery::Applications { lock, format } => applications(lock.as_deref(), *format),
    }
}

fn read(path: Option<&Path>) -> anyhow::Result<ResolvedProduct> {
    let path = path.unwrap_or_else(|| Path::new(DEFAULT_LOCK));
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read `{}`: {e}", path.display()))?;
    // With the path, because `LockError` does not carry one: `--lock other.lock`
    // failed with `failed to parse product.lock`, which names the wrong file.
    let product = gearbox_lock::read(&text)
        .with_context(|| format!("cannot read the lock `{}`", path.display()))?;

    // The hash check proves the file was not edited, not that the resolution
    // finished. `diagnostics` is serialized into the lock, so one carrying its
    // own errors loaded cleanly here and every answer below then came from a
    // topology the resolver could not finish deciding. `generate` gates on
    // `is_writable()` for exactly this reason.
    if !product.is_writable() {
        crate::report(product.diagnostics.as_slice());
        anyhow::bail!(
            "the lock `{}` records a resolution that reported errors; \
             re-resolve the product rather than answering from it",
            path.display()
        );
    }
    Ok(product)
}

fn gears(
    path: Option<&Path>,
    application: &str,
    order: Order,
    with_deps: bool,
) -> anyhow::Result<ExitCode> {
    let lock = read(path)?;
    let id = ApplicationId::new(application)?;
    for line in gear_lines(&lock, &id, order, with_deps)? {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The lines `gears` prints, so the ordering contract can be tested.
///
/// Separate from the printing because these lines are the reference side of the
/// `--list-registered-gears` oracle: nothing else pins the order `--order name`
/// promises, and dropping the sort would silently fall back to the lock's topo
/// order -- which the doc on [`Order::Topo`] says is not canonical, so the
/// comparison against a running binary would become order-dependent and flaky
/// rather than failing here.
///
/// # Errors
/// Returns an error when the lock names no such application, or when it lists a
/// gear it has no entry for.
fn gear_lines(
    lock: &ResolvedProduct,
    application: &ApplicationId,
    order: Order,
    with_deps: bool,
) -> anyhow::Result<Vec<String>> {
    let Some(resolved) = lock.application(application) else {
        let known: Vec<&str> = lock.applications.iter().map(|p| p.name.as_str()).collect();
        anyhow::bail!(
            "the lock has no application `{application}`; it has: {}",
            known.join(", ")
        );
    };

    let mut gears: Vec<&GearId> = resolved.gears.iter().collect();
    if order == Order::Name {
        gears.sort();
    }

    let mut lines = Vec::with_capacity(gears.len());
    for gear in gears {
        // Nothing validates this cross-reference on read -- `gearbox_lock::read`
        // checks the hash and no more -- and an empty dep column reads as "this
        // gear has no co-location deps" rather than "the lock is inconsistent".
        // Refused whether or not `--with-deps` was asked for: the fault is in
        // the lock, so the plain listing is no more answerable than the other.
        let Some(entry) = lock.gears.get(gear) else {
            anyhow::bail!(
                "the lock lists gear `{gear}` in application `{application}` but has no entry \
                 for it; re-resolve the product rather than answering from it"
            );
        };
        if with_deps {
            // Sorted, because the registry's `deps()` is in attribute order and
            // the lock's is a `BTreeSet`. Sorting both sides is what lets the
            // two be compared at all.
            let deps: Vec<&str> = entry.colocated_deps.iter().map(GearId::as_str).collect();
            lines.push(format!("{gear}\t{}", deps.join(",")));
        } else {
            lines.push(gear.to_string());
        }
    }
    Ok(lines)
}

fn applications(path: Option<&Path>, format: Format) -> anyhow::Result<ExitCode> {
    let lock = read(path)?;
    match format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&lock.applications)?),
        Format::Text => {
            for application in &lock.applications {
                let gears: Vec<&str> = application.gears.iter().map(GearId::as_str).collect();
                println!(
                    "{}\t{}\tx{}\t{}",
                    application.name,
                    application.bin_name,
                    application.replicas,
                    gears.join(",")
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use gearbox_ir::{
        ApplicationId, ApplicationKind, CargoRef, Diagnostic, DiagnosticCode, Entrypoint, GearId,
        InclusionReason, LOCK_SCHEMA_VERSION, ProfileId, RelPath, ResolvedApplication,
        ResolvedGear, ResolvedProduct, ResolvedProductHeader, ResolvedSource, SourceId, SourceKind,
    };

    use super::{Order, gear_lines, read};

    fn gid(id: &str) -> GearId {
        GearId::new(id).unwrap()
    }

    fn source() -> SourceId {
        SourceId::new("gears-rust").unwrap()
    }

    fn gear(id: &str, deps: &[&str]) -> ResolvedGear {
        ResolvedGear {
            id: gid(id),
            source: source(),
            gdl_path: RelPath::new(format!("gears/{id}/gear.gdl")).unwrap(),
            package: CargoRef::new(
                format!("cf-gears-{id}"),
                id.replace('-', "_"),
                RelPath::new(format!("gears/{id}")).unwrap(),
            ),
            crate_dir: RelPath::new(format!("gears/{id}")).unwrap(),
            runtime_caps: BTreeSet::new(),
            colocated_deps: deps.iter().map(|d| gid(d)).collect(),
            selected_by: vec![InclusionReason::Selected],
            selected_features: BTreeSet::new(),
            config: BTreeMap::new(),
        }
    }

    /// One application holding two gears that do not depend on each other, so
    /// the lock's own order says nothing about what `--order name` must print.
    fn fixture() -> ResolvedProduct {
        let gears = ["zeta-gear", "alpha-gear"].map(|id| gear(id, &["types-registry", "cluster"]));
        ResolvedProduct {
            database: None,
            sqlite_databases: BTreeMap::new(),
            schema_version: LOCK_SCHEMA_VERSION,
            product: ResolvedProductHeader {
                id: "lock-test".to_owned(),
                version: "0.1.0".to_owned(),
                profile: ProfileId::new("dev").unwrap(),
                profile_kind: "embedded".to_owned(),
                layout: gearbox_ir::DEFAULT_LAYOUT.to_owned(),
                gearbox_version: "0.1.0".to_owned(),
                lock_hash: String::new(),
            },
            kubernetes: None,
            self_hosted: None,
            sources: BTreeMap::from([(
                source(),
                ResolvedSource {
                    id: source(),
                    kind: SourceKind::Path,
                    location: "../gears-rust".to_owned(),
                    digest: "git:0000000000000000000000000000000000000000".to_owned(),
                },
            )]),
            gears: gears.iter().map(|g| (g.id.clone(), g.clone())).collect(),
            applications: vec![ResolvedApplication {
                name: ApplicationId::new("host").unwrap(),
                kind: ApplicationKind::Host,
                anchor: gid("zeta-gear"),
                role: None,
                gears: vec![gid("zeta-gear"), gid("alpha-gear")],
                replicas: 1,
                entrypoint: Entrypoint::RunServer,
                bin_name: "host".to_owned(),
                crate_name: "host".to_owned(),
                listens: Vec::new(),
                rest_host: None,
                grpc_hub: None,
                needs_db: false,
                cargo_features: BTreeSet::new(),
                spawns: Vec::new(),
                serve: None,
                image: None,
                subchart: None,
                service_port: None,
            }],
            bindings: Vec::new(),
            cluster: Vec::new(),
            cuttable_if_declared: Vec::new(),
            provenance: Vec::new(),
            diagnostics: gearbox_ir::Diagnostics::new(),
        }
    }

    fn host() -> ApplicationId {
        ApplicationId::new("host").unwrap()
    }

    /// A scratch directory of this test's own, qualified by pid so two runs of
    /// the suite cannot collide.
    fn scratch(marker: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gbx-cli-lock-{marker}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(marker: &str, product: &ResolvedProduct) -> PathBuf {
        let path = scratch(marker).join("product.lock");
        std::fs::write(&path, gearbox_lock::write_canonical(product).unwrap()).unwrap();
        path
    }

    #[test]
    fn name_order_sorts_the_gears() {
        let lock = fixture();
        assert_eq!(
            gear_lines(&lock, &host(), Order::Name, false).unwrap(),
            ["alpha-gear", "zeta-gear"],
            "`--order name` is the order a binary's own output is compared against"
        );
        assert_eq!(
            gear_lines(&lock, &host(), Order::Topo, false).unwrap(),
            ["zeta-gear", "alpha-gear"],
            "`--order topo` answers in the order the lock recorded"
        );
    }

    #[test]
    fn with_deps_appends_sorted_comma_separated_deps() {
        let lines = gear_lines(&fixture(), &host(), Order::Name, true).unwrap();
        assert_eq!(
            lines,
            [
                "alpha-gear\tcluster,types-registry",
                "zeta-gear\tcluster,types-registry"
            ],
            "the registry's `deps()` is in attribute order, so both sides sort or neither \
             comparison holds"
        );
    }

    #[test]
    fn an_unknown_application_lists_the_known_ones() {
        let err = gear_lines(
            &fixture(),
            &ApplicationId::new("audit").unwrap(),
            Order::Name,
            false,
        )
        .expect_err("the lock names no `audit`");
        let message = err.to_string();
        assert!(message.contains("no application `audit`"), "{message}");
        assert!(
            message.contains("it has: host"),
            "the refusal has to say what the lock does hold: {message}"
        );
    }

    #[test]
    fn a_gear_the_lock_has_no_entry_for_is_refused() {
        let mut lock = fixture();
        lock.gears.remove(&gid("alpha-gear"));
        // Both queries, because the fault is in the lock: an empty dep column
        // read as "this gear has no co-location deps", and a plain listing that
        // answered 0 was no more trustworthy.
        for with_deps in [false, true] {
            let err = gear_lines(&lock, &host(), Order::Name, with_deps)
                .expect_err("an application listing a gear the lock does not describe");
            assert!(
                err.to_string().contains("no entry for it"),
                "{}",
                err.to_string()
            );
        }
    }

    #[test]
    fn a_hand_edited_lock_fails_the_hash_check() {
        let path = write("tampered", &fixture());
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("replicas = 1", "replicas = 4")).unwrap();

        let err = read(Some(&path)).expect_err("a hand-edited lock must not answer");
        let message = format!("{err:#}");
        assert!(
            message.contains(&path.display().to_string()),
            "the refusal has to name the file it read: {message}"
        );
        assert!(
            message.contains("hash"),
            "a hand edit is caught by the hash check, and the refusal should say so: {message}"
        );
    }

    #[test]
    fn a_lock_recording_its_own_resolution_errors_is_refused() {
        let mut product = fixture();
        product.diagnostics.push(Diagnostic::error(
            DiagnosticCode::TopologyUnknownGear,
            "gear `no-such-gear` is not in the catalogue",
            "check the spelling, or add a source that describes it",
        ));
        let path = write("unwritable", &product);

        let err =
            read(Some(&path)).expect_err("the resolver never finished deciding this topology");
        assert!(
            format!("{err:#}").contains("reported errors"),
            "{}",
            format!("{err:#}")
        );
    }

    #[test]
    fn a_lock_that_is_not_there_names_the_path_it_looked_at() {
        let path = scratch("missing").join("nowhere.lock");
        let err = read(Some(&path)).expect_err("nothing to read");
        assert!(format!("{err:#}").contains("nowhere.lock"));
    }
}
