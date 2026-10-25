//! Evaluating one GDL file into the facts it declares.
//!
//! Note what this does *not* produce: a [`gearbox_ir::GearDescriptor`]. Under
//! ADR `cpt-gearbox-adr-macro-projected-catalogue` a description does not carry
//! the gear's id, capabilities, co-location dependencies, lifecycle or client
//! trait -- those are projected from the Rust attributes that own them. So a
//! `gear.gdl` alone cannot name the gear it describes, and assembling a
//! descriptor is `gearbox-engine`'s job, where both halves are in scope.
//!
//! The order here is deliberate: token scan, then parse, then evaluate.
//! Scanning first means a file full of conditionals reports every one of them
//! (GBX0103) instead of stopping at whatever the parser happens to dislike
//! first.
//!
//! Nothing in this crate reads the filesystem except the `load()` sandbox, and
//! that only within a root the caller names -- which keeps GDL testable from
//! string literals.

use gearbox_ir::{Diagnostic, DiagnosticCode, Diagnostics, Location, Range, RelPath, SourceId};
use starlark::environment::Module;
use starlark::eval::Evaluator;
use starlark::syntax::AstModule;

use crate::declarative::{dialect, scan_forbidden_tokens};
use crate::globals::gear_globals;
use crate::loader::{GdlLoader, apply_eval_limits, is_load_escape, is_recorded};
use crate::sink::{GdlSink, GearDecl};

/// Where a GDL file came from, as far as the catalogue is concerned.
#[derive(Debug, Clone)]
pub struct FileIdentity {
    /// A `file://` URI, used for diagnostics and editor navigation.
    pub uri: String,
    /// The declared source this file was read from.
    pub source: SourceId,
    /// The file's path relative to that source's root.
    pub gdl_path: RelPath,
    /// Where `load()` may read from. `None` disables it, which is what a caller
    /// evaluating a string literal wants: there is no directory for a fragment
    /// to live in, and resolving against the process's cwd would be worse than
    /// refusing.
    pub load_paths: Option<LoadPaths>,
}

/// The directories `load()` is confined to.
#[derive(Debug, Clone)]
pub struct LoadPaths {
    /// The declaring file's directory.
    pub base: std::path::PathBuf,
    /// The boundary no `load()` may cross.
    pub root: std::path::PathBuf,
}

/// The result of evaluating one file.
#[derive(Debug)]
pub struct EvalOutcome<T> {
    /// `None` when evaluation could not produce a value. Diagnostics say why.
    pub value: Option<T>,
    pub diagnostics: Diagnostics,
}

impl<T> EvalOutcome<T> {
    fn failed(diagnostics: Diagnostics) -> Self {
        Self {
            value: None,
            diagnostics,
        }
    }
}

/// Evaluates GDL. Holds no mutable state, so one engine serves many files.
#[derive(Debug, Default)]
pub struct GdlEngine {
    _private: (),
}

