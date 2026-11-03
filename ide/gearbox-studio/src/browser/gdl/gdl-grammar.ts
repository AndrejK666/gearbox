// The TextMate grammar for `.gdl`.
//
// Hand-written, but not hand-informed: every word list it matches comes from
// `./generated/vocabulary.ts`, which `cargo test -p gearbox-gdl --test
// export_grammar` derives from the globals the interpreter actually evaluates
// against. The split is deliberate -- the volatile part of a grammar is its
// vocabulary, the hard part is its regexes, and only the first can drift.
//
// GDL is a restricted Starlark dialect, so the lexical rules below are
// Starlark's, checked against `starlark_syntax::lexer` rather than assumed from
// Python: escapes are `\xHH` / `\uHHHH` / `\UHHHHHHHH` / octal and *not*
// `\u{...}`, a string is raw exactly when its prefix contains `r`, and f-strings
// do not exist (`enable_f_strings: false`).
//
// One grammar for `gear.gdl`, `product.gdl` and `frontx.gdl`. Splitting on filename
// would need two language ids and would still not cover the `load()` fragments,
// which may be named anything -- and highlighting is not validation. The engine
// already refuses `gear()` inside a product with a better message than a
// missing colour.

import {
  FORBIDDEN_KEYWORDS,
  FUNCTION_NAMESPACES,
  FRONTX_FUNCTIONS,
  GEAR_FUNCTIONS,
  IMPORT_KEYWORD,
  PRODUCT_FUNCTIONS,
  RESERVED_KEYWORDS,
  STARLARK_BUILTINS,
  VALUE_NAMESPACES,
} from "./generated/vocabulary";

export const GDL_SCOPE_NAME = "source.gdl";

/** An alternation. Every word is an identifier, so none needs escaping. */
const alt = (words: readonly string[]): string => words.join("|");

/** An identifier, as the lexer spells it: ASCII, no leading digit. */
const IDENT = String.raw`[A-Za-z_][A-Za-z0-9_]*`;

/**
 * The GDL callables, gear-side, product-side and `frontx.gdl` together.
 *
 * A union rather than two rules: `path()` is a product function and `cargo()` a
 * gear one, but a reader opening a `load()` fragment should see both coloured.
 */
const GDL_FUNCTIONS = [...new Set([...GEAR_FUNCTIONS, ...PRODUCT_FUNCTIONS, ...FRONTX_FUNCTIONS])].sort();

/**
 * Escape sequences, per `starlark_syntax::lexer::escape`.
 *
 * Note there is no `\u{...}` and no `\N{...}`: those are Python's, and a
 * grammar that colours them would be telling the reader something false.
 */
const STRING_ESCAPES = {
  name: "constant.character.escape.gdl",
  match: String.raw`\\(?:[abfnrtv'"\\]|[0-7]{1,3}|x[0-9A-Fa-f]{2}|u[0-9A-Fa-f]{4}|U[0-9A-Fa-f]{8}|\r?\n)`,
};

/**
 * `${VAR}` inside a string.
 *
 * A *convention*, not a language feature -- nothing in the workspace expands
 * it, and it travels verbatim into `product.lock`. Coloured anyway, on the same
 * grounds VS Code colours it in a Dockerfile: it is the one part of a
 * connection string a reader has to fill in.
 */
const ENV_INTERPOLATION = {
  match: String.raw`(\$\{)(${IDENT})(\})`,
  captures: {
    1: { name: "punctuation.definition.variable.gdl" },
    2: { name: "variable.other.env.gdl" },
    3: { name: "punctuation.definition.variable.gdl" },
  },
};

/**
 * In a raw string a backslash still shields the following character from
 * ending the literal (`starlark_syntax::lexer::string`, the `if raw` arm), it
 * just is not an escape. Consuming it unnamed is what keeps `r"\d+"` from
 * terminating early without painting the `\d` as something it is not.
 */
const RAW_CONTINUATION = { match: String.raw`\\.` };

type StringForm = {
  readonly kind: "triple" | "single";
  readonly quote: string;
  readonly raw: boolean;
};

/**
 * The eight string literals, longest opener first.
 *
 * Triple quotes must precede single ones: both start at the same offset for
 * `"""`, and a tie is broken by order in the array, so `"` would otherwise win
 * and treat the docstring as an empty string followed by garbage.
 */
