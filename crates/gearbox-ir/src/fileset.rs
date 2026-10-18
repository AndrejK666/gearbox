//! What a generator produces, and who owns each byte of it.
//!
//! Every generator returns a [`FileSet`]: a sorted list of [`FileEntry`], each
//! carrying its own [`Ownership`]. Nothing here writes anything --
//! `gearbox_engine::generate::apply` is the only writer, and it decides what to
//! do with a file by reading the class recorded here rather than by inspecting
//! the path. That split is deliberate. A writer that guessed ownership from a
//! filename would be one rename away from overwriting an operator's edits, and
//! the guess would live in a different crate from the decision it implements.
//!
//! The three classes come from ADR `cpt-gearbox-adr-authoring-ownership-tiers`,
//! which states them as a rule rather than a list: the tool may freely write
//! what it solely owns, may write once into a place the human has not yet
//! occupied, and may never rewrite human-authored content without a fallback.
//! [`Ownership`] is that rule made into a type, so a new generator has to pick a
//! side rather than inherit one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::RelPath;

/// Who owns a generated file, and therefore what a second run may do to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum Ownership {
    /// The tool owns it entirely. Overwritten on every run.
    ///
    /// Carries a `DO NOT EDIT` header wherever the format has comments, because
    /// the only protection an editor can offer is telling the truth early.
    Generated,

    /// Written once, into a path that does not exist yet, and never revisited.
    ///
    /// This is what makes scaffolding compatible with the one-author invariant
    /// of ADR `cpt-gearbox-adr-macro-projected-catalogue`: a fact a machine
    /// writes once and a human owns thereafter still has exactly one author.
    GeneratedOnce,

    /// The operator owns it; the tool proposes changes by three-way merge.
    ///
    /// The only class that can fail. An unresolvable overlap is `GBX0701` and
    /// leaves the file untouched, because a file with conflict markers in it is
    /// a file the deployment tooling can no longer read
    /// (`cpt-gearbox-fr-preserve-operator-values`).
    OperatorOwned,
}

impl Ownership {
    /// A stable lowercase spelling, for diagnostics and CLI output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::GeneratedOnce => "generated-once",
            Self::OperatorOwned => "operator-owned",
        }
    }

    /// Whether an existing file at this path may be replaced outright.
    #[must_use]
    pub const fn overwrites(self) -> bool {
        matches!(self, Self::Generated)
    }
}

impl std::fmt::Display for Ownership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What kind of content a generated file holds.
///
/// Not derived from the extension: `rust-toolchain.toml` and a Dockerfile are
/// both distinguishable only by name, and a client that has to re-derive this
/// to syntax-highlight a preview would be re-deriving a decision the generator
/// already made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum FileKind {
    Rust,
    Toml,
    Yaml,
    Json,
    Dockerfile,
    Shell,
    /// Anything with no better answer -- `.dockerignore`, a `.gitignore`.
    Text,
    /// Opaque bytes, with no text preview or comment syntax.
    Binary,
}

impl FileKind {
    /// A stable lowercase spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Toml => "toml",
            Self::Yaml => "yaml",
            Self::Json => "json",
            Self::Dockerfile => "dockerfile",
            Self::Shell => "shell",
            Self::Text => "text",
            Self::Binary => "binary",
        }
    }

    /// The comment prefix this format uses, when it has line comments.
    ///
    /// `None` for JSON, which has none -- which is why the generated
    /// `values.schema.json` cannot carry a `DO NOT EDIT` header and has to say
    /// so in a `$comment` key instead.
    #[must_use]
    pub const fn line_comment(self) -> Option<&'static str> {
        match self {
            Self::Rust => Some("//"),
            Self::Toml | Self::Yaml | Self::Dockerfile | Self::Shell | Self::Text => Some("#"),
            Self::Json | Self::Binary => None,
        }
    }
}

impl std::fmt::Display for FileKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One file a generator produced.
///
/// `bytes` rather than `String` because the set has to be able to carry a
/// non-text artefact without a second type; every producer in this milestone
/// happens to emit UTF-8, and [`text`](Self::text) is how a consumer says it
/// expects that.
///
/// No `TS` derive, and not an oversight: the wire form of a generated file is
/// [`FilePlan`] plus a text preview, never a byte array. Deriving `TS` here
/// would offer clients an `Array<number>` that nothing should ever send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative to the output root, e.g. `apps/api-gateway/Cargo.toml`.
    pub path: RelPath,
    pub bytes: Vec<u8>,
    pub kind: FileKind,
    pub ownership: Ownership,
}

impl FileEntry {
    /// A file whose content is text.
    #[must_use]
    pub fn text(
        path: RelPath,
        content: impl Into<String>,
        kind: FileKind,
        ownership: Ownership,
    ) -> Self {
        Self {
            path,
            bytes: content.into().into_bytes(),
            kind,
            ownership,
        }
    }

    /// The content as text, or `None` if it is not valid UTF-8.
    ///
    /// Returns `Option` rather than lossily converting: a preview that silently
    /// substitutes replacement characters shows the operator something the
    /// writer will not write.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok()
    }

    /// The content hash, in the `blake3:<hex>` form the lock already uses.
    ///
    /// blake3 rather than the `sha256` named in the plan's §8: every other
    /// digest in this repository is blake3 (`gearbox_lock::compute_hash`), and
    /// two hash families in one tool means every comparison first has to
    /// establish which one it is looking at.
    #[must_use]
    pub fn digest(&self) -> String {
        format!("blake3:{}", blake3::hash(&self.bytes).to_hex())
    }
}

