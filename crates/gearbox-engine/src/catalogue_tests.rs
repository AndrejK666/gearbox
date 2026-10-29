//! The contract merge, at the predicate rather than through a tree.
//!
//! Three ranks, and the middle one is the whole reason there are three. A
//! two-rank "provided beats consumed" would be simpler and would change what
//! happens on every tree that has two providers for one contract -- so the
//! ranks are tested for what they leave alone as much as for what they fix.

use gearbox_ir::{
    CargoRef, ContractDescriptor, ContractId, ContractKind, ContractVersion, GearId, RelPath,
    RestProjection, RestVisibility,
};

use super::{ContractAuthority, ContractMerge, discover};

fn gid(id: &str) -> GearId {
    GearId::new(id).expect("a valid gear id")
}

/// One descriptor, with `rest` standing in for "the complete copy": it is the
/// field a consumer's restatement deliberately leaves absent.
fn descriptor(owner: &str, rest: bool) -> ContractDescriptor {
    let owner = gid(owner);
    ContractDescriptor {
        id: ContractId::new(format!("{owner}/ThingApi@v1")).expect("a valid contract id"),
        owner,
        base_name: "ThingApi".to_owned(),
        version: ContractVersion::parse("v1").expect("a valid version"),
        kind: ContractKind::Api,
        rust_path: "thing_sdk::ThingApi".to_owned(),
        sdk: CargoRef {
            crate_name: "thing-sdk".to_owned(),
            lib_ident: "thing_sdk".to_owned(),
            path: RelPath::new("thing-sdk").expect("a valid relative path"),
            features: Vec::new(),
            default_features: true,
            link: Vec::new(),
        },
        rest: rest.then(|| RestProjection {
            base_path: "/thing/v1".to_owned(),
            visibility: RestVisibility::Internal,
            require_full_coverage: false,
        }),
        grpc: None,
    }
}

#[test]
fn a_consumers_stub_absorbed_first_loses_to_a_later_provider() {
    // The defect, at the predicate. Owner is a role, so it equals neither
    // declaring gear and the old `owner == declared_by` test was false twice.
    let mut merge = ContractMerge::default();
    merge.absorb(
        Vec::new(),
        vec![descriptor("provider-ingest", false)],
        &gid("consumer"),
    );
    merge.absorb(
        vec![descriptor("provider-ingest", true)],
        Vec::new(),
        &gid("provider"),
    );

    let merged = merge.finish();
    assert!(
        merged.values().next().expect("one contract").rest.is_some(),
        "the provider's copy carries the projection and must win"
    );
}

#[test]
fn the_owners_own_copy_beats_another_providers_whichever_arrives_first() {
    // The inertness guarantee: on a tree with no roles this rule must decide
    // exactly what the old one decided, and the old one preferred the owner.
    for owner_first in [true, false] {
        let mut merge = ContractMerge::default();
        let owners = || vec![descriptor("provider", true)];
        let others = || vec![descriptor("provider", false)];
        if owner_first {
            merge.absorb(owners(), Vec::new(), &gid("provider"));
            merge.absorb(others(), Vec::new(), &gid("other"));
        } else {
            merge.absorb(others(), Vec::new(), &gid("other"));
            merge.absorb(owners(), Vec::new(), &gid("provider"));
        }
        assert!(
            merge.finish().values().next().expect("one").rest.is_some(),
            "owner_first={owner_first}: the owner's copy wins either way"
        );
    }
}

#[test]
fn a_second_consumers_stub_does_not_displace_the_first() {
    // Equal authority does not replace, so the first declared wins -- the rule
    // the gear map itself uses.
    let mut merge = ContractMerge::default();
    merge.absorb(Vec::new(), vec![descriptor("owner", true)], &gid("first"));
    merge.absorb(Vec::new(), vec![descriptor("owner", false)], &gid("second"));

    assert!(
        merge.finish().values().next().expect("one").rest.is_some(),
        "the first stub stays"
    );
}

#[test]
fn the_ranks_order_as_they_read() {
    assert!(ContractAuthority::Consumed < ContractAuthority::Provided);
    assert!(ContractAuthority::Provided < ContractAuthority::OwnerProvided);
}

/// A worktree under `.claude/` is a second checkout; reading it declares every
/// gear twice. The root is walked even when its own name is hidden.
#[test]
fn the_walk_skips_every_dot_directory_below_the_root() {
    let root = std::env::temp_dir()
        .join(format!("gbx-discover-{}", std::process::id()))
        .join(".root");
    std::fs::remove_dir_all(&root).ok();
    for dir in [
        "gears/a",
        ".claude/worktrees/x/gears/a",
        ".hidden/b",
        "node_modules/c",
        "target/d",
    ] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join(super::GEAR_FILE), "").unwrap();
    }
    std::fs::write(root.join(super::GEAR_FILE), "").unwrap();

    let (found, failures) = discover(&root);
    std::fs::remove_dir_all(root.parent().unwrap()).ok();

    assert!(failures.is_empty(), "{failures:?}");
    let found: Vec<_> = found
        .iter()
        .map(|path| path.strip_prefix(&root).unwrap().to_path_buf())
        .collect();
    assert_eq!(
        found,
        [
            std::path::PathBuf::from(super::GEAR_FILE),
            ["gears", "a", super::GEAR_FILE].iter().collect(),
        ]
    );
}
