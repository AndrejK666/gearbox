//! Generates the vocabulary the editor's `.gdl` grammar colours.
//!
//! The grammar itself is hand-written TypeScript
//! (`ide/gearbox-studio/src/browser/gdl/gdl-grammar.ts`) because its hard part
//! is regex craft, which does not change when someone adds a GDL function. Its
//! *volatile* part is six word lists, and those are generated here from the
//! very globals the interpreter evaluates against -- so the editor cannot
//! colour a function the engine does not have, nor miss one it does.
//!
//! Run by `cargo test --package gearbox-gdl --test export_grammar`, which is
//! what `make grammar` invokes. Anti-drift is enforced in the build, not by
//! habit: after regenerating, `git diff --exit-code` over the output directory
//! must be empty (`cpt-gearbox-nfr-no-type-drift`, applied to the editor's view
//! of the language rather than to the wire).
//!
//! Deliberately not emitted:
//!
//! * `KNOWN_CATEGORIES` -- it occurs only inside string literals, and it drives
//!   a *warning* about a taxonomy `vocabulary.rs` itself calls "visibly still
//!   settling". A warning is the wrong thing to paint.
//! * The `gear()` parameters refused as restatement (`id`, `runtime_caps`, ...)
//!   -- they are illegal only *inside* `gear(...)`, which a `TextMate` grammar
//!   cannot scope without a `begin`/`end` whose `end` would trip on the first
//!   nested `)`.
//!   Meanwhile `id = "payments-demo"` is legal in a `product.gdl`, so a global
//!   rule would paint false alarms in the one product file we ship. GBX0210
//!   already names the owning attribute, which is more useful than red.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers `#[test]` functions but not the \
              helpers in this file; a generator that propagates errors instead of panicking \
              obscures the assertion it exists to support"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use gearbox_gdl::declarative::{KeywordVerdict, keyword_verdicts};
use gearbox_gdl::frontx::frontx_vocabulary;
use gearbox_gdl::globals::gear_vocabulary;
use gearbox_gdl::product::product_vocabulary;
use starlark::environment::{Globals, GlobalsBuilder};

/// Where the editor expects to find it.
const OUT: &str = "../../ide/gearbox-studio/src/browser/gdl/generated/vocabulary.ts";

/// One namespace's callable or value members.
type Members = BTreeMap<String, BTreeSet<String>>;

/// Everything one set of globals contributes, split by how it must be coloured.
#[derive(Default)]
struct Surface {
    /// Bare callables: `gear(...)`, `use_gear(...)`.
    functions: BTreeSet<String>,
    /// Namespaces of values: `cap.db`, `transport.rest`.
    value_namespaces: Members,
    /// Namespaces of functions: `cluster.cache(...)`, `prefer.isolate(...)`.
    function_namespaces: Members,
}

/// Sort every global by its starlark type, which discriminates exactly.
///
/// `"function"` comes from `#[starlark_module] fn`, `"gdl_namespace"` from
/// `builder.set(name, *namespace)` and `"namespace"` from
/// `builder.namespace(...)`. Anything else is a kind of global nobody has
/// invented yet, and the panic is deliberate: a new kind needs a decision about
/// what colour it is, and defaulting it to "uncoloured" would make that
/// decision silently.
fn surface_of(globals: &Globals) -> Surface {
    let mut out = Surface::default();
    for (name, value) in globals.iter() {
        let value = value.to_value();
        let members = || value.dir_attr().into_iter().collect();
        match value.get_type() {
            "function" => {
                out.functions.insert(name.to_owned());
            }
            "gdl_namespace" => {
                out.value_namespaces.insert(name.to_owned(), members());
            }
            "namespace" => {
                out.function_namespaces.insert(name.to_owned(), members());
            }
            other => panic!(
                "unclassified GDL global `{name}` of starlark type `{other}`; teach \
                 export_grammar what colour it should be"
            ),
        }
    }
    out
}

/// The Starlark standard callables GDL keeps.
///
/// Only the callables: `True`/`False`/`None` are also standard globals, but the
/// grammar scopes them as `constant.language` from a fixed pattern rather than
/// from a generated list.
fn starlark_builtins() -> BTreeSet<String> {
    GlobalsBuilder::standard()
        .build()
        .iter()
        .filter(|(_, v)| v.to_value().get_type() == "function")
        .map(|(name, _)| name.to_owned())
        .collect()
}

fn keywords(wanted: KeywordVerdict) -> BTreeSet<String> {
    keyword_verdicts()
        .into_iter()
        .filter(|(_, verdict)| *verdict == wanted)
        .map(|(word, _)| word.to_owned())
        .collect()
}

/// `["a", "b"]` on one line, or split across lines once it stops fitting.
///
/// Wrapping keeps a one-word addition to a long list readable as a one-line
/// diff instead of re-flowing the whole array.
fn array(words: &BTreeSet<String>) -> String {
    let quoted: Vec<String> = words.iter().map(|w| format!("\"{w}\"")).collect();
    let one_line = format!("[{}]", quoted.join(", "));
    if one_line.len() <= 92 {
        return one_line;
    }
    let mut out = String::from("[\n");
    for item in &quoted {
        writeln!(out, "  {item},").expect("write to String");
    }
    out.push(']');
    out
}

fn record(namespaces: &Members) -> String {
    let mut out = String::from("{\n");
    for (name, members) in namespaces {
        writeln!(out, "  {name}: {},", array(members)).expect("write to String");
    }
    out.push('}');
    out
}