impl GdlEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluate one `gear.gdl` into the facts it declares.
    ///
    /// Never returns `Err`: a malformed description is a diagnostic, not a
    /// failure of the tool.
    #[must_use]
    #[expect(
        clippy::unused_self,
        reason = "cached Globals and the loader cache will live on the engine; \
                  taking &self now keeps adding them non-breaking"
    )]
    pub fn eval_gear(&self, identity: &FileIdentity, source: &str) -> EvalOutcome<GearDecl> {
        let (decl, mut diagnostics) = match evaluate(
            identity,
            source,
            &gear_globals(),
            "gear",
            GdlSink::take_gear,
        ) {
            Ok(pair) => pair,
            Err(diagnostics) => return EvalOutcome::failed(diagnostics),
        };

        // A design gear has no crate by definition; `gear()` refused a
        // `package` on one, so only a stable gear can be missing it.
        if decl.package.is_none() && !decl.is_design() {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlEval,
                    "gear has no `package = cargo(...)`",
                    "declare the crate: `package = cargo(crate_name = \"...\", lib = \"...\")`. \
                     It is what tells the projector which crate to scan. A gear with no code \
                     yet is `implementation = \"design\"` and names its `id` instead.",
                )
                .at(Location::file(identity.uri.clone())),
            );
            diagnostics.finish();
            return EvalOutcome::failed(diagnostics);
        }

        diagnostics.finish();
        EvalOutcome {
            value: Some(decl),
            diagnostics,
        }
    }

    /// Evaluate one `product.gdl` into operator intent.
    ///
    /// The conversion is where a product description is checked for internal
    /// consistency -- ids well-formed, `default_profile` naming a declared
    /// profile, no two declarations colliding on the same profile -- because
    /// those are questions the file can answer about itself, with no catalogue
    /// needed. Whether a named gear or provider *exists* is the resolver's
    /// question, and deliberately not asked here.
    #[must_use]
    #[expect(
        clippy::unused_self,
        reason = "symmetry with eval_gear; cached Globals will live on the engine"
    )]
    pub fn eval_product(
        &self,
        identity: &FileIdentity,
        source: &str,
    ) -> EvalOutcome<gearbox_ir::ProductIntent> {
        let (decl, mut diagnostics) = match evaluate(
            identity,
            source,
            &crate::product::product_globals(),
            "product",
            GdlSink::take_product,
        ) {
            Ok(pair) => pair,
            Err(diagnostics) => return EvalOutcome::failed(diagnostics),
        };

        let intent = crate::product_intent::build(identity, &decl, &mut diagnostics);
        diagnostics.finish();
        match intent {
            Some(intent) if !diagnostics.has_errors() => EvalOutcome {
                value: Some(intent),
                diagnostics,
            },
            _ => EvalOutcome::failed(diagnostics),
        }
    }
}

/// The shared pipeline: token scan, parse, evaluate, drain the sink.
///
/// The order is deliberate. Scanning first means a file full of conditionals
/// reports every one of them (GBX0103) instead of stopping at whatever the
/// parser happens to dislike first.
///
/// `decl_name` is the top-level call this file is expected to make, so the
/// cardinality diagnostic can name it. `gear()` and `product()` are registered
/// in different global sets, so a file that calls the wrong one fails at the
/// call rather than producing half of each.
fn evaluate<T>(
    identity: &FileIdentity,
    source: &str,
    globals: &starlark::environment::Globals,
    decl_name: &str,
    take: impl FnOnce(&GdlSink) -> Option<T>,
) -> Result<(T, Diagnostics), Diagnostics> {
    let forbidden = scan_forbidden_tokens(&identity.uri, source);
    if !forbidden.is_empty() {
        let mut diagnostics: Diagnostics = forbidden.into_iter().collect();
        diagnostics.finish();
        return Err(diagnostics);
    }

    let version = match crate::version::declared(&identity.uri, source) {
        Ok(version) => version,
        Err(refused) => {
            let mut diagnostics: Diagnostics = [*refused].into_iter().collect();
            diagnostics.finish();
            return Err(diagnostics);
        }
    };
    // A newer minor is read anyway; if it fails, that is the likely reason.
    let newer = version.filter(|v| *v > crate::version::CURRENT);

    let ast = match AstModule::parse(&identity.uri, source.to_owned(), &dialect()) {
        Ok(ast) => ast,
        Err(e) => {
            let mut diagnostics: Diagnostics =
                [starlark_error(&identity.uri, &e, DiagnosticCode::GdlParse)]
                    .into_iter()
                    .collect();
            diagnostics.finish();
            return Err(diagnostics);
        }
    };

    let sink = GdlSink::new();

    // Declared before the evaluator: `set_loader` borrows it for the
    // evaluator's whole lifetime.
    let loader = identity
        .load_paths
        .as_ref()
        .map(|p| GdlLoader::new(p.base.clone(), p.root.clone(), globals));

    let eval_err = Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        apply_eval_limits(&mut eval);
        eval.extra = Some(&sink);
        if let Some(loader) = loader.as_ref() {
            eval.set_loader(loader);
        }
        eval.eval_module(ast, globals).err()
    });

    let mut diagnostics = sink.take_diagnostics();
    if let Some(loader) = loader.as_ref() {
        diagnostics.extend(loader.take_diagnostics());
    }

    if let Some(e) = eval_err {
        // A fragment that recorded its own finding is already in `diagnostics`;
        // restating it as a parent-file GBX0102 would hide the real span.
        if !is_recorded(&e) {
            let mut diagnostic = starlark_error(&identity.uri, &e, classify(&e));
            if let Some(newer) = newer {
                diagnostic = diagnostic.with_help(format!(
                    "this file is written for GDL {newer}, and this Gearbox reads GDL {}: \
                     update Gearbox, or remove what the newer version added",
                    crate::version::CURRENT
                ));
            }
            diagnostics.push(diagnostic);
        }
        diagnostics.finish();
        return Err(diagnostics);
    }

    if sink.saw_duplicate() {
        diagnostics.push(cardinality_error(
            &identity.uri,
            &format!("more than one `{decl_name}()` declaration"),
            &format!(
                "a {decl_name}.gdl describes exactly one {decl_name}; split the extra \
                 declaration into its own file"
            ),
        ));
    }

    let Some(value) = take(&sink) else {
        diagnostics.push(cardinality_error(
            &identity.uri,
            &format!("no `{decl_name}()` declaration"),
            &format!("add a `{decl_name}(...)` call"),
        ));
        diagnostics.finish();
        return Err(diagnostics);
    };

    Ok((value, diagnostics))
}

