//! Write gates refuse paths that look inside the workspace until they are
//! resolved, and they refuse a clone source that is not a `.gdl` the session
//! may read.
//!
//! `writable_out_root` used to canonicalize the nearest *existing* ancestor
//! and `join` the rest, including `..`. `Path::starts_with` then compared the
//! unresolved path, so `workspace/keep/missing/../../../outside` passed the
//! gate and `create_dir_all` created the escaped location.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use lsp_server::RequestId;

use super::*;
use crate::protocol::{CreateProductParams, ScaffoldGearParams};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn scratch(label: &str) -> PathBuf {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives under the workspace");
    let nth = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = repo.join(format!(
        "target/gbx-write-gate-{label}-{}-{nth}",
        std::process::id()
    ));
    drop(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn write_state(workspace: PathBuf) -> State {
    state_with_roots(workspace, &[])
}

/// A write-enabled session with source roots, for the gate that is about them.
///
/// `write_state` declares none, which is why the source-root branch of
/// `writable_out_root` went uncovered by `make check` for as long as it did: only
/// `ide/scripts/rpc-smoke.mjs` ever passed a real corpus, and that script is not
/// part of it.
fn state_with_roots(workspace: PathBuf, roots: &[PathBuf]) -> State {
    let (roots, failed_roots) = open_roots(roots);
    State {
        roots,
        registry_plans: BTreeMap::new(),
        catalogue: None,
        failed_roots,
        creation_boundary: None,
        initialized: true,
        allow_writes: true,
        workspace: Some(workspace),
        documents: BTreeMap::new(),
    }
}

/// The same session with the capability it never declared, which is the posture
/// every client starts in.
fn read_only_state(workspace: PathBuf) -> State {
    State {
        allow_writes: false,
        ..write_state(workspace)
    }
}

fn create_params(path: &Path, clone_from: Option<String>) -> CreateProductParams {
    CreateProductParams {
        path: path.display().to_string(),
        id: "demo".to_owned(),
        name: "Demo".to_owned(),
        version: "0.1.0".to_owned(),
        sources: vec![],
        profile_kind: "embedded".to_owned(),
        profile_id: "dev".to_owned(),
        clone_from,
        rebase_relative_paths: false,
        dry_run: true,
    }
}

/// `create` is judged by the declared creation boundary, not by the session.
///
/// **The bug this closes.** `writable_out_root` reads the session's roots, so
/// opening a product whose `sources` contain the directory products live in made
/// every later `create` there refuse with "is inside a source root" -- the first
/// create in a session worked and the next did not, and unchecking the source in
/// the next wizard changed nothing because the engine had already been told.
/// ADR-0013 says start-screen create runs against the workspace the engine knows
/// from boot; this is the state that makes that true.
///
/// Both directions asserted: with a boundary the create passes, and without one
/// the old refusal still stands -- the CLI declares no boundary and must keep the
/// behaviour it had.
#[test]
fn create_is_judged_by_the_creation_boundary_not_the_session() {
    let tmp = scratch("creation-boundary");
    let workspace = tmp.join("ws");
    let products = workspace.join("products");
    std::fs::create_dir_all(&products).unwrap();
    let target = products.join("demo").join("product.gdl");

    // The session has the workspace itself as a source root, which is exactly
    // what a product created with the wizard's old default declared.
    let session_roots = [workspace.clone()];

    let mut without = state_with_roots(workspace.clone(), &session_roots);
    let refused = create_product(
        &mut without,
        RequestId::from(1),
        &create_params(&target, None),
    );
    let message = match refused.response_result {
        Err(e) => e.message,
        Ok(_) => panic!("with no boundary the session's roots must still govern"),
    };
    assert!(
        message.contains("inside a source root"),
        "the session refusal must be unchanged for clients that declare no boundary: {message}"
    );

    // Same session, plus a boundary that does not contain the destination --
    // which is what the client declares from its own defaults.
    let mut with = state_with_roots(workspace.clone(), &session_roots);
    with.creation_boundary = Some(CreationBoundaryState {
        roots: vec![tmp.join("corpus")],
        workspace: Some(workspace),
    });
    let allowed = create_product(&mut with, RequestId::from(1), &create_params(&target, None));
    assert!(
        allowed.response_result.is_ok(),
        "a create inside the boundary must pass even with the session naming its parent: {:?}",
        allowed.response_result
    );

    // And the boundary is a boundary, not a bypass: a destination inside one of
    // *its* roots is still refused.
    let corpus = tmp.join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    let mut inside = state_with_roots(corpus.clone(), &[]);
    inside.creation_boundary = Some(CreationBoundaryState {
        roots: vec![corpus.clone()],
        workspace: Some(corpus.clone()),
    });
    let refused_again = create_product(
        &mut inside,
        RequestId::from(1),
        &create_params(&corpus.join("demo").join("product.gdl"), None),
    );
    let message = match refused_again.response_result {
        Err(e) => e.message,
        Ok(_) => panic!("the boundary's own roots must still refuse"),
    };
    assert!(message.contains("inside a source root"), "{message}");
}

/// Ids are checked before anything is created, and before the path is.
///
/// **Neither id was checked at all.** `create_product` stamped whatever arrived,
/// so an empty box produced `id = ""` with a destination of
/// `products//product.gdl`, and a single space produced `id = " "` in a product
/// that then opened and resolved with no diagnostics -- a product whose identity
/// is a space. The same function built a validated `SourceId` for the literal
/// `"product"` eighty lines further down, so the validator was present for the
/// value that could not be wrong and missing for the two that come from a person.
///
/// Checked before `writable_out_root` on purpose: a refusal that also created a
/// directory would be a refusal with a side effect.
#[test]
fn a_blank_or_malformed_id_is_refused_before_anything_is_written() {
    let tmp = scratch("product-id");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let target = workspace.join("products").join("demo").join("product.gdl");

    for bad in [
        "",
        " ",
        "Demo",
        "demo_product",
        "-demo",
        "demo-",
        "de--mo",
        "1demo",
    ] {
        let mut params = create_params(&target, None);
        params.id = bad.to_owned();
        let mut state = write_state(workspace.clone());
        let response = create_product(&mut state, RequestId::from(1), &params);
        let message = match response.response_result {
            Err(e) => e.message,
            Ok(_) => panic!("`{bad}` must not be accepted as a product id"),
        };
        assert!(
            message.contains("product id"),
            "`{bad}`: the refusal must name what was wrong: {message}"
        );
        assert!(
            !target.exists(),
            "`{bad}`: nothing may be written for a refused id"
        );
    }

    // The profile id goes through the same gate, on the same call.
    let mut params = create_params(&target, None);
    params.profile_id = " ".to_owned();
    let mut state = write_state(workspace.clone());
    let response = create_product(&mut state, RequestId::from(1), &params);
    let message = match response.response_result {
        Err(e) => e.message,
        Ok(_) => panic!("a blank profile id must not be accepted"),
    };
    assert!(message.contains("profile id"), "{message}");

    // And the shape the corpus already uses is accepted, so the rule is not
    // merely refusing everything.
    let mut params = create_params(&target, None);
    params.id = "payments-demo".to_owned();
    let mut state = write_state(workspace);
    let response = create_product(&mut state, RequestId::from(1), &params);
    assert!(
        response.response_result.is_ok(),
        "a kebab-case id must be accepted: {response:?}"
    );
}

/// `create` reaches its handler in a session with no source root open.
///
/// **Through `dispatch`, because the coupling was in the dispatch arm.** Every
/// other test here calls `create_product` directly, so the readiness guard on
/// the arm above it was pinned by nothing: it refused whenever `state.roots` was
/// empty, and a start-screen session that declares a creation boundary and has
/// opened no product is exactly that -- `WORKSPACE_NOT_OPEN` from the one method
/// ADR `cpt-gearbox-adr-create-product` says must not be judged by the session.
/// The boundary decides, and `initialize` is still required.
#[test]
fn create_is_dispatched_in_a_session_with_no_roots() {
    let tmp = scratch("create-dispatch");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(workspace.join("products")).unwrap();
    let target = workspace.join("products").join("demo").join("product.gdl");

    let mut state = write_state(workspace.clone());
    state.creation_boundary = Some(CreationBoundaryState {
        roots: vec![],
        workspace: Some(workspace),
    });
    assert!(
        state.roots.is_empty(),
        "the session this is about has opened nothing"
    );

    let (server, _client) = lsp_server::Connection::memory();
    let request = lsp_server::Request {
        id: RequestId::from(1),
        method: crate::protocol::method::PRODUCT_CREATE.to_owned(),
        params: serde_json::to_value(create_params(&target, None)).expect("params serialize"),
    };
    let response = dispatch(&server, &mut state, request).expect("create answers its own request");
    assert!(
        response.response_result.is_ok(),
        "a boundary-judged create must not be refused for the session's empty roots: {:?}",
        response.response_result
    );

    // And the other guard still stands: `initialize` comes first.
    let mut fresh = write_state(tmp.join("ws"));
    fresh.initialized = false;
    let request = lsp_server::Request {
        id: RequestId::from(2),
        method: crate::protocol::method::PRODUCT_CREATE.to_owned(),
        params: serde_json::to_value(create_params(&target, None)).expect("params serialize"),
    };
    let refused = dispatch(&server, &mut fresh, request).expect("a refusal is still an answer");
    match refused.response_result {
        Err(e) => assert_eq!(e.code, error_code::NOT_INITIALIZED, "{}", e.message),
        Ok(_) => panic!("`initialize` must still come first"),
    }
}

/// A fresh install has no source root configured, and that is an empty
/// catalogue rather than a refusal the Studio would show as a dead engine.
#[test]
fn a_catalogue_load_with_no_roots_answers_empty() {
    let tmp = scratch("catalogue-no-roots");
    let mut state = write_state(tmp.join("ws"));
    assert!(state.roots.is_empty() && state.failed_roots.is_empty());

    let (server, client) = lsp_server::Connection::memory();
    let request = lsp_server::Request {
        id: RequestId::from(1),
        method: crate::protocol::method::CATALOGUE_LOAD.to_owned(),
        params: serde_json::Value::Null,
    };
    let response = dispatch(&server, &mut state, request).or_else(|| {
        client
            .receiver
            .try_iter()
            .find_map(|message| match message {
                lsp_server::Message::Response(response) => Some(response),
                _ => None,
            })
    });
    let response = response.expect("the load answers");
    assert!(
        response.response_result.is_ok(),
        "no root is nothing to read, not a refusal: {:?}",
        response.response_result
    );
}

/// A session that declared no write capability is refused, dry run or not.
///
/// **`dry_run` used to be a way past this gate.** `create_product` and
/// `scaffold_gear` both read `!state.allow_writes && !params.dry_run`, so a
/// read-only session reached everything after it: `create`'s clone branch read
/// the source `.gdl` and handed its whole text back in `after`, and `scaffold`
/// reached `writable_out_root` and `out_root.exists()`, which answers "does this
/// path exist" and "is it inside a source root" through the refusal text.
/// `edit_gear` and `edit_with` refuse the dry run for exactly that reason, and
/// the gate is one function now so the five copies cannot diverge again.
#[test]
fn a_dry_run_is_still_refused_without_write_capability() {
    let tmp = scratch("dry-run-gate");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    // A real description, so the clone branch would have something to hand back:
    // a source that does not evaluate would be refused for that instead, and the
    // test would pass without the gate.
    let source = workspace.join("source.gdl");
    let secret =
        gearbox_gdl::edit::render_product_template(&gearbox_gdl::edit::CreateProductParams {
            id: "secret-product".to_owned(),
            name: "Confidential Sauce".to_owned(),
            version: "0.1.0".to_owned(),
            sources: vec![],
            profile_kind: "embedded".to_owned(),
            profile_id: "dev".to_owned(),
        });
    std::fs::write(&source, &secret).unwrap();

    let mut state = read_only_state(workspace.clone());
    let cloned = create_product(
        &mut state,
        RequestId::from(1),
        &create_params(
            &workspace.join("demo").join("product.gdl"),
            Some(source.display().to_string()),
        ),
    );
    let message = match cloned.response_result {
        Err(e) => {
            assert_eq!(e.code, error_code::WRITES_NOT_ALLOWED);
            e.message
        }
        Ok(value) => panic!("a read-only session must not read a description back: {value}"),
    };
    assert!(
        !message.contains("Confidential Sauce"),
        "the refusal must carry nothing out of the file: {message}"
    );

    // The same for the scaffold, whose refusals answer questions about the
    // filesystem: this destination does not exist, and a read-only session must
    // not learn that either.
    let scaffolded = scaffold_gear(
        &mut state,
        RequestId::from(1),
        &ScaffoldGearParams {
            id: "payments-audit".to_owned(),
            name: "Payments Audit".to_owned(),
            version: "0.1.0".to_owned(),
            kind: crate::protocol::GearKind::Minimal,
            plugin: None,
            destination_dir: tmp.join("does-not-exist").display().to_string(),
            dry_run: true,
        },
    );
    match scaffolded.response_result {
        Err(e) => assert_eq!(e.code, error_code::WRITES_NOT_ALLOWED, "{}", e.message),
        Ok(value) => panic!("a read-only session must not probe destinations: {value}"),
    }

    // And with the capability declared, the same dry run is answered -- so the
    // gate is about the capability and not about the dry run.
    let mut allowed = write_state(workspace.clone());
    let response = create_product(
        &mut allowed,
        RequestId::from(1),
        &create_params(
            &workspace.join("demo").join("product.gdl"),
            Some(source.display().to_string()),
        ),
    );
    assert!(
        response.response_result.is_ok(),
        "a declared session's dry run still works: {response:?}"
    );
}

/// The version is checked with the two ids, before anything is written.
///
/// It was the one wizard field with nothing checking it: rendered into the
/// template or stamped onto a clone, and `product()` does not check it either, so
/// whatever arrived reached a written file. `scaffold_gear_files` has always
/// required a triple of the same field.
#[test]
fn a_malformed_version_is_refused() {
    let tmp = scratch("product-version");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let target = workspace.join("products").join("demo").join("product.gdl");

    for bad in ["", " ", "0.1", "1.0.0-rc1", "latest", "0.1.0.0"] {
        let mut params = create_params(&target, None);
        params.version = bad.to_owned();
        let mut state = write_state(workspace.clone());
        let response = create_product(&mut state, RequestId::from(1), &params);
        let message = match response.response_result {
            Err(e) => e.message,
            Ok(_) => panic!("`{bad}` must not be accepted as a product version"),
        };
        assert!(
            message.contains("semver triple"),
            "`{bad}`: the refusal must name what a version is: {message}"
        );
        assert!(!target.exists(), "`{bad}`: nothing may be written");
    }

    let mut params = create_params(&target, None);
    params.version = "1.2.3".to_owned();
    let mut state = write_state(workspace);
    let response = create_product(&mut state, RequestId::from(1), &params);
    assert!(
        response.response_result.is_ok(),
        "a semver triple must be accepted: {response:?}"
    );
}

#[test]
fn missing_dir_plus_dotdot_is_outside_the_workspace() {
    let tmp = scratch("dotdot");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(workspace.join("keep")).unwrap();
    let escaped = tmp.join("pwned");
    let attack = workspace
        .join("keep")
        .join("missing")
        .join("..")
        .join("..")
        .join("..")
        .join("pwned");

    let state = write_state(workspace);
    let err = writable_out_root(&state, &attack).expect_err("must refuse a lexical escape");
    assert!(err.contains("outside the declared workspace"), "{err}");
    assert!(
        !escaped.exists(),
        "the gate must not create the escaped location"
    );
}

/// A source root beside the workspace gets the source-root refusal, not the
/// workspace one.
///
/// **Both rules apply and the specific one has to win.** The corpus is a
/// *sibling* of this repository -- `products/payments-demo` names
/// `../../../gears-rust` -- so generating into it is simultaneously "outside the
/// declared workspace" and "inside a source root". The second sentence is the one
/// a person can act on: it names the rule (ADR-0010 tier 5, do not write next to
/// human-authored crates) instead of describing a boundary they did not cross on
/// purpose.
///
/// `writable_out_root` said so in a comment and did the opposite: the workspace
/// check returned first, which made its own source-root loop unreachable for
/// exactly the layout the comment names. `ide/scripts/rpc-smoke.mjs` asserted the
/// intent and had been failing on it; this is the same claim where `make check`
/// can see it.
#[test]
fn a_source_root_beside_the_workspace_gets_the_specific_refusal() {
    let tmp = scratch("out-in-sibling-source-root");
    let workspace = tmp.join("ws");
    let corpus = tmp.join("gears-rust");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(corpus.join("gears")).unwrap();

    let state = state_with_roots(workspace, std::slice::from_ref(&corpus));
    let err = writable_out_root(&state, &corpus.join("generated"))
        .expect_err("generation must not write into a source root");
    assert!(
        err.contains("inside a source root"),
        "the refusal must name the rule, not the boundary: {err}"
    );
}

/// The other order still holds: outside everything is a workspace refusal.
///
/// The pair matters. Making the source-root sentence win must not turn every
/// out-of-workspace path into one, and this is the case that would catch it --
/// a directory that is outside the workspace and inside no source root.
#[test]
fn a_root_outside_everything_is_still_a_workspace_refusal() {
    let tmp = scratch("out-outside-everything");
    let workspace = tmp.join("ws");
    let corpus = tmp.join("gears-rust");
    let elsewhere = tmp.join("elsewhere");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(corpus.join("gears")).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();

    let state = state_with_roots(workspace, &[corpus]);
    let err = writable_out_root(&state, &elsewhere.join("generated"))
        .expect_err("generation must stay inside the workspace");
    assert!(err.contains("outside the declared workspace"), "{err}");
}

#[test]
fn a_new_directory_inside_the_workspace_is_allowed() {
    let tmp = scratch("inside");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let wanted = workspace.join("brand").join("new");
    let state = write_state(workspace.clone());
    let resolved = writable_out_root(&state, &wanted).expect("missing dirs under the workspace");
    assert!(resolved.starts_with(workspace.canonicalize().unwrap()));
    assert!(!wanted.exists(), "the gate resolves; it does not create");
}

/// A product's templates directory is written into the product, so it is held to
/// the workspace: generation may create it there, and nowhere else.
#[test]
fn a_products_templates_directory_is_written_only_inside_the_workspace() {
    let tmp = scratch("templates");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(workspace.join("products/p")).unwrap();
    let state = write_state(workspace.clone());
    writable_templates_dir(&state, &workspace.join("products/p/templates"))
        .expect("a templates directory beside a product in the workspace");

    let elsewhere = tmp.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let err = writable_templates_dir(&state, &elsewhere.join("templates"))
        .expect_err("a declared templates directory outside the workspace");
    assert!(err.contains("outside the declared workspace"), "{err}");
}

/// A folder the person chose outside the workspace is a generation target when it
/// is new, holds only dotfiles, or was generated before, and nothing else is.
///
/// The Studio's Generate view sends `out` when someone picks a folder, typically
/// the repository the product ships from (ADR-0010, Amendment 2026-09-29).
/// Without `out` the workspace rule stands, and it is asserted here too, so the
/// widening cannot leak into the default.
#[test]
fn an_explicit_out_outside_the_workspace_is_new_or_gearboxs() {
    let tmp = scratch("standalone-out");
    let workspace = tmp.join("ws");
    let corpus = tmp.join("gears-rust");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(corpus.join("gears")).unwrap();
    let state = state_with_roots(workspace, std::slice::from_ref(&corpus));
    let standalone = |path: &Path| match generation_out_root(&state, path, true) {
        Ok(OutRoot::Standalone(root)) => Ok(root),
        Ok(OutRoot::Workspace(root)) => panic!("{} judged as the workspace", root.display()),
        Err(refusal) => Err(refusal),
    };

    // New, and new under a folder that does not exist yet.
    let fresh = tmp.join("app");
    assert!(standalone(&fresh).is_ok());
    assert!(standalone(&tmp.join("repos").join("app")).is_ok());

    // A fresh `git init` is empty: dotfiles do not make a folder somebody's.
    let repo = tmp.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    assert!(standalone(&repo).is_ok(), "a fresh repository is a target");

    // A folder with a file of its own and no lock is refused, and says which file.
    let theirs = tmp.join("theirs");
    std::fs::create_dir_all(&theirs).unwrap();
    std::fs::write(theirs.join("notes.txt"), "mine").unwrap();
    let err = standalone(&theirs).expect_err("somebody's folder");
    assert!(
        err.contains("notes.txt") && err.contains("product.lock"),
        "{err}"
    );

    // ...until Gearbox generated it: a second run goes to the same place.
    std::fs::write(theirs.join("product.lock"), "").unwrap();
    assert!(standalone(&theirs).is_ok(), "a regeneration is allowed");

    // Tier 5 holds outside the workspace as it does inside it.
    let err = standalone(&corpus.join("generated")).expect_err("a source root");
    assert!(err.contains("inside a source root"), "{err}");

    // A `..` in the part that does not exist would land somewhere it was not
    // judged.
    let err = standalone(&tmp.join("missing").join("..").join("gears-rust"))
        .expect_err("a lexical escape");
    assert!(err.contains("walks out"), "{err}");

    // Relative is not a choice anybody made in a dialog.
    assert!(standalone(Path::new("relative/app")).is_err());

    // And without `out`, outside the workspace is still refused.
    let Err(refusal) = generation_out_root(&state, &fresh, false) else {
        panic!("the default must stay inside the workspace");
    };
    assert!(
        refusal.contains("outside the declared workspace"),
        "{refusal}"
    );
}

/// A standalone root keeps its merge base inside itself.
///
/// `base_root_for` answers `<out>/../.base`, which for a repository root is a
/// folder beside it in somebody's home directory.
#[test]
fn a_standalone_root_keeps_its_base_inside() {
    let out = Path::new("/home/someone/app");
    assert_eq!(
        gearbox_engine::generate::standalone_base_root(out),
        out.join(".gearbox").join("base")
    );
}

/// A clone source that is not a description is refused for being one, and
/// refused before it is read.
///
/// **The source sits inside the workspace on purpose.** It used to sit outside,
/// where it failed two gates at once, and the assertion accepted either
/// refusal -- so deleting the extension check in `writable_path` left this test
/// green on the message the next gate produced, which is the one thing the test
/// is named for. Inside the workspace, only the `.gdl` rule can refuse it.
#[test]
fn clone_from_a_non_gdl_path_is_refused_before_read() {
    let tmp = scratch("clone-passwd");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let inside = workspace.join("secret.txt");
    std::fs::write(&inside, "not a description\n").unwrap();

    let mut state = write_state(workspace.clone());
    let dest = workspace.join("product.gdl");
    let response = create_product(
        &mut state,
        RequestId::from(1),
        &create_params(&dest, Some(inside.display().to_string())),
    );
    let message = match response.response_result {
        Err(e) => e.message,
        Ok(_) => panic!("must refuse"),
    };
    assert!(
        message.contains("is not a `.gdl` description"),
        "the extension gate is the one that must refuse this, not the boundary: {message}"
    );
    // And before the read: the destination was never written, and nothing in the
    // refusal came out of the file.
    assert!(!dest.exists(), "a refused create must write nothing");
    assert!(
        !message.contains("not a description"),
        "the file's own text must not appear in the refusal: {message}"
    );
}

/// A clone written one level deeper than its source still names the same
/// directories.
///
/// **The defect this closes, measured in the browser:** Clone Local copied
/// `path("../../../gears-rust")` verbatim into a folder one level deeper, the
/// description evaluated -- so the preview and the write both looked fine -- and
/// then could not be opened, because its only source root did not exist.
#[test]
fn a_clone_elsewhere_re_bases_its_relative_paths() {
    let tmp = scratch("clone-rebase");
    let workspace = tmp.join("ws");
    let source_dir = workspace.join("products").join("demo");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(workspace.join("corpus")).unwrap();
    let source = source_dir.join("product.gdl");
    std::fs::write(
        &source,
        r#"# the corpus is two levels up
product(
    id = "demo",
    name = "Demo",
    version = "0.1.0",
    sources = [source(id = "corpus", at = path("../../corpus"))],
    profiles = [
        embedded(id = "dev"),
        self_hosted(id = "local", host = "gateway", worker_discovery = "directory",
                    target_dir = "../../corpus/target"),
    ],
    default_profile = "dev",
    gears = [],
)
"#,
    )
    .unwrap();

    let mut state = write_state(workspace.clone());
    let dest = workspace
        .join("elsewhere")
        .join("deeper")
        .join("clone")
        .join("product.gdl");
    let response = create_product(
        &mut state,
        RequestId::from(1),
        &CreateProductParams {
            rebase_relative_paths: true,
            ..create_params(&dest, Some(source.display().to_string()))
        },
    );
    let after = match response.response_result {
        Ok(value) => value["after"].as_str().unwrap().to_owned(),
        Err(e) => panic!("the clone must be accepted: {} {:?}", e.message, e.data),
    };
    assert!(
        after.contains(r#"at = path("../../../corpus")"#),
        "the source must still name `ws/corpus` from three levels down:\n{after}"
    );
    assert!(
        after.contains(r#"target_dir = "../../../corpus/target""#),
        "{after}"
    );
    assert!(after.contains("# the corpus is two levels up"), "{after}");
    assert!(!dest.exists(), "a dry run writes nothing");
}

/// Without the flag the clone keeps each path as written.
///
/// That is the git clone's case: its source is a temporary checkout whose folder
/// means nothing, and re-basing from it named `<workspace>/gears-rust` -- a
/// directory that does not exist -- which the Clone Git claim caught.
#[test]
fn a_clone_that_does_not_ask_keeps_the_spelling() {
    let tmp = scratch("clone-keep");
    let workspace = tmp.join("ws");
    let source_dir = workspace
        .join(".gearbox")
        .join("git-clones")
        .join("attempt");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("product.gdl");
    std::fs::write(
        &source,
        "product(id = \"demo\", name = \"Demo\", version = \"0.1.0\", \
         sources = [source(id = \"corpus\", at = path(\"../../../corpus\"))], \
         profiles = [embedded(id = \"dev\")], default_profile = \"dev\", gears = [])\n",
    )
    .unwrap();

    let mut state = write_state(workspace.clone());
    let dest = workspace.join("products").join("demo").join("product.gdl");
    let response = create_product(
        &mut state,
        RequestId::from(1),
        &create_params(&dest, Some(source.display().to_string())),
    );
    let after = match response.response_result {
        Ok(value) => value["after"].as_str().unwrap().to_owned(),
        Err(e) => panic!("the clone must be accepted: {}", e.message),
    };
    assert!(after.contains(r#"path("../../../corpus")"#), "{after}");
}

/// A relative `load` names a fragment a clone does not copy, so a clone into
/// another folder is refused -- before anything is written -- rather than written
/// and then found not to evaluate.
#[test]
fn a_clone_elsewhere_with_a_relative_load_is_refused() {
    let tmp = scratch("clone-load");
    let workspace = tmp.join("ws");
    let source_dir = workspace.join("products").join("demo");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("product.gdl");
    std::fs::write(
        &source,
        "load(\"common.gdl\", \"SOURCES\")\nproduct(id = \"demo\", name = \"Demo\", version = \"0.1.0\", \
         sources = SOURCES, profiles = [embedded(id = \"dev\")], default_profile = \"dev\", \
         gears = [])\n",
    )
    .unwrap();

    let mut state = write_state(workspace.clone());
    let dest = workspace.join("elsewhere").join("product.gdl");
    let response = create_product(
        &mut state,
        RequestId::from(1),
        &CreateProductParams {
            rebase_relative_paths: true,
            ..create_params(&dest, Some(source.display().to_string()))
        },
    );
    let message = match response.response_result {
        Err(e) => e.message,
        Ok(_) => panic!("a relative load cannot follow a clone elsewhere"),
    };
    assert!(message.contains("loads `common.gdl`"), "{message}");
    assert!(!dest.exists(), "a refused create must write nothing");

    // A clone that keeps its spelling -- the git clone -- does not copy the
    // fragment either, so it is refused the same way.
    let kept = create_product(
        &mut state,
        RequestId::from(2),
        &create_params(&dest, Some(source.display().to_string())),
    );
    let message = match kept.response_result {
        Err(e) => e.message,
        Ok(_) => panic!("a relative load cannot follow a git clone elsewhere either"),
    };
    assert!(message.contains("loads `common.gdl`"), "{message}");
}

#[test]
fn clone_from_a_gdl_outside_the_workspace_is_refused() {
    let tmp = scratch("clone-outside");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let outside = tmp.join("evil.gdl");
    std::fs::write(&outside, "product(id = \"x\")\n").unwrap();

    let mut state = write_state(workspace.clone());
    let dest = workspace.join("product.gdl");
    let response = create_product(
        &mut state,
        RequestId::from(1),
        &create_params(&dest, Some(outside.display().to_string())),
    );
    let message = match response.response_result {
        Err(e) => e.message,
        Ok(_) => panic!("must refuse"),
    };
    assert!(
        message.contains("outside the declared workspace")
            || message.contains("outside the declared workspace and every source root"),
        "{message}"
    );
}

#[test]
fn scaffold_refuses_a_dotdot_id() {
    let tmp = scratch("scaffold-dotdot");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let dest = workspace.join("gears");
    std::fs::create_dir_all(&dest).unwrap();

    let mut state = write_state(workspace);
    let response = scaffold_gear(
        &mut state,
        RequestId::from(1),
        &ScaffoldGearParams {
            id: "..".to_owned(),
            name: "Escape".to_owned(),
            version: "0.1.0".to_owned(),
            kind: crate::protocol::GearKind::Minimal,
            plugin: None,
            destination_dir: dest.display().to_string(),
            dry_run: false,
        },
    );
    let message = match response.response_result {
        Err(e) => e.message,
        Ok(_) => panic!("must refuse `..`"),
    };
    assert!(message.contains("kebab-case"), "{message}");
    assert!(
        !dest.join("Cargo.toml").exists(),
        "parent dest must stay unchanged"
    );
}

/// The wire shape the Studio client actually sends, deserialised.
///
/// A round trip through the type is not the same as a round trip through the
/// *wire*: `plugin` arriving under a name serde does not expect deserialises to
/// `None` and the scaffold silently writes the commented shape -- which is a
/// working feature and a broken one that look identical in a preview.
#[test]
fn the_clients_scaffold_request_carries_its_plugin() {
    let json = r#"{
        "id": "ldap-authn-plugin",
        "name": "LDAP AuthN",
        "version": "0.1.0",
        "kind": "plugin",
        "plugin": {
            "spec": "cf.core.authn_resolver.plugin.v1~",
            "trait_ident": "AuthNResolverPluginClient",
            "crate_name": "cf-gears-authn-resolver-sdk",
            "lib_ident": "authn_resolver_sdk",
            "path": "../../authn-resolver-sdk"
        },
        "destination_dir": "/tmp/gears",
        "dry_run": true
    }"#;
    let params: ScaffoldGearParams = serde_json::from_str(json).expect("the client's shape parses");
    assert_eq!(params.kind, crate::protocol::GearKind::Plugin);
    let plugin = params.plugin.expect("`plugin` survived the wire");
    assert_eq!(plugin.lib_ident, "authn_resolver_sdk");
    assert_eq!(plugin.spec, "cf.core.authn_resolver.plugin.v1~");
}

/// A plugin scaffold given a host writes `implements` live, and it still evaluates.
///
/// The commented shape exists because a spec no described gear declares is
/// GBX0519. A host picked out of a loaded catalogue is not that -- but writing
/// the declaration live means writing GDL from strings that came off the wire,
/// and this is the gate that says the result still parses as a gear.
#[test]
fn a_plugin_scaffold_with_a_host_writes_a_live_implements() {
    use crate::protocol::GearKind;

    let files = super::scaffold_gear_files(
        &ScaffoldGearParams {
            id: "ldap-authn-plugin".to_owned(),
            name: "LDAP AuthN".to_owned(),
            version: "0.1.0".to_owned(),
            kind: GearKind::Plugin,
            plugin: Some(authn_point("../../authn-resolver-sdk")),
            destination_dir: "/tmp".to_owned(),
            dry_run: true,
        },
        None,
        None,
    )
    .expect("the shape renders");
    let gdl = gear_gdl(&files);

    // **Live, not commented, and checked over lines rather than by `contains`.**
    // The commented shape's line is `# implements = ...`, which contains `implements =`.
    let live = |needle: &str| {
        gdl.lines()
            .any(|line| line.trim_start().starts_with(needle))
    };
    assert!(
        live(r#"implements = "cf.core.authn_resolver.plugin.v1~""#),
        "the declaration is still commented: {gdl}"
    );
    assert!(!live("sdk = cargo("), "a plugin declares no sdk: {gdl}");
    assert!(
        gdl.contains("AuthNResolverPluginClient") && gdl.contains("cf-gears-authn-resolver-sdk"),
        "the trait and its crate are named for the author: {gdl}"
    );
    assert!(
        evaluates(gdl),
        "a live implements must still evaluate: {gdl}"
    );
}

fn authn_point(path: &str) -> crate::protocol::PluginScaffold {
    crate::protocol::PluginScaffold {
        spec: "cf.core.authn_resolver.plugin.v1~".to_owned(),
        trait_ident: "AuthNResolverPluginClient".to_owned(),
        crate_name: "cf-gears-authn-resolver-sdk".to_owned(),
        lib_ident: "authn_resolver_sdk".to_owned(),
        path: path.to_owned(),
    }
}

fn gear_gdl(files: &[super::ScaffoldFile]) -> &str {
    &files
        .iter()
        .find(|(rel, _, _)| rel.as_str() == "gear.gdl")
        .expect("a gear.gdl")
        .1
}

fn evaluates(gdl: &str) -> bool {
    let identity = gearbox_gdl::FileIdentity {
        uri: "file:///tmp/ldap-authn-plugin/gear.gdl".to_owned(),
        source: gearbox_ir::SourceId::new("scaffold").expect("kebab"),
        gdl_path: gearbox_ir::RelPath::new("gear.gdl").expect("valid"),
        load_paths: None,
    };
    gearbox_gdl::GdlEngine::new()
        .eval_gear(&identity, gdl)
        .value
        .is_some()
}

/// Wire values shown in comments cannot break out of them.
///
/// The crate name, path and trait are written into `#` comments, where escaping
/// is not the hazard -- a line break is. A path carrying one would end the
/// comment and put the rest on its own line, evaluated as GDL; this one would
/// declare a second `implements` and a `category` nobody chose.
#[test]
fn a_plugin_scaffold_keeps_wire_values_inside_their_comments() {
    use crate::protocol::GearKind;

    for path in [
        r"..\shared\authn-resolver-sdk",
        "../sdk\nimplements = \"x.injected.plugin.v1~\",\ncategory = \"oss\",",
    ] {
        let files = super::scaffold_gear_files(
            &ScaffoldGearParams {
                id: "ldap-authn-plugin".to_owned(),
                name: "LDAP AuthN".to_owned(),
                version: "0.1.0".to_owned(),
                kind: GearKind::Plugin,
                plugin: Some(authn_point(path)),
                destination_dir: "/tmp".to_owned(),
                dry_run: true,
            },
            None,
            None,
        )
        .expect("the shape renders");
        let gdl = gear_gdl(&files);
        assert!(evaluates(gdl), "`{path}` must stay a comment: {gdl}");
        assert!(
            !gdl.lines()
                .any(|l| l.trim_start().starts_with("implements = \"x.injected")),
            "nothing escaped its comment: {gdl}"
        );
        assert!(
            !gdl.lines()
                .any(|l| l.trim_start().starts_with("category =")),
            "nothing escaped its comment: {gdl}"
        );
    }
}

/// Without a host, the locator stays a comment.
///
/// The other half of the same decision, asserted so that "absent keeps the old
/// behaviour" is a check rather than a promise in a doc comment.
#[test]
fn a_plugin_scaffold_without_a_host_keeps_the_commented_locator() {
    use crate::protocol::GearKind;

    let files = super::scaffold_gear_files(
        &ScaffoldGearParams {
            id: "ldap-authn-plugin".to_owned(),
            name: "LDAP AuthN".to_owned(),
            version: "0.1.0".to_owned(),
            kind: GearKind::Plugin,
            plugin: None,
            destination_dir: "/tmp".to_owned(),
            dry_run: true,
        },
        None,
        None,
    )
    .expect("the shape renders");

    let gdl = &files
        .iter()
        .find(|(rel, _, _)| rel.as_str() == "gear.gdl")
        .expect("a gear.gdl")
        .1;
    assert!(gdl.contains("# implements = "), "{gdl}");
    for line in gdl.lines() {
        assert!(
            !line.trim_start().starts_with("implements ="),
            "an uncommented implements with no host to point at: {gdl}"
        );
    }
}

/// Every shape evaluates, and each one offers what its kind needs.
///
/// The gate this guards is the scaffold's own: `scaffold_gear` evaluates the
/// `gear.gdl` it just rendered and refuses if it does not parse as a gear
/// description. Three shapes mean three chances to write a file that does not --
/// and the shapes are mostly *comments*, so a stray `#` or an unbalanced paren
/// would be caught here and nowhere else until somebody used it.
#[test]
fn every_scaffold_shape_evaluates_and_carries_its_own_hints() {
    use crate::protocol::GearKind;

    for kind in [GearKind::Minimal, GearKind::Service, GearKind::Plugin] {
        let files = super::scaffold_gear_files(
            &ScaffoldGearParams {
                id: "payments-audit".to_owned(),
                name: "Payments Audit".to_owned(),
                version: "0.1.0".to_owned(),
                kind,
                plugin: None,
                destination_dir: "/tmp".to_owned(),
                dry_run: true,
            },
            None,
            None,
        )
        .expect("the shape renders");

        let gdl = &files
            .iter()
            .find(|(rel, _, _)| rel.as_str() == "gear.gdl")
            .expect("a gear.gdl")
            .1;
        let lib = &files
            .iter()
            .find(|(rel, _, _)| rel.as_str() == "src/lib.rs")
            .expect("a lib.rs")
            .1;

        let identity = gearbox_gdl::FileIdentity {
            uri: "file:///scaffold/gear.gdl".to_owned(),
            source: SourceId::new("scaffold").expect("kebab"),
            gdl_path: RelPath::new("gear.gdl").expect("valid"),
            load_paths: None,
        };
        let outcome = gearbox_gdl::GdlEngine::new().eval_gear(&identity, gdl);
        assert!(
            outcome.value.is_some(),
            "{kind:?} does not evaluate: {:?}",
            outcome.diagnostics
        );

        // Every shape names the crate and carries the configuration hint.
        assert!(gdl.contains(r#"crate_name = "payments-audit""#), "{kind:?}");
        assert!(gdl.contains("config_schema = config(exposes"), "{kind:?}");

        // Shapes are comments until there is something true to write. Live
        // placeholders fail load (sdk) or diagnostics (fills naming no
        // declared point / invented category), so every hint must stay behind
        // `#`.
        for line in gdl.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                continue;
            }
            assert!(
                !trimmed.starts_with("config_schema =")
                    && !trimmed.starts_with("sdk = ")
                    && !trimmed.starts_with("implements")
                    && !trimmed.starts_with("provides =")
                    && !trimmed.starts_with("consumes =")
                    && !trimmed.starts_with("description =")
                    && !trimmed.starts_with("category =")
                    && !trimmed.starts_with("visibility ="),
                "{kind:?} writes `{trimmed}` as a value, and it has nothing true to put there"
            );
        }

        match kind {
            GearKind::Minimal => {
                // Nothing beyond the crate, the name and that one hint: this is
                // the shape the method wrote before kinds existed.
                assert!(!gdl.contains("sdk = cargo"), "{kind:?}");
                assert!(!gdl.contains("provides = ["), "{kind:?}");
                assert!(!lib.contains("impl Gear"), "{kind:?}");
            }
            GearKind::Service => {
                assert!(gdl.contains("provides = [provide("), "{kind:?}");
                assert!(gdl.contains("consumes = [consume("), "{kind:?}");
                assert!(!gdl.contains("implements ="), "{kind:?}");
                // Doc in the stub, not code -- the toolkit path is unknown here.
                assert!(lib.contains("impl Gear"), "{kind:?}");
                assert!(
                    lib.lines().all(|line| {
                        let t = line.trim_start();
                        t.is_empty() || t.starts_with("//")
                    }),
                    "{kind:?} lib stub must stay comments only"
                );
            }
            GearKind::Plugin => {
                // The declaration that makes it a plugin, commented until a
                // host is chosen.
                assert!(gdl.contains("# implements = "), "{kind:?}");
                assert!(lib.contains("GBX0518"), "{kind:?}");
            }
        }
    }
}

/// The two `cluster_profile(...)` entries `payments-demo` has, minimised.
const TWO_SCOPES: &str = r#"product(
    id = "p",
    name = "P",
    version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [embedded(id = "dev"), kubernetes(id = "prod", discovery = "static",
                namespace = "n", image_registry = "r")],
    default_profile = "dev",
    gears = [use_gear("g", source = "s")],
    cluster_profiles = [
        cluster_profile(name = "event-broker", cache = provider("standalone"), profiles = ["dev"]),
        cluster_profile(name = "event-broker", profiles = ["prod"],
                        cache = provider("postgres", schema = "cluster")),
    ],
)
"#;

/// **The document version, which a written position cannot stand in for.**
///
/// A provider option is addressed by where it is written, and two entries here
/// share a name -- so position 1 is meaningful only against the text the preview
/// was computed from. If the file changed underneath, position 1 may be a
/// different entry, or the same entry with different options. `expected_before`
/// is what refuses that, before a single edit is applied, and it is checked on
/// the engine rather than in the browser: a client that forgot to re-check would
/// otherwise write against text nobody reviewed.
#[test]
fn a_stale_document_is_refused_before_any_edit_is_applied() {
    let dir = scratch("stale-doc");
    let path = dir.join("product.gdl");
    std::fs::write(&path, TWO_SCOPES).expect("write");
    let mut state = write_state(dir);

    let edits = vec![ProductEdit::SetProviderOption {
        scope: "event-broker".to_owned(),
        entry_index: 1,
        primitive: "cache".to_owned(),
        key: "pool_max_size".to_owned(),
        value: Some(gearbox_ir::ConfigValue::Int(10)),
    }];

    // The text the caller believed it was editing, which is not what is there.
    let outdated = TWO_SCOPES.replace("schema = \"cluster\"", "schema = \"other\"");
    let refused = edit_apply_edits(
        &mut state,
        RequestId::from(1),
        &ApplyEditsParams {
            expected_before: Some(outdated),
            path: path.display().to_string(),
            dry_run: false,
            edits: edits.clone(),
        },
    );
    let rendered = format!("{refused:?}");
    assert!(
        rendered.contains("the document changed after preview"),
        "a stale document was written against: {rendered}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        TWO_SCOPES,
        "the file was modified despite the refusal"
    );

    // And with the text it really holds, the same edit lands -- on the entry the
    // position names, leaving the one before it alone.
    let accepted = edit_apply_edits(
        &mut state,
        RequestId::from(2),
        &ApplyEditsParams {
            expected_before: Some(TWO_SCOPES.to_owned()),
            path: path.display().to_string(),
            dry_run: false,
            edits,
        },
    );
    let rendered = format!("{accepted:?}");
    assert!(
        !rendered.contains("the document changed"),
        "the honest document was refused: {rendered}"
    );
    let written = std::fs::read_to_string(&path).expect("read");
    assert!(
        written.contains("pool_max_size = 10"),
        "the option was not written:\n{written}"
    );
    assert!(
        written.contains(r#"cache = provider("standalone"), profiles = ["dev"]"#),
        "the other entry with the same name was rewritten:\n{written}"
    );
}

/// A relative destination is refused by name, for a product and for a gear.
///
/// Both used to be joined to the engine's working directory -- wherever the
/// backend was started -- so a preview of `relative/dir/product.gdl` looked
/// fine and the write would have landed inside the IDE's own package.
#[test]
fn a_relative_destination_is_refused_for_a_product_and_a_gear() {
    let tmp = scratch("relative-dest");
    let workspace = tmp.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let mut state = write_state(workspace);
    let product = create_product(
        &mut state,
        RequestId::from(1),
        &create_params(Path::new("relative/dir/product.gdl"), None),
    );
    let message = match product.response_result {
        Err(e) => e.message,
        Ok(_) => panic!("a relative product path must be refused"),
    };
    assert!(message.contains("is not an absolute path"), "{message}");

    let gear = scaffold_gear(
        &mut state,
        RequestId::from(2),
        &ScaffoldGearParams {
            id: "rel-gear".to_owned(),
            name: "Rel".to_owned(),
            version: "0.1.0".to_owned(),
            kind: crate::protocol::GearKind::Minimal,
            plugin: None,
            destination_dir: "relative/gears".to_owned(),
            dry_run: true,
        },
    );
    let message = match gear.response_result {
        Err(e) => e.message,
        Ok(_) => panic!("a relative gear destination must be refused"),
    };
    assert!(message.contains("is not an absolute path"), "{message}");
}

/// A description that cannot be read is refused with its diagnostic.
///
/// Regressed when `product/load` started reading the file itself (b63bb7c): the
/// refusal went out with a bare I/O message and no `data`, and `rpc-smoke`
/// said so for a week.
#[test]
fn a_product_that_cannot_be_read_is_refused_with_its_diagnostic() {
    let response = product_load(
        RequestId::from(1),
        &crate::protocol::ProductLoadParams {
            path: "/definitely/absent/product.gdl".to_owned(),
        },
    );
    let Err(refusal) = response.response_result else {
        panic!("an absent description must be refused");
    };
    assert_eq!(refusal.code, error_code::PRODUCT_LOAD_FAILED);
    let diagnostics = refusal
        .data
        .as_ref()
        .and_then(|d| d["diagnostics"].as_array())
        .cloned();
    assert!(
        diagnostics.is_some_and(|list| !list.is_empty()),
        "the refusal carries the reason: {:?}",
        refusal.data
    );
}

/// With the toolkit found, a scaffold is a real gear: its attribute projects
/// into the catalogue with no diagnostics, and its manifest names the SDK.
///
/// **The defect this closes.** A gear created for a product was written with a
/// lib of comments, so the projector found no `#[toolkit::gear]` (GBX0211), the
/// gear was absent from the catalogue, and the product it had just been added
/// to failed with GBX0301 the moment the flow finished.
#[test]
fn a_scaffold_with_the_toolkit_is_a_gear_the_catalogue_projects() {
    use crate::protocol::{GearKind, PluginScaffold};

    let tmp = scratch("scaffold-real");
    for (kind, plugin) in [
        (GearKind::Minimal, None),
        (GearKind::Service, None),
        (
            GearKind::Plugin,
            Some(PluginScaffold {
                spec: "cf.core.tenant_resolver.plugin.v1~".to_owned(),
                trait_ident: "TenantResolverPluginClient".to_owned(),
                crate_name: "cf-gears-tenant-resolver-sdk".to_owned(),
                lib_ident: "tenant_resolver_sdk".to_owned(),
                // A newline in a wire value must not break the manifest.
                path: "../sdk\nevil = 1".to_owned(),
            }),
        ),
    ] {
        let id = format!("demo-{}", format!("{kind:?}").to_lowercase());
        let files = super::scaffold_gear_files(
            &ScaffoldGearParams {
                id: id.clone(),
                name: "Demo".to_owned(),
                version: "0.1.0".to_owned(),
                kind,
                plugin: plugin.clone(),
                destination_dir: tmp.display().to_string(),
                dry_run: false,
            },
            Some("../libs/toolkit"),
            plugin.as_ref().map(|p| p.path.as_str()),
        )
        .expect("the shape renders");

        let crate_dir = tmp.join(&id);
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        for (rel, body, _) in &files {
            std::fs::write(crate_dir.join(rel.as_str()), body).unwrap();
        }
        let lib = std::fs::read_to_string(crate_dir.join("src/lib.rs")).unwrap();
        assert!(
            lib.contains(&format!("#[toolkit::gear(name = \"{id}\")]")),
            "{kind:?}: the attribute is live:\n{lib}"
        );
        // The manifest is parsed by the catalogue load below -- GBX0209 reads it --
        // so a broken one fails there rather than needing a parser here.
        let cargo = std::fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
        assert!(
            cargo.contains(
                r#"toolkit = { package = "cf-gears-toolkit", path = "../libs/toolkit" }"#
            ),
            "{kind:?}: {cargo}"
        );
        if kind == GearKind::Plugin {
            assert!(
                cargo.contains(
                    r#""tenant_resolver_sdk" = { package = "cf-gears-tenant-resolver-sdk""#
                ),
                "{cargo}"
            );
            assert!(
                !cargo.contains("\nevil = 1"),
                "a wire newline must stay inside the string: {cargo}"
            );
        }
    }

    // And the three together, as a source root: every one projects, none errs.
    // The plugin implements a point no gear in this root declares, which is GBX0519
    // here and nothing to do with the scaffold, so it is left out of the count.
    let root = gearbox_engine::SourceRoot::open(SourceId::new("scaffolds").unwrap(), &tmp)
        .expect("a root");
    let scan = gearbox_engine::load_catalogue(&[root]);
    for id in ["demo-minimal", "demo-service", "demo-plugin"] {
        assert!(
            scan.catalogue
                .gears
                .contains_key(&gearbox_ir::GearId::new(id).unwrap()),
            "`{id}` must be in the catalogue: {:?}",
            scan.catalogue
                .diagnostics
                .iter()
                .map(|d| &d.message)
                .collect::<Vec<_>>()
        );
    }
    assert!(
        scan.catalogue
            .diagnostics
            .iter()
            .all(|d| !d.is_error() || d.code == gearbox_ir::DiagnosticCode::PluginSpecUndeclared),
        "{:?}",
        scan.catalogue
            .diagnostics
            .iter()
            .map(|d| (d.code, &d.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_plugin_scaffold_outside_its_hosts_source_resolves_in_a_product() {
    use crate::protocol::{GearKind, PluginScaffold};
    let workspace = scratch("plugin-cross-source");
    let corpus = workspace.join("corpus");
    let plugins = workspace.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let files = [
        (
            "libs/toolkit/Cargo.toml",
            "[package]\nname = \"cf-gears-toolkit\"\nversion = \"0.1.0\"\n[lib]\nname = \"toolkit\"\n",
        ),
        (
            "host/Cargo.toml",
            "[package]\nname = \"host\"\nversion = \"0.1.0\"\n[lib]\nname = \"host\"\n",
        ),
        (
            "host/src/lib.rs",
            "#[toolkit::gear(name = \"host\")] pub struct Host;",
        ),
        (
            "sdk/Cargo.toml",
            "[package]\nname = \"host-sdk\"\nversion = \"0.1.0\"\n[lib]\nname = \"host_sdk\"\n",
        ),
        (
            "sdk/src/lib.rs",
            r#"pub trait HostPluginClient: Send + Sync {} #[gts_type_schema(base = PluginV1, type_id = gts_id!("cf.toolkit.plugins.plugin.v1~x.host.plugin.v1~"), description = "d", properties = "")] pub struct HostSpec;"#,
        ),
        (
            "host/gear.gdl",
            r#"gear(name = "Host", maturity = "stable", package = cargo(crate_name = "host", lib = "host", path = "."), sdk = cargo(crate_name = "host-sdk", lib = "host_sdk", path = "../sdk"), extension_points = [extension_point("x.host.plugin.v1~", trait = "HostPluginClient")])"#,
        ),
    ];
    for (relative, body) in files {
        let path = corpus.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let mut state = state_with_roots(workspace.clone(), std::slice::from_ref(&corpus));
    let host_scan = gearbox_engine::load_catalogue(&state.roots);
    assert!(
        !host_scan.catalogue.diagnostics.has_errors(),
        "{:?}",
        host_scan.catalogue.diagnostics
    );
    let point = &host_scan.catalogue.gears[&GearId::new("host").unwrap()].extension_points[0];
    let response = scaffold_gear(
        &mut state,
        RequestId::from(1),
        &ScaffoldGearParams {
            id: "new-plugin".to_owned(),
            name: "New Plugin".to_owned(),
            version: "0.1.0".to_owned(),
            kind: GearKind::Plugin,
            plugin: Some(PluginScaffold {
                spec: point.spec.clone(),
                trait_ident: point.trait_ident.clone(),
                crate_name: "host-sdk".to_owned(),
                lib_ident: "host_sdk".to_owned(),
                path: "../../corpus/sdk".to_owned(),
            }),
            destination_dir: plugins.display().to_string(),
            dry_run: false,
        },
    );
    assert!(response.response_result.is_ok(), "{response:?}");
    let gdl = std::fs::read_to_string(plugins.join("new-plugin/gear.gdl")).unwrap();
    assert!(
        !gdl.lines()
            .any(|line| line.trim_start().starts_with("sdk ="))
    );
    let mut roots = state.roots.clone();
    roots.push(
        gearbox_engine::SourceRoot::open(SourceId::new("plugins").unwrap(), &plugins).unwrap(),
    );
    let scan = gearbox_engine::load_catalogue(&roots);
    assert!(
        !scan.catalogue.diagnostics.has_errors(),
        "{:?}",
        scan.catalogue.diagnostics
    );
    assert!(
        scan.catalogue.gears[&GearId::new("new-plugin").unwrap()]
            .implements
            .as_ref()
            .unwrap()
            .point
            .is_some()
    );
    let product =
        gearbox_gdl::edit::render_product_template(&gearbox_gdl::edit::CreateProductParams {
            id: "scaffold-product".to_owned(),
            name: "Scaffold Product".to_owned(),
            version: "0.1.0".to_owned(),
            sources: vec![
                ("corpus".to_owned(), "corpus".to_owned()),
                ("plugins".to_owned(), "plugins".to_owned()),
            ],
            profile_kind: "embedded".to_owned(),
            profile_id: "dev".to_owned(),
        });
    let added =
        gearbox_gdl::edit::add_gear("file:///product.gdl", &product, "host", "corpus").unwrap();
    let added = gearbox_gdl::edit::add_gear_plugin(
        "file:///product.gdl",
        added.changed().unwrap(),
        "host",
        "new-plugin",
    )
    .unwrap();
    let product_path = workspace.join("product.gdl");
    std::fs::write(&product_path, added.changed().unwrap()).unwrap();
    let intent = gearbox_engine::load_product(&product_path, None)
        .intent
        .unwrap();
    let resolution = gearbox_engine::resolve::resolve(
        &scan.catalogue,
        &intent,
        &gearbox_ir::ProfileId::new("dev").unwrap(),
    );
    assert!(!resolution.has_errors(), "{:?}", resolution.diagnostics);
    assert!(
        resolution
            .closure
            .members
            .contains_key(&GearId::new("new-plugin").unwrap())
    );
    // Generation's ownership rule still refuses a scaffold inside the corpus.
    assert!(writable_out_root(&state, &corpus.join("new-plugin")).is_err());
}
