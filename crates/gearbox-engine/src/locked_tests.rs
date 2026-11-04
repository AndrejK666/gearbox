#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;

use super::*;

/// A product lock as Studio's has it: its own crate (no `source`), the
/// platform gear and toolkit from crates.io, and `gts` at two versions.
const LOCK: &str = r#"
version = 4

[[package]]
name = "studio-backend"
version = "0.1.0"

[[package]]
name = "cf-gears-api-gateway"
version = "0.5.2"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "cf-gears-toolkit"
version = "0.10.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "gts"
version = "0.12.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "gts"
version = "0.16.1"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "cf-gears-from-git"
version = "0.1.0"
source = "git+https://example.invalid/gears#abc"
"#;

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn a_lock_yields_only_its_crates_io_packages() {
    let locked = LockedVersions::parse(LOCK).unwrap();
    assert_eq!(locked.single("cf-gears-toolkit"), Some("0.10.0"));
    assert_eq!(
        locked.single("studio-backend"),
        None,
        "built by the product"
    );
    assert_eq!(locked.single("cf-gears-from-git"), None, "not crates.io");
    assert_eq!(locked.single("gts"), None, "two versions are not one");
    assert_eq!(locked.versions("gts").unwrap().len(), 2);
}

#[test]
fn a_locked_crate_is_named_at_its_locked_version() {
    let locked = LockedVersions::parse(LOCK).unwrap();
    let mut plan = RegistryPlan::default();
    let pinned = pin_crates(
        &mut plan,
        &names(&["cf-gears-api-gateway", "cf-gears-toolkit", "cf-gears-oagw"]),
        &locked,
    );
    assert_eq!(pinned.pinned.len(), 2);
    let gateway = plan.get("cf-gears-api-gateway").unwrap();
    assert_eq!(gateway.version, "0.5.2");
    assert_eq!(gateway.registry, "crates.io");
    assert!(gateway.patch.is_none());
    // Not in the lock: stays what it was, a path dependency.
    assert!(plan.get("cf-gears-oagw").is_none());
}

#[test]
fn pinning_takes_a_patch_back_to_the_checkout_off() {
    // `crates = registry(...)` found the checkout's toolkit changed since its
    // release and patched it in; the product's lock pins the release, and the
    // patch would put the second copy straight back.
    let locked = LockedVersions::parse(LOCK).unwrap();
    let mut plan = RegistryPlan::default();
    plan.crates.insert(
        "cf-gears-toolkit".to_owned(),
        RegistryCrate {
            registry: "crates.io".to_owned(),
            version: "0.12.0".to_owned(),
            patch: Some(PathBuf::from("/src/gears-rust/libs/toolkit")),
        },
    );
    drop(pin_crates(&mut plan, &BTreeSet::new(), &locked));
    let toolkit = plan.get("cf-gears-toolkit").unwrap();
    assert_eq!(toolkit.version, "0.10.0");
    assert!(toolkit.patch.is_none());
    assert!(plan.patches().is_empty());
}

#[test]
fn a_crate_locked_twice_is_reported_not_guessed() {
    let locked = LockedVersions::parse(LOCK).unwrap();
    let mut plan = RegistryPlan::default();
    let pinned = pin_crates(&mut plan, &names(&["gts"]), &locked);
    assert!(plan.get("gts").is_none());
    assert_eq!(
        pinned.ambiguous.get("gts").unwrap(),
        &vec!["0.12.0".to_owned(), "0.16.1".to_owned()]
    );
}

#[test]
fn an_alternate_registry_decision_is_kept() {
    let locked = LockedVersions::parse(LOCK).unwrap();
    let mut plan = RegistryPlan::default();
    plan.crates.insert(
        "cf-gears-toolkit".to_owned(),
        RegistryCrate {
            registry: "internal".to_owned(),
            version: "0.11.0".to_owned(),
            patch: None,
        },
    );
    drop(pin_crates(
        &mut plan,
        &names(&["cf-gears-toolkit"]),
        &locked,
    ));
    assert_eq!(plan.get("cf-gears-toolkit").unwrap().registry, "internal");
}

#[test]
fn the_switch_reads_its_three_spellings() {
    assert_eq!("auto".parse::<GearsFrom>(), Ok(GearsFrom::Auto));
    assert_eq!("registry".parse::<GearsFrom>(), Ok(GearsFrom::Registry));
    assert_eq!("source".parse::<GearsFrom>(), Ok(GearsFrom::Source));
    assert!("crates".parse::<GearsFrom>().is_err());
    assert_eq!(GearsFrom::default(), GearsFrom::Auto);
}
