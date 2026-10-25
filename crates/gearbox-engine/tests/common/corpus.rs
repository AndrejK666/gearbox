//! The real source corpus, shared by integration-test binaries.
use std::path::{Path, PathBuf};

/// Returns the explicit or discovered checkout, refusing to skip a required corpus.
#[must_use]
pub fn root() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GEARBOX_CORPUS_ROOT") {
        let path = PathBuf::from(path)
            .canonicalize()
            .unwrap_or_else(|error| panic!("GEARBOX_CORPUS_ROOT: {error}"));
        assert!(
            path.join("gears").is_dir(),
            "GEARBOX_CORPUS_ROOT must name a gears-rust checkout containing gears/"
        );
        return Some(path);
    }
    let found = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|dir| dir.join("gears-rust"))
        .find(|candidate| candidate.join("gears").is_dir())
        .map(|candidate| {
            candidate
                .canonicalize()
                .unwrap_or_else(|error| panic!("gears-rust corpus: {error}"))
        });
    assert!(
        found.is_some() || std::env::var_os("GEARBOX_CORPUS_REQUIRED").is_none(),
        "GEARBOX_CORPUS_REQUIRED is set but no gears-rust checkout was found; set GEARBOX_CORPUS_ROOT"
    );
    found
}
