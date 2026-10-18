//! `gearbox generate` -- resolve, then write (or preview) the artefact tree.
//!
//! The command resolves rather than reading an existing lock, so a `--dry-run`
//! can never be answering about a stale one. The lock it produces is written
//! into the output root as part of the same file set, which is what makes
//! `gearbox lock gears` answerable from inside a generated tree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use gearbox_engine::generate::{GenerateInput, TemplateSet, base_root_for, summarize};
use gearbox_ir::{FileAction, FilePlan, ProductId, ProfileId};

use crate::pipeline::{Outcome, Resolution};
use crate::{Format, report};

/// Where generated output goes, relative to the working directory.
///
/// The engine owns the layout because `gearbox/product/lock` diffs the lock
/// inside the tree this writes: two spellings match until one of them changes,
/// and then the lock is compared against a directory nothing writes.
use gearbox_engine::generate::OUTPUT_DIR;

/// Resolve `product` for `profile` and generate its artefacts.
///
/// # Errors
/// Returns an error when a source root cannot be opened, when the product
/// cannot be evaluated, or when generation or writing fails.
#[allow(
    clippy::too_many_arguments,
    reason = "one argument per command-line flag; bundling them into a struct would put the \
              clap definition and its consumer out of sight of one another"
)]
pub fn run(
    roots: &[PathBuf],
    source_id: Option<&str>,
    product_file: &Path,
    profile: Option<&str>,
    out: Option<&Path>,
    dry_run: bool,
    format: Format,
) -> anyhow::Result<ExitCode> {
    let resolved = match crate::pipeline::resolve(roots, source_id, product_file, profile)? {
        Outcome::Refused(code) => return Ok(code),
        Outcome::Resolved(resolved) => resolved,
    };
    let Resolution {
        opened,
        scan,
        product_file,
        intent,
        profile,
        lock,
        mut diagnostics,
    } = *resolved;

    // A lock with errors in it describes a topology the resolver could not
    // finish deciding. Generating from it would produce a tree that compiles
    // into something nobody asked for, which is worse than producing nothing.
    if !lock.is_writable() {
        report(&diagnostics);
        anyhow::bail!("resolution reported errors; nothing was generated");
    }

    let out_root = absolute(match out {
        Some(dir) => dir.to_path_buf(),
        None => default_out_root(&lock.product.id, &profile)?,
    })?;
    let base_root = base_root_for(&out_root);

    let source_roots: BTreeMap<_, _> = opened
        .iter()
        .map(|root| (root.id.clone(), root.root.clone()))
        .collect();
    let templates = TemplateSet::load_for_product(&product_file, intent.templates.as_deref())?;
    // `crates = registry(...)`: which crates the build names by version, each
    // checked against what was published. Asks cargo, so only when a source
    // asked for it.
    let (registry, registry_diagnostics) = gearbox_engine::published::for_product(
        &gearbox_engine::published::registry_sources(&intent.sources),
        &lock,
        &source_roots,
        &std::env::temp_dir()
            .join("gearbox-published")
            .join(&lock.product.id),
    );
    diagnostics.extend(registry_diagnostics);
    let generated = gearbox_engine::generate(&GenerateInput {
        lock: &lock,
        source_roots: &source_roots,
        out_root: &out_root,
        templates,
        product_dir: product_file.parent(),
        catalogue: Some(&scan.catalogue),
        registry: registry.as_ref(),
    })?;

    // Generation's own diagnostics, before the plan's: a credential replaced in
    // the lock is something the operator must act on, and computing it without
    // reporting it is the mistake `overridden_templates` already made once.
    diagnostics.extend(generated.diagnostics.as_slice().iter().cloned());

    let (plans, apply_diagnostics, written) = if dry_run {
        let (plans, diagnostics) =
            gearbox_engine::generate::plan(&generated.files, &out_root, &base_root)?;
        (plans, diagnostics, 0)
    } else {
        let outcome = gearbox_engine::apply_generate(&generated.files, &out_root, &base_root)?;
        (outcome.plans, outcome.diagnostics, outcome.written)
    };
    diagnostics.extend(apply_diagnostics.as_slice().iter().cloned());

    match format {
        // stdout: the machine-readable contract, and the whole of it. A client
        // asking for the plan gets the plan, not the plan plus a summary line.
        Format::Json => println!("{}", serde_json::to_string_pretty(&plans)?),
        Format::Text => {
            print_plans(&plans, &out_root, dry_run, written);
            if !generated.overridden_templates.is_empty() {
                println!(
                    "  overrode templates: {}",
                    generated.overridden_templates.join(", ")
                );
            }
        }
    }

    report(&diagnostics);
    Ok(if diagnostics.iter().any(|d| d.severity.is_error()) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Where generation writes when `--out` names nowhere.
///
/// The product id is validated before it names a directory. It reaches the lock
/// as a plain `String` lowered from `product(id = ...)`, and `Path::join` on an
/// absolute or `..`-bearing segment walks straight out of `.gearbox/`: an id of
/// `/tmp/x` put the whole artefact tree wherever the description asked. The GDL
/// boundary refuses `layout` for exactly this reason, and the RPC's product
/// create already refuses an id `ProductId` rejects.
fn default_out_root(product_id: &str, profile: &ProfileId) -> anyhow::Result<PathBuf> {
    let id = ProductId::new(product_id).with_context(|| {
        format!(
            "`{OUTPUT_DIR}/<product>/<profile>/` cannot be named from product id `{product_id}`"
        )
    })?;
    Ok(gearbox_engine::generate::default_out_root(&id, profile))
}

/// Resolve a path against the working directory without requiring it to exist.
///
/// `canonicalize` is not usable here: the output root is usually a directory
/// generation is about to create.
/// The output root as an absolute, symlink-free path.
///
/// **Canonical, not just absolute.** Every relative path the tree writes --
/// `path = "..."` to a gear, a `[patch]` entry, the shared target directory --
/// is computed between this and the source roots, which are canonical. On macOS
/// `/tmp` is a link to `/private/tmp`, so `--out /tmp/app` counted one level
/// fewer than the directory really has and every dependency path missed by one
/// `..`. The directory need not exist yet, so the nearest ancestor that does is
/// canonicalized and the rest is appended; a `..` in that rest has nothing to
/// resolve against and is refused.
fn absolute(path: PathBuf) -> anyhow::Result<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut existing = path.as_path();
    let mut rest = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            break;
        };
        rest.push(name.to_owned());
        existing = existing.parent().unwrap_or(existing);
    }
    let mut out = existing.canonicalize()?;
    for part in rest.iter().rev() {
        if part == ".." {
            anyhow::bail!(
                "`{}` walks up out of a directory that does not exist yet; name the folder directly",
                path.display()
            );
        }
        out.push(part);
    }
    Ok(out)
}

