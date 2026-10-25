//! The caret scan, and the vocabulary's answers for it.
//!
//! The first four tests are the four rows of the table in
//! `cpt-gearbox-adr-gdl-completion-and-hover`: three buffer shapes that do not
//! parse and one that does. They are the reason this scanner exists instead of
//! an AST walk, so they are the reason to keep them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use super::*;

/// The context at the very end of `source`.
fn at_end(source: &str) -> CaretContext {
    context_at(source, source.len())
}

#[test]
fn an_open_paren_names_the_call_it_opened() {
    assert_eq!(
        at_end("gear("),
        CaretContext {
            call: Some("gear".to_owned()),
            named: Vec::new(),
        }
    );
}

#[test]
fn a_half_written_parameter_list_keeps_what_it_already_names() {
    assert_eq!(
        at_end("gear(\n  name = \"x\",\n  "),
        CaretContext {
            call: Some("gear".to_owned()),
            named: vec!["name".to_owned()],
        }
    );
}

/// A caret immediately after the comma, with no whitespace between.
///
/// Distinct from the test above, which reaches the same state through a newline
/// and indentation: this one ends at the comma itself, so the scan's last action
/// is the `,` arm rather than the whitespace fallthrough.
#[test]
fn a_caret_directly_after_a_comma_names_no_parameter() {
    assert_eq!(
        at_end("gear(name = \"x\","),
        CaretContext {
            call: Some("gear".to_owned()),
            named: vec!["name".to_owned()],
        }
    );
}

/// A finished call is not a context: the caret is outside it again.
#[test]
fn a_closed_call_leaves_the_top_level() {
    assert_eq!(
        at_end("gear(\n  name = \"x\",\n)\n"),
        CaretContext::default()
    );
}

/// The shape a backwards scan gets wrong.
///
/// `"a ( b"` holds an unbalanced paren inside a string. A scanner reading
/// backwards from the caret sees it and reports a call that was never opened;
/// reading forward from the start, the string is skipped whole.
#[test]
fn a_paren_inside_a_string_opens_nothing() {
    assert_eq!(
        at_end("gear(description = \"a ( b\", "),
        CaretContext {
            call: Some("gear".to_owned()),
            named: vec!["description".to_owned()],
        }
    );
}

/// And the same inside a comment, which is where a `(` is most likely to be
/// written without meaning one.
#[test]
fn a_paren_inside_a_comment_opens_nothing() {
    assert_eq!(
        at_end("# see gear( for the shape\n"),
        CaretContext::default()
    );
    assert_eq!(
        at_end("gear(\n  # cargo( lives below\n  "),
        CaretContext {
            call: Some("gear".to_owned()),
            named: Vec::new(),
        }
    );
}

/// A triple-quoted string is skipped whole, quotes inside it included.
///
/// **A regression test for a wrong comment as much as for wrong code.** The
/// scanner's first version claimed triple quotes "are handled by the same loop
/// ... it never needs to know the difference", and they are not: `"""` was read
/// as an empty `""` followed by a new string, so the first lone `"` inside the
/// text closed it and everything after was scanned as code. Three descriptions
/// in the corpus use `"""`, so the shape is reachable.
#[test]
fn a_triple_quoted_string_is_skipped_whole() {
    assert_eq!(
        at_end("gear(description = \"\"\"he said \" hi (x\"\"\", "),
        CaretContext {
            call: Some("gear".to_owned()),
            named: vec!["description".to_owned()],
        },
        "a lone quote inside a triple-quoted string must not end it, and the `(` \
         after it must not open a call"
    );
    // The single-quoted form too, which GDL also admits.
    assert_eq!(
        at_end("gear(description = '''it can ( hold one''', "),
        CaretContext {
            call: Some("gear".to_owned()),
            named: vec!["description".to_owned()],
        }
    );
    // An unterminated one swallows the rest, which is the honest answer:
    // everything after it is inside a string until the person closes it.
    assert_eq!(
        at_end("gear(description = \"\"\"unfinished ( "),
        CaretContext {
            call: Some("gear".to_owned()),
            named: vec!["description".to_owned()],
        }
    );
}

/// An escaped quote does not end the string it is written in.
#[test]
fn an_escaped_quote_does_not_end_the_string() {
    assert_eq!(
        at_end("gear(description = \"he said \\\" ( \", "),
        CaretContext {
            call: Some("gear".to_owned()),
            named: vec!["description".to_owned()],
        }
    );
}

