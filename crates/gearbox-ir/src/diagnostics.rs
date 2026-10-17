//! Diagnostics.
//!
//! Every diagnostic the engine can emit is declared here, once, as an enum
//! variant. That makes the set exhaustive and greppable, lets the compiler catch
//! a mistyped code, and gives each code a single home for its title, default
//! severity, and doc comment.
//!
//! Two rules from the PRD are enforced structurally rather than by review:
//!
//! - `cpt-gearbox-nfr-actionable-diagnostics` -- every error-severity code
//!   carries a remedy. [`Diagnostic::error`] demands one.
//! - `cpt-gearbox-nfr-evidence-cited` -- every code asserting the runtime does
//!   not support something carries a `file:line` in `gears-rust`.
//!   [`DiagnosticCode::requires_evidence`] marks those, and
//!   [`Diagnostic::validate`] rejects them without it.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::NodeId;

/// How much a diagnostic matters.
///
/// `Error` blocks writing the lock and generating artifacts (unless explicitly
/// overridden); the rest are informational. Resolution itself never stops on an
/// error -- a partial product plus its errors is more useful to a UI than
/// nothing at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational; the smallest thing worth saying.
    Hint,
    /// Worth knowing, no action implied.
    Info,
    /// The product resolved, but not the way it was asked for.
    Warning,
    /// The product is invalid. Blocks lock write and generation.
    Error,
}

impl Severity {
    #[must_use]
    pub const fn is_error(self) -> bool {
        matches!(self, Self::Error)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hint => "hint",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which part of the pipeline produced a diagnostic.
///
/// Mirrors the numeric ranges of the codes, so a reader can place a code without
/// consulting a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticDomain {
    /// `GBX01xx` -- parsing and evaluating GDL.
    Gdl,
    /// `GBX02xx` -- cross-checking `gear.gdl` against Rust source.
    Validate,
    /// `GBX03xx` -- process topology and structural constraints.
    Topology,
    /// `GBX04xx` -- contract bindings and severability.
    Binding,
    /// `GBX05xx` -- cluster capabilities and providers.
    Cluster,
    /// `GBX06xx` -- capabilities the runtime does not implement.
    RuntimeGap,
    /// `GBX07xx` -- artifact generation.
    Generator,
}

impl DiagnosticDomain {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gdl => "gdl",
            Self::Validate => "validate",
            Self::Topology => "topology",
            Self::Binding => "binding",
            Self::Cluster => "cluster",
            Self::RuntimeGap => "runtime-gap",
            Self::Generator => "generator",
        }
    }
}

/// An AIP-193 `(domain, code)` pair, spelled the way `#[derive(ContractError)]`
/// spells it in `gears-rust`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalErrorId {
    /// `#[error_domain("...")]`, e.g. `"cluster.v1"`.
    pub domain: &'static str,
    /// `#[error_code("...")]`, e.g. `"profile_not_bound"`.
    pub code: &'static str,
}

/// A `gears-rust` runtime error that a composition-time diagnostic exists to
/// prevent.
///
/// **Deliberately not a `path:line`.** That is what [`Diagnostic::evidence`]
/// already is, and a line number is the part that rots first: a file moves and
/// the citation is wrong while every identifier in it is still correct. A
/// package name, an enum identifier and a variant identifier are the three
/// strings a *rename* has to touch, and a file move touches none of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeErrorRef {
    /// The declaring crate's `[package] name`, e.g. `"cf-gears-toolkit"`.
    ///
    /// A package name rather than a directory, for the reason `sdk = cargo(...)`
    /// names a crate: the name is declared, the directory is a layout choice.
    /// It is also what disambiguates -- `gears-rust` declares two enums called
    /// `RegistryError`, in `cf-gears-toolkit` and `cf-gears-bss-fixtures`.
    pub krate: &'static str,
    /// The enum's identifier, e.g. `"RegistryError"`.
    pub ty: &'static str,
    /// The variant's identifier, e.g. `"UnknownDependency"`.
    pub variant: &'static str,
    /// The identity this failure travels under on the wire, where it has one.
    ///
    /// Only the `#[derive(ContractError)]` enums do, and in `cluster-sdk` that
    /// derive sits on a *wire twin* (`ClusterWireError`) paired by hand with the
    /// local enum. Naming both therefore pins three things at once: the local
    /// variant exists, the frozen code exists, and the two still agree.
    pub canonical: Option<CanonicalErrorId>,
}

/// What a diagnostic prevents on the runtime side, if anything nameable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Prevents {
    /// No single named runtime error corresponds to this code.
    ///
    /// **An answer, not a gap.** `GBX0503`'s failure is a *silence* -- a
    /// process-local backend starts cleanly in every replica and elects one
    /// leader per replica -- and inventing a variant for a failure the runtime
    /// never raises would be worse than saying nothing. `GBX0409`'s is a
    /// function, `remap_gear_env_key`, not an error: this type references
    /// runtime *errors*, and widening it to arbitrary Rust items is a separate
    /// decision nobody has needed yet.
    Nothing,
    /// A named variant of a `gears-rust` error enum.
    RuntimeError(RuntimeErrorRef),
}

impl Prevents {
    /// A plain `thiserror` variant, with no wire identity.
    #[must_use]
    pub const fn error(krate: &'static str, ty: &'static str, variant: &'static str) -> Self {
        Self::RuntimeError(RuntimeErrorRef {
            krate,
            ty,
            variant,
            canonical: None,
        })
    }

    /// A variant that also travels under a frozen `(domain, code)` pair.
    #[must_use]
    pub const fn canonical_error(
        krate: &'static str,
        ty: &'static str,
        variant: &'static str,
        domain: &'static str,
        code: &'static str,
    ) -> Self {
        Self::RuntimeError(RuntimeErrorRef {
            krate,
            ty,
            variant,
            canonical: Some(CanonicalErrorId { domain, code }),
        })
    }

    /// The reference, when there is one.
    #[must_use]
    pub const fn as_ref(self) -> Option<RuntimeErrorRef> {
        match self {
            Self::Nothing => None,
            Self::RuntimeError(reference) => Some(reference),
        }
    }
}

/// The default for the optional `prevents` column.
///
/// A helper macro rather than a conditional inside the arm, because the arm has
/// to stay a `const` expression and the generated `match` has to have one shape
/// whether or not the column was written.
macro_rules! prevents_or_nothing {
    () => {
        Prevents::Nothing
    };
    ($prevents:expr) => {
        $prevents
    };
}

/// Declares the diagnostic catalogue.
///
/// Each entry is `Variant = "CODE", domain, default severity, evidence
/// requirement, title`, optionally followed by `, prevents = ...`. The doc
/// comment on a variant is the canonical explanation of the condition, and is
/// what the generated reference page shows.
///
/// The `prevents` column is optional where `evidence` is a mandatory bool, and
/// the asymmetry is deliberate. "Does this assert a runtime limitation?" is a
/// policy question every code must answer. "Which runtime error does this
/// prevent?" is not: most codes prevent no single named error, and a mandatory
/// column would assert that somebody considered and cleared all of them.
macro_rules! diagnostic_codes {
    (
        $(
            $(#[doc = $doc:literal])+
            $variant:ident = $code:literal, $domain:ident, $severity:ident, $evidence:literal, $title:literal
                $(, prevents = $prevents:expr)? ;
        )+
    ) => {
        /// A stable diagnostic code.
        ///
        /// Serializes as its string form (`"GBX0402"`) so a code in a
        /// `product.lock` or an RPC payload stays readable and stable even if
        /// this enum is reordered.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
        #[ts(type = "string")]
        #[non_exhaustive]
        pub enum DiagnosticCode {
            $(
                $(#[doc = $doc])+
                $variant,
            )+
        }

        impl DiagnosticCode {
            /// Every code, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The stable string form, e.g. `"GBX0402"`.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $code,)+
                }
            }

            /// Which pipeline stage owns this code.
            #[must_use]
            pub const fn domain(self) -> DiagnosticDomain {
                match self {
                    $(Self::$variant => DiagnosticDomain::$domain,)+
                }
            }

            /// The severity used unless a call site deliberately raises or lowers it.
            #[must_use]
            pub const fn default_severity(self) -> Severity {
                match self {
                    $(Self::$variant => Severity::$severity,)+
                }
            }

            /// Whether this code asserts a runtime limitation and therefore must
            /// cite a `file:line` in `gears-rust`
            /// (`cpt-gearbox-nfr-evidence-cited`).
            #[must_use]
            pub const fn requires_evidence(self) -> bool {
                match self {
                    $(Self::$variant => $evidence,)+
                }
            }

            /// A short human title, independent of any particular occurrence.
            #[must_use]
            pub const fn title(self) -> &'static str {
                match self {
                    $(Self::$variant => $title,)+
                }
            }

            /// The `gears-rust` runtime error this code exists to prevent.
            ///
            /// Metadata a check *can* verify rather than a promise every build
            /// verifies: `gearbox-project`'s corpus test resolves each reference
            /// against the real tree and skips when no checkout is reachable.
            /// Nothing resolves it at load time, so a missing source root is
            /// not a degradation case here -- it cannot arise.
            #[must_use]
            pub const fn prevents(self) -> Prevents {
                match self {
                    $(Self::$variant => prevents_or_nothing!($($prevents)?),)+
                }
            }

            /// The variant's doc comment, line by line, exactly as written.
            ///
            /// Emitted as data for two readers. The generated reference page
            /// needs the prose; and a test needs it in order to hold the prose
            /// to the same standard as the columns, because a doc comment
            /// naming an error enum that no [`Self::prevents`] points at is
            /// precisely the unchecked claim that column exists to replace.
            #[must_use]
            pub const fn docs(self) -> &'static [&'static str] {
                match self {
                    $(Self::$variant => &[$($doc),+],)+
                }
            }

            /// Parse a code from its string form.
            ///
            /// # Errors
            /// Returns [`UnknownDiagnosticCode`] if `s` is not a known code.
            pub fn parse(s: &str) -> Result<Self, UnknownDiagnosticCode> {
                match s {
                    $($code => Ok(Self::$variant),)+
                    other => Err(UnknownDiagnosticCode(other.to_owned())),
                }
            }
        }
    };
}

