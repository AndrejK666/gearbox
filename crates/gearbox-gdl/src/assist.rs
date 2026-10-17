//! What an editor can be told about a description *while it is being typed*.
//!
//! The caret's surroundings, and the vocabulary's own answer for them. Both
//! halves exist because of one measured fact
//! (`cpt-gearbox-adr-gdl-completion-and-hover`): a buffer mid-edit does not
//! parse. `gear(`, `gear(name = "x", ` and a half-written parameter list all
//! fail `AstModule::parse`, and those are exactly the moments completion is
//! asked for. So the six AST walks in `edit.rs` are no use here, and this reads
//! the text directly.
//!
//! **Forward to the caret, not backwards from it.** Backwards is the first
//! instinct and it cannot work: whether a `(` belongs to the code or sits inside
//! a string literal is not decidable without reading from the start, and a
//! scanner that guessed would offer the wrong call's parameters inside every
//! description that mentions a bracket in a comment. One pass over a file of a
//! few hundred lines costs nothing per keystroke.

use std::sync::LazyLock;

use starlark::docs::{DocItem, DocMember, DocModule, DocParam};

/// The two vocabularies' documentation, derived once.
///
/// **Built once because the ADR says so.** `cpt-gearbox-adr-gdl-completion-and-hover`
/// describes a request as "a scan of one buffer and a map lookup", and the first
/// version of this file did not do that: it rebuilt `Globals` through
/// `GlobalsBuilder` and re-derived the whole `DocModule` -- every member's doc
/// string parsed, every `DocParam` and `Ty` allocated -- on each keystroke, to
/// read one entry out of it. Measured at 0.058 ms per request, which is three
/// orders below the websocket-and-stdio round trip that carries it, so this is
/// not a speed fix; it is making the documented claim true and not doing work
/// twice for nothing.
///
/// `Globals` and `DocModule` are both `Send + Sync`, which is what allows this;
/// starlark holds its own globals the same way.
/// The gear vocabulary's documentation. The `Globals` itself is not kept: every
/// question this module answers is a lookup in the derived `DocModule`.
static GEAR: LazyLock<DocModule> =
    LazyLock::new(|| crate::globals::gear_vocabulary().documentation());
static PRODUCT: LazyLock<DocModule> =
    LazyLock::new(|| crate::product::product_vocabulary().documentation());

/// Where the caret is, in terms the vocabulary can answer for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CaretContext {
    /// The innermost call the caret sits inside, by name.
    ///
    /// `None` at the top level of the file, which is where the declaration
    /// itself is completed.
    pub call: Option<String>,
    /// Parameter names already written in that call, so they are not offered
    /// twice.
    pub named: Vec<String>,
}

/// Byte offset of a zero-based LSP position, clamped into `source`.
///
/// `character` counts UTF-8 bytes here, not UTF-16 code units. That is a
/// deviation from LSP's default and it is bounded: `.gdl` is ASCII apart from
/// string contents, and the caller compares the result against ASCII delimiters
/// only. Documented rather than hidden, because a description with a non-ASCII
/// `description = "..."` and a caret after it is where it would show.
#[must_use]
pub fn offset_of(source: &str, line: u32, character: u32) -> usize {
    let mut offset = 0usize;
    for (n, text) in source.split_inclusive('\n').enumerate() {
        if n == line as usize {
            return offset + (character as usize).min(text.trim_end_matches('\n').len());
        }
        offset += text.len();
    }
    source.len()
}

/// One open call, while the scan is inside it.
struct Open {
    name: Option<String>,
    named: Vec<String>,
}

/// How deep a nesting the scan tracks, and how many named arguments it keeps
/// per call.
///
/// The state is bounded rather than the input, and the distinction is the
/// point. A cap on document length would need an arbitrary constant -- there is
/// no honest answer to "how large is too large for a `.gdl`" -- while these two
/// have one: the deepest real description nests a handful of calls, and the
/// widest takes a dozen or so arguments. Past that the text is not a
/// description, and the scan stops growing rather than allocating a `String`
/// per `=` for a pasted megabyte of `a=a=a=`.
///
/// Reaching a cap degrades the answer, it does not break it: the context found
/// so far is still returned, so a pathological file gets a shorter parameter
/// list rather than a hang. It also keeps `parameters`' `already.iter().any(..)`
/// linear in a constant.
const MAX_DEPTH: usize = 64;
const MAX_NAMED: usize = 256;