function stringPattern({ kind, quote, raw }: StringForm) {
  const fence = kind === "triple" ? quote.repeat(3) : quote;
  const prefix = raw ? "(br|rb|r)" : "(b)?";
  const named = quote === '"' ? "double" : "single";
  return {
    name: `string.quoted.${kind === "triple" ? "triple." : ""}${named}.gdl`,
    begin: `${prefix}(${fence})`,
    beginCaptures: {
      1: { name: "storage.type.string.gdl" },
      2: { name: "punctuation.definition.string.begin.gdl" },
    },
    // A single-quoted form also ends at end-of-line. Starlark forbids a raw
    // newline there, and without this an unterminated string would paint the
    // rest of the file.
    end: kind === "triple" ? `(${fence})` : `(${fence})|(?<!\\\\)$`,
    endCaptures: { 1: { name: "punctuation.definition.string.end.gdl" } },
    patterns: raw ? [RAW_CONTINUATION, ENV_INTERPOLATION] : [STRING_ESCAPES, ENV_INTERPOLATION],
  };
}

const STRING_FORMS: readonly StringForm[] = [
  { kind: "triple", quote: '"', raw: true },
  { kind: "triple", quote: "'", raw: true },
  { kind: "triple", quote: '"', raw: false },
  { kind: "triple", quote: "'", raw: false },
  { kind: "single", quote: '"', raw: true },
  { kind: "single", quote: "'", raw: true },
  { kind: "single", quote: '"', raw: false },
  { kind: "single", quote: "'", raw: false },
];

/**
 * `cap.db`, `transport.rest` -- and `cap.bogus`, painted red.
 *
 * Red is truthful rather than opinionated: `GdlNamespace::get_attr` returns
 * `None` for an unlisted member, which the interpreter turns into a hard
 * attribute error. This is what makes the generated *member* lists load-bearing
 * rather than decorative.
 *
 * These rules also settle the `rest` problem. `rest` is a gear function, a
 * `cap` member and a `transport` member all at once; in `transport.rest` this
 * rule matches at `transport`, which is to the left of `rest`, so it consumes
 * the whole thing and the function rule never sees it.
 */
function namespaceRules(
  namespaces: Readonly<Record<string, readonly string[]>>,
  memberScope: string,
) {
  const known = Object.entries(namespaces).map(([ns, members]) => ({
    match: String.raw`\b(${ns})(\s*\.\s*)(${alt(members)})\b`,
    captures: {
      1: { name: "support.class.gdl" },
      2: { name: "punctuation.accessor.gdl" },
      3: { name: memberScope },
    },
  }));
  return [
    ...known,
    {
      match: String.raw`\b(${alt(Object.keys(namespaces))})(\s*\.\s*)(${IDENT})\b`,
      captures: {
        1: { name: "support.class.gdl" },
        2: { name: "punctuation.accessor.gdl" },
        3: { name: "invalid.illegal.unknown-member.gdl" },
      },
    },
  ];
}