diagnostic_codes! {
    // ---------------------------------------------------------------- GBX01xx
    /// A `.gdl` file could not be parsed as Starlark.
    GdlParse = "GBX0101", Gdl, Error, false, "GDL parse error";

    /// A `.gdl` file parsed but failed during evaluation.
    GdlEval = "GBX0102", Gdl, Error, false, "GDL evaluation error";

    /// A GDL file used a construct that encodes a decision -- `if`, `for`,
    /// `while`, `def`, `lambda`, a conditional expression, a comprehension, or a
    /// boolean short-circuit.
    ///
    /// GDL describes facts; the resolver makes decisions. Allowing a branch here
    /// would move a decision out of the resolver and destroy determinism and
    /// explainability (`cpt-gearbox-fr-gdl-declarative`).
    GdlForbiddenConstruct = "GBX0103", Gdl, Error, false, "forbidden GDL construct";

    /// A `load()` referenced a path outside the declaring file's source root.
    GdlLoadEscape = "GBX0104", Gdl, Error, false, "GDL load escapes its source root";

    /// A file declared no `gear()`/`product()`, or more than one.
    GdlCardinality = "GBX0105", Gdl, Error, false, "wrong number of top-level declarations";

    /// A call received an argument this vocabulary does not define.
    GdlUnknownArgument = "GBX0106", Gdl, Error, false, "unknown GDL argument";

    /// A construct was accepted for forward compatibility but excluded from
    /// resolution.
    GdlDowngraded = "GBX0107", Gdl, Warning, true, "GDL construct accepted but not resolved";

    /// A gear's `category` is not one the platform uses.
    ///
    /// A warning rather than an error, deliberately. The taxonomy is visibly
    /// still settling -- `cluster` is filed under `serverless` and
    /// `account-management` under `oss` -- so treating the set as closed would
    /// claim more than the evidence supports. It still catches a typo, which is
    /// the failure that matters: a category nothing else uses puts a gear in a
    /// bucket of one.
    GdlUnknownCategory = "GBX0108", Gdl, Warning, false, "gear category is not one the platform uses";

    /// A declared documentation, specification or test path does not exist,
    /// and the gear names no `source = git(...)` to find it in.
    ///
    /// An error, unlike the absence of a file found by convention: a gear may
    /// genuinely have no PRD, but a path written by hand and pointing nowhere is
    /// a typo.
    GdlMissingDocPath = "GBX0109", Gdl, Error, false, "declared documentation or test path does not exist";

    /// Two declarations scoped to the same profile collide.
    GdlDuplicateProfileScoped = "GBX0110", Gdl, Error, false, "duplicate profile-scoped declaration";

    /// Resolution was asked for a profile the description does not declare.
    ///
    /// A caller's mistake rather than the description's, and reported rather
    /// than defaulted: silently resolving `embedded` when someone asked for
    /// `prod` would produce a plausible product with the wrong topology, which
    /// is the one outcome worse than refusing.
    GdlUnknownProfile = "GBX0111", Gdl, Error, false, "unknown deployment profile";

    /// A declared `config_schema` names a struct the gear's crate does not
    /// declare, or the crate deserializes more than one type as its config.
    ///
    /// An error for the same reason `GBX0109` is: a locator written by hand and
    /// pointing at nothing is a typo, and a configuration surface projected from
    /// the wrong struct would offer fields the gear never reads.
    GdlConfigStructNotFound = "GBX0112", Gdl, Error, false, "config_schema names no usable struct";

    /// A `config = {...}` value does not match the type its gear declares for
    /// that field.
    ///
    /// An error rather than a warning: unlike an unfamiliar category (GBX0108),
    /// there is no reading under which the product still works. A `bind_addr`
    /// set to `True` builds a binary that starts and then fails to deserialize
    /// its own generated configuration -- the same failure, moved somewhere
    /// nobody is looking.
    ///
    /// Only fields in the projected schema are type-checked. A key the schema
    /// does not name is [`GdlUnknownConfigKey`], not this code.
    GdlConfigTypeMismatch = "GBX0113", Gdl, Error, false, "config value does not match the declared field type",
        prevents = Prevents::error("cf-gears-toolkit", "ConfigError", "InvalidConfig");

    /// A config key a gear declares as an endpoint's `config_key` was also set
    /// in the description, where it has no effect.
    ///
    /// A warning, not an error: the product resolves and builds, and the value
    /// is simply not the one used. But it is worth saying, because the failure
    /// it prevents is an operator setting a bind address, seeing the generated
    /// file disagree, and having nothing to read that explains why. The port
    /// comes from the topology the resolver decided; a value written by hand
    /// would be describing a product that was not resolved.
    GdlConfigKeyDerived = "GBX0114", Gdl, Warning, false, "config key is derived from the topology and cannot be set here";

    /// A `config = {...}` key is not a field the gear's projected schema names.
    ///
    /// An error because generated YAML is deserialized with `deny_unknown_fields`:
    /// writing the key produces a file the runtime refuses at startup. A field
    /// the gear reads must appear in the schema; a typo must not reach the file.
    GdlUnknownConfigKey = "GBX0115", Gdl, Error, false, "config key is not declared by the gear",
        prevents = Prevents::error("cf-gears-toolkit", "ConfigError", "InvalidConfig");

    /// A credential written into the description, where it would be committed.
    ///
    /// An error rather than a rewrite: replacing the value would leave the
    /// original in the `.gdl` file, which is the place the requirement names.
    /// The author has to take it out, and the help says what to write instead.
    GdlLiteralSecret = "GBX0116", Gdl, Error, false, "a credential is written into the description";

    /// Two roles claim the gear's own id as their directory name.
    ///
    /// A role registers under its own name, and the one that registers under
    /// the gear's bare id is the front door: a bare-name lookup reaches it and
    /// only it. That guarantee is structural rather than a filter -- an
    /// internal role is unreachable by the bare name because it was never
    /// registered under one -- and it holds only while exactly one role claims
    /// it. Two, and a bare-name lookup round-robins over two different
    /// services.
    ///
    /// Zero is allowed and ordinary: a gear whose roles are all internal has no
    /// front door, which is a shape the platform's model permits.
    GdlDuplicateFrontDoor = "GBX0117", Gdl, Error, false, "two roles claim the gear's own name";

    /// A role's directory registration name is not kebab-case.
    ///
    /// **The one identifier in the system that was not held to the rule every
    /// other one obeys.** `GearId`, `ApplicationId`, `SourceId` and `ProfileId`
    /// all go through the same validator -- lowercase, digits, single interior
    /// hyphens, no `_` -- because it is the rule `#[toolkit::gear]` itself
    /// enforces. `directory_name` was a plain `String`, checked by nobody, and
    /// it names the same thing a `GearId` names: an entry in the directory that
    /// a bare-name lookup resolves.
    ///
    /// The default is where this actually bites. Left out, a role's
    /// registration name is `<gear-id>-<role-name>`, and a role name is a
    /// *serde variant spelling* -- `cluster_ingest` -- so the default splices a
    /// kebab id onto a `snake_case` value and yields `event-broker-cluster_ingest`.
    /// That is not the `event-broker-ingest` the platform's own ADR tabulates,
    /// and it is not a name a Kubernetes `Service` can carry.
    ///
    /// An error, because the remedy is one field and the alternative is a
    /// workload registered under a name the rest of the system cannot express.
    GdlRoleNameNotKebab = "GBX0118", Gdl, Error, false, "a role's directory name is not kebab-case";

    /// A role names a value the gear's own configuration enum does not accept.
    ///
    /// **The check three doc comments promised and none performed.** `role(name
    /// = ...)` is defined -- in `gear_globals`, in `DeclaredRole` and in
    /// `ApplicationRole` -- as "the value the gear's own mode selector accepts,
    /// which is the only spelling checkable against a projected enum". Nothing
    /// checked it, so a role could name a mode the gear would refuse at startup,
    /// and the join between the description's half and Rust's half was a string
    /// nobody compared.
    ///
    /// It is the same comparison `GBX0113` already makes for a config value a
    /// product sets, against the same projected `Enum` variants, so the rule is
    /// not new -- only its second caller.
    ///
    /// **Silent when there is nothing to check against**, which is the honest
    /// answer rather than a guess: a gear that exposes no enum field may still
    /// select its mode from a field its description does not put in front of an
    /// integrator, and refusing on that would refuse descriptions that are
    /// correct. Reported only when an enum field exists and no enum field
    /// accepts the name.
    GdlRoleNotAMode = "GBX0119", Gdl, Error, false, "a role names a value the gear's config enum does not accept";

    /// A required configuration field has no value and no default.
    ///
    /// **The code the Studio's own configurator already promised.** Its
    /// `valueMissing` says of a required field with nothing set that "what it
    /// buys is that `required` is visible before a GBX code explains it from the
    /// other side of the screen" -- and there was no such code. `required` is
    /// projected from the gear's config struct and reached exactly one reader,
    /// a marker in a panel.
    ///
    /// What it costs is measurable. `EventBrokerConfig.mode` is required with no
    /// default and the runtime's loader is strict, so a product selecting that
    /// gear without setting `mode` generates a file the gear refuses at `init`
    /// with `missing field`. Nothing between the description and that failure
    /// said a word.
    ///
    /// **Derived keys are excluded, and that exclusion is load-bearing.**
    /// `ApiGatewayConfig.bind_addr` is required, has no default, and is written
    /// by the generator from the port the resolver assigned -- so a rule that
    /// counted it would fire on every product in the corpus. The keys a gear's
    /// `serves` declares are the same set `GBX0114` already refuses a value for.
    ///
    /// A warning rather than an error: a value may still arrive from a profile,
    /// or from an operator editing the generated file, and this cannot see
    /// either. What it can say is that nothing in the description supplies it.
    GdlRequiredConfigUnset = "GBX0120", Gdl, Warning, false, "a required config field has no value and no default";

    /// A gear's `source = git(...)` disagrees with the checkout its
    /// description was read from: no remote has that url, or the gear is not
    /// at that `path` in it.
    ///
    /// A warning: what is on disk is used either way. But a reader holding
    /// the description alone follows `source`, and would land in the wrong
    /// repository or directory.
    GdlSourceMismatch = "GBX0121", Gdl, Warning, false, "a gear's declared git source disagrees with its checkout";

    /// A gear says `implementation = "done"` while a scenario it declares is
    /// not `done`.
    ///
    /// A warning: either the list or the status is stale, and only the author
    /// knows which.
    GdlOpenScenarios = "GBX0122", Gdl, Warning, false, "a gear is declared done with scenarios still open";

    /// A file's `gdl_version` is not `MAJOR.MINOR`, or names a major version
    /// this Gearbox does not read.
    ///
    /// Reported before evaluation: a different major may have removed or
    /// renamed what this reader knows, so its errors would be the wrong ones.
    GdlVersionUnsupported = "GBX0123", Gdl, Error, false, "unreadable or unsupported gdl_version";

    // ---------------------------------------------------------------- GBX02xx
    // GBX0201-GBX0205 are deliberately absent. They compared a `gear.gdl`
    // restatement of the gear id, co-location dependencies, runtime
    // capabilities, provided contracts and consumed contracts against the Rust
    // attributes that also carried them. Under the macro-projected catalogue
    // (ADR `cpt-gearbox-adr-macro-projected-catalogue`) those facts exist in
    // exactly one place, so there is no second copy left to diverge and nothing
    // for a comparison to report. Their replacement is GBX0210, which refuses
    // the restatement outright rather than detecting it afterwards.
    //
    // The codes are not reused: a lock or a transcript naming GBX0203 should
    // stay findable rather than silently meaning something else.

    /// The `consumer_wiring` override key does not match the one the macro
    /// emits.
    ///
    /// The runtime's static endpoint resolver reads
    /// `gears.{owner_gear}.config.consumer_wiring.{dep_gear}`, and
    /// `#[toolkit::consumes]` fills both segments out of Rust:
    ///
    /// * `owner_gear` from the kebab-case of the annotated struct's identifier,
    ///   *not* from `#[toolkit::gear(name = ...)]` -- a separate attribute
    ///   cannot read that argument;
    /// * `dep_gear` verbatim from `from = "..."`, which the macro crate's own
    ///   test pins with the comment "`from` is a directory lookup key and must
    ///   survive untouched".
    ///
    /// Either segment being wrong makes the override key unreachable: the
    /// generator writes it under one name, the runtime looks for it under
    /// another, and the runtime only warns.
    ///
    /// Two conditions, one code, because the outcome is one outcome. The other
    /// is a description that declares a consumption with no attribute behind
    /// it: no registration is emitted, so the wiring phase has nothing to
    /// replay and the edge is never established -- and there is not even a gear
    /// name to resolve against, since the attribute is the only place one is
    /// written.
    ///
    /// `dep_gear` is not cross-checked, because there is nothing to cross-check
    /// it against. `consume(from_ = ...)` was a second copy of the attribute's
    /// `from`, the class ADR `cpt-gearbox-adr-macro-projected-catalogue`
    /// retired GBX0201-GBX0205 over, and it is refused by GBX0210 rather than
    /// compared.
    ValidateConsumerWiringMismatch = "GBX0206", Validate, Error, true, "the consumer_wiring override key does not match the one the macro emits";

    // GBX0207 is deliberately absent. It would have compared a contract trait's
    // name against its `#[toolkit::contract]` -- and both halves are already
    // compile errors in `toolkit-contract-macros`:
    //
    //   * an unrecognised suffix is rejected by `ContractKind::from_suffix`
    //     (`libs/toolkit-contract-macros/src/parse.rs:70`);
    //   * a trailing major marker that disagrees with `version` is rejected a
    //     few lines below it (`parse.rs:83`), citing ADR-0007.
    //
    // So a crate exhibiting either does not build, and a catalogue is only ever
    // assembled from crates that do. Retired for the same reason as
    // GBX0201-GBX0205 but by a different mechanism: not "one authority" but
    // "the divergence cannot survive compilation".
    //
    // One residual case does compile: an *unmarked* trait name with
    // `version = "v2"` or later. The macro permits it on purpose -- ADR-0007
    // makes an unmarked name unconstrained, because a v1 contract keeps its
    // unmarked name when v2 is added beside it. Reporting it would contradict
    // the platform's own decision, and the tree contains no instance, so it is
    // recorded as a known gap rather than as a code. What *is* checked is that
    // the projector's suffix and marker rules still agree with the macro's, in
    // `crates/gearbox-ir/tests/contract_shape.rs`.

    /// A selected gear crate has no `gear.gdl`.
    ValidateMissingDescription = "GBX0208", Validate, Error, false, "gear has no gear.gdl";

    /// The declared library identifier does not match the crate's actual one.
    ///
    /// A crate without an explicit `[lib]` section takes its library identifier
    /// from the package name, which is why the identifier must be declared
    /// rather than derived.
    ValidateLibIdentMismatch = "GBX0209", Validate, Error, false, "declared library identifier does not match the crate";

    /// A description restated a fact that is projected from the Rust attributes.
    ///
    /// Refusing rather than tolerating is what keeps the projection decision
    /// alive: without an active rejection the mirrored surface returns by
    /// accretion, one convenient field at a time, and the design decays back
    /// into a cross-check (`cpt-gearbox-fr-gdl-no-restatement`).
    ValidateRestatement = "GBX0210", Validate, Error, false, "description restates a projected fact";

    /// A gear's `#[toolkit::gear]` attribute could not be located
    /// unambiguously.
    ///
    /// Either the scanned tree held none, or it held several -- one crate may
    /// legitimately declare more than one gear, and `gears/mini-chat/mini-chat`
    /// declares three. Projection is meaningless until exactly one attribute is
    /// identified, so this is an error rather than a guess
    /// (`cpt-gearbox-fr-attribute-location`).
    ValidateAttributeAmbiguous = "GBX0211", Validate, Error, false, "gear attribute could not be located unambiguously";

    /// A description exposes a configuration field its gear's struct does not
    /// declare.
    ///
    /// The check that keeps `exposes` from becoming a second copy of the struct.
    /// A curated list is a product judgement and legitimately declared, but it
    /// refers to Rust facts, and a reference that no longer resolves is drift --
    /// detected here rather than surfacing as a control writing a key the gear
    /// ignores.
    ValidateConfigFieldUnknown = "GBX0212", Validate, Error, false, "exposed config field is not declared by the gear";

    /// A description offers a Cargo feature its gear's crate does not declare.
    ///
    /// The same check `GBX0212` makes for `exposes`, for the same reason:
    /// `cargo_features` is a curation of a projected fact, and a curation whose
    /// names have drifted from the `[features]` table would offer a build that
    /// cannot succeed. Cargo fails on an unknown `--features` name, so the
    /// alternative to reporting it here is a build failure two steps later with
    /// nothing pointing back at the description.
    ValidateFeatureUnknown = "GBX0213", Validate, Error, false, "offered Cargo feature is not declared by the crate";

    // ---------------------------------------------------------------- GBX03xx
    /// A selected or depended-upon gear is not in the catalogue.
    TopologyUnknownGear = "GBX0301", Topology, Error, false, "unknown gear",
        prevents = Prevents::error("cf-gears-toolkit", "RegistryError", "UnknownGear");

    /// Co-location dependencies form a cycle.
    TopologyDepsCycle = "GBX0302", Topology, Error, false, "co-location dependency cycle",
        prevents = Prevents::error("cf-gears-toolkit", "RegistryError", "CycleDetected");

    /// An application contains more than one REST host gear.
    ///
    /// The runtime registry permits exactly one, and says so before any gear
    /// starts: the build phase stops with `RegistryError::MultipleRestHosts`.
    TopologyMultipleRestHost = "GBX0303", Topology, Error, true, "more than one REST host in an application",
        prevents = Prevents::error("cf-gears-toolkit", "RegistryError", "MultipleRestHosts");

    /// An application contains more than one gRPC hub gear.
    ///
    /// The same rule as [`TopologyMultipleRestHost`] and the same enforcement:
    /// the registry refuses the build with `RegistryError::MultipleGrpcHubs`.
    /// Written down here for the first time, because until the reference
    /// existed there was nothing to write it against.
    TopologyMultipleGrpcHub = "GBX0304", Topology, Error, true, "more than one gRPC hub in an application",
        prevents = Prevents::error("cf-gears-toolkit", "RegistryError", "MultipleGrpcHubs");

    /// A process exposes REST interfaces but contains no REST host to mount them.
    TopologyRestWithoutHost = "GBX0305", Topology, Error, false, "REST gears with no REST host",
        prevents = Prevents::error("cf-gears-toolkit", "RegistryError", "RestRequiresHost");

    /// A process contains a database-backed gear but no database is configured.
    TopologyDbWithoutDatabase = "GBX0306", Topology, Error, false, "database gear with no database configured";

    /// The requested shape cannot exist in the `embedded` profile, which is a
    /// single process by definition.
    TopologyEmbeddedViolation = "GBX0307", Topology, Warning, false, "request is incompatible with the embedded profile";

    /// Directory-based discovery was selected but the host process has no
    /// directory server gear.
    TopologyNoOrchestrator = "GBX0308", Topology, Error, false, "directory discovery without a directory server";

    /// Directory-based discovery was selected but the host process has no gRPC
    /// hub.
    ///
    /// The host's worker-spawn phase blocks waiting for the gRPC hub endpoint,
    /// because that is the directory address it hands to each child.
    TopologyNoGrpcHub = "GBX0309", Topology, Error, true, "directory discovery without a gRPC hub";

    /// A worker process has no resolvable executable path.
    TopologyNoTargetDir = "GBX0310", Topology, Error, false, "worker has no resolvable executable path";

    /// A gear in the closure was not placed in any application.
    TopologyOrphanGear = "GBX0311", Topology, Error, false, "gear placed in no application";

    /// A worker application contains a REST host gear.
    ///
    /// A worker serves over its own out-of-process HTTP router, not through the
    /// API gateway, so a REST host there would never receive traffic.
    TopologyRestHostInWorker = "GBX0312", Topology, Error, true, "REST host in a worker application";

    /// A `self_hosted` profile produced workers but no host to spawn them.
    ///
    /// Spawn specs are attached to the host application. Without one they are
    /// dropped, and the workers the lock named never start.
    TopologyNoHost = "GBX0313", Topology, Error, false, "workers have no host application to spawn them";

    /// A process registers gRPC services and contains no gRPC hub to mount them.
    ///
    /// The counterpart of [`TopologyRestWithoutHost`], and the one that was
    /// missing: the runtime refuses this outright with
    /// `RegistryError::GrpcRequiresHub`, so a product that resolves clean here
    /// dies while building its registry.
    ///
    /// **Unlike its REST neighbour this applies to every process, not only the
    /// host.** `run_grpc_phase` is reached from `run_phases_internal` *and*
    /// from `run_oop_serving`, so a worker that links a service-registering
    /// gear needs a hub of its own. The asymmetry is the runtime's, not an
    /// oversight here: a worker publishes REST through its own out-of-process
    /// router and needs no `rest_host`, while it has no such second path for
    /// gRPC.
    ///
    /// The corpus makes this reachable rather than theoretical: `cluster` and
    /// `service-discovery` both declare `grpc` and neither declares `deps`, so
    /// nothing drags a hub in beside them.
    TopologyGrpcWithoutHub = "GBX0314", Topology, Error, true, "gRPC gears with no gRPC hub",
        prevents = Prevents::error("cf-gears-toolkit", "RegistryError", "GrpcRequiresHub");

    /// A process contains a registration host and nothing for it to host.
    ///
    /// The mirror of [`TopologyRestWithoutHost`] and
    /// [`TopologyGrpcWithoutHub`], which ask whether the gears have a host;
    /// this asks whether the host has any gears. A `grpc-hub` alone binds its
    /// port, mounts zero routes and registers an empty service list -- a
    /// process that starts cleanly and does nothing, which is the shape a
    /// person least expects to be told about.
    ///
    /// **Derived from `runtime_caps`, not declared.** The two capabilities that
    /// [`RuntimeCap::is_process_singleton`] names are exactly the two
    /// in-process registration hosts: a gear hands its routes or its service
    /// registrations to them through a Rust closure in one address space, so a
    /// host is only meaningful as a co-tenant. Nothing new has to be stated in
    /// a `gear.gdl` for this to be knowable.
    ///
    /// A warning rather than an error: the process is pointless, not wrong, and
    /// where isolating a host actually breaks something the breakage is
    /// reported on the other side -- the gears it left behind raise GBX0305 or
    /// GBX0314, which are errors.
    TopologyHostWithNothingToHost = "GBX0315", Topology, Warning, true, "registration host with nothing to host";

    /// A selected Cargo feature belongs to a deployment kind this profile is not.
    ///
    /// `cargo_features` lets a gear say where a feature belongs, and `k8s-auth`
    /// is why: it wires authentication to a Kubernetes service account, so a
    /// Kubernetes deployment needs it and an embedded or self-hosted one must
    /// not have it. Selecting it anyway produces a binary that looks for a token
    /// path that does not exist, and it fails at startup rather than at build
    /// time -- which is exactly the class of mistake the projected feature list
    /// was introduced to stop.
    ///
    /// An error, because the remedy is always available and always the same:
    /// drop the feature, or resolve for the profile it belongs to.
    TopologyFeatureWrongKind = "GBX0316", Topology, Error, true, "a selected Cargo feature does not belong to this deployment kind";

    /// A contract's owner is not a name the catalogue answers to.
    ///
    /// `#[toolkit::contract(gear = "...")]` is a free string that flows verbatim
    /// into the descriptor's owner and into the contract's own id, which is
    /// `{owner}/{base}@{version}`. Both paths that build it guard only against a
    /// *malformed* id -- one falls back to the declaring gear on a parse failure,
    /// the other diagnoses one -- so any valid kebab name passes and the owner
    /// can name a gear nothing describes. The catalogue then holds a contract
    /// whose identity nothing else can match.
    ///
    /// Two causes, both actionable: a typo in the attribute, and a description
    /// that was never written.
    ///
    /// **Deliberately not `TopologyUnknownGear`, which would otherwise be the
    /// code for this.** That one carries a claim about a registry failure at
    /// startup, and it earns it: it is raised for `use_gear`, for a plugin under
    /// a host and for a `deps` entry, all of which the runtime tries to link.
    /// Nothing tries to link a contract owner, so the claim does not transfer.
    ///
    /// **A role's directory name satisfies this.** Under ADR
    /// `cpt-gearbox-adr-role-qualified-names` a role registers under its own
    /// directory name, and a contract answering to that role names it here. So
    /// the question is whether the owner is any name the catalogue answers to --
    /// a gear id, or a declared role's `directory_name` -- rather than whether
    /// it is a gear id.
    TopologyUnknownContractOwner = "GBX0317", Topology, Error, false, "contract owner is not a name the catalogue declares";

    /// A selected gear declares roles, and a product can deploy at most one of
    /// them.
    ///
    /// A role registers under its own directory name, so a role-split gear is
    /// several named workloads built from one binary. This tool builds one
    /// application per anchor gear -- the worker anchors are a set, and a pin
    /// is looked up rather than filtered for -- so a second role has nowhere
    /// to be.
    ///
    /// The limitation is here rather than in the runtime, which takes whatever
    /// directory name it is given. Reported at resolution rather than at load
    /// because it is a statement about a product: a gear whose roles nobody
    /// selects costs nothing.
    ///
    /// A warning, because the description is not wrong -- it describes a shape
    /// this tool does not build yet (ADR
    /// `cpt-gearbox-adr-role-qualified-names`).
    TopologyRolesNotDeployable = "GBX0318", Topology, Warning, false, "a gear's roles cannot all be deployed";

    /// An `application(...)` names a role its anchor does not declare.
    ///
    /// The product's half of the join is a string, and the gear's half is the
    /// list of roles it declares -- so a typo names a role nothing answers to,
    /// and the application would be built under a directory name no instance
    /// ever registers. An error, because the remedy is a spelling and the
    /// alternative is a workload nobody can reach.
    ///
    /// Reported at resolution rather than when the description is read: whether
    /// a gear declares a role is a fact about the catalogue, and the layer that
    /// lowers `product.gdl` does not have one.
    TopologyUnknownRole = "GBX0319", Topology, Error, false, "an application names a role its anchor does not declare";

    /// Two applications register the same name in the directory.
    ///
    /// A worker registers one name -- its own, or its role's. A host registers
    /// one per REST provider it contains, plus one per gRPC provider, each
    /// under that gear's own name. So the same name can be registered twice:
    /// by a host that reaches a gear through its closure, and by a worker
    /// anchored on that same gear because a description forced it out. A
    /// consumer resolving the name then round-robins between two endpoints
    /// with nothing marking either as the one meant.
    ///
    /// **Not about linking.** A gear compiled into several binaries is expected
    /// and correct -- co-location is a closure, not a partition -- and produces
    /// no second registration by itself: a gear linked into a worker anchored
    /// on something else is not registered there at all. Linked, serving
    /// routes, and registered under a name are three different sets, and only
    /// the third can collide.
    ///
    /// **A warning ordinarily and an error for a gear declared
    /// `one_per_installation`.** Round-robin between two copies of a stateless
    /// service is load balancing; between two that own disjoint state it is
    /// corruption, and only the gear can say which it is
    /// (ADR `cpt-gearbox-adr-one-per-installation`).
    TopologyDuplicateRegistration = "GBX0320", Topology, Warning, false, "two applications register the same directory name";

    /// A product selects a gear that is described but has no code yet.
    ///
    /// `implementation = "design"` in its `gear.gdl`: the catalogue knows the gear's
    /// id, purpose and documents, and there is no crate to link. Distinct from
    /// GBX0301 because "not in the catalogue" sends the reader looking for a
    /// typo or a closed source root, and neither is the problem.
    TopologyDesignGear = "GBX0321", Topology, Error, false, "gear is at design maturity";

    /// A product links a gear its description calls `experimental`: its API
    /// and behaviour may change freely. A warning, because using one is a
    /// choice a product may make on purpose -- but it should be a choice.
    TopologyExperimentalGear = "GBX0322", Topology, Warning, false, "gear is experimental";

    /// A product links a gear at `preview`: usable, not declared stable.
    /// Information, not a warning: most of the platform is here today, and a
    /// warning on every product would be noise that hides GBX0322 and GBX0324.
    TopologyPreviewGear = "GBX0323", Topology, Info, false, "gear is at preview";

    /// A product links a gear its description calls `deprecated`: still
    /// available, not for new products.
    TopologyDeprecatedGear = "GBX0324", Topology, Warning, false, "gear is deprecated";

    // ---------------------------------------------------------------- GBX04xx
    /// This consumer and provider could be placed in separate processes, but the
    /// dependency between them is not declared as a contract consumption.
    ///
    /// Not an error: it is the actionable work list
    /// (`cpt-gearbox-fr-report-cuttable-if-declared`). Direct type-keyed client
    /// lookups are widespread, so separating an undeclared pair would fail at
    /// runtime; the accompanying help text is the exact edit that would make the
    /// separation legal.
    BindingCuttableIfDeclared = "GBX0401", Binding, Info, false, "edge would be severable if declared";

    /// gRPC was requested for a severed edge, and is not available there.
    ///
    /// The consumption macro emits a REST resolving client only; there is no
    /// gRPC path. Cross-process gRPC exists in the runtime, but only through
    /// hand-written wiring.
    BindingGrpcUnsupported = "GBX0402", Binding, Warning, true, "gRPC is unavailable on a severed edge";

    /// An in-process-only contract would cross a process boundary.
    ///
    /// Only the remote-capable contract kinds may cross; the others are
    /// in-process by definition of their kind.
    BindingInProcessOnlyContract = "GBX0403", Binding, Error, true, "in-process-only contract crosses a process boundary";

    /// A consumed contract has no provider in the product.
    BindingNoProvider = "GBX0404", Binding, Error, false, "consumed contract has no provider";

    /// The provider offers no matching major version of the consumed contract.
    ///
    /// Compatibility is exact major equality: parallel majors coexist by design
    /// and there is no adapter between them.
    BindingMajorMismatch = "GBX0405", Binding, Error, false, "contract major version mismatch";

    /// The provider declares no remote-capable transport, so the edge cannot be
    /// severed.
    BindingNoRemoteTransport = "GBX0406", Binding, Warning, false, "provider declares no remote transport";

    /// The binding is local because the provider is inside the consumer's
    /// co-location closure, regardless of configuration.
    ///
    /// The runtime short-circuits to a local instance when one is present in the
    /// process, so configuring a remote endpoint here would have no effect.
    BindingForcedLocal = "GBX0407", Binding, Info, true, "binding forced local by co-location";

    /// A remote binding depends on a directory that the resolved topology does
    /// not make reachable.
    BindingDirectoryUnreachable = "GBX0408", Binding, Error, false, "directory unreachable for a remote binding";

    /// The consumer endpoint override cannot be expressed as an environment
    /// variable and must be written into configuration.
    ///
    /// The runtime's environment-key remapping converts underscores to hyphens
    /// only in the segment immediately following the gears prefix, so a
    /// hyphenated dependency name nested deeper can never be matched.
    BindingEnvCannotExpressWiring = "GBX0409", Binding, Warning, true, "endpoint override cannot come from the environment";

    /// A product preference was parsed and recorded, but the resolver does not
    /// yet honour it. Silent ignore would let an operator believe the topology
    /// changed when it did not.
    PreferenceNotHonoured = "GBX0410", Binding, Warning, false, "preference is recorded but not honoured";

    /// A binding declares an `endpoint`, and nothing reads it.
    ///
    /// The field is in the language and in the IR, with a doc comment promising
    /// "a pinned address, overriding whatever discovery would produce" -- and
    /// the resolver takes only `mode` and `transport` from the declaration, so
    /// the address is dropped in silence. Somebody reached for the escape hatch
    /// and the hand did not close.
    ///
    /// Refused rather than honoured, and that is a holding position rather than
    /// a verdict. Honouring it means deciding what an address outside the
    /// product *means* -- a provider this product does not build, does not
    /// place in a process and cannot see the topology of -- which is a model
    /// question and not a resolver patch. Until that is answered, a refusal is
    /// the honest reading of a value that changes nothing.
    BindingEndpointNotHonoured = "GBX0411", Binding, Error, false, "declared endpoint is not honoured";

    /// The endpoint override key names the provider the resolver selected, and
    /// the runtime reads the one the consumer declared.
    ///
    /// The key is built out of the provider the resolver chose, while
    /// `#[toolkit::consumes]` emits its last segment verbatim from its own
    /// `from`. The two agree in every well-formed product and diverge in
    /// exactly one place: when [`BindingNoProvider`] found the declared
    /// provider does not provide the contract and fell back to the only other
    /// gear that does. The generated configuration then looks right, the
    /// operator's override lands where nothing looks for it, and the consumer
    /// resolves nothing at run time.
    ///
    /// **An error, and scoped so that is defensible.** Only a statically
    /// discovered remote binding consults this key at all -- directory
    /// discovery names a lookup instead and is silent here -- so the report is
    /// confined to the one configuration where the key is load-bearing. The
    /// fallback that produces it is deliberately tolerant, because a gear
    /// rename should not make a product unresolvable; refusing to *generate*
    /// from it is a narrower claim than refusing to resolve it, and the remedy
    /// is one string in one attribute.
    BindingWiringKeySkew = "GBX0412", Binding, Error, false, "the endpoint override key names a different provider than the consumer declared";

    // ---------------------------------------------------------------- GBX05xx
    /// A cluster provider was selected automatically.
    ClusterAutoSelected = "GBX0501", Cluster, Info, false, "cluster provider selected automatically";

    /// No registered provider satisfies the required capabilities.
    ClusterUnsatisfiable = "GBX0502", Cluster, Error, true, "no cluster provider satisfies the required capabilities",
        prevents = Prevents::canonical_error(
            "cf-gears-cluster-sdk", "ClusterError", "CapabilityNotMet",
            "cluster.v1", "capability_not_met",
        );

    /// A process-local coordination backend was selected for a topology with
    /// more than one process or replica.
    ///
    /// This is a silent correctness failure at runtime rather than a startup
    /// error: an in-memory backend starts successfully in every replica and
    /// elects one leader per replica.
    ClusterProcessLocalInMultiProcess = "GBX0503", Cluster, Error, true, "process-local cluster backend in a multi-process topology";

    /// The primitive resolved to the SDK's compare-and-swap default layered over
    /// the profile's cache, because no dedicated backend is registered for it.
    ClusterSdkDefault = "GBX0504", Cluster, Info, true, "resolved to the SDK compare-and-swap default";

    /// The named cluster provider is not registered in the runtime.
    ClusterUnregisteredProvider = "GBX0505", Cluster, Error, true, "cluster provider is not registered";

    /// The selected provider needs credentials and none were supplied.
    ClusterNoCredentialSource = "GBX0506", Cluster, Error, false, "cluster provider has no credential source";

    /// A stateful gear runs with several replicas and nothing coordinates them.
    ///
    /// Filed as a cluster concern rather than a runtime gap: the runtime is
    /// perfectly capable of leader election, so this is a statement about the
    /// product, not about a missing capability.
    ClusterStatefulReplicasWithoutElection = "GBX0507", Cluster, Warning, false, "replicated stateful gear without leader election";

    /// A cluster requirement names a profile the requiring gear does not
    /// implement.
    ///
    /// The profile is the routing key: the SDK maps it to
    /// `ClientScope::new("cluster:{name}")` and resolves whatever backend is
    /// registered there. A name nothing implements cannot be bound, so the
    /// requirement fails at startup with `ProfileNotBound` -- which is why this
    /// is caught here instead. Filed as a user error rather than a runtime gap:
    /// the runtime behaves correctly, the description is wrong.
    ClusterProfileNotImplemented = "GBX0508", Cluster, Error, false, "cluster profile is not implemented by the gear",
        prevents = Prevents::canonical_error(
            "cf-gears-cluster-sdk", "ClusterError", "ProfileNotBound",
            "cluster.v1", "profile_not_bound",
        );

    /// A registered cluster provider's name or capabilities could not be read
    /// out of Rust.
    ///
    /// Reported rather than skipped. A provider missing from the catalogue would
    /// silently narrow what the resolver believes is available, turning a
    /// readable failure into an unsatisfiable-capability error somewhere else.
    ClusterProviderUnprojectable = "GBX0509", Cluster, Error, false, "cluster provider could not be projected";

    /// A plugin crate holds more than one implementation of a backend trait, so
    /// which one a provider builds cannot be determined by trait alone.
    ///
    /// Capabilities live on the backend, not the provider, and the value flow
    /// from provider to backend runs through a builder and an `Arc<dyn _>` that
    /// no source-level parse can follow. Uniqueness within the crate is what
    /// makes the backend locatable; when it does not hold, the description must
    /// narrow it explicitly.
    ClusterBackendAmbiguous = "GBX0510", Cluster, Error, false, "cluster backend implementation is ambiguous";

    /// A selected host has an extension point with no implementation selected.
    ///
    /// The host starts and then fails at the first request that needs the
    /// plugin: it queries types-registry, finds no instance for its vendor, and
    /// has nothing to route to. Whether a product tolerates that is a product
    /// decision, not a property of the gear's code, which is why there is no
    /// `optional` field on the gear side.
    PluginPointUnfilled = "GBX0511", Cluster, Error, false, "plugin extension point has no implementation",
        prevents = Prevents::error("cf-gears-toolkit", "ChoosePluginError", "PluginNotFound");

    /// The host's effective vendor matches no selected plugin's.
    ///
    /// Both sides read `vendor` from their own config and both compile in a
    /// default, so a product that overrides one and not the other produces a
    /// host that resolves nothing -- silently, at runtime. `gears-rust` keeps
    /// this correct today with a hand-written comment in its E2E config.
    PluginVendorMismatch = "GBX0512", Cluster, Error, false, "no selected plugin matches the host's vendor",
        prevents = Prevents::error("cf-gears-toolkit", "ChoosePluginError", "PluginNotFound");

    /// A plugin is selected but no selected gear expects its extension point.
    PluginHostNotSelected = "GBX0513", Cluster, Error, false, "plugin selected without its host";

    /// A plugin and its host were placed in different applications.
    ///
    /// A plugin registers itself with `register_scoped` into the process-local
    /// `ClientHub`, and `get_scoped` has no remote path, so the host can only
    /// find a plugin that shares its process. Neither side declares this in
    /// `deps`, which is why it has to be checked here.
    PluginNotColocated = "GBX0514", Cluster, Error, true, "plugin and host are in different applications",
        prevents = Prevents::error("cf-gears-toolkit", "ClientHubError", "ScopedNotFound");

    /// A gear other than the host consumes a plugin gear's contract.
    ///
    /// The Plugin Isolation Rule: plugin functionality is reachable only through
    /// the host's public API, which is what keeps implementations swappable.
    PluginIsolationViolated = "GBX0515", Cluster, Error, true, "plugin consumed directly instead of through its host";

    /// A declared extension point does not check out against its SDK.
    ///
    /// Points are declared, and a declaration is checked rather than trusted:
    /// the spec must be a `PluginV1`-derived GTS type the gear's SDK declares,
    /// and the trait a `pub trait` in the SDK it names. Either missing is a
    /// point that exists only in the description.
    PluginPointUndetermined = "GBX0516", Cluster, Error, false, "plugin extension point could not be determined";

    /// Several selected plugins share a vendor for one extension point.
    ///
    /// Defined behaviour rather than a fault: the host takes the lowest
    /// `priority`. Reported so the winner is visible, because linking several
    /// implementations is legitimate -- selection may be per-tenant at runtime.
    PluginVendorAmbiguous = "GBX0517", Cluster, Info, false, "several plugins share a vendor for one extension point";

    /// A gear lists a plugin under a host that does not declare that point.
    ///
    /// The gap [`PluginHostNotSelected`] leaves. That code asks whether *some*
    /// selected gear expects the plugin's point, which is the right question for
    /// a plugin selected as an ordinary gear -- and it says nothing about the
    /// `plugins = [...]` list a plugin was actually written into. So a product
    /// could list an authentication plugin under `types-registry`, whose
    /// projected `extension_points` is empty, and be told nothing: the host
    /// looks for no implementation, the plugin registers for a trait nobody
    /// queries, and the link is inert.
    ///
    /// Found by a UX pass rather than by a resolution, which is the useful part:
    /// the Add Gear panel offered the choice because nothing refused it, and a
    /// client is not a boundary (`cpt-gearbox-fr-rpc-writes-opt-in`).
    PluginPointNotDeclared = "GBX0518", Cluster, Error, false, "plugin implements a point its host does not declare";

    /// A plugin implements a spec no described gear declares as an extension point.
    ///
    /// The plugin names only the spec; which trait and which SDK are the host's
    /// to say. With no host describing it, the fill has nothing to join to, so
    /// it is reported rather than left to look connected.
    PluginSpecUndeclared = "GBX0519", Cluster, Error, false, "plugin implements a point no described gear declares";

    /// A cluster backend decides a capability at run time, so none is claimed
    /// for it at composition time.
    ///
    /// Not a defect and not a gap in the projection: the backend genuinely has
    /// no answer to give yet. The redis cache reads its consistency off the
    /// server it connects to -- single node and cluster mode differ -- so
    /// `consistency()` returns a field its startup preflight set, and
    /// `features()` computes prefix-watch the same way. Nothing in Rust states
    /// the value, so nothing can be projected as if it did.
    ///
    /// What follows is exactly right and worth saying out loud: such a provider
    /// can be named and configured like any other, answers the primitives it
    /// registers for, and satisfies **only a requirement that asks for no
    /// capability**. A `requires = [cluster.cache(capabilities = [...])]` is
    /// refused against it, because at composition time nobody can promise what
    /// depends on the server the operator will point at.
    ClusterCapabilityRuntimeDetermined = "GBX0520", Cluster, Info, true, "cluster backend decides a capability at run time";

    /// A `cluster_plugin(*_options = "...")` names a struct that cannot be read.
    ///
    /// The declaration is a join key -- it says which type a primitive's options
    /// are deserialized into -- and a key that resolves to nothing is a mistake
    /// in the declaration rather than a reason to fall back to an untyped bag.
    /// Falling back would be the worse failure: every option would validate,
    /// silently, because nothing was checking.
    ///
    /// A plugin that declares no options struct at all is not this: it keeps the
    /// untyped bag it always had, deliberately.
    ClusterProviderOptionsUnprojectable = "GBX0521", Cluster, Error, false, "a declared provider options struct cannot be read";

    /// A `provider(...)` option the backend does not read.
    ///
    /// The same statement `GBX0115` makes for a gear's config key, one layer
    /// down: every options struct in the corpus is `#[serde(deny_unknown_fields)]`,
    /// so an unknown key is already an error -- at backend startup, in a
    /// deployed system, where the description that caused it is not to hand.
    ClusterUnknownProviderOption = "GBX0522", Cluster, Error, false, "provider option is not one the backend reads",
        prevents = Prevents::error("cf-gears-toolkit", "ConfigError", "InvalidConfig");

    /// A `provider(...)` option whose value the field cannot take.
    ///
    /// A wrong scalar type, or a variant of a closed set that does not exist:
    /// `watch_mode = "disbaled"` is refused here rather than at the moment the
    /// backend parses its configuration. The same comparison `GBX0113` makes for
    /// a gear's config value.
    ClusterProviderOptionTypeMismatch = "GBX0523", Cluster, Error, false, "provider option value does not match the field's type";

    /// A `provider(...)` that omits an option the backend cannot supply itself.
    ///
    /// Required means serde would fail: no `#[serde(default)]` on the field or
    /// its container, no `default = "fn"`, and not an `Option<T>`. A default
    /// this projection cannot *read* -- `Duration::from_secs(5)`, a `const` --
    /// is still a default, and a field carrying one is never reported here. That
    /// distinction is the whole reason `required` and `default` are two fields
    /// rather than one nullable one.
    ClusterProviderOptionMissing = "GBX0524", Cluster, Error, false, "a required provider option was not supplied";

    /// A `provider(...)` names a backend this build will not contain.
    ///
    /// A registration inside a `#[cfg(feature = "...")]` exists only where that
    /// feature is enabled. The cluster crate says so of its Kubernetes
    /// providers in its own words -- "a profile binding `provider: k8s`
    /// requires a build with this feature" -- and until the catalogue could
    /// carry the condition, the only thing standing between a product and a
    /// binding its binary cannot honour was that no such provider existed. One
    /// does now, and it is the tree's first native leader election, so the
    /// failure it would cause is the silent one: a scope that looks bound and
    /// elects a leader per replica.
    ///
    /// Reported against the description rather than at startup, which is the
    /// whole point: the feature is selected two lines away, in the same file.
    ClusterProviderNeedsFeature = "GBX0525", Cluster, Error, true, "a cluster provider is not in this build";

    /// A gear declares it implements a point, and its crate implements none of that
    /// point's trait.
    ///
    /// A warning, because the implementation is evidence and not the source of
    /// the role: an impl can sit in a generic wrapper or a macro this reader
    /// cannot see. But the ordinary cause is an `implements` naming the wrong spec,
    /// and that one is worth a line.
    PluginImplMissing = "GBX0526", Cluster, Warning, false, "a plugin implements none of its point's trait";

    // ---------------------------------------------------------------- GBX06xx
    // GBX0601 is deliberately absent. It said roles were "not supported by the
    // runtime" and cited `OopRunOptions.gear_name` for it. Both halves were
    // checkable and the citation did not support the claim: that field is a
    // plain `String` the runtime accepts as given, and the literal in it is
    // written by this tool, from `generate::rust` into `worker_main.rs`.
    //
    // What cannot express a role is the model here -- one application per
    // anchor gear -- so the claim belongs in the topology range, where its
    // evidence is our own source. A runtime-gap code may not cite that, which
    // is what made the misclassification findable at all. Replaced by GBX0318
    // (ADR `cpt-gearbox-adr-role-qualified-names`).
    //
    // The code is not reused: a lock or a transcript naming GBX0601 should stay
    // findable rather than silently meaning something else.

    /// Sharding or per-instance addressing was declared, and this tool cannot
    /// emit it.
    ///
    /// **Not a runtime limitation any more, for two of the three profiles.**
    /// The directory carries instance labels, `LabelSelector` filters on them,
    /// and `oop_http.labels` is a first-class config field with environment
    /// sourcing -- so an out-of-process instance can be addressed individually
    /// today. What is missing is on this side: a resolved worker carries no
    /// labels and the generated `oop_http` section writes three keys, none of
    /// them a label, so a declaration here has no field to reach.
    ///
    /// The surviving runtime half is the embedded profile, where one process
    /// cannot host two instances of a name at all -- and the platform's own ADR
    /// states that as a coverage gap in its voice rather than a defect.
    GapShards = "GBX0602", RuntimeGap, Warning, true, "sharding and per-instance addressing are not generated";

    /// The Kubernetes profile resolves endpoints statically because no
    /// cluster-native endpoint resolver exists.
    GapNoK8sDnsResolver = "GBX0603", RuntimeGap, Warning, true, "no cluster-native endpoint resolver exists";

    /// Workers are local operating-system processes; no other spawn backend is
    /// implemented.
    GapNoRemoteSpawnBackend = "GBX0604", RuntimeGap, Warning, true, "only local process spawning is implemented";

    // GBX0605 is deliberately absent. It refused a `registry(...)` source as
    // out of scope, on the grounds that nothing existed to fetch a published
    // gear with. That stopped being true: `gearbox_engine::registry` synthesises
    // a seed manifest and lets `cargo metadata` do the download, the unpack,
    // authentication, offline mode and any configured mirror, so a registry
    // source resolves like any other and nothing refuses it. A code with no
    // firing site is a claim the tool no longer makes.
    //
    // It was also the one member of this range exempt from
    // `requires_evidence`, because it asserted a scope decision of this tool
    // rather than a limitation of the runtime -- so retiring it makes the range
    // mean one thing again.
    //
    // The code is not reused: a lock or a transcript naming GBX0605 should stay
    // findable rather than silently meaning something else.

    /// The deployment profile is a composition-time concept, not a runtime type.
    ///
    /// It is projected onto per-gear runtime kind and deployment topology.
    GapProfileNotRuntimeType = "GBX0606", RuntimeGap, Hint, true, "deployment profile is not a runtime type";

    /// A cluster consumer declares `deps = [cluster]`, which now only pins it.
    ///
    /// **The premise this code was written on has been withdrawn.** It read
    /// "cluster has no remote surface, so its consumers must be co-located",
    /// and that was true while cluster was an in-process library. It is not
    /// now: the gear is deployable out of process, `RemoteClusterClient`
    /// implements the backend traits over gRPC, and a consumer resolves through
    /// the process's single `dyn ClusterClient` on either side of a boundary.
    ///
    /// So the dep buys nothing and still costs: `deps` is a hard topo-sort
    /// edge, so declaring it pins the gear into cluster's process and makes an
    /// out-of-process build fail outright with
    /// `RegistryError::UnknownDependency`.
    ///
    /// Kept as a hint, not raised to a warning: staying co-located is a
    /// legitimate choice, and a gear that will never be spawned loses nothing
    /// by declaring the edge.
    GapClusterNotDeployable = "GBX0607", RuntimeGap, Hint, true, "`deps = [cluster]` pins a consumer that no longer needs pinning",
        prevents = Prevents::error("cf-gears-toolkit", "RegistryError", "UnknownDependency");

    /// A gear attribute argument the platform accepts and this tool does not
    /// model.
    ///
    /// **A field promised this and showed it to nobody.**
    /// `ProjectedGear::unmodelled` says it records unknown arguments "so a
    /// future macro argument surfaces as a known gap instead of a silent
    /// omission", and it had one writer and no reader outside its own unit test,
    /// which said as much in a comment.
    ///
    /// It is non-empty only in a skew window, and that window is real rather
    /// than theoretical: `#[toolkit::gear]` refuses an argument it does not
    /// know, so the platform necessarily lands a new one first and this parser
    /// catches up after. That is exactly how `one_per_installation` arrived --
    /// ADR `cpt-gearbox-adr-one-per-installation` leans on this field by name
    /// while doing it.
    ///
    /// In this range rather than `Validate` because it is not the description
    /// author's mistake: it is a claim about another repository -- that its
    /// macro takes an argument this tool drops -- and the range exists for
    /// claims of that kind, which is why every code in it must cite what it is
    /// reading.
    GapUnmodelledGearArgument = "GBX0608", RuntimeGap, Warning, true, "a gear attribute argument is not modelled by this tool";

    // ---------------------------------------------------------------- GBX07xx
    /// Generation would overwrite an operator-owned file whose edits cannot be
    /// merged.
    GenClobberOperatorFile = "GBX0701", Generator, Error, false, "would overwrite operator-owned edits";

    /// A generated path escapes its output root.
    GenPathEscape = "GBX0702", Generator, Error, false, "generated path escapes the output root";

    /// Chart rendering or linting failed.
    GenHelmFailed = "GBX0703", Generator, Error, false, "chart render or lint failed";

    /// Package metadata for a gear crate could not be read.
    GenCargoMetadataFailed = "GBX0704", Generator, Error, false, "could not read package metadata";

    /// A lock resolved by an older build carries a credential.
    ///
    /// A warning, not an error: generation replaces it, so nothing it writes
    /// carries the value. What the operator must still do is re-resolve, because
    /// the lock they have keeps it until they do.
    GenLiteralSecretInLock = "GBX0705", Generator, Warning, false, "the lock carries a credential, which generation replaced";

    /// A generated crate directory survives that this run does not write.
    ///
    /// **A warning with a named remedy, because the generator cannot delete.**
    /// Every fate `apply` has is create, update, keep or conflict, so an
    /// application removed from the description -- or a `layout` changed --
    /// leaves its old crate directory behind while the rewritten root
    /// `Cargo.toml` stops listing it. That is a package inside a workspace that
    /// neither includes nor excludes it, which `cargo metadata` and
    /// rust-analyzer refuse and a root `cargo build` does not, so nothing fails
    /// until someone opens the tree in an editor.
    ///
    /// Reported rather than pruned: the directory is the operator's, this build
    /// has no delete path worth trusting with a recursive remove, and a tree
    /// generated once under a different layout may hold work nobody wants gone.
    GenOrphanedCrate = "GBX0706", Generator, Warning, false, "a generated crate directory is no longer part of the product";

    /// A generated file survives that this run does not write.
    ///
    /// **The sibling claim to `GBX0706`, and it had to be one rather than a
    /// rewording.** That code is about a crate directory, and everything about
    /// it is crate-shaped: it compares the paths a run plans by stripping
    /// `/Cargo.toml`, looks for a manifest on disk, and says what `cargo
    /// metadata` will refuse. An application renamed under a Kubernetes profile
    /// leaves four other things behind -- its configuration file, its
    /// Dockerfile, and a Helm subchart of nine files -- and a `subchart = "..."`
    /// renamed on its own leaves the subchart with no crate having moved at all.
    /// None of that is a crate, and none of it was reported.
    ///
    /// Marker-gated exactly as `GBX0706` is, and that gate is what keeps it
    /// honest: a file is reported only when it carries this tool's own
    /// generated header, so an operator's file in the output root is never
    /// named, and neither is `values.yaml` -- the one file generation hands
    /// over, which deliberately does not carry the marker.
    ///
    /// A warning with the same remedy and for the same reason: `apply` has no
    /// delete path, and a file in a tree someone has been working in is theirs
    /// to remove.
    GenOrphanedFile = "GBX0707", Generator, Warning, false, "a generated file is no longer part of the product";

    /// A house template replaced a builtin for this run.
    ///
    /// Reported rather than only summarized, and that is the whole point of it
    /// being a diagnostic: the overlay was named in the text output alone, so
    /// `--format json` and every RPC client saw an unexpected Dockerfile or
    /// chart with no visible cause and read it as a generator change. A
    /// template is chosen by the description, so the file it produces is the
    /// product's doing -- which is a fact an operator has to be told.
    GenTemplateOverridden = "GBX0708", Generator, Warning, false, "a product template replaced a builtin";

    /// A crate the product takes from a registry differs from what was published.
    ///
    /// `crates = registry(...)` on a source says its gears are published, and
    /// generation checks that per crate: the published package records the
    /// commit it was cut from (`.cargo_vcs_info.json`), and the checkout is
    /// compared against it. Where they differ the build still names the
    /// published version, and `[patch]` points it at the checkout -- so one copy
    /// of the crate is linked, and it is the code the resolver read. A warning,
    /// because the product builds; the output simply is not self-contained until
    /// the change is released.
    GenCrateNotAsPublished = "GBX0709", Generator, Warning, false, "a crate differs from the version published, so the checkout is patched in";

    /// A crate the product takes from a registry is not published at the
    /// version the checkout declares, so it stays a path dependency.
    GenCrateNotPublished = "GBX0710", Generator, Warning, false, "a crate is not published at the checkout's version";

    /// The registry could not be asked at all -- offline, or cargo failed -- so
    /// every crate of the source stays a path dependency.
    GenRegistryUnavailable = "GBX0711", Generator, Warning, false, "the registry could not be reached";
}