/// The caret's context, read from the text up to `offset`.
#[must_use]
pub fn context_at(source: &str, offset: usize) -> CaretContext {
    let bytes = source.as_bytes();
    let end = offset.min(bytes.len());
    let mut open: Vec<Open> = Vec::new();
    // The identifier most recently completed, which is the callee when the next
    // non-space byte is `(`.
    let mut last_ident: Option<String> = None;
    // An identifier seen at the current depth that may turn out to be `name =`.
    let mut pending_named: Option<String> = None;
    let mut i = 0usize;

    while i < end {
        let b = bytes[i];
        match b {
            // A comment runs to the end of the line and contains nothing.
            b'#' => {
                while i < end && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            // A string literal, skipped whole.
            //
            // **The triple-quoted form needs its own arm**, and an earlier
            // version of this comment claimed otherwise: treating `"""` as an
            // empty `""` plus a new string means the first lone `"` inside the
            // text closes it, and everything after is scanned as code. Three
            // descriptions in the corpus use `"""`, and one of them holds a
            // quote, so that was a live defect rather than a hypothetical.
            b'"' | b'\'' => {
                let quote = b;
                let triple = bytes.get(i + 1) == Some(&quote) && bytes.get(i + 2) == Some(&quote);
                i += if triple { 3 } else { 1 };
                while i < end {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        if !triple {
                            i += 1;
                            break;
                        }
                        if bytes.get(i + 1) == Some(&quote) && bytes.get(i + 2) == Some(&quote) {
                            i += 3;
                            break;
                        }
                    }
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => {
                let name = if b == b'(' { last_ident.take() } else { None };
                if open.len() < MAX_DEPTH {
                    open.push(Open {
                        name,
                        named: Vec::new(),
                    });
                }
                pending_named = None;
                last_ident = None;
                i += 1;
            }
            b')' | b']' | b'}' => {
                open.pop();
                pending_named = None;
                last_ident = None;
                i += 1;
            }
            b',' => {
                pending_named = None;
                last_ident = None;
                i += 1;
            }
            b'=' => {
                // `name =` is a named argument; `==` is a comparison and names
                // nothing. GDL has no augmented assignment to worry about.
                let comparison = bytes.get(i + 1) == Some(&b'=');
                if let Some(name) = pending_named.take()
                    && !comparison
                    && let Some(current) = open.last_mut()
                    && current.named.len() < MAX_NAMED
                {
                    current.named.push(name);
                }
                last_ident = None;
                i += if comparison { 2 } else { 1 };
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                while i < end && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let ident = source[start..i].to_owned();
                pending_named = Some(ident.clone());
                last_ident = Some(ident);
            }
            _ => {
                i += 1;
            }
        }
    }

    match open.last() {
        Some(current) => CaretContext {
            call: current.name.clone(),
            named: current.named.clone(),
        },
        None => CaretContext::default(),
    }
}

/// One thing the editor can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    /// The one-line summary from the vocabulary's own doc comment, when it has
    /// one.
    pub detail: Option<String>,
    /// The type the parameter takes, for a parameter suggestion.
    pub type_name: Option<String>,
}

/// Everything callable at the top level of a description.
#[must_use]
pub fn top_level(docs: &DocModule) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = docs
        .members
        .iter()
        .map(|(label, item)| Suggestion {
            label: label.clone(),
            detail: summary_of(item),
            type_name: None,
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// The named parameters of `call` that `already` does not name.
#[must_use]
pub fn parameters(docs: &DocModule, call: &str, already: &[String]) -> Vec<Suggestion> {
    let Some(DocItem::Member(DocMember::Function(function))) = docs.members.get(call) else {
        return Vec::new();
    };
    function
        .params
        .named_only
        .iter()
        .chain(function.params.pos_or_named.iter())
        .filter(|param| !already.iter().any(|seen| seen == &param.name))
        .filter(|param| !refused_only(call, &param.name))
        .map(|param| Suggestion {
            label: param.name.clone(),
            detail: detail_of(param),
            type_name: Some(param.typ.to_string()),
        })
        .collect()
}

/// Parameters a call accepts only so it can refuse them by name.
///
/// They exist in the signature so the error can say which attribute owns the
/// fact (GBX0210), or what a renamed keyword became -- not to be written.
/// Offering them in completion invites exactly the mistake they catch. `id`
/// is not here: an `implementation = "design"` gear writes it.
fn refused_only(call: &str, param: &str) -> bool {
    match call {
        "gear" => matches!(
            param,
            "fills"
                | "runtime_caps"
                | "colocated_deps"
                | "lifecycle"
                | "client"
                | "cluster_providers"
        ),
        "provide" => matches!(param, "version" | "transports" | "kind"),
        "consume" => matches!(param, "from_" | "version" | "kind"),
        _ => false,
    }
}

/// The documentation for one name, as the interpreter carries it.
#[must_use]
pub fn documentation(docs: &DocModule, name: &str) -> Option<String> {
    let DocItem::Member(DocMember::Function(function)) = docs.members.get(name)? else {
        // A value namespace carries no documentation: `GdlNamespace` does not
        // override `documentation()`, so `cap.db` has a name and nothing else.
        // Recorded in the ADR rather than papered over here.
        return None;
    };
    let string = function.docs.as_ref()?;
    Some(match &string.details {
        Some(details) => format!("{}\n\n{details}", string.summary),
        None => string.summary.clone(),
    })
}

fn summary_of(item: &DocItem) -> Option<String> {
    match item {
        DocItem::Member(DocMember::Function(function)) => {
            Some(function.docs.as_ref()?.summary.clone())
        }
        _ => None,
    }
}

fn detail_of(param: &DocParam) -> Option<String> {
    let docs = param.docs.as_ref()?;
    Some(docs.summary.clone())
}

/// Which vocabulary answers for a description, by the name of its file.
///
/// The same rule `gearbox_engine::check_description` uses, and for the same
/// reason: a `product.gdl` cannot declare a gear, so offering `gear(...)` in one
/// would be offering a construct the evaluator will refuse. `None` for anything
/// that is neither kind.
#[must_use]
pub fn vocabulary_for(path: &std::path::Path) -> Option<&'static DocModule> {
    match path.file_name()?.to_str()? {
        crate::PRODUCT_FILE => Some(&PRODUCT),
        crate::GEAR_FILE => Some(&GEAR),
        _ => None,
    }
}

/// What the caret is inside, and what may be typed there.
///
/// The whole answer for one completion request, so the caller needs to know
/// nothing about starlark. `in_call` distinguishes a parameter list from the top
/// level of the file, which is the only thing the caller renders differently.
///
/// An empty list for a file that is neither description kind, or a call the
/// vocabulary does not know. Both are ordinary -- an editor asks about whatever
/// the caret is in -- so neither is an error.
#[must_use]
pub fn completion(path: &std::path::Path, source: &str, offset: usize) -> Completion {
    let Some(docs) = vocabulary_for(path) else {
        return Completion::default();
    };
    let context = context_at(source, offset);
    match context.call.as_deref() {
        Some(call) => Completion {
            in_call: true,
            suggestions: parameters(docs, call, &context.named),
        },
        None => Completion {
            in_call: false,
            suggestions: top_level(docs),
        },
    }
}

/// The answer to one completion request.
#[derive(Debug, Default)]
pub struct Completion {
    /// Whether these are a call's parameters rather than the file's top-level
    /// constructs.
    pub in_call: bool,
    pub suggestions: Vec<Suggestion>,
}

/// The documentation for whatever call the caret is inside.
///
/// The enclosing call, not the token under the caret. Coarser than LSP hover
/// usually is, and it is what the scan can answer honestly: it tracks which call
/// is open, not which identifier a character belongs to. Hovering anywhere
/// inside `cargo(...)` therefore explains `cargo`, which is the thing somebody
/// hovering there wants to know.
#[must_use]
pub fn hover(path: &std::path::Path, source: &str, offset: usize) -> Option<String> {
    let docs = vocabulary_for(path)?;
    let call = context_at(source, offset).call?;
    documentation(docs, &call)
}

#[cfg(test)]
#[path = "assist_tests.rs"]
mod assist_tests;
