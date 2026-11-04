//! A product's `path(...)` sources, under the ids the product declares them with.
//!
//! A root's default id is its directory's canonical name, so a product that
//! says `source(id = "studio-web", at = path("../.."))` and is checked out as
//! `repo` -- or reached through a link called `studio-web` -- was scanned under
//! `repo`. Gears are found by name and did not notice; the `FrontX` generator
//! looks a shell up by source id and refused it (`GBX0720`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use gearbox_engine::{SourceRoot, with_declared_paths};
use gearbox_ir::{SourceDecl, SourceId};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// `<tmp>/repo/products/demo/product.gdl`, and the repository root.
fn layout(name: &str) -> (PathBuf, PathBuf) {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let top = std::env::temp_dir().join(format!(
        "gearbox-declared-{}-{name}-{n}",
        std::process::id()
    ));
    if top.exists() {
        std::fs::remove_dir_all(&top).unwrap();
    }
    let repo = top.join("repo");
    let product_dir = repo.join("products").join("demo");
    std::fs::create_dir_all(&product_dir).unwrap();
    std::fs::create_dir_all(repo.join("backend")).unwrap();
    (product_dir.join("product.gdl"), repo)
}

fn sources(entries: &[(&str, &str)]) -> BTreeMap<SourceId, SourceDecl> {
    entries
        .iter()
        .map(|(id, at)| {
            (
                SourceId::new(*id).unwrap(),
                SourceDecl::Path {
                    at: (*at).to_owned(),
                    crates: None,
                    declared_at: None,
                },
            )
        })
        .collect()
}

fn ids(roots: &[SourceRoot]) -> Vec<(String, PathBuf)> {
    roots
        .iter()
        .map(|r| (r.id.as_str().to_owned(), r.root.clone()))
        .collect()
}

#[test]
fn a_root_at_a_declared_path_takes_the_declared_id() {
    let (product, repo) = layout("rename");
    let given = vec![SourceRoot::open(SourceId::new("repo").unwrap(), &repo).unwrap()];
    let roots = with_declared_paths(given, &sources(&[("studio-web", "../..")]), &product, false);
    assert_eq!(
        ids(&roots),
        vec![("studio-web".to_owned(), repo.canonicalize().unwrap())]
    );
}

#[test]
fn an_explicit_id_is_kept() {
    let (product, repo) = layout("explicit");
    let given = vec![SourceRoot::open(SourceId::new("mine").unwrap(), &repo).unwrap()];
    let roots = with_declared_paths(given, &sources(&[("studio-web", "../..")]), &product, true);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].id.as_str(), "mine");
}

#[test]
fn a_declared_path_no_root_names_is_opened_from_the_description() {
    let (product, repo) = layout("open");
    let roots = with_declared_paths(
        Vec::new(),
        &sources(&[("studio-web", "../.."), ("missing", "../../nowhere")]),
        &product,
        false,
    );
    assert_eq!(
        ids(&roots),
        vec![("studio-web".to_owned(), repo.canonicalize().unwrap())]
    );
}

#[test]
fn a_declared_path_around_a_given_root_is_not_scanned_twice() {
    let (product, repo) = layout("overlap");
    let given =
        vec![SourceRoot::open(SourceId::new("backend").unwrap(), repo.join("backend")).unwrap()];
    let roots = with_declared_paths(given, &sources(&[("studio-web", "../..")]), &product, false);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].id.as_str(), "backend");
}