/// A diagnostic code string that this build does not know.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown diagnostic code `{0}`")]
pub struct UnknownDiagnosticCode(pub String);

impl std::fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for DiagnosticCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DiagnosticCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for DiagnosticCode {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DiagnosticCode")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let codes: Vec<&str> = Self::ALL.iter().map(|c| c.as_str()).collect();
        schemars::json_schema!({
            "type": "string",
            "description": "stable Gearbox diagnostic code",
            "enum": codes,
        })
    }
}

/// A zero-based text position, matching Language Server Protocol semantics so it
/// can be handed to an editor without conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    #[must_use]
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }

    /// The start of a file, used when a diagnostic concerns a file as a whole.
    #[must_use]
    pub const fn origin() -> Self {
        Self::new(0, 0)
    }
}

/// A half-open span between two positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    #[must_use]
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    /// A zero-width range at the start of a file.
    #[must_use]
    pub const fn whole_file() -> Self {
        Self::new(Position::origin(), Position::origin())
    }
}

/// Where a fact lives.
///
/// `uri` is a `file://` URI so it can be passed to an editor unchanged.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

impl Location {
    #[must_use]
    pub fn new(uri: impl Into<String>, range: Range) -> Self {
        Self {
            uri: uri.into(),
            range,
        }
    }