export const GDL_GRAMMAR = {
  $schema:
    "https://raw.githubusercontent.com/martinring/tmlanguage/master/tmlanguage.json",
  name: "GDL",
  scopeName: GDL_SCOPE_NAME,
  fileTypes: ["gdl"],

  // Order matters only where two rules can match at the same offset; otherwise
  // the leftmost match wins regardless. The three placements that are load
  // bearing: strings before comments' `#` can reach into them, the namespace
  // rules before both the function rule and the generic attribute rule, and
  // the generic attribute rule last of all the dotted forms.
  patterns: [
    { include: "#comments" },
    { include: "#strings" },
    { include: "#load" },
    { include: "#illegal-keywords" },
    { include: "#language-constants" },
    { include: "#numbers" },
    { include: "#value-namespaces" },
    { include: "#function-namespaces" },
    { include: "#gdl-functions" },
    { include: "#starlark-builtins" },
    { include: "#top-level-binding" },
    { include: "#named-arguments" },
    { include: "#attribute-access" },
    { include: "#operators" },
    { include: "#punctuation" },
  ],

  repository: {
    comments: {
      name: "comment.line.number-sign.gdl",
      match: String.raw`(#).*$`,
      captures: { 1: { name: "punctuation.definition.comment.gdl" } },
    },

    strings: { patterns: STRING_FORMS.map(stringPattern) },

    // The one keyword GDL keeps. Scoped as an import because that is what it
    // is: `load("//shared.gdl", "SDK")`.
    load: {
      name: "keyword.control.import.gdl",
      match: String.raw`\b(${IMPORT_KEYWORD})\b(?=\s*\()`,
    },

    // Both groups are refused, one layer apart: the first by the token scan as
    // GBX0103, the second by the parser because Starlark reserves them. A GDL
    // author sees either go red while typing, before the engine is asked.
    "illegal-keywords": {
      patterns: [
        {
          name: "invalid.illegal.forbidden-construct.gdl",
          match: String.raw`\b(${alt(FORBIDDEN_KEYWORDS)})\b`,
        },
        {
          name: "invalid.illegal.reserved.gdl",
          match: String.raw`\b(${alt(RESERVED_KEYWORDS)})\b`,
        },
      ],
    },

    "language-constants": {
      name: "constant.language.gdl",
      match: String.raw`\b(True|False|None)\b`,
    },

    numbers: {
      patterns: [
        { name: "constant.numeric.hex.gdl", match: String.raw`\b0[xX][0-9A-Fa-f]+\b` },
        { name: "constant.numeric.octal.gdl", match: String.raw`\b0[oO][0-7]+\b` },
        { name: "constant.numeric.binary.gdl", match: String.raw`\b0[bB][01]+\b` },
        {
          name: "constant.numeric.float.gdl",
          match: String.raw`(\b[0-9]+\.[0-9]*([eE][-+]?[0-9]+)?|\.[0-9]+([eE][-+]?[0-9]+)?|\b[0-9]+[eE][-+]?[0-9]+)`,
        },
        { name: "constant.numeric.integer.gdl", match: String.raw`\b[0-9]+\b` },
      ],
    },

    // `support.constant.gdl` alone would render in the default foreground:
    // the bundled dark themes have rules for `support.constant.dom`,
    // `.math`, `.json` and so on, but none for a bare `support.constant`. The
    // second scope is what actually carries the colour, and it is apt --
    // these members *are* enum members (`GdlEnum` in the Rust).
    "value-namespaces": {
      patterns: namespaceRules(
        VALUE_NAMESPACES,
        "support.constant.gdl variable.other.enummember",
      ),
    },

    // No `(?=\s*\()` here, unlike the bare-function rule below: the namespace
    // qualifier has already disambiguated, and `cluster.cache` written without
    // its call is still recognisably `cluster.cache`.
    "function-namespaces": {
      patterns: namespaceRules(FUNCTION_NAMESPACES, "support.function.gdl"),
    },

    // The call lookahead is what keeps a *named argument* called `rest` or a
    // bare mention of `docs` from being painted as a call.
    "gdl-functions": {
      name: "support.function.gdl",
      match: String.raw`\b(${alt(GDL_FUNCTIONS)})\b(?=\s*\()`,
    },

    // Listed after the GDL rule so that `fail`, which is both, reads as GDL's.
    // The value of this rule is negative space: an unknown call stays
    // uncoloured, so a typo'd `use_gears(` is visibly plain beside `use_gear(`.
    "starlark-builtins": {
      name: "support.function.builtin.gdl",
      match: String.raw`\b(${alt(STARLARK_BUILTINS)})\b(?=\s*\()`,
    },

    // `enable_top_level_stmt: false` admits assignments and nothing else at the
    // top level, and an assignment cannot be indented -- so `^` is enough to
    // tell the `SDK = cargo(...)` binding from a named argument.
    "top-level-binding": {
      match: String.raw`^(${IDENT})\s*(=)(?!=)`,
      captures: {
        1: { name: "variable.other.constant.gdl" },
        2: { name: "keyword.operator.assignment.gdl" },
      },
    },

    // `(?!=)` excludes `==`. The other compound operators cannot reach here:
    // each has a non-identifier character where this rule needs whitespace.
    "named-arguments": {
      match: String.raw`\b(${IDENT})\s*(=)(?!=)`,
      captures: {
        1: { name: "variable.parameter.gdl" },
        2: { name: "keyword.operator.assignment.gdl" },
      },
    },

    // The catch-all for dotted forms, so it must stay below the namespace
    // rules. The owner is left uncoloured on purpose: it is an unknown.
    "attribute-access": {
      match: String.raw`\b(${IDENT})(\s*\.\s*)(${IDENT})`,
      captures: {
        2: { name: "punctuation.accessor.gdl" },
        3: { name: "variable.other.property.gdl" },
      },
    },

    operators: {
      name: "keyword.operator.gdl",
      match: String.raw`//|==|!=|<=|>=|[-+*/%<>]`,
    },

    punctuation: {
      patterns: [
        { name: "punctuation.section.parens.begin.gdl", match: String.raw`\(` },
        { name: "punctuation.section.parens.end.gdl", match: String.raw`\)` },
        { name: "punctuation.section.brackets.begin.gdl", match: String.raw`\[` },
        { name: "punctuation.section.brackets.end.gdl", match: String.raw`\]` },
        { name: "punctuation.section.braces.begin.gdl", match: String.raw`\{` },
        { name: "punctuation.section.braces.end.gdl", match: String.raw`\}` },
        { name: "punctuation.separator.comma.gdl", match: "," },
        { name: "punctuation.separator.colon.gdl", match: ":" },
      ],
    },
  },
};
