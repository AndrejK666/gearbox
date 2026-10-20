//! Parsing `#[toolkit::gear(...)]`.
//!
//! Mirrors the argument grammar in `libs/toolkit-macros/src/lib.rs`: `name`,
//! `deps`, `capabilities`, `client`, `ctor`, and a nested `lifecycle(...)`.
//! Anything else is a compile error there, so anything else here is a fact we
//! do not understand and must say so about rather than ignore.
//!
//! `ctor` is read and discarded: it is an arbitrary Rust expression, it is
//! unprojectable, and nothing in the product model needs it.

use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, Ident, LitBool, LitStr, Token};

/// What `lifecycle(...)` said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectedLifecycle {
    pub entry: Option<String>,
    pub stop_timeout: Option<String>,
    pub await_ready: bool,
}

/// The facts `#[toolkit::gear]` owns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectedGear {
    /// `name = "..."`, already kebab-case.
    pub name: String,
    /// `deps = [snake_idents]`, converted to the kebab gear names the runtime
    /// uses. These are link-time co-location edges: the macro emits a hidden
    /// re-export per entry, so the crate is physically in the binary.
    pub colocated_deps: Vec<String>,
    /// `capabilities = [...]`, spelled as the macro spells them.
    pub runtime_caps: Vec<String>,
    /// `client = path::To::Trait`.
    pub client_trait: Option<String>,
    pub lifecycle: Option<ProjectedLifecycle>,
    /// The annotated item's identifier.
    ///
    /// `#[toolkit::consumes]` derives its endpoint-override config key from the
    /// kebab-case of this, *not* from `name`, so a mismatch silently breaks that
    /// override. That is the one cross-check the projected catalogue still needs
    /// (GBX0206).
    pub struct_ident: String,
    /// The gear name `#[toolkit::consumes]` keys `consumer_wiring` by. Written
    /// *above* `#[toolkit::gear]`, the consumes macro expands first and reads the
    /// declared `name`; otherwise it falls back to the kebab-case of
    /// [`struct_ident`](Self::struct_ident).
    pub consumer_wiring_key: String,
    /// Arguments the macro accepts but this parser does not model.
    ///
    /// Recorded rather than dropped so a future macro argument surfaces as a
    /// known gap instead of a silent omission. It is `merge` that surfaces it,
    /// as `GBX0608`; for a long while nothing did, and the claim in this comment
    /// was the only place the promise existed.
    ///
    /// Non-empty only in a skew window, because `#[toolkit::gear]` refuses an
    /// argument it does not know -- so the platform lands one first and this
    /// parser catches up afterwards. `one_per_installation` arrived that way.
    pub unmodelled: Vec<String>,
    /// True when the gear sits under a `#[cfg(...)]` this crate cannot
    /// evaluate, so its presence in a build is conditional.
    ///
    /// Read from two places, because a gear can be gated in two: a `cfg` beside
    /// the gear attribute, and a `cfg` on a `mod` declaration on the way to the
    /// file. The second is the one that was invisible -- `scan_crate` discovers
    /// files by walking directories and never reads the `mod` tree, so a gear
    /// behind `#[cfg(feature = "x")] mod gear;` carries no attribute of its own
    /// to read and used to project as unconditional.
    ///
    /// The limit that remains: a file reached through an explicit `#[path]`, or
    /// through a `mod` nested inside an inline `mod`, is not traced back, so a
    /// `cfg` there is still unobserved. No gear crate writes either.
    pub conditional: bool,
    /// What each sibling `#[toolkit::provides]` states.
    ///
    /// Which transports a provider wires up is stated here, not in GDL and not
    /// by the contract's projection traits: the traits say what is *possible*,
    /// this says what this gear actually offers.
    pub provides: Vec<crate::contract::ProjectedProvide>,

    /// Every `#[toolkit::consumes]` on the gear item.
    ///
    /// The half of the `consumer_wiring` key the runtime actually reads. The
    /// description restates it as `consume(from_ = ...)`, and until this field
    /// existed nothing could tell the two apart.
    pub consumes: Vec<crate::contract::ProjectedConsume>,

    /// Whether only one of this gear may run in an installation.
    ///
    /// A fact about the gear with no capability behind it: the closed set of
    /// seven each stands for a trait the macro asserts, and there is no trait
    /// behind "one of me". Nor is it the per-process rule `RestHost` and
    /// `GrpcHub` carry -- a process can refuse a second of those because it
    /// sees its own gears, and no process sees another.
    ///
    /// So the runtime cannot enforce it and this tool must
    /// (ADR `cpt-gearbox-adr-one-per-installation`).
    pub one_per_installation: bool,
}

/// A `lifecycle(...)` clause: a bare flag, or `key = value` pairs.
///
/// Carries the keys it could not model out with it, the way [`GearArgs`] does.
/// They used to be consumed and thrown away, so the next lifecycle argument to
/// land in the macro would have been reported by nothing at all.
struct LifecycleArgs {
    lifecycle: ProjectedLifecycle,
    unmodelled: Vec<String>,
}