    /// A whole file, with no interesting span inside it.
    #[must_use]
    pub fn file(uri: impl Into<String>) -> Self {
        Self::new(uri, Range::whole_file())
    }

    /// The declaration's own span when the description recorded one, the whole
    /// file otherwise.
    ///
    /// One rule, next to the sentinel it falls back to, because it is applied at
    /// roughly thirty call sites across two crates and the wrong answer is not a
    /// cosmetic slip. A diagnostic carrying `Range::whole_file` is *not
    /// published* to an editor at all (`gearbox_rpc::lsp::publishable`), and one
    /// carrying the span of the *wrong* declaration is worse than that: it
    /// underlines a line that is not at fault, which is a false claim about
    /// where the error is rather than an imprecise one
    /// (`cpt-gearbox-adr-gdl-language-server`).
    ///
    /// The fallback is deliberate and permanent. Not every declaration carries a
    /// span -- an intent built in a test has `declared_at: None` throughout, and
    /// a record reached through a `load()`ed fragment names the fragment rather
    /// than the importing file -- so every caller needs an answer for "no span",
    /// and the file is the honest one.
    ///
    /// Callers pass the span of the thing the diagnostic is *about*. Passing a
    /// nearby declaration because it happens to be in scope is the failure this
    /// cannot prevent and reviewers have to.
    #[must_use]
    pub fn or_file(declared_at: Option<&Self>, uri: &str) -> Self {
        declared_at
            .cloned()
            .unwrap_or_else(|| Self::file(uri.to_owned()))
    }
}