fn print_plans(plans: &[FilePlan], out_root: &Path, dry_run: bool, written: usize) {
    println!(
        "{} {}",
        if dry_run {
            "would write into"
        } else {
            "wrote into"
        },
        out_root.display()
    );
    for plan in plans {
        println!(
            "  {:<9} {:<15} {}",
            plan.action.as_str(),
            plan.ownership.as_str(),
            plan.path
        );
    }

    let counts = summarize(plans);
    let rendered: Vec<String> = counts
        .iter()
        .map(|(action, count)| format!("{count} {action}"))
        .collect();
    println!("  {}", rendered.join(", "));
    if !dry_run {
        println!("  {written} file(s) written");
    }
    if counts.contains_key(&FileAction::Conflict) {
        println!("  nothing was written: resolve the conflicts above and re-run");
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use gearbox_ir::ProfileId;

    use super::{OUTPUT_DIR, absolute, default_out_root, run};
    use crate::Format;

    fn dev() -> ProfileId {
        ProfileId::new("dev").unwrap()
    }

    /// A scratch directory of this test's own, qualified by pid so two runs of
    /// the suite cannot collide.
    fn scratch(marker: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gbx-cli-generate-{marker}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("root")).unwrap();
        dir
    }

    /// A product that resolves cleanly and selects nothing.
    ///
    /// Enough to generate a tree -- a workspace manifest, a toolchain file and
    /// the lock -- without a source root holding any gear, which keeps this a
    /// test of the command rather than of the corpus.
    fn product(dir: &Path, gears: &str) -> PathBuf {
        let path = dir.join("product.gdl");
        std::fs::write(
            &path,
            format!(
                "product(\n\
                 \x20   id = \"gen-test\",\n\
                 \x20   name = \"Gen Test\",\n\
                 \x20   version = \"0.1.0\",\n\
                 \x20   sources = [source(id = \"local\", at = path(\"root\"))],\n\
                 \x20   profiles = [embedded(id = \"dev\")],\n\
                 \x20   default_profile = \"dev\",\n\
                 \x20   gears = [{gears}],\n\
                 )\n"
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn a_product_id_that_is_not_one_segment_cannot_name_the_output_root() {
        for id in ["../../..", "/tmp/x", "", ".", "..", "a/b"] {
            let refusal = default_out_root(id, &dev());
            assert!(
                refusal.is_err(),
                "`{id}` would put the artefact tree outside `{OUTPUT_DIR}`"
            );
        }
    }

    #[test]
    fn a_kebab_case_product_id_names_the_documented_output_root() {
        assert_eq!(
            default_out_root("payments-demo", &dev()).unwrap(),
            Path::new(OUTPUT_DIR).join("payments-demo").join("dev")
        );
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let dir = scratch("dry-run");
        let product = product(&dir, "");
        let out = dir.join("out");

        let code = run(
            &[dir.join("root")],
            None,
            &product,
            None,
            Some(&out),
            true,
            Format::Text,
        )
        .expect("the product resolves");

        assert!(
            !out.exists(),
            "`--dry-run` reported a plan and then created `{}`",
            out.display()
        );
        // `ExitCode` has no `PartialEq`, so the rendering is the comparison.
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::SUCCESS),
            "a clean resolution previewed is a success"
        );
    }

    #[test]
    fn a_resolution_that_reported_errors_generates_nothing() {
        let dir = scratch("unwritable");
        // A gear no open source describes: the resolver reports GBX0301, so the
        // lock is not writable and the tree would describe a topology nobody
        // decided on.
        let product = product(&dir, "use_gear(\"no-such-gear\", source = \"local\")");
        let out = dir.join("out");

        let err = run(
            &[dir.join("root")],
            None,
            &product,
            None,
            Some(&out),
            false,
            Format::Text,
        )
        .expect_err("a lock carrying errors must not be generated from");

        assert!(
            err.to_string().contains("nothing was generated"),
            "{}",
            err.to_string()
        );
        assert!(!out.exists(), "the refusal came after a write");
    }

    #[cfg(windows)]
    #[test]
    fn windows_plain_and_verbatim_output_roots_agree_before_creation() {
        let dir = scratch("windows-canonical");
        let plain = dir.join("missing").join("app");
        let canonical = dir.canonicalize().unwrap().join("missing").join("app");
        assert_eq!(absolute(plain.clone()).unwrap(), canonical);
        assert_eq!(absolute(canonical.clone()).unwrap(), canonical);
        assert!(!plain.exists());
    }

    /// An output root reached through a symlink is the directory it points at,
    /// even before it exists: relative dependency paths are counted from it.
    #[cfg(unix)]
    #[test]
    fn an_output_root_through_a_symlink_is_canonical() {
        let base = std::env::temp_dir().join(format!("gbx-cli-out-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&base));
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, base.join("link")).unwrap();

        let got = absolute(base.join("link").join("new").join("app")).unwrap();
        assert_eq!(got, real.canonicalize().unwrap().join("new").join("app"));
        assert!(absolute(base.join("link").join("new").join("..").join("x")).is_err());
        drop(std::fs::remove_dir_all(&base));
    }
}
