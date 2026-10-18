//! The staged catalogue load.
//!
//! The behaviour that matters is the *order*: every description is evaluated
//! before any crate is parsed, so a registry view can show its whole shape while
//! the expensive stage is still running. A loader that interleaved would look
//! correct in every other test and still make the tree trickle in one row at a
//! time.

#![allow(
    clippy::unwrap_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use gearbox_engine::{Continue, LoadEvent, SourceRoot, load_catalogue, load_catalogue_staged};
use gearbox_ir::{LoadStage, SourceId};

#[path = "common/corpus.rs"]
mod corpus_support;

fn root() -> Option<SourceRoot> {
    corpus_support::root().map(|root| {
        SourceRoot::open(SourceId::new("gears-rust").unwrap(), root)
            .unwrap_or_else(|error| panic!("gears-rust corpus: {error}"))
    })
}

macro_rules! require {
    () => {
        match root() {
            Some(r) => r,
            None => {
                eprintln!("skipping: ../gears-rust not present");
                return;
            }
        }
    };
}

/// One event, flattened to what a test can compare.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Discovered(usize),
    Declared(String),
    Design(String),
    DeclarationComplete(usize),
    Projected(String),
    Joined(String),
}

fn record(root: &SourceRoot) -> (Vec<Seen>, gearbox_engine::CatalogueScan) {
    let mut seen = Vec::new();
    let scan = load_catalogue_staged(std::slice::from_ref(root), &mut |event| {
        seen.push(match event {
            LoadEvent::Discovered { total } => Seen::Discovered(total),
            LoadEvent::Declared(p) => Seen::Declared(p.gdl_path.as_str().to_owned()),
            LoadEvent::Design(d) => Seen::Design(d.id.as_str().to_owned()),
            LoadEvent::DeclarationComplete { declared, .. } => Seen::DeclarationComplete(declared),
            LoadEvent::Projected(g) => Seen::Projected(g.id.as_str().to_owned()),
            LoadEvent::Joined(g) => Seen::Joined(g.id.as_str().to_owned()),
        });
        Continue::Yes
    });
    (seen, scan)
}

#[test]
fn a_streaming_consumer_ends_with_every_plugin_joined() {
    // **What the Studio actually receives**, which is not the returned scan. It
    // keeps the last copy of each gear it is sent, and a plugin is projected
    // before its host may have been: `implements` names only a spec, and the point
    // is the host's declaration. The first conformance run after roles became
    // declared showed oidc-authn-plugin as "no described host declares it" --
    // true of the copy sent during projection, false of the catalogue.
    let root = require!();
    let mut last: std::collections::BTreeMap<String, gearbox_ir::GearDescriptor> =
        std::collections::BTreeMap::new();
    let mut joined_after_projection = true;
    let mut projecting_done = false;
    drop(load_catalogue_staged(
        std::slice::from_ref(&root),
        &mut |event| {
            match event {
                LoadEvent::Projected(g) => {
                    joined_after_projection &= !projecting_done;
                    last.insert(g.id.as_str().to_owned(), g.clone());
                }
                LoadEvent::Joined(g) => {
                    projecting_done = true;
                    last.insert(g.id.as_str().to_owned(), g.clone());
                }
                _ => {}
            }
            Continue::Yes
        },
    ));
    assert!(
        joined_after_projection,
        "every Joined comes after the last Projected"
    );
    let plugins: Vec<&gearbox_ir::GearDescriptor> =
        last.values().filter(|g| g.implements.is_some()).collect();
    assert!(!plugins.is_empty(), "the corpus has plugins");
    for plugin in plugins {
        assert!(
            plugin
                .implements
                .as_ref()
                .and_then(|f| f.point.as_ref())
                .is_some(),
            "`{}` reached the consumer unjoined",
            plugin.id
        );
    }
}

#[test]
fn discovery_reports_a_total_once_and_first() {
    // A progress bar needs its denominator before the work that could take a
    // while, so this event is first and singular.
    let (seen, scan) = record(&require!());

    assert!(
        matches!(seen.first(), Some(Seen::Discovered(_))),
        "first event must be discovery, got {:?}",
        seen.first()
    );
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(e, Seen::Discovered(_)))
            .count(),
        1
    );
    let Some(Seen::Discovered(total)) = seen.first() else {
        unreachable!()
    };
    assert_eq!(*total, scan.files.len());
}

#[test]
fn every_description_is_declared_before_any_crate_is_parsed() {
    // The load-bearing assertion. Names and categories are what a tree groups
    // by, and they cost milliseconds; parsing costs 255 files on this slice and
    // ~2658 on the full tree. Interleaving would defeat the whole point.
    let (seen, _) = record(&require!());

    let last_declared = seen
        .iter()
        .rposition(|e| matches!(e, Seen::Declared(_)))
        .expect("some gear was declared");
    let first_projected = seen
        .iter()
        .position(|e| matches!(e, Seen::Projected(_)))
        .expect("some gear was projected");

    assert!(
        last_declared < first_projected,
        "all {} declarations must precede the first projection; got order {seen:#?}",
        seen.iter()
            .filter(|e| matches!(e, Seen::Declared(_)))
            .count()
    );
}