/// A `file://` URI for a path on this machine.
///
/// One function because there were five hand-rolled `format!("file://{}",
/// path.display())` calls, and `Path::display` is the wrong input for a URI on
/// Windows twice over: it yields `C:\\src\\gear.gdl`, so the result was
/// `file://C:\\src\\gear.gdl` -- backslashes a URI parser does not accept as
/// separators, and `C:` read as the *authority* rather than the path, which is
/// how an editor is handed a link to a host called `c` and opens nothing.
///
/// A leading slash is added when the path does not start with one, which is both
/// the Windows drive-letter case and the relative-path case. The relative one is
/// a lie -- `file:///product.gdl` says the file is at the filesystem root -- but
/// it is the lie that was already there, and it is the caller's business:
/// `resolve_at` exists precisely so a product's diagnostics get an absolute path
/// to build this from.
///
/// Not percent-encoded. A path with a space or a `#` in it still produces a URI
/// that is strictly invalid; encoding it is a separate change, because every
/// consumer that today compares these strings would have to be looked at.
#[must_use]
pub fn file_uri(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if let Some(unc) = text.strip_prefix("//") {
        // `\\server\share\file` → `file://server/share/file`, not four slashes.
        format!("file://{unc}")
    } else if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// A secondary location that helps explain a diagnostic.
///
/// Maps onto the Language Server Protocol's related-information, so an editor
/// renders these as navigable sub-entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RelatedLocation {
    pub location: Location,
    pub message: String,
}

