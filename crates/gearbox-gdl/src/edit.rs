//! Editing a description in place, without disturbing what surrounds the edit.
//!
//! **Re-serialising is not an option, and that decides the design.** A real
//! `product.gdl` is mostly comments, and the comments carry the reasoning: why
//! `vendor` is better left alone, why `grpc-hub` is not listed, why the profiles
//! are data rather than a branch. Evaluating the file and printing the resulting
//! [`ProductIntent`] back would produce something equivalent to the machine and
//! useless to the next reader. So an edit here is a byte insertion at a span the
//! parser found, and everything outside that span is copied verbatim.
//!
//! Which tier this is, since ADR `cpt-gearbox-adr-authoring-ownership-tiers`
//! governs what may be written: **tier 3, a structured manifest edited
//! surgically.** The ADR calls that "the single most universal behaviour in the
//! set -- `cargo add`, `dotnet package add`, Gazelle", and permits it. The tier-5
//! prohibition is on rewriting *human logic*, and GDL cannot be logic: the
//! dialect refuses every branching construct (`cpt-gearbox-fr-gdl-declarative`),
//! so `use_gear("x", source = "y")` is a data entry in a list and nothing else.
//!
//! What this module will not do:
//!
//! - **Guess.** If `gears` is missing, or is not a literal list -- built by a
//!   `load()`ed helper, say -- there is no span to insert into, and the answer is
//!   a diagnostic rather than an approximation.
//! - **Reformat.** The indentation of a new entry is read off the entries already
//!   there, so a file with four-space lists keeps them and a one-line list stays
//!   on one line.
//! - **Duplicate.** Adding a gear the list already names is a no-op, which is
//!   ADR-0010's "idempotent by content" and costs nothing.

#[path = "edit_call.rs"]
mod edit_call;

pub use edit_call::{
    CreateProductParams, PROFILE_KINDS, RelativePaths, add_gear_plugin, add_plugin_selection,
    add_profile, add_source, clone_product_text, edit_plugin_entry, is_secret_config_key,
    quote_string, rebase_product_paths, remove_profile, remove_source, render_product_template,
    set_gear_config, set_gear_features, set_gear_plugins, set_profile_field, set_provider_option,
};

use std::collections::BTreeSet;

use gearbox_ir::{Diagnostic, DiagnosticCode, Diagnostics, Location};
use starlark::syntax::AstModule;
use starlark_syntax::codemap::{Pos, Span};
use starlark_syntax::syntax::ast::{ArgumentP, AstExprP, AstStmtP, ExprP, StmtP};

use crate::declarative::dialect;

/// The outcome of an edit request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// The file needs to change, and this is what it becomes.
    Changed { source: String },
    /// The file already says what was asked for. Nothing to write.
    Unchanged,
}

impl Edit {
    /// The new text, when there is one.
    #[must_use]
    pub fn changed(&self) -> Option<&str> {
        match self {
            Self::Changed { source } => Some(source),
            Self::Unchanged => None,
        }
    }
}

/// Add `use_gear(<gear>, source = <source>)` to a product's `gears` list.
///
/// # Errors
/// Returns the diagnostics explaining why the file could not be edited: it did
/// not parse, it declares no `product(...)` call, or its `gears` argument is not
/// a literal list this can insert into.
pub fn add_gear(uri: &str, source: &str, gear: &str, source_id: &str) -> Result<Edit, Diagnostics> {
    let list = gears_list(uri, source)?;
    refuse_undeclared_source(uri, source, source_id)?;

    if list
        .entries
        .iter()
        .any(|entry| names_gear(source, *entry, gear))
    {
        return Ok(Edit::Unchanged);
    }

    let entry = format!(
        "use_gear({}, source = {})",
        quote_string(gear),
        quote_string(source_id)
    );
    Ok(Edit::Changed {
        source: insert_entry(source, &list, &entry),
    })
}

