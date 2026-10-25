//! The `load()` sandbox: `cpt-gearbox-fr-gdl-sandbox`.
//!
//! `enable_load` is on because shared fragments are a legitimate need. What
//! makes that safe is that a fragment must live inside the declaring file's
//! source root -- otherwise a description could read any file the process can,
//! which is exactly the hermeticity the resolver's determinism rests on.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::fs;
use std::path::{Path, PathBuf};

use gearbox_gdl::engine::LoadPaths;
use gearbox_gdl::{FileIdentity, GdlEngine};
use gearbox_ir::{DiagnosticCode, RelPath, SourceId};

/// A source root laid out on disk, plus a secret alongside it that no
/// description should be able to reach.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gearbox-sandbox-{name}"));
        drop(fs::remove_dir_all(&dir)); // may not exist yet; nothing to report
        fs::create_dir_all(dir.join("root/gears/demo")).expect("create fixture");

        // Outside the root: the thing the sandbox exists to keep out.
        fs::write(dir.join("outside.gdl"), "SECRET = \"leaked\"\n").expect("write outside");

        // Inside the root: a legitimate shared fragment.
        fs::write(
            dir.join("root/shared.gdl"),
            "SHARED = cargo(crate_name = \"cf-shared\", lib = \"shared\")\n",
        )
        .expect("write shared");

        Self { dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.join("root")
    }

    /// An identity for a description at `root/gears/demo/gear.gdl`.
    fn identity(&self) -> FileIdentity {
        let base = self.root().join("gears/demo");
        FileIdentity {
            uri: format!("file://{}/gear.gdl", base.display()),
            source: SourceId::new("fixture").unwrap(),
            gdl_path: RelPath::new("gears/demo/gear.gdl").unwrap(),
            load_paths: Some(LoadPaths {
                base,
                root: self.root(),
            }),
        }
    }

    fn eval(&self, src: &str) -> Vec<DiagnosticCode> {
        GdlEngine::new()
            .eval_gear(&self.identity(), src)
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.dir)); // best-effort cleanup
    }
}

#[test]
fn a_fragment_inside_the_root_loads() {
    let fx = Fixture::new("inside");
    // `//` addresses the source root explicitly.
    let codes = fx.eval(
        r#"
load("//shared.gdl", "SHARED")
gear(maturity = "stable", package = SHARED, name = "Demo")
"#,
    );
    assert!(
        codes.is_empty(),
        "a legitimate fragment should load: {codes:?}"
    );
}

#[test]
fn climbing_above_the_root_is_refused() {
    let fx = Fixture::new("escape");
    let codes = fx.eval(
        r#"
load("../../../outside.gdl", "SECRET")
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"))
"#,
    );
    assert_eq!(
        codes,
        [DiagnosticCode::GdlLoadEscape],
        "an escape is a sandbox violation with its own code, not a generic eval error"
    );
}

#[test]
fn an_absolute_path_is_refused() {
    let fx = Fixture::new("absolute");
    let absolute = fx.dir.join("outside.gdl");
    let src = format!(
        r#"
load({}, "SECRET")
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"))
"#,
        serde_json::to_string(&absolute.to_string_lossy()).unwrap()
    );
    assert_eq!(fx.eval(&src), [DiagnosticCode::GdlLoadEscape]);
}

#[test]
fn a_root_relative_escape_is_refused() {
    let fx = Fixture::new("root-escape");
    // `//..` tries to climb out from the root itself.
    let codes = fx.eval(
        r#"
load("//../outside.gdl", "SECRET")
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"))
"#,
    );
    assert_eq!(codes, [DiagnosticCode::GdlLoadEscape]);
}

#[test]
fn a_fragment_is_held_to_the_same_declarative_standard() {
    let fx = Fixture::new("fragment-conditional");
    // A fragment must not be able to smuggle in a construct the file loading it
    // could not have written itself.
    fs::write(fx.root().join("sneaky.gdl"), "X = 1 if True else 2\n").expect("write sneaky");

    let codes = fx.eval(
        r#"
load("//sneaky.gdl", "X")
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"))
"#,
    );
    assert!(
        !codes.is_empty(),
        "a conditional in a fragment must be rejected"
    );
    assert!(
        codes.contains(&DiagnosticCode::GdlForbiddenConstruct),
        "a fragment conditional is GBX0103 with the fragment's own span, not a parent-file GBX0102: {codes:?}"
    );
    assert!(
        !codes.contains(&DiagnosticCode::GdlEval),
        "must not also restated as GBX0102: {codes:?}"
    );
}