impl RelatedLocation {
    #[must_use]
    pub fn new(location: Location, message: impl Into<String>) -> Self {
        Self {
            location,
            message: message.into(),
        }
    }
}

/// One thing the engine has to say.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,

    /// What is wrong, in one sentence, naming the specific subjects involved.
    pub message: String,

    /// Where in a file this arose, when it arose in a file at all. Resolution
    /// diagnostics often have no location and are anchored by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,

    /// Other places that help explain this.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<RelatedLocation>,

    /// The graph node this concerns, so a client can select it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<NodeId>,

    /// The configuration key this concerns, when one key is the whole subject.
    ///
    /// **Narrower than `subject` on purpose, and it exists because a location is
    /// not an answer.** `subject` names the gear, which is enough to open its
    /// settings; `location` names the `use_gear` line, which is where a value
    /// would be *typed*. Neither is the control. In the Studio configuration is
    /// a form, so "set `mode`" is answered by a box called `mode`, and reaching
    /// it from a message that names `mode` should not require a person to read
    /// the name out of the sentence and find it themselves.
    ///
    /// Only the rules that are *about* one key set it -- an unset required
    /// field, a key outside the schema. A diagnostic about a gear as a whole
    /// leaves it empty rather than picking one of its fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_key: Option<String>,

    /// What to do about it. Required for errors
    /// (`cpt-gearbox-nfr-actionable-diagnostics`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,

    /// The `file:line` in `gears-rust` that substantiates the claim. Required
    /// for codes that assert a runtime limitation
    /// (`cpt-gearbox-nfr-evidence-cited`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

