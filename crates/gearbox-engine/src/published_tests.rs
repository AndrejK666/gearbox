#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gbx-published-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    drop(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn run(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A checkout with one crate at `gears/demo`, committed; returns the repo and
/// the commit, which is what a release of it would record.
fn checkout() -> (PathBuf, String) {
    let repo = scratch("repo");
    let krate = repo.join("gears/demo");
    std::fs::create_dir_all(krate.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[workspace]\nmembers = [\"gears/demo\"]\n[workspace.package]\nversion = \"1.2.3\"\n",
    )
    .unwrap();
    std::fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"cf-demo\"\nversion.workspace = true\n",
    )
    .unwrap();
    std::fs::write(krate.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    run(&repo, &["init", "-q"]);
    run(&repo, &["add", "-A"]);
    run(&repo, &["commit", "-q", "-m", "release"]);
    let sha = run(&repo, &["rev-parse", "HEAD"]);
    (repo, sha)
}

/// What cargo unpacks: the crate plus the record of where it was cut from.
fn published(sha: &str, path_in_vcs: &str) -> PathBuf {
    let dir = scratch("unpacked");
    std::fs::write(
        dir.join(".cargo_vcs_info.json"),
        format!("{{\"git\": {{\"sha1\": \"{sha}\"}}, \"path_in_vcs\": \"{path_in_vcs}\"}}"),
    )
    .unwrap();
    dir
}

#[test]
fn a_workspace_inherited_version_is_read_from_the_workspace() {
    let (repo, _) = checkout();
    assert_eq!(
        local_version(&repo.join("gears/demo")).as_deref(),
        Some("1.2.3")
    );
}

/// The three verdicts, each against a real repository.
#[test]
fn a_checkout_is_compared_with_the_commit_the_package_was_cut_from() {
    let (repo, sha) = checkout();
    let krate = repo.join("gears/demo");
    let unpacked = published(&sha, "gears/demo");

    assert_eq!(compare(&krate, &unpacked), Verdict::AsPublished);

    // `gear.gdl` is Gearbox's and never in a package: adding one changes nothing.
    std::fs::write(krate.join("gear.gdl"), "gear(name = \"Demo\")\n").unwrap();
    assert_eq!(compare(&krate, &unpacked), Verdict::AsPublished);

    // A change to what compiles, committed or not, is a difference.
    std::fs::write(krate.join("src/lib.rs"), "pub fn g() {}\n").unwrap();
    let Verdict::Changed(files) = compare(&krate, &unpacked) else {
        panic!("an edit to src must be seen");
    };
    assert!(files.iter().any(|f| f.contains("src/lib.rs")), "{files:?}");

    // So is a new file nobody has added yet.
    std::fs::write(krate.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(krate.join("src/extra.rs"), "").unwrap();
    assert!(matches!(compare(&krate, &unpacked), Verdict::Changed(_)));
}

#[test]
fn a_commit_the_checkout_does_not_have_cannot_be_compared() {
    let (repo, _) = checkout();
    let unpacked = published(&"0".repeat(40), "gears/demo");
    assert!(matches!(
        compare(&repo.join("gears/demo"), &unpacked),
        Verdict::Unverifiable(_)
    ));
    // Nor can a package that records no commit at all.
    let bare = scratch("bare");
    assert!(matches!(
        compare(&repo.join("gears/demo"), &bare),
        Verdict::Unverifiable(_)
    ));
}

/// A gear beside the product depends on the toolkit by path; the registry
/// toolkit is patched to that same path, so one copy links.
#[test]
fn a_path_gear_pulls_its_path_dependency_into_the_patch() {
    let toolkit = scratch("toolkit");
    let gear = scratch("gear");
    std::fs::write(
        gear.join("Cargo.toml"),
        format!(
            "[package]\nname = \"greeter\"\nversion = \"0.1.0\"\n[dependencies]\n\
             toolkit = {{ package = \"cf-gears-toolkit\", path = {} }}\n",
            toml::Value::String(toolkit.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let mut plan = RegistryPlan::default();
    plan.crates.insert(
        "cf-gears-toolkit".to_owned(),
        RegistryCrate {
            registry: "crates.io".to_owned(),
            version: "0.10.0".to_owned(),
            patch: None,
        },
    );
    plan.graph
        .insert("cf-gears-toolkit".to_owned(), "crates.io".to_owned());
    unify_path_dependents(&mut plan, &[gear]);
    assert_eq!(
        plan.get("cf-gears-toolkit").unwrap().patch.as_deref(),
        Some(toolkit.as_path())
    );
    assert_eq!(plan.patches().get("crates.io").map(Vec::len), Some(1));
}