#[test]
fn the_boundary_between_the_passes_is_announced_exactly_once() {
    // What the RPC layer responds on: the moment the tree has its whole shape
    // and none of its badges. Inferring it from the first `Projected` would fail
    // when nothing projects at all.
    let (seen, _) = record(&require!());

    let boundary = seen
        .iter()
        .position(|e| matches!(e, Seen::DeclarationComplete(_)))
        .expect("the boundary is announced");
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(e, Seen::DeclarationComplete(_)))
            .count(),
        1
    );

    let declared = seen[..boundary]
        .iter()
        .filter(|e| matches!(e, Seen::Declared(_)))
        .count();
    assert!(matches!(seen[boundary], Seen::DeclarationComplete(n) if n == declared));
    // Projections follow the boundary, and then the plugins the join completed
    // -- never interleaved, because the join needs every host.
    let after = &seen[boundary + 1..];
    let first_join = after
        .iter()
        .position(|e| matches!(e, Seen::Joined(_)))
        .unwrap_or(after.len());
    assert!(
        after[..first_join]
            .iter()
            .all(|e| matches!(e, Seen::Projected(_))),
        "only projections follow the boundary, until the join"
    );
    assert!(
        after[first_join..]
            .iter()
            .all(|e| matches!(e, Seen::Joined(_))),
        "and only joins follow the last projection"
    );
}

#[test]
fn a_completed_load_leaves_nothing_pending() {
    // Otherwise `pending` becomes a place where gears go missing quietly, which
    // is the failure `Option` was protected from in the first place.
    let (_, scan) = record(&require!());
    assert!(
        scan.pending.is_empty(),
        "still pending after a full load: {:?}",
        scan.pending
            .iter()
            .map(|p| p.gdl_path.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_unstaged_entry_point_agrees_with_the_staged_one() {
    // `load_catalogue` is a wrapper, and the CLI plus 248 existing tests depend
    // on it being exactly that.
    let root = require!();
    let plain = load_catalogue(std::slice::from_ref(&root));
    let (_, staged) = record(&root);

    assert_eq!(
        serde_json::to_string(&plain.catalogue).unwrap(),
        serde_json::to_string(&staged.catalogue).unwrap()
    );
    assert_eq!(plain.crates_scanned, staged.crates_scanned);
    assert_eq!(plain.scan_requests, staged.scan_requests);
    assert_eq!(plain.files, staged.files);
}

#[test]
fn declared_events_carry_what_a_tree_needs_and_no_id() {
    // The consequence of ADR-0002 that shapes the UI: `id` is projected, so a
    // row has a name to show and no identifier to key by. `PendingGear` has no
    // `id` field at all, which is the point -- the type makes the mistake
    // unavailable rather than merely discouraged.
    let root = require!();
    let mut declared = Vec::new();
    drop(load_catalogue_staged(
        std::slice::from_ref(&root),
        &mut |event| {
            if let LoadEvent::Declared(p) = event {
                declared.push((
                    p.gdl_path.as_str().to_owned(),
                    p.display_name.clone(),
                    p.category.clone(),
                    p.stage,
                ));
            }
            Continue::Yes
        },
    ));

    assert!(!declared.is_empty());
    for (path, name, category, stage) in &declared {
        assert_eq!(*stage, LoadStage::Declared);
        assert!(name.is_some(), "`{path}` has no display name to render");
        assert!(category.is_some(), "`{path}` has no category to group by");
    }
}

// ---------------------------------------------------------------- cancelling

#[test]
fn stopping_during_declaration_leaves_the_rest_pending() {
    // What `$/cancelRequest` needs: stopping keeps what is done and reports what
    // is not, rather than discarding the load.
    let root = require!();
    let mut declared = 0;
    let scan = load_catalogue_staged(std::slice::from_ref(&root), &mut |event| {
        if let LoadEvent::Declared(_) = event {
            declared += 1;
            if declared == 2 {
                return Continue::Stop;
            }
        }
        Continue::Yes
    });

    assert_eq!(
        scan.pending.len(),
        2,
        "the two declared so far stay pending"
    );
    assert!(
        scan.catalogue.gears.is_empty(),
        "stopping before projection means nothing reached the catalogue"
    );
}

#[test]
fn stopping_at_discovery_yields_an_empty_but_valid_catalogue() {
    let root = require!();
    let scan = load_catalogue_staged(std::slice::from_ref(&root), &mut |_| Continue::Stop);

    assert!(scan.catalogue.gears.is_empty());
    assert!(scan.pending.is_empty(), "nothing was declared yet");
    assert!(
        !scan.files.is_empty(),
        "discovery still happened, so the paths are known"
    );
    assert!(
        scan.catalogue
            .sources
            .contains_key(&SourceId::new("gears-rust").unwrap()),
        "the source is recorded even on an immediate stop"
    );
}

#[test]
fn stopping_midway_through_projection_keeps_what_is_done() {
    let root = require!();
    let mut projected = 0;
    let scan = load_catalogue_staged(std::slice::from_ref(&root), &mut |event| {
        if let LoadEvent::Projected(_) = event {
            projected += 1;
            if projected == 3 {
                return Continue::Stop;
            }
        }
        Continue::Yes
    });

    assert_eq!(scan.catalogue.gears.len(), 3);
    assert!(
        !scan.pending.is_empty(),
        "the gears not reached are still pending, not lost"
    );
    // The invariant, not the number: nothing discovered is lost on a stop. The
    // total is the corpus's description count -- fifty-nine: forty-nine gears
    // with code and ten designs -- and it is
    // written out rather than read from the scan so that a gear vanishing
    // between discovery and projection cannot satisfy both sides of the
    // equation at once. Designs complete in the first pass, before the stop.
    assert_eq!(
        scan.catalogue.gears.len() + scan.pending.len() + scan.catalogue.designs.len(),
        59,
        "every discovered description is projected, pending or a design"
    );
}