impl Diagnostic {
    /// Start a diagnostic at its code's default severity.
    #[must_use]
    pub fn new(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: code.default_severity(),
            message: message.into(),
            location: None,
            related: Vec::new(),
            subject: None,
            config_key: None,
            help: None,
            evidence: None,
        }
    }

    /// Start an error, which must carry a remedy.
    ///
    /// Taking `help` as a parameter rather than a builder step is deliberate:
    /// it makes an actionless error impossible to write.
    #[must_use]
    pub fn error(
        code: DiagnosticCode,
        message: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        Self::new(code, message)
            .with_severity(Severity::Error)
            .with_help(help)
    }

    #[must_use]
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    #[must_use]
    pub fn at(mut self, location: Location) -> Self {
        self.location = Some(location);
        self
    }

    #[must_use]
    pub fn with_related(mut self, related: RelatedLocation) -> Self {
        self.related.push(related);
        self
    }

    #[must_use]
    pub fn about(mut self, subject: NodeId) -> Self {
        self.subject = Some(subject);
        self
    }

    /// Name the one configuration key this is about. See [`Self::config_key`].
    #[must_use]
    pub fn about_config(mut self, key: impl Into<String>) -> Self {
        self.config_key = Some(key.into());
        self
    }

    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Cite the source that proves the claim, as `path:line`.
    #[must_use]
    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = Some(evidence.into());
        self
    }

    #[must_use]
    pub const fn is_error(&self) -> bool {
        self.severity.is_error()
    }

    /// Check the invariants the PRD requires of every diagnostic.
    ///
    /// Called by a test over the whole emitted set rather than on every
    /// construction, so a violation is a build failure rather than a runtime
    /// panic in front of a user.
    ///
    /// # Errors
    /// Returns a description of each violated invariant.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();

        if self.message.trim().is_empty() {
            problems.push(format!("{}: message is empty", self.code));
        }
        if self.is_error() && self.help.as_ref().is_none_or(|h| h.trim().is_empty()) {
            problems.push(format!(
                "{}: error severity requires a remedy (cpt-gearbox-nfr-actionable-diagnostics)",
                self.code
            ));
        }
        if self.code.requires_evidence()
            && self.evidence.as_ref().is_none_or(|e| e.trim().is_empty())
        {
            problems.push(format!(
                "{}: asserts a runtime limitation and requires cited evidence \
                 (cpt-gearbox-nfr-evidence-cited)",
                self.code
            ));
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

/// A set of diagnostics, kept in a stable order.
///
/// Ordering is by `(code, message)` rather than emission order, so the same
/// resolution always yields the same sequence regardless of internal iteration
/// (`cpt-gearbox-nfr-determinism`).
#[derive(Clone, Debug, Default, PartialEq, Eq, TS)]
pub struct Diagnostics(Vec<Diagnostic>);

impl Serialize for Diagnostics {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Diagnostics {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::<Diagnostic>::deserialize(d).map(Self)
    }
}

impl Diagnostics {
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.0.push(diagnostic);
    }

    pub fn extend(&mut self, other: impl IntoIterator<Item = Diagnostic>) {
        self.0.extend(other);
    }

    /// Sort into canonical order and drop exact duplicates.
    ///
    /// Duplicates are expected: the same structural fact is often reached from
    /// several directions during resolution.
    pub fn finish(&mut self) {
        self.0.sort_by(|a, b| {
            a.code
                .cmp(&b.code)
                .then_with(|| a.message.cmp(&b.message))
                .then_with(|| a.location.cmp(&b.location))
        });
        self.0.dedup();
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.0.iter().any(Diagnostic::is_error)
    }

    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.0.iter().filter(|d| d.is_error())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Diagnostic] {
        &self.0
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.0.iter()
    }

    /// Highest severity present, if any.
    #[must_use]
    pub fn max_severity(&self) -> Option<Severity> {
        self.0.iter().map(|d| d.severity).max()
    }
}

impl FromIterator<Diagnostic> for Diagnostics {
    fn from_iter<I: IntoIterator<Item = Diagnostic>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type Item = &'a Diagnostic;
    type IntoIter = std::slice::Iter<'a, Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