impl Parse for LifecycleArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut out = ProjectedLifecycle::default();
        let mut unmodelled = Vec::new();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            let name = key.to_string();

            if input.peek(Token![=]) {
                input.parse::<Token![=]>()?;
                match name.as_str() {
                    "entry" => out.entry = Some(input.parse::<LitStr>()?.value()),
                    "stop_timeout" => out.stop_timeout = Some(input.parse::<LitStr>()?.value()),
                    "await_ready" => out.await_ready = input.parse::<LitBool>()?.value(),
                    other => {
                        unmodelled.push(format!("lifecycle.{other}"));
                        // Consume the value so parsing can continue.
                        let _: Expr = input.parse()?;
                    }
                }
            } else if name == "await_ready" {
                // The bare-flag form, which is how every real gear writes it.
                out.await_ready = true;
            } else {
                unmodelled.push(format!("lifecycle.{name}"));
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(Self {
            lifecycle: out,
            unmodelled,
        })
    }
}

/// The whole `#[toolkit::gear(...)]` argument list.
struct GearArgs(ProjectedGear);

impl Parse for GearArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut out = ProjectedGear::default();

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            let name = key.to_string();

            // `lifecycle(...)` is the one argument that takes a nested list
            // rather than `= value`.
            if name == "lifecycle" && input.peek(syn::token::Paren) {
                let inner;
                syn::parenthesized!(inner in input);
                let parsed = inner.parse::<LifecycleArgs>()?;
                out.lifecycle = Some(parsed.lifecycle);
                out.unmodelled.extend(parsed.unmodelled);
                if input.peek(Token![,]) {
                    input.parse::<Token![,]>()?;
                }
                continue;
            }

            input.parse::<Token![=]>()?;

            match name.as_str() {
                "name" => out.name = input.parse::<LitStr>()?.value(),

                "deps" => {
                    let list = bracketed_idents(input)?;
                    // The macro derives the runtime gear name by replacing `_`
                    // with `-`; reproducing that here rather than guessing.
                    out.colocated_deps = list.into_iter().map(|i| i.replace('_', "-")).collect();
                }

                "capabilities" => {
                    // Bare idents or string literals; both appear in the wild.
                    out.runtime_caps = bracketed_idents(input)?;
                }

                "client" => {
                    let path: syn::Path = input.parse()?;
                    out.client_trait = Some(path_to_string(&path));
                }

                "one_per_installation" => {
                    out.one_per_installation = input.parse::<syn::LitBool>()?.value;
                }

                // Read and discarded: an arbitrary Rust expression with no
                // product meaning.
                "ctor" => {
                    let _: Expr = input.parse()?;
                }

                other => {
                    out.unmodelled.push(other.to_owned());
                    let _: Expr = input.parse()?;
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Self(out))
    }
}

/// Parse `[a, b, c]` where each element is a bare ident or a string literal.
fn bracketed_idents(input: ParseStream<'_>) -> syn::Result<Vec<String>> {
    let inner;
    syn::bracketed!(inner in input);
    let items = Punctuated::<Expr, Token![,]>::parse_terminated(&inner)?;
    Ok(items
        .into_iter()
        .filter_map(|expr| match expr {
            Expr::Path(p) => Some(path_to_string(&p.path)),
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Some(s.value()),
            _ => None,
        })
        .collect())
}

/// Render a path as source text without the whitespace `quote` would insert.
fn path_to_string(path: &syn::Path) -> String {
    let leading = if path.leading_colon.is_some() {
        "::"
    } else {
        ""
    };
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    format!("{leading}{}", segments.join("::"))
}

/// Project the facts `#[toolkit::gear]` owns.
///
/// # Errors
/// Returns a [`syn::Error`] when the attribute's arguments do not parse, which
/// would mean the crate does not compile either.
pub fn project_gear(site: &crate::attribute::AttributeSite<'_>) -> syn::Result<ProjectedGear> {
    let mut projected: ProjectedGear = site.attr.parse_args::<GearArgs>()?.0;
    projected.struct_ident.clone_from(&site.struct_ident);
    projected.provides = crate::contract::project_provides(site.item_attrs)?;
    projected.consumes = crate::contract::project_consumes(site.item_attrs)?;
    let gear_position = site
        .item_attrs
        .iter()
        .position(|attr| std::ptr::eq(attr, site.attr));
    let reads_gear_name = gear_position.is_some_and(|gear| {
        site.item_attrs
            .iter()
            .enumerate()
            .filter(|(_, attr)| crate::contract::is_consumes_attribute(attr))
            .all(|(index, _)| index < gear)
    });
    projected.consumer_wiring_key = if reads_gear_name {
        projected.name.clone()
    } else {
        heck::ToKebabCase::to_kebab_case(projected.struct_ident.as_str())
    };
    // `AttributeSite` carries the item's whole attribute list -- the same list
    // `provides` is read from -- so the cfg is here to be read. It used to be
    // left `false` for a caller to fill in, which made the primary projection
    // claim every cfg-gated gear was unconditional. The module half comes from
    // the site because answering it takes the whole scanned tree.
    projected.conditional = crate::attribute::has_cfg(site.item_attrs) || site.module_gated;
    Ok(projected)
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