/// Refuse a `source =` the product does not declare.
///
/// The check belongs here, where the entry is written, rather than one layer
/// down where the description is loaded. A source id is a join: `sources`
/// declares it and every `use_gear` refers to it. Writing an id nothing declares
/// produces a file that parses, saves, and then refuses to load -- and the
/// refusal names the description rather than the edit that put it there. That is
/// how a scaffolded product ended up with `sources = [source(id = "source-1")]`
/// and `use_gear("service-discovery", source = "gears-rust")` in the same file.
///
/// Silent when the product has no `sources` list to read. Absent is not the same
/// answer as empty, and a description that declares none is the loader's
/// business -- refusing here would turn a missing argument into a failed edit.
fn refuse_undeclared_source(uri: &str, source: &str, source_id: &str) -> Result<(), Diagnostics> {
    let Ok(sources) = named_list_literal(uri, source, "sources") else {
        return Ok(());
    };
    let declared: BTreeSet<String> = sources
        .entries
        .iter()
        .filter_map(|entry| edit_call::entry_id(source, *entry))
        .collect();
    if declared.contains(source_id) {
        return Ok(());
    }
    let known = if declared.is_empty() {
        "it declares none".to_owned()
    } else {
        format!(
            "it declares {}",
            declared
                .iter()
                .map(|id| format!("`{id}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    // `GdlEval`, the same code the loader raises for this exact mistake
    // (`product_intent`'s "names source `x`, which is not declared"), rather
    // than the module's generic `refuse`. One claim, one code, said at whichever
    // end notices first -- and `GdlCardinality` is about the number of top-level
    // declarations, which this is not.
    Err(refuse_with(
        uri,
        DiagnosticCode::GdlEval,
        &format!("this product declares no source `{source_id}`; {known}"),
        "add the source to the description first, or name one it already declares -- \
         a `use_gear` pointing at an undeclared source makes the file unloadable",
    ))
}

/// Remove every `use_gear` naming `gear` from a product's `gears` list.
///
/// # Errors
/// As [`add_gear`].
pub fn remove_gear(uri: &str, source: &str, gear: &str) -> Result<Edit, Diagnostics> {
    let list = gears_list(uri, source)?;
    let mut targets: Vec<_> = list
        .entries
        .iter()
        .copied()
        .filter(|entry| names_gear(source, *entry, gear))
        .collect();
    if targets.is_empty() {
        return Ok(Edit::Unchanged);
    }
    // From the end so earlier spans stay valid after each cut.
    targets.sort_by_key(|span| offset(span.begin(), source));
    let mut text = source.to_owned();
    for target in targets.into_iter().rev() {
        text = remove_entry(&text, target);
    }
    Ok(Edit::Changed { source: text })
}

/// A named list argument on `product(...)`, located.
pub(crate) struct NamedList {
    /// The list expression's own span, brackets included.
    span: Span,
    /// The spans of the entries already in it.
    entries: Vec<Span>,
}

impl NamedList {
    /// A list located somewhere other than the top-level `product(...)`.
    ///
    /// `plugins` lives inside one `use_gear(...)` entry, so it cannot be found by
    /// [`named_list_literal`], which walks the product call. [`insert_entry`]
    /// only needs the two spans, and it is what keeps an appended entry indented
    /// like its neighbours -- so the sibling module builds one of these rather
    /// than growing a second insertion routine.
    pub(crate) fn of(span: Span, entries: Vec<Span>) -> Self {
        Self { span, entries }
    }

    /// The entries, for a caller deciding whether one of them is already there.
    pub(crate) fn entries(&self) -> &[Span] {
        &self.entries
    }
}

/// Find a named list literal on the top-level `product(...)` call.
pub(crate) fn named_list_literal(
    uri: &str,
    source: &str,
    arg: &str,
) -> Result<NamedList, Diagnostics> {
    let ast = AstModule::parse(uri, source.to_owned(), &dialect())
        .map_err(|e| {
            refuse_with(
                uri,
                DiagnosticCode::GdlParse,
                &format!("`{uri}` does not parse: {e}"),
                "fix the description before editing it; an edit cannot be placed in a file whose shape is unknown",
            )
        })?;

    let call = find_product_call(ast.statement()).ok_or_else(|| {
        refuse(
            uri,
            "no top-level `product(...)` call to edit",
            "point this at a product description; a `gear.gdl` has no `gears` list to add to",
        )
    })?;

    let value = call
        .iter()
        .find_map(|argument| match &argument.node {
            ArgumentP::Named(name, value) if name.node == arg => Some(value),
            _ => None,
        })
        .ok_or_else(|| {
            refuse(
                uri,
                &format!("`product(...)` declares no `{arg}` argument"),
                &format!(
                    "add `{arg} = []` to the product and retry, so there is a list to insert into"
                ),
            )
        })?;

    match &value.node {
        ExprP::List(entries) => Ok(NamedList {
            span: value.span,
            entries: entries.iter().map(|entry| entry.span).collect(),
        }),
        _ => Err(refuse(
            uri,
            &format!("`{arg}` is not a list literal, so there is no place to insert an entry"),
            "write the entries as a literal list, or add this one by hand -- a computed list has no span to edit",
        )),
    }
}

fn gears_list(uri: &str, source: &str) -> Result<NamedList, Diagnostics> {
    named_list_literal(uri, source, "gears")
}

/// The argument list of the first top-level `product(...)` call.
pub(crate) fn find_product_call<P>(
    stmt: &AstStmtP<P>,
) -> Option<&[starlark_syntax::syntax::ast::AstArgumentP<P>]>
where
    P: starlark_syntax::syntax::ast::AstPayload,
{
    match &stmt.node {
        StmtP::Statements(statements) => statements.iter().find_map(find_product_call),
        StmtP::Expression(expr) => match &expr.node {
            ExprP::Call(callee, args) if is_identifier(callee, "product") => Some(&args.args),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn is_identifier<P>(expr: &AstExprP<P>, name: &str) -> bool
where
    P: starlark_syntax::syntax::ast::AstPayload,
{
    matches!(&expr.node, ExprP::Identifier(id) if id.node.ident == name)
}

/// Whether a list entry is a `use_gear` naming `gear`.
///
/// Read off the source text of the entry rather than off the AST: the first
/// positional argument is a string literal in every form this accepts, and
/// comparing the rendered text keeps this indifferent to how the rest of the
/// entry is written.
fn names_gear(source: &str, entry: Span, gear: &str) -> bool {
    edit_call::names_entry(source, entry, gear) && is_use_gear_entry(source, entry)
}

/// Honour `use_gear("x")` and `UG = use_gear` / `UG("x")`. A prefix check on
/// the source text misses the alias, so `remove_gear` would leave it behind.
pub(crate) fn is_use_gear_entry(source: &str, entry: Span) -> bool {
    let Some(callee) = edit_call::entry_callee(source, entry) else {
        return false;
    };
    callee == "use_gear" || use_gear_aliases(source).contains(&callee)
}

fn use_gear_aliases(source: &str) -> BTreeSet<String> {
    // One-step `NAME = use_gear` on a line. `A = B = use_gear` and an alias of
    // an alias are invisible: GDL assignments are rare, and walking a full
    // binding chain would be a second parser for a shape nobody writes.
    let mut names = BTreeSet::new();
    for line in source.lines() {
        let code = line.split('#').next().unwrap_or("").trim();
        let Some((lhs, rhs)) = code.split_once('=') else {
            continue;
        };
        if rhs.trim() == "use_gear" {
            let lhs = lhs.trim();
            if is_gdl_ident(lhs) {
                names.insert(lhs.to_owned());
            }
        }
    }
    names
}

fn is_gdl_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// Insert `entry` as the last element of `list`.
pub(crate) fn insert_entry(source: &str, list: &NamedList, entry: &str) -> String {
    let close = offset(list.span.end(), source);
    let inside = offset(list.span.begin(), source) + 1;
    let bracket = close - 1;
    // An empty inline list has no entry from which to infer layout. Inserting at
    // the previous newline can escape the brackets when other arguments share
    // this line; insert inside the list itself and leave surrounding text intact.
    if list.entries.is_empty() && !source[inside..bracket].contains('\n') {
        let mut out = source.to_owned();
        out.insert_str(bracket, entry);
        return out;
    }
    // The insertion point is just before the closing bracket, and the text before
    // it decides the shape: a list whose entries are on their own lines gets a
    // new line, a one-line list gets a comma and a space.
    let head = &source[..close];
    let before_bracket = head.trim_end_matches(']');
    let multiline = list
        .entries
        .last()
        .is_some_and(|last| slice(source, *last).contains('\n') || spans_own_line(source, *last));

    let insertion = if multiline || list.entries.is_empty() {
        // An empty list's first entry goes one level in from the line the list
        // opens on. A fixed four spaces put `use_gear(...)` at the depth of
        // `gears = [` itself in every product the blank template wrote.
        let indent = list.entries.last().map_or_else(
            || {
                format!(
                    "{}    ",
                    line_indent(source, offset(list.span.begin(), source))
                )
            },
            |last| indent_of(source, *last),
        );
        format!("{indent}{entry},\n")
    } else {
        format!(", {entry}")
    };

    let mut out = String::with_capacity(source.len() + insertion.len());
    if multiline || list.entries.is_empty() {
        // Everything up to and including the last newline before `]`, so the
        // closing bracket keeps its own indentation.
        let cut = before_bracket
            .rfind('\n')
            .map_or(before_bracket.len(), |at| at + 1);
        // **The last entry may have no trailing comma.** One entry per line with
        // no comma after the last is valid Starlark, and it is what the blank
        // template wrote for `sources`; appending a line after it produced
        // `source(...)\n    source(...),`, which does not parse -- so Create Gear
        // wrote the scaffold and then could not declare its source. The comma goes
        // right after the entry, before any comment on its line.
        let last_end = list.entries.last().map(|last| offset(last.end(), source));
        match last_end {
            Some(end) if end <= cut && !source[end..cut].trim_start().starts_with(',') => {
                out.push_str(&source[..end]);
                out.push(',');
                out.push_str(&source[end..cut]);
            }
            _ => out.push_str(&source[..cut]),
        }
        out.push_str(&insertion);
        out.push_str(&source[cut..]);
    } else {
        let at = close - (head.len() - before_bracket.len()) - 1;
        out.push_str(&source[..=at]);
        out.push_str(&insertion);
        out.push_str(&source[at + 1..]);
    }
    out
}

/// Remove one entry, and the comma and blank line it leaves behind.
pub(crate) fn remove_entry(source: &str, entry: Span) -> String {
    let start = offset(entry.begin(), source);
    let mut end = offset(entry.end(), source);

    // Take the trailing comma and the rest of the line with it, so removing an
    // entry does not leave `,\n` hanging.
    let tail = &source[end..];
    if let Some(stripped) = tail.strip_prefix(',') {
        end += 1;
        if let Some(newline) = stripped.find('\n')
            && stripped[..newline].trim().is_empty()
        {
            end += newline + 1;
        }
    }

    // And the indentation in front of it, for the same reason.
    let head = &source[..start];
    let line_start = head.rfind('\n').map_or(0, |at| at + 1);
    let cut = if head[line_start..].trim().is_empty() {
        line_start
    } else {
        start
    };

    let mut out = String::with_capacity(source.len());
    out.push_str(&source[..cut]);
    out.push_str(&source[end..]);
    out
}

/// A Starlark byte position as an index into the same source.
///
/// Clamped rather than trusted: the span and the text come from the same parse,
/// so they agree -- but an index that could panic on a mismatch is not worth the
/// risk in a function whose whole job is slicing.
pub(crate) fn offset(pos: Pos, source: &str) -> usize {
    (pos.get() as usize).min(source.len())
}

pub(crate) fn slice(source: &str, span: Span) -> &str {
    &source[offset(span.begin(), source)..offset(span.end(), source)]
}

/// Whether the entry is the first thing on its line -- the sign of a list
/// written one element per line.
fn spans_own_line(source: &str, span: Span) -> bool {
    let begin = offset(span.begin(), source);
    source[..begin]
        .rfind('\n')
        .is_some_and(|at| source[at + 1..begin].trim().is_empty())
}

/// The leading whitespace of the line `at` is on.
fn line_indent(source: &str, at: usize) -> String {
    let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    source[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

fn indent_of(source: &str, span: Span) -> String {
    let begin = offset(span.begin(), source);
    let line_start = source[..begin].rfind('\n').map_or(0, |at| at + 1);
    source[line_start..begin]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect()
}

/// A refusal, pointing at the file rather than at a position inside it.
///
/// Shape problems are not parse errors: the file may be valid Starlark that
/// simply is not a product this editor can rewrite. `Location::file` carries
/// no interesting span -- inventing byte zero would claim more precision than
/// there is. `help` is not optional -- `Diagnostic::error` requires it, which
/// is `cpt-gearbox-nfr-actionable-diagnostics` enforced by the type.
pub(crate) fn refuse(uri: &str, message: &str, help: &str) -> Diagnostics {
    refuse_with(uri, DiagnosticCode::GdlCardinality, message, help)
}

pub(crate) fn refuse_with(
    uri: &str,
    code: DiagnosticCode,
    message: &str,
    help: &str,
) -> Diagnostics {
    let mut diagnostics: Diagnostics =
        [Diagnostic::error(code, message, help).at(Location::file(uri.to_owned()))]
            .into_iter()
            .collect();
    diagnostics.finish();
    diagnostics
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod edit_tests;
