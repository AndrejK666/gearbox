//! The crate-scan cache.
//!
//! The cache is invisible in the result -- the catalogue is byte-identical with
//! or without it, which is the whole point -- so these tests hold on to the two
//! counters instead. The gap between them is the parsing avoided.

#![allow(
    clippy::unwrap_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;

use gearbox_engine::{CatalogueScan, SourceRoot, load_catalogue};
use gearbox_ir::SourceId;

#[path = "common/corpus.rs"]
mod corpus_support;

fn gears_rust() -> Option<PathBuf> {
    corpus_support::root()
}

fn scan() -> Option<CatalogueScan> {
    let root = gears_rust()?;
    let source = SourceRoot::open(SourceId::new("gears-rust").unwrap(), root)
        .unwrap_or_else(|error| panic!("the discovered corpus must be readable: {error}"));
    Some(load_catalogue(&[source]))
}

macro_rules! require {
    () => {
        match scan() {
            Some(s) => s,
            None => {
                eprintln!("skipping: ../gears-rust not present");
                return;
            }
        }
    };
}

#[test]
fn a_crate_named_by_several_gears_is_parsed_once() {
    // The measured case: on this slice `tenant-resolver-sdk` is declared as `sdk`
    // by its host and three plugins, and `authn-resolver-sdk` by a host and two.
    // Without the cache those are seven scans of two crates.
    let scan = require!();
    assert!(
        scan.scan_requests > scan.crates_scanned,
        "sharing must actually happen: {} requests, {} distinct crates",
        scan.scan_requests,
        scan.crates_scanned
    );
    assert!(
        scan.scan_requests - scan.crates_scanned >= 5,
        "expected at least five avoided scans (tenant-resolver-sdk x3, \
         authn-resolver-sdk x2 as repeats), got {} requests over {} crates",
        scan.scan_requests,
        scan.crates_scanned
    );
}

#[test]
fn every_gear_still_gets_its_crate_scanned() {
    // The cache must not swallow a gear: one scan per gear crate at minimum,
    // plus the SDKs. If a cache-key collision merged two crates, this drops.
    let scan = require!();
    assert!(
        scan.crates_scanned >= scan.catalogue.gears.len(),
        "{} crates for {} gears -- a key collision would look exactly like this",
        scan.crates_scanned,
        scan.catalogue.gears.len()
    );
}

#[test]
fn the_catalogue_is_unchanged_by_caching() {
    // Two loads of the same tree must agree. The cache is per-load, so this also
    // catches state leaking between loads.
    let a = require!();
    let b = require!();
    assert_eq!(
        serde_json::to_string(&a.catalogue).unwrap(),
        serde_json::to_string(&b.catalogue).unwrap()
    );
    assert_eq!(a.crates_scanned, b.crates_scanned);
    assert_eq!(a.scan_requests, b.scan_requests);
}