/// Render the whole file. Pure, so determinism is testable without touching disk.
fn render() -> String {
    let gear = surface_of(&gear_vocabulary());
    let product = surface_of(&product_vocabulary());
    let frontx = surface_of(&frontx_vocabulary());

    // The value namespaces are registered into both, and identically -- both
    // loops walk `vocabulary::ALL_NAMESPACES`. Merging rather than emitting two
    // copies says that in the artefact.
    let mut value_namespaces = gear.value_namespaces.clone();
    value_namespaces.extend(product.value_namespaces.clone());
    let mut function_namespaces = gear.function_namespaces.clone();
    function_namespaces.extend(product.function_namespaces.clone());

    let mut out = String::from(
        "// GENERATED by `cargo test -p gearbox-gdl --test export_grammar`. Do not edit.\n\
         // Regenerate with `make grammar`; the result must be a no-op in git.\n\
         //\n\
         // Derived from the globals the interpreter itself evaluates against\n\
         // (`gearbox_gdl::globals::gear_vocabulary`, `product::product_vocabulary`,\n\
         // `frontx::frontx_vocabulary`) and from\n\
         // the keyword verdicts of `gearbox_gdl::declarative::keyword_verdicts`, so the editor\n\
         // cannot colour a function the engine does not have, nor miss one it does.\n\n",
    );

    let sections: [(&str, &str, String); 9] = [
        (
            "The callable vocabulary of a `gear.gdl`.",
            "GEAR_FUNCTIONS: readonly string[]",
            array(&gear.functions),
        ),
        (
            "The callable vocabulary of a `product.gdl`.",
            "PRODUCT_FUNCTIONS: readonly string[]",
            array(&product.functions),
        ),
        (
            "The callable vocabulary of a `frontx.gdl` (GDL 1.5).",
            "FRONTX_FUNCTIONS: readonly string[]",
            array(&frontx.functions),
        ),
        (
            "Starlark standard callables GDL keeps. Legal, but not GDL's own -- coloured\n\
              differently so a typo'd `use_gears(` stays visibly plain next to a `use_gear(`.",
            "STARLARK_BUILTINS: readonly string[]",
            array(&starlark_builtins()),
        ),
        (
            "Namespaces of values: `cap.db`, `transport.rest`. A member not listed here is a\n\
              hard attribute error at evaluation time, which is why the grammar may paint it red.",
            "VALUE_NAMESPACES: Readonly<Record<string, readonly string[]>>",
            record(&value_namespaces),
        ),
        (
            "Namespaces of functions: `cluster.cache(...)`, `prefer.isolate(...)`.",
            "FUNCTION_NAMESPACES: Readonly<Record<string, readonly string[]>>",
            record(&function_namespaces),
        ),
        (
            "Keywords the token scan refuses outright: GBX0103.",
            "FORBIDDEN_KEYWORDS: readonly string[]",
            array(&keywords(KeywordVerdict::Forbidden)),
        ),
        (
            "Keywords Starlark reserves. Refused one layer earlier than the forbidden set, as a\n\
              parse error rather than GBX0103 -- but just as refused, so just as red.",
            "RESERVED_KEYWORDS: readonly string[]",
            array(&keywords(KeywordVerdict::Reserved)),
        ),
        (
            "The one keyword GDL keeps.",
            "IMPORT_KEYWORD",
            format!(
                "\"{}\"",
                keywords(KeywordVerdict::Allowed)
                    .iter()
                    .next()
                    .expect("GDL keeps at least one keyword")
            ),
        ),
    ];

    for (doc, decl, value) in sections {
        // A `\n` in a doc means a wrapped comment, so it needs the ` * ` a
        // second JSDoc line takes.
        let doc = doc.replace('\n', "\n * ");
        writeln!(out, "/** {doc} */\nexport const {decl} = {value};\n").expect("write to String");
    }
    // One trailing newline, not two: the last section's blank separator would
    // otherwise make every editor that trims on save produce a spurious diff,
    // and `make grammar-check` would blame the generator for it.
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

#[test]
fn export_grammar_vocabulary() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join(OUT);
    std::fs::create_dir_all(out.parent().expect("an output directory"))
        .expect("create output directory");
    let rendered = render();
    std::fs::write(&out, &rendered).expect("write vocabulary.ts");

    // Spot-checks on the shapes the grammar actually depends on.
    assert!(
        rendered.contains("\"use_gear\""),
        "the product vocabulary is missing"
    );
    // The whole reason `gear_vocabulary` builds up from an empty builder rather
    // than subtracting the standard set: `fail` is in both, and a subtraction
    // by name would have dropped it here.
    assert!(
        surface_of(&gear_vocabulary()).functions.contains("fail"),
        "`fail` must survive as a GDL function despite also being a Starlark one"
    );
    for expected in [
        "\"rest_host\"",               // a cap member
        "\"leader_election\"",         // a cluster.* member
        "\"existing_infrastructure\"", // a prefer.* member
        "\"while\"",                   // reserved, not forbidden
        "\"lambda\"",                  // forbidden
    ] {
        assert!(
            rendered.contains(expected),
            "{expected} missing from:\n{rendered}"
        );
    }

    let builtins = starlark_builtins();
    assert!(
        builtins.contains("len"),
        "the standard set should have `len`"
    );
    assert!(
        !surface_of(&gear_vocabulary()).functions.contains("len"),
        "`len` is Starlark's, not GDL's"
    );

    let forbidden = keywords(KeywordVerdict::Forbidden);
    assert!(forbidden.contains("if") && !forbidden.contains("load"));
}

#[test]
fn export_is_deterministic() {
    // Two renders must be byte-identical, or the anti-drift check would fail
    // spuriously and get switched off. Cheap here because the whole artefact is
    // one string -- no temp directories needed.
    assert_eq!(render(), render(), "the generator is not deterministic");
}