/// The innermost call wins, which is what makes nested constructors completable.
#[test]
fn the_innermost_call_is_the_context() {
    assert_eq!(
        at_end("gear(\n  name = \"x\",\n  package = cargo(crate_name = \"c\", "),
        CaretContext {
            call: Some("cargo".to_owned()),
            named: vec!["crate_name".to_owned()],
        }
    );
}

/// `==` names no parameter.
#[test]
fn a_comparison_is_not_a_named_argument() {
    let context = at_end("gear(name = \"x\", visibility = a == b, ");
    assert_eq!(context.call.as_deref(), Some("gear"));
    assert_eq!(
        context.named,
        vec!["name".to_owned(), "visibility".to_owned()],
        "`a == b` must not register `a` as a parameter"
    );
}

/// A list is a bracket, not a call, so a caret inside one is in no call.
#[test]
fn a_list_is_not_a_call() {
    let context = at_end("gear(\n  provides = [\n    ");
    assert_eq!(context.call, None, "a `[` names nothing callable");
}

#[test]
fn a_position_becomes_the_offset_of_that_line_and_column() {
    let source = "one\ntwo\nthree\n";
    assert_eq!(offset_of(source, 0, 0), 0);
    assert_eq!(offset_of(source, 1, 0), 4);
    assert_eq!(offset_of(source, 2, 3), 11);
    // Past the end of a line clamps to the line, not into the next one.
    assert_eq!(offset_of(source, 0, 99), 3);
    // Past the end of the file clamps to the file.
    assert_eq!(offset_of(source, 99, 0), source.len());
}

/// `character` counts UTF-8 bytes, which is not what LSP defaults to.
///
/// Pinned because the deviation is deliberate and documented on `offset_of`: an
/// editor sending UTF-16 code units disagrees with this the moment a line holds
/// a non-ASCII character before the caret. The test records the behaviour that
/// exists so a change to it is a decision rather than a drift.
#[test]
fn a_column_counts_bytes_rather_than_utf16_units() {
    // `\u{e9}` is `é`: two UTF-8 bytes, one UTF-16 unit. Escaped rather than
    // written literally because `clippy::non_ascii_literal` is on in this
    // workspace, and the escape is the point of the test anyway.
    let source = "a = \"\u{e9}\"\nnext\n";
    // Byte 8 is the end of the first line: `a = "é"` is 8 bytes.
    assert_eq!(offset_of(source, 0, 8), 8);
    // An editor counting UTF-16 would send 7 for the same caret, and land one
    // byte short -- inside the multi-byte character rather than after it.
    assert_eq!(offset_of(source, 0, 7), 7);
    // Clamping still uses the line's byte length, so a column past it is the
    // line end and never the next line.
    assert_eq!(offset_of(source, 0, 99), 8);
    assert_eq!(offset_of(source, 1, 0), 9);
}

/// A file that is neither description kind answers with nothing.
///
/// The `None` arm of `vocabulary_for`, and the two public entry points, which no
/// test reached: `completion` and `hover` were only ever exercised through the
/// RPC layer, and only for the two names that do have a vocabulary.
#[test]
fn a_file_that_is_no_description_offers_nothing() {
    let notes = std::path::Path::new("notes.gdl");
    let answer = completion(notes, "gear(", 5);
    assert!(answer.suggestions.is_empty(), "{:?}", answer.suggestions);
    assert!(!answer.in_call, "no vocabulary means no context either");
    assert_eq!(hover(notes, "gear(", 5), None);

    // And the two that do, to show the emptiness is about the name.
    assert!(
        !completion(std::path::Path::new("gear.gdl"), "gear(", 5)
            .suggestions
            .is_empty()
    );
    assert!(
        !completion(std::path::Path::new("product.gdl"), "", 0)
            .suggestions
            .is_empty()
    );
}

// ----------------------------------------------------------- the vocabulary

#[test]
fn the_top_level_offers_what_the_vocabulary_declares() {
    let offered = top_level(&GEAR);
    let labels: Vec<&str> = offered.iter().map(|s| s.label.as_str()).collect();
    assert!(labels.contains(&"gear"), "{labels:?}");
    assert!(labels.contains(&"cargo"), "{labels:?}");
    // Sorted, so the list an editor shows is stable between keystrokes.
    let mut sorted = labels.clone();
    sorted.sort_unstable();
    assert_eq!(labels, sorted);
}