/// Everything one generation run produced, keyed by path.
///
/// A map rather than a `Vec`, for one reason: two generators emitting the same
/// path is a bug, and [`insert`](Self::insert) returns the displaced entry so
/// the caller has to decide rather than silently keeping the last writer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileSet {
    entries: BTreeMap<RelPath, FileEntry>,
}

impl FileSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `entry`, returning whatever it displaced.
    ///
    /// A `Some` return is always a generator bug -- two producers claiming one
    /// path -- so callers surface it rather than dropping it.
    #[must_use]
    pub fn insert(&mut self, entry: FileEntry) -> Option<FileEntry> {
        self.entries.insert(entry.path.clone(), entry)
    }

    /// Merge `other` into this set, returning every path both claimed.
    #[must_use]
    pub fn merge(&mut self, other: Self) -> Vec<RelPath> {
        let mut collisions = Vec::new();
        for (path, entry) in other.entries {
            if self.entries.insert(path.clone(), entry).is_some() {
                collisions.push(path);
            }
        }
        collisions
    }

    #[must_use]
    pub fn get(&self, path: &RelPath) -> Option<&FileEntry> {
        self.entries.get(path)
    }

    /// Every entry, in path order. Deterministic by construction, which is what
    /// makes a generated tree diffable against the previous run.
    pub fn iter(&self) -> impl Iterator<Item = &FileEntry> {
        self.entries.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<'a> IntoIterator for &'a FileSet {
    type Item = &'a FileEntry;
    type IntoIter = std::collections::btree_map::Values<'a, RelPath, FileEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.values()
    }
}

impl FromIterator<FileEntry> for FileSet {
    fn from_iter<T: IntoIterator<Item = FileEntry>>(iter: T) -> Self {
        let mut set = Self::new();
        for entry in iter {
            // A collision here is a caller bug, but `FromIterator` has no way to
            // report it, so the constructor that can -- `insert` -- is the one
            // generators use. This exists for tests and for rebuilding a set
            // from its own iterator.
            drop(set.insert(entry));
        }
        set
    }
}

/// What applying a [`FileSet`] would do to one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum FileAction {
    /// Nothing is there yet.
    Create,
    /// Something is there and differs.
    Update,
    /// Something is there and matches byte for byte.
    Unchanged,
    /// An `OperatorOwned` file whose edits and ours overlap. `GBX0701`; the
    /// file is left exactly as the operator left it.
    Conflict,
    /// A `GeneratedOnce` file that already exists, and is therefore the
    /// human's now. Distinguished from `Unchanged` because the content may
    /// differ wildly and still be correct.
    Kept,
}

impl FileAction {
    /// A stable lowercase spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Unchanged => "unchanged",
            Self::Conflict => "conflict",
            Self::Kept => "kept",
        }
    }

    /// Whether applying this plan entry writes bytes to disk.
    #[must_use]
    pub const fn writes(self) -> bool {
        matches!(self, Self::Create | Self::Update)
    }
}

impl std::fmt::Display for FileAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One line of the preview `gearbox generate --dry-run` prints, and of the
/// `gearbox/generate/plan` response (`cpt-gearbox-fr-generate-preview`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FilePlan {
    pub path: RelPath,
    pub action: FileAction,
    pub ownership: Ownership,
    pub kind: FileKind,

    /// The digest of the bytes that *would be on disk after applying*, in
    /// `blake3:<hex>` form.
    ///
    /// After, not before: for a three-way merge the interesting artefact is the
    /// merged result, and a plan quoting the proposal's hash would not match
    /// what a later run finds.
    pub blake3: String,

    /// Whether the content can be shown as text.
    pub preview_available: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, body: &str) -> FileEntry {
        FileEntry::text(
            RelPath::new(path).expect("test path"),
            body,
            FileKind::Text,
            Ownership::Generated,
        )
    }

    #[test]
    fn insert_returns_the_displaced_entry() {
        let mut set = FileSet::new();
        assert!(set.insert(entry("a.txt", "one")).is_none());
        let displaced = set.insert(entry("a.txt", "two")).expect("displaced");
        assert_eq!(displaced.as_text(), Some("one"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn merge_names_every_collision() {
        let mut left: FileSet = [entry("a.txt", "one"), entry("b.txt", "two")]
            .into_iter()
            .collect();
        let right: FileSet = [entry("b.txt", "other"), entry("c.txt", "three")]
            .into_iter()
            .collect();
        let collisions = left.merge(right);
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].as_str(), "b.txt");
        assert_eq!(left.len(), 3);
    }

    #[test]
    fn iteration_is_path_ordered_whatever_the_insertion_order() {
        let forward: Vec<String> = [entry("b.txt", ""), entry("a.txt", ""), entry("c.txt", "")]
            .into_iter()
            .collect::<FileSet>()
            .iter()
            .map(|e| e.path.as_str().to_owned())
            .collect();
        assert_eq!(forward, ["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn digest_is_the_lock_s_hash_family() {
        // Not a golden value -- the point is only that a `FilePlan` hash and a
        // `product.lock` hash can be compared without first asking which family
        // each belongs to.
        assert!(entry("a.txt", "one").digest().starts_with("blake3:"));
    }

    #[test]
    fn non_utf8_content_has_no_preview() {
        let raw = FileEntry {
            path: RelPath::new("blob.bin").expect("test path"),
            bytes: vec![0xff, 0xfe],
            kind: FileKind::Text,
            ownership: Ownership::Generated,
        };
        assert!(raw.as_text().is_none());
    }
}