/// Which diagnostic code a starlark failure deserves.
///
/// Three distinct, actionable mistakes hide behind starlark's generic errors,
/// and matching on the message is the only signal it gives. The alternative --
/// declaring `**kwargs` everywhere and validating by hand -- would trade the
/// macro's own arity and type checking for a cleaner match, which is a bad deal.
fn classify(error: &starlark::Error) -> DiagnosticCode {
    let message = error.to_string();
    if is_load_escape(error) {
        DiagnosticCode::GdlLoadEscape
    } else if message.contains("is projected from Rust and must not be declared here") {
        DiagnosticCode::ValidateRestatement
    } else if message.contains("extra named parameter") {
        DiagnosticCode::GdlUnknownArgument
    } else {
        DiagnosticCode::GdlEval
    }
}

/// Turn a starlark error into a diagnostic, carrying its span when it has one.
fn starlark_error(uri: &str, error: &starlark::Error, code: DiagnosticCode) -> Diagnostic {
    // `without_diagnostic` drops starlark's own rendered span block, which the
    // structured location below would otherwise duplicate.
    let message = error.without_diagnostic().to_string();
    let help = match code {
        DiagnosticCode::ValidateRestatement => {
            "delete the field; it is already declared in Rust and projecting it is what \
             makes the two impossible to disagree"
        }
        DiagnosticCode::GdlUnknownArgument => {
            "check the spelling against the GDL vocabulary; unsupported fields are rejected \
             rather than ignored"
        }
        DiagnosticCode::GdlParse => "fix the syntax error",
        DiagnosticCode::GdlLoadEscape => {
            "load() may only read fragments inside the declaring file's source root; \
             use a `//`-prefixed path to address the root explicitly"
        }
        _ => "see the message above",
    };

    let mut diagnostic = Diagnostic::error(code, message, help);
    if let Some(span) = error.span() {
        // The span names the file that failed, which for `load()` is the
        // fragment, not the parent. Attributing a fragment error to the
        // loading file is how an editor opens the wrong document.
        let file = span.filename();
        let at = if file.is_empty() { uri } else { file };
        // starlark's ResolvedPos is 0-based, like LSP and like our own Position,
        // so this is a field copy rather than arithmetic.
        let resolved = span.resolve_span();
        diagnostic = diagnostic.at(Location::new(
            at.to_owned(),
            Range::new(
                gearbox_ir::Position::new(
                    u32::try_from(resolved.begin.line).unwrap_or(u32::MAX),
                    u32::try_from(resolved.begin.column).unwrap_or(u32::MAX),
                ),
                gearbox_ir::Position::new(
                    u32::try_from(resolved.end.line).unwrap_or(u32::MAX),
                    u32::try_from(resolved.end.column).unwrap_or(u32::MAX),
                ),
            ),
        ));
    }
    diagnostic
}

fn cardinality_error(uri: &str, message: &str, help: &str) -> Diagnostic {
    Diagnostic::error(DiagnosticCode::GdlCardinality, message, help)
        .at(Location::file(uri.to_owned()))
}