#[test]
fn load_is_refused_outright_when_no_paths_are_configured() {
    // A caller evaluating a string literal has no directory for a fragment to
    // live in; resolving against the process's cwd would be worse than refusing.
    let identity = FileIdentity {
        uri: "file:///virtual/gear.gdl".to_owned(),
        source: SourceId::new("virtual").unwrap(),
        gdl_path: RelPath::new("gear.gdl").unwrap(),
        load_paths: None,
    };
    let out = GdlEngine::new().eval_gear(
        &identity,
        "load(\"//shared.gdl\", \"X\")\ngear(package = cargo(crate_name = \"c\", lib = \"c\"))\n",
    );
    assert!(out.value.is_none());
    assert!(out.diagnostics.has_errors());
}

#[test]
fn the_secret_never_becomes_reachable() {
    // The point of all of the above, stated once: no spelling of load() reaches
    // the file outside the root.
    let fx = Fixture::new("never");
    for attempt in [
        "../outside.gdl",
        "../../outside.gdl",
        "../../../outside.gdl",
        "//../outside.gdl",
        "./../outside.gdl",
        "gears/../../outside.gdl",
    ] {
        let src = format!(
            "load(\"{attempt}\", \"SECRET\")\ngear(package = cargo(crate_name = \"c\", lib = \"c\"))\n"
        );
        let codes = fx.eval(&src);
        assert!(
            !codes.is_empty(),
            "`{attempt}` was not refused -- the sandbox has a hole"
        );
        assert!(
            codes.contains(&DiagnosticCode::GdlLoadEscape)
                || codes.contains(&DiagnosticCode::GdlEval),
            "`{attempt}` produced {codes:?}"
        );
    }
    assert!(
        Path::new(&fx.dir.join("outside.gdl")).exists(),
        "sanity: the target exists"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_inside_the_root_pointing_out_of_it_is_refused() {
    // The hole a purely lexical check leaves: `link.gdl` spells a path that
    // stays inside the root, and the filesystem then hands back a file that
    // does not. Nothing about the request looks like an escape.
    let fx = Fixture::new("symlink");
    std::os::unix::fs::symlink(fx.dir.join("outside.gdl"), fx.root().join("link.gdl"))
        .expect("create symlink");

    let codes = fx.eval(
        r#"
load("//link.gdl", "SECRET")
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"))
"#,
    );
    assert_eq!(
        codes,
        [DiagnosticCode::GdlLoadEscape],
        "a symlink out of the root is a sandbox violation, not a missing file"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_to_a_directory_outside_the_root_is_refused() {
    // The same hole one level up: the fragment is a real file, but the
    // directory it sits in is the link.
    let fx = Fixture::new("symlink-dir");
    fs::create_dir_all(fx.dir.join("elsewhere")).expect("create elsewhere");
    fs::write(fx.dir.join("elsewhere/frag.gdl"), "SECRET = \"leaked\"\n").expect("write frag");
    std::os::unix::fs::symlink(fx.dir.join("elsewhere"), fx.root().join("linked"))
        .expect("symlink");

    let codes = fx.eval(
        r#"
load("//linked/frag.gdl", "SECRET")
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"))
"#,
    );
    assert_eq!(codes, [DiagnosticCode::GdlLoadEscape]);
}

#[test]
fn a_fragment_may_load_another_fragment() {
    // Nested `load()` is the case the cache and the cycle check are shared
    // across, so it needs to work before either can be trusted.
    let fx = Fixture::new("nested");
    fs::write(
        fx.root().join("outer.gdl"),
        "load(\"//shared.gdl\", \"SHARED\")\nOUTER = SHARED\n",
    )
    .expect("write outer");

    let codes = fx.eval(
        r#"
load("//outer.gdl", "OUTER")
gear(maturity = "stable", package = OUTER, name = "Demo")
"#,
    );
    assert!(codes.is_empty(), "a nested fragment should load: {codes:?}");
}

#[test]
fn a_fragment_may_not_climb_out_of_the_root_either() {
    // A nested loader resolves against the *fragment's* directory. If it also
    // reset the root, a fragment one directory down would be able to reach a
    // file the description that loaded it could not.
    let fx = Fixture::new("nested-escape");
    fs::write(
        fx.root().join("gears/demo/climber.gdl"),
        "load(\"../../../outside.gdl\", \"SECRET\")\nSTOLEN = SECRET\n",
    )
    .expect("write climber");

    let codes = fx.eval(
        r#"
load("climber.gdl", "STOLEN")
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"))
"#,
    );
    assert!(
        codes.contains(&DiagnosticCode::GdlLoadEscape),
        "a fragment's own load() is confined to the same root: {codes:?}"
    );
}

#[test]
fn a_load_cycle_is_a_diagnostic_rather_than_a_stack_overflow() {
    // Two fragments loading each other. Without a shared in-flight set this
    // recurses until the native stack runs out, which aborts the process --
    // there is no diagnostic to report from a crashed evaluator.
    let fx = Fixture::new("cycle");
    fs::write(fx.root().join("a.gdl"), "load(\"//b.gdl\", \"B\")\nA = B\n").expect("write a");
    fs::write(fx.root().join("b.gdl"), "load(\"//a.gdl\", \"A\")\nB = A\n").expect("write b");

    let codes = fx.eval(
        r#"
load("//a.gdl", "A")
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"))
"#,
    );
    assert!(
        codes.contains(&DiagnosticCode::GdlEval),
        "a cycle is an ordinary evaluation failure: {codes:?}"
    );
}

#[test]
fn one_fragment_loaded_twice_is_evaluated_once() {
    // The cache is shared across nested loaders, so a diamond -- two fragments
    // both loading a third -- must not evaluate the third twice.
    let fx = Fixture::new("diamond");
    fs::write(
        fx.root().join("left.gdl"),
        "load(\"//shared.gdl\", \"SHARED\")\nLEFT = SHARED\n",
    )
    .expect("write left");
    fs::write(
        fx.root().join("right.gdl"),
        "load(\"//shared.gdl\", \"SHARED\")\nRIGHT = SHARED\n",
    )
    .expect("write right");

    let codes = fx.eval(
        r#"
load("//left.gdl", "LEFT")
load("//right.gdl", "RIGHT")
gear(maturity = "stable", package = LEFT, name = "Demo")
"#,
    );
    assert!(codes.is_empty(), "a diamond should load cleanly: {codes:?}");
}

/// A fragment may bind names. Declaring is the loading file's job.
///
/// The rule always held, but only by accident: the fragment evaluator carried no
/// sink, so `gear()` failed inside it with `internal error: no GdlSink installed
/// on the evaluator` -- a message that blames the tool for what the file did, and
/// one a genuine wiring bug would produce word for word. What is asserted here is
/// the message and the location, not just the code, because the code was already
/// right and useless.
#[test]
fn a_fragment_may_not_declare_a_gear() {
    let fx = Fixture::new("declares");
    fs::write(
        fx.root().join("declaring.gdl"),
        "gear(maturity = \"stable\", package = cargo(crate_name = \"c\", lib = \"c\"))\n",
    )
    .expect("write declaring");

    let outcome = GdlEngine::new().eval_gear(
        &fx.identity(),
        "load(\"//declaring.gdl\", \"X\")\ngear(package = cargo(crate_name = \"c\", lib = \"c\"))\n",
    );

    let reported: Vec<_> = outcome.diagnostics.iter().collect();
    assert_eq!(reported.len(), 1, "{reported:?}");
    let d = reported[0];
    assert_eq!(d.code, DiagnosticCode::GdlEval);
    assert!(
        d.message.contains("declaring.gdl") && !d.message.contains("internal error"),
        "the fragment's author must be told it is their file: {:?}",
        d.message
    );
    assert!(
        d.location
            .as_ref()
            .is_some_and(|l| l.uri.ends_with("declaring.gdl")),
        "an editor opening this must land on the fragment: {:?}",
        d.location
    );
    assert!(
        outcome.value.is_none(),
        "the load failed, so nothing is built"
    );
}

#[test]
fn a_broken_fragment_is_a_parse_error_on_the_fragment() {
    let fx = Fixture::new("parse");
    fs::write(fx.root().join("broken.gdl"), "this is not (\n").expect("write broken");
    let outcome = GdlEngine::new().eval_gear(
        &fx.identity(),
        "load(\"//broken.gdl\", \"X\")\ngear(package = cargo(crate_name = \"c\", lib = \"c\"))\n",
    );
    assert!(
        outcome
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlParse && d.message.contains("broken.gdl")),
        "{:?}",
        outcome.diagnostics
    );
}