/// The parameter list comes from the interpreter, and drops what is written.
///
/// Both halves matter. That `cargo` offers `lib` is what makes the feature
/// useful; that it stops offering `crate_name` once the buffer names it is what
/// keeps the list from restating the file back at the person.
#[test]
fn parameters_come_from_the_interpreter_and_exclude_what_is_named() {
    let docs: &DocModule = &GEAR;

    let all = parameters(docs, "cargo", &[]);
    let labels: Vec<&str> = all.iter().map(|s| s.label.as_str()).collect();
    assert!(labels.contains(&"crate_name"), "{labels:?}");
    assert!(labels.contains(&"lib"), "{labels:?}");

    // And each carries the type the interpreter will enforce.
    let crate_name = all.iter().find(|s| s.label == "crate_name").unwrap();
    assert!(
        crate_name
            .type_name
            .as_deref()
            .is_some_and(|t| t.contains("str")),
        "{:?}",
        crate_name.type_name
    );

    let rest = parameters(docs, "cargo", &["crate_name".to_owned()]);
    let rest_labels: Vec<&str> = rest.iter().map(|s| s.label.as_str()).collect();
    assert!(!rest_labels.contains(&"crate_name"), "{rest_labels:?}");
    assert!(rest_labels.contains(&"lib"), "{rest_labels:?}");
}

#[test]
fn a_name_the_vocabulary_does_not_have_offers_nothing() {
    let docs: &DocModule = &GEAR;
    assert!(parameters(docs, "not_a_builtin", &[]).is_empty());
    assert!(documentation(docs, "not_a_builtin").is_none());
}

/// Hover text is the doc comment in `globals.rs`, not a second copy of it.
///
/// The assertion is deliberately against a phrase written in the source rather
/// than against "something non-empty": the point of reading
/// `Globals::documentation()` is that the help cannot drift from the language,
/// and only comparing the actual words proves it did not.
#[test]
fn hover_returns_the_doc_comment_the_builtin_carries() {
    let text = documentation(&GEAR, "cargo").expect("`cargo` carries a doc comment");
    assert!(
        text.contains("where a gear's or SDK's crate lives"),
        "the summary from globals.rs must reach the editor verbatim: {text}"
    );
    assert!(
        text.contains("crate_name"),
        "and the details paragraph with it: {text}"
    );
}

/// A value namespace has a name and no prose, and that is recorded behaviour.
#[test]
fn a_value_namespace_carries_no_documentation() {
    let docs: &DocModule = &GEAR;
    let labels: Vec<String> = top_level(docs).into_iter().map(|s| s.label).collect();
    assert!(labels.contains(&"cap".to_owned()), "{labels:?}");
    assert!(
        documentation(docs, "cap").is_none(),
        "`GdlNamespace` does not override `documentation()`; if this starts \
         returning text, the ADR's note about it is out of date"
    );
}

/// The product vocabulary answers for the other file kind.
#[test]
fn the_product_vocabulary_offers_product_constructs() {
    let labels: Vec<String> = top_level(&PRODUCT).into_iter().map(|s| s.label).collect();
    assert!(labels.contains(&"product".to_owned()), "{labels:?}");
    assert!(labels.contains(&"use_gear".to_owned()), "{labels:?}");
    assert!(
        !labels.contains(&"gear".to_owned()),
        "a product description cannot declare a gear: {labels:?}"
    );
}

/// Every parameter a person can write has a sentence for completion and hover.
///
/// This is what tells a reader which fields exist and what each is for, so
/// none can be left bare: a new parameter without a `# Arguments` line fails
/// here, in the commit that adds it.
#[test]
fn every_parameter_is_documented() {
    let mut bare = Vec::new();
    for file in ["gear.gdl", "product.gdl"] {
        let docs = vocabulary_for(std::path::Path::new(file)).expect("a vocabulary");
        for (name, item) in &docs.members {
            if !matches!(item, DocItem::Member(DocMember::Function(_))) {
                continue;
            }
            for param in parameters(docs, name, &[]) {
                if param.detail.as_deref().is_none_or(|d| d.trim().is_empty()) {
                    bare.push(format!("{file}: {name}({})", param.label));
                }
            }
        }
    }
    assert!(
        bare.is_empty(),
        "{} undocumented:\n{}",
        bare.len(),
        bare.join("\n")
    );
}

#[test]
fn a_parameter_sentence_survives_its_line_breaks() {
    let docs = vocabulary_for(std::path::Path::new("gear.gdl")).expect("a vocabulary");
    let sdk = parameters(docs, "gear", &[])
        .into_iter()
        .find(|p| p.label == "sdk")
        .expect("gear(sdk)");
    let detail = sdk.detail.expect("documented");
    assert!(
        detail.starts_with("The gear's SDKs, in any language"),
        "{detail}"
    );
    assert!(detail.ends_with("A plugin has none."), "{detail}");
}
