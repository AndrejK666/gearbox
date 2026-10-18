//! Finding the real gear tree, for the tests that project from it.
//!
//! Every locator here used to spell the corpus as
//! `$CARGO_MANIFEST_DIR/../../../gears-rust`, which is the sibling of the
//! *repository* root. From a git worktree -- `.claude/worktrees/<name>/crates/…`
//! -- that resolves to nothing, so every test that projects from real gears
//! skipped, printed a reason nobody reads, and the suite reported success having
//! exercised none of the corpus. An agent working in a worktree got that
//! silently, which is the whole problem: the skip was designed for a checkout
//! without the sibling repository, and it quietly covered a second case it was
//! never meant to.
//!
//! Walking up finds the corpus from either layout, and stops at the filesystem
//! root rather than guessing a depth.

use std::path::{Path, PathBuf};

/// The `gears-rust` checkout, if one is reachable from here.
///
/// Identified by containing a `gears/` directory, the same test the counted-`..`
/// version used -- so a stray directory of that name higher up is still not
/// mistaken for the corpus.
pub fn corpus_root() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GEARBOX_CORPUS_ROOT") {
        return PathBuf::from(path).canonicalize().ok();
    }
    let mut dir: &Path = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = dir.join("gears-rust");
        if candidate.join("gears").is_dir() {
            return candidate.canonicalize().ok();
        }
        dir = dir.parent()?;
    }
}

/// A path inside the corpus, canonicalized. `None` when the corpus is absent.
pub fn corpus(relative: &str) -> Option<PathBuf> {
    corpus_root()?.join(relative).canonicalize().ok()
}

/// Whether this run said it requires the corpus.
///
/// The switch `error_enum_corpus_tests`'s `decide` already turns into a failure,
/// available to every corpus test rather than to one.
pub fn required() -> bool {
    std::env::var("GEARBOX_CORPUS_REQUIRED").is_ok()
}

/// Take a corpus locator's `Option`, or leave the test with a reason.
///
/// Four test files used to spell this themselves, and each copy printed a line
/// and passed with no assertion run -- so on a checkout without the sibling
/// repository the whole real-tree tier reported green having verified nothing.
/// The locator can also come back `None` when the directory is there and the
/// scan is unreachable, so the skip covered more than a missing checkout.
///
/// `GEARBOX_CORPUS_REQUIRED=1` makes it a failure, which is what an
/// authoritative run wants. A plain `cargo test` on a machine without the corpus
/// still passes, because refusing to run at all is not this suite's call to
/// make.
macro_rules! require {
    ($e:expr) => {
        match $e {
            Some(value) => value,
            None => {
                assert!(
                    !$crate::test_corpus::required(),
                    "GEARBOX_CORPUS_REQUIRED is set and `{}` is not reachable, so this test \
                     would have passed without asserting anything",
                    stringify!($e)
                );
                eprintln!(
                    "SKIP {}: `{}` is not reachable, so nothing was asserted. Set \
                     GEARBOX_CORPUS_REQUIRED=1 to make this a failure.",
                    module_path!(),
                    stringify!($e)
                );
                return;
            }
        }
    };
}

pub(crate) use require;
