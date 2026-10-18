//! Errors from serializing, parsing, or verifying a `product.lock`.

/// Something went wrong reading or writing a lock.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// The lock declares a schema version this build does not understand.
    ///
    /// A reader that guessed at an unknown version would silently misread a
    /// future format; refusing is the safe failure.
    #[error(
        "product.lock schema_version {found} is not supported by this build \
         (supports {supported}); run `gearbox resolve` to regenerate it with this gearbox version"
    )]
    UnsupportedSchemaVersion { found: u32, supported: u32 },

    /// The product could not be rendered as TOML.
    ///
    /// Reached from both directions: writing a lock renders it, and verifying
    /// one on the way in re-renders the parsed product to hash it. The message
    /// names the rendering rather than the direction for that reason.
    #[error("failed to render product.lock as TOML: {0}")]
    Serialize(#[from] toml::ser::Error),

    /// The lock text could not be parsed as TOML, or did not match the
    /// resolved-product schema.
    #[error("failed to parse product.lock: {0}")]
    Parse(#[from] toml::de::Error),

    /// The lock carries a key no field of the resolved-product schema reads.
    ///
    /// Refused rather than ignored: an ignored key never reaches the hash, so
    /// it would ride along inside a lock that reports as verified.
    #[error(
        "product.lock carries `{path}`, which this build does not read; \
         run `gearbox resolve` to regenerate it"
    )]
    UnknownField { path: String },

    /// The lock lists the same entry twice in a collection whose entries are
    /// unique by construction.
    ///
    /// Refused rather than deduplicated: canonicalization would erase the
    /// duplicate before hashing, so the edit that introduced it would verify.
    #[error(
        "product.lock lists the same {collection} entry twice ({entry}); \
         run `gearbox resolve` to regenerate it"
    )]
    DuplicateEntry {
        collection: &'static str,
        entry: String,
    },

    /// The lock parses and verifies, but carries a value the rest of the
    /// toolchain cannot act on.
    ///
    /// The lock is the one way into the generator that does not pass through
    /// the GDL front end's checks, so the values that reach a path join or a
    /// process spawn are checked here instead.
    #[error("product.lock has an unusable value for {field}: {reason}")]
    InvalidContent { field: String, reason: String },

    /// The lock's recorded hash does not match its own content.
    ///
    /// A hand edit is the usual cause and not the only one: the hash is
    /// recomputed from the re-rendered product, so a lock written by a build
    /// whose serialization differed -- a field that has since gained a
    /// `serde` default, within the same `schema_version` -- lands here too.
    /// The message states the mismatch and names the writing version instead
    /// of asserting which of the two happened.
    #[error(
        "product.lock does not match its own recorded hash: it claims \
         {expected}, but its content hashes to {found}. It was written by \
         gearbox {gearbox_version}; run `gearbox resolve` to regenerate it \
         with this build rather than editing it by hand."
    )]
    HashMismatch {
        expected: String,
        found: String,
        gearbox_version: String,
    },
}
