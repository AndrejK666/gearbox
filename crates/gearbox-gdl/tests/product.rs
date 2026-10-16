//! Evaluating a `product.gdl`.
//!
//! The theme running through these: a product description declares every
//! deployment profile as *data* and is checked for what it can know about
//! itself. Whether a named gear or provider exists is the resolver's question,
//! and asking it here would mean a product file could not be edited until every
//! source it names had been scanned.

#![allow(
    clippy::unwrap_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use gearbox_gdl::{FileIdentity, GdlEngine};
use gearbox_ir::{
    DeploymentProfileDecl, DiagnosticCode, ProductIntent, ProfileId, RelPath, SourceId,
};

fn identity() -> FileIdentity {
    FileIdentity {
        uri: "file:///repo/products/demo/product.gdl".to_owned(),
        source: SourceId::new("product").unwrap(),
        gdl_path: RelPath::new("product.gdl").unwrap(),
        load_paths: None,
    }
}

fn eval(src: &str) -> (Option<ProductIntent>, Vec<DiagnosticCode>, String) {
    let out = GdlEngine::new().eval_product(&identity(), src);
    let codes = out.diagnostics.iter().map(|d| d.code).collect();
    let messages = out
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join(" | ");
    (out.value, codes, messages)
}

/// A minimal well-formed product, with a hole for the field under test.
fn product(extra: &str) -> String {
    format!(
        r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "gears-rust", at = path("../gears-rust"))],
    profiles = [embedded(id = "dev"), kubernetes(id = "prod", discovery = "static")],
    default_profile = "dev",
    gears = [use_gear("api-gateway", source = "gears-rust")],
    {extra}
)
"#
    )
}

#[test]
fn a_minimal_product_evaluates() {
    let (intent, codes, messages) = eval(&product(""));
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let intent = intent.unwrap();
    assert_eq!(intent.id, "demo");
    // `name` defaults to the id rather than being mandatory: a product that has
    // not been named yet is not an error worth blocking on.
    assert_eq!(intent.display_name, "demo");
    assert_eq!(intent.profiles.len(), 2);
}

#[test]
fn use_gear_on_the_third_line_records_line_two() {
    // 0-based: a call written on the third source line must not claim the start
    // of the file. That is the unit proof behind "the link opens on use_gear".
    let src = "\
product(
    id = \"demo\", version = \"0.1.0\", sources = [source(id = \"s\", at = path(\".\"))], profiles = [embedded(id = \"dev\")], default_profile = \"dev\",
    gears = [use_gear(\"api-gateway\", source = \"s\")],
)
";
    let (intent, codes, messages) = eval(src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let intent = intent.expect("evaluates");
    let at = intent.selected_gears[0]
        .declared_at
        .as_ref()
        .expect("use_gear records its call site");
    assert_eq!(at.range.start.line, 2, "{at:?}");
    assert!(at.uri.contains("product.gdl"), "{}", at.uri);
}

#[test]
fn every_profile_is_data_and_scoping_selects_among_them() {
    let src = product(
        r#"bindings = [
            bind(consumer = "c", contract = "p/Api@v1", mode = binding_mode.remote,
                 transport = transport.rest, profiles = ["prod"]),
        ],"#,
    );
    let (intent, codes, messages) = eval(&src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let intent = intent.unwrap();

    let dev = ProfileId::new("dev").unwrap();
    let prod = ProfileId::new("prod").unwrap();
    assert!(
        intent.bindings_for(&dev).is_empty(),
        "the binding is scoped to prod only"
    );
    assert_eq!(intent.bindings_for(&prod).len(), 1);
}

#[test]
fn an_unscoped_declaration_applies_to_every_profile() {
    let src = product(
        r#"bindings = [
            bind(consumer = "c", contract = "p/Api@v1", mode = binding_mode.auto),
        ],"#,
    );
    let (intent, codes, _) = eval(&src);
    assert!(codes.is_empty(), "{codes:?}");
    let intent = intent.unwrap();
    for id in intent.profiles.keys() {
        assert_eq!(
            intent.bindings_for(id).len(),
            1,
            "an empty `profiles` means every profile, not none"
        );
    }
}

// ---------------------------------------------------------------- GBX0110

#[test]
fn two_bindings_for_one_edge_in_one_profile_collide() {
    let src = product(
        r#"bindings = [
            bind(consumer = "c", contract = "p/Api@v1", mode = binding_mode.remote,
                 profiles = ["prod"]),
            bind(consumer = "c", contract = "p/Api@v1", mode = binding_mode.local,
                 profiles = ["prod"]),
        ],"#,
    );
    let (_, codes, messages) = eval(&src);
    assert!(
        codes.contains(&DiagnosticCode::GdlDuplicateProfileScoped),
        "one edge cannot be bound two ways at once: {codes:?} {messages}"
    );
}

#[test]
fn an_unscoped_binding_collides_with_a_scoped_one() {
    // The unscoped entry already claims every profile, so the second is a
    // contradiction rather than a narrowing.
    let src = product(
        r#"bindings = [
            bind(consumer = "c", contract = "p/Api@v1", mode = binding_mode.remote),
            bind(consumer = "c", contract = "p/Api@v1", mode = binding_mode.local,
                 profiles = ["prod"]),
        ],"#,
    );
    let (_, codes, messages) = eval(&src);
    assert!(
        codes.contains(&DiagnosticCode::GdlDuplicateProfileScoped),
        "{codes:?} {messages}"
    );
}

#[test]
fn disjoint_profile_scopes_do_not_collide() {
    // The whole point of profile-scoping: the same edge may resolve differently
    // per profile, which must not read as a duplicate.
    let src = product(
        r#"bindings = [
            bind(consumer = "c", contract = "p/Api@v1", mode = binding_mode.local,
                 profiles = ["dev"]),
            bind(consumer = "c", contract = "p/Api@v1", mode = binding_mode.remote,
                 profiles = ["prod"]),
        ],"#,
    );
    let (_, codes, messages) = eval(&src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
}

#[test]
fn one_cluster_scope_bound_twice_in_a_profile_collides() {
    let src = product(
        r#"cluster_profiles = [
            cluster_profile(name = "event-broker", cache = provider("standalone"),
                            profiles = ["dev"]),
            cluster_profile(name = "event-broker", cache = provider("postgres"),
                            profiles = ["dev"]),
        ],"#,
    );
    let (_, codes, messages) = eval(&src);
    assert!(
        codes.contains(&DiagnosticCode::GdlDuplicateProfileScoped),
        "one scope resolves to one backend per profile: {codes:?} {messages}"
    );
}

#[test]
fn a_duplicate_profile_id_collides() {
    let src = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [embedded(id = "dev"), embedded(id = "dev")],
    default_profile = "dev",
    gears = [use_gear("g", source = "s")],
)
"#;
    let (_, codes, messages) = eval(src);
    assert!(
        codes.contains(&DiagnosticCode::GdlDuplicateProfileScoped),
        "{codes:?} {messages}"
    );
}

// ---------------------------------------------------------------- references

#[test]
fn default_profile_must_name_a_declared_profile() {
    let src = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [embedded(id = "dev")],
    default_profile = "nope",
    gears = [use_gear("g", source = "s")],
)
"#;
    let (intent, _, messages) = eval(src);
    assert!(intent.is_none(), "an unresolvable default cannot be used");
    assert!(
        messages.contains("nope") && messages.contains("dev"),
        "the message must name both the bad value and the choices: {messages}"
    );
}

#[test]
fn scoping_to_an_undeclared_profile_is_reported() {
    let src = product(
        r#"applications = [application("p", anchor = "api-gateway", profiles = ["staging"])],"#,
    );
    let (_, _, messages) = eval(&src);
    assert!(
        messages.contains("staging") && messages.contains("not declared"),
        "{messages}"
    );
}

#[test]
fn a_gear_from_an_undeclared_source_is_reported() {
    let src = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [use_gear("g", source = "other")],
)
"#;
    let (_, _, messages) = eval(src);
    assert!(messages.contains("other"), "{messages}");
}

/// A registry is a source now, and the prefix is how a gear id names a package.
///
/// It used to be spelled in the vocabulary only so the refusal could name it.
/// The refusal is gone; what stays is that the *kind* is checked, so a typo in
/// the constructor still reads as a typo rather than as an unknown function.
#[test]
fn a_registry_source_declares_a_registry_and_a_prefix() {
    let src = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "cf", at = registry("crates.io", prefix = "cf-gears-"))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [],
)
"#;
    let (intent, codes, messages) = eval(src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let sources = intent.unwrap().sources;
    let source = sources
        .get(&SourceId::new("cf").unwrap())
        .expect("the source");
    let gearbox_ir::SourceDecl::Registry { url, prefix, .. } = source else {
        panic!("a `registry()` source must be the Registry variant: {source:?}");
    };
    assert_eq!(url, "crates.io");
    assert_eq!(prefix.as_deref(), Some("cf-gears-"));

    // Field by field rather than comparing the whole variant, because it now
    // carries the `source(...)` span and a literal expectation would have to
    // restate the fixture's own line numbers to match.
    let at = source
        .declared_at()
        .expect("`source(...)` records where it was written");
    assert_eq!(
        at.range.start.line,
        u32::try_from(
            src.lines()
                .position(|line| line.contains("source(id = \"cf\""))
                .expect("the fixture declares it")
        )
        .unwrap(),
        "the span must be the `source(...)` line, not the top of the file"
    );
}

/// A version requirement on a gear drawn from a path source is refused.
///
/// Dropping it in silence would let a description carry a pin nobody honours:
/// the reader would believe a version was fixed while the directory on disk is
/// whatever happens to be checked out.
#[test]
fn a_version_on_a_path_source_is_refused() {
    let src = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "local", at = path("../gears"))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [use_gear("api-gateway", source = "local", version = "0.4")],
)
"#;
    let (_, codes, messages) = eval(src);
    assert!(codes.contains(&DiagnosticCode::GdlEval), "{codes:?}");
    assert!(messages.contains("is not a registry"), "{messages}");
}

#[test]
fn git_must_pin_something() {
    let src = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "s", at = git(url = "https://example.com/x.git"))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [],
)
"#;
    let (_, _, messages) = eval(src);
    assert!(messages.contains("pins nothing"), "{messages}");
}

#[test]
fn a_branch_is_accepted_but_recorded_as_not_immutable() {
    let src = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "s", at = git(url = "https://example.com/x.git", branch = "main"))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [],
)
"#;
    let (intent, codes, messages) = eval(src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let intent = intent.unwrap();
    let source = intent.sources.values().next().unwrap();
    assert!(
        !source.is_immutable(),
        "a branch makes the lock repeatable but not reproducible, and the IR says so"
    );
}

// ---------------------------------------------------------------- provider options

#[test]
fn provider_options_become_json_and_sort_stably() {
    let src = product(
        r#"cluster_profiles = [
            cluster_profile(name = "s", cache = provider("postgres",
                schema = "cluster", pool_max_size = 10, tls = True)),
        ],"#,
    );
    let (intent, codes, messages) = eval(&src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let intent = intent.unwrap();
    let options = &intent.cluster_scopes[0].cache.options;

    // A BTreeMap in the IR, so order is by key regardless of how it was typed.
    let keys: Vec<&str> = options.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["pool_max_size", "schema", "tls"]);
    assert_eq!(options["pool_max_size"], serde_json::json!(10));
    assert_eq!(options["schema"], serde_json::json!("cluster"));
    assert_eq!(options["tls"], serde_json::json!(true));
}

#[test]
fn a_non_json_option_is_refused_rather_than_nulled() {
    // `None` would encode as JSON null, and the lock is TOML, which has no null.
    let src = product(
        r#"cluster_profiles = [
            cluster_profile(name = "s", cache = provider("postgres", schema = None)),
        ],"#,
    );
    let (_, _, messages) = eval(&src);
    assert!(
        messages.contains("schema") && messages.contains("not a string"),
        "{messages}"
    );
}

#[test]
fn a_cluster_scope_without_a_cache_is_refused() {
    // The cache is the anchor the SDK compare-and-swap defaults are layered
    // over, so a scope without one has nothing to fall back to.
    let src = product(
        r#"cluster_profiles = [cluster_profile(name = "s", lock = provider("postgres"))],"#,
    );
    let (_, codes, _) = eval(&src);
    assert!(
        !codes.is_empty(),
        "omitting the mandatory `cache` must be refused"
    );
}

// ---------------------------------------------------------------- surfaces

#[test]
fn gear_is_not_callable_in_a_product() {
    // Separate global sets, so a file that mixes the two fails at the call
    // rather than producing half of each.
    let (_, _, messages) =
        eval(r#"gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"))"#);
    assert!(
        messages.contains("gear"),
        "expected an unbound-name error naming `gear`: {messages}"
    );
}

#[test]
fn a_file_with_no_product_is_a_cardinality_error() {
    let (_, codes, _) = eval("X = 1\n");
    assert_eq!(codes, vec![DiagnosticCode::GdlCardinality]);
}

#[test]
fn two_products_in_one_file_is_a_cardinality_error() {
    let src = format!("{}\n{}", product(""), product(""));
    let (_, codes, _) = eval(&src);
    assert!(codes.contains(&DiagnosticCode::GdlCardinality), "{codes:?}");
}

#[test]
fn a_product_is_held_to_the_same_declarative_standard() {
    let (_, codes, _) = eval(
        r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [use_gear(g, source = "s") for g in ["a", "b"]],
)
"#,
    );
    // Every forbidden construct is reported, not just the first -- a file full
    // of conditionals should list them all -- so assert the code, not the count.
    assert!(
        !codes.is_empty(),
        "the comprehension ban is not gear-specific"
    );
    assert!(
        codes
            .iter()
            .all(|c| *c == DiagnosticCode::GdlForbiddenConstruct),
        "{codes:?}"
    );
}

#[test]
fn an_unknown_argument_is_refused_not_ignored() {
    let (_, codes, _) = eval(&product(r"replicas = 3,"));
    assert_eq!(codes, vec![DiagnosticCode::GdlUnknownArgument]);
}

#[test]
fn preferences_deduplicate() {
    let src = product(
        r#"preferences = [prefer.fewer_applications(), prefer.fewer_applications(),
                          prefer.isolate(gear = "api-gateway")],"#,
    );
    let (intent, codes, messages) = eval(&src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    assert_eq!(intent.unwrap().preferences.len(), 2);
}

#[test]
fn an_application_with_zero_replicas_is_refused() {
    let src =
        product(r#"applications = [application("p", anchor = "api-gateway", replicas = 0)],"#);
    let (_, _, messages) = eval(&src);
    assert!(messages.contains("does not run"), "{messages}");
}

/// A product may name a template overlay, and may put it outside itself.
///
/// The overlay used to be found only by convention -- a `templates/` directory
/// beside the description -- which works for one product and makes a house that
/// keeps twenty hold twenty copies of the same chart. Naming a path lets one
/// directory serve the fleet, and `..` is exactly what the convention could not
/// express.
#[test]
fn a_product_may_declare_where_its_templates_live() {
    let (intent, codes, messages) = eval(&product(r#"templates = path("../../house-templates"),"#));
    assert!(codes.is_empty(), "{codes:?} {messages}");
    assert_eq!(
        intent.unwrap().templates.as_deref(),
        Some("../../house-templates")
    );

    // Undeclared keeps the convention, and says so by being absent rather than
    // by naming `templates/` -- the fallback is the loader's, not the schema's.
    let (intent, codes, _) = eval(&product(""));
    assert!(codes.is_empty(), "{codes:?}");
    assert_eq!(intent.unwrap().templates, None);
}

/// `git()` is refused by name, not as an unknown argument.
///
/// It is spellable, so a reader will try it. The refusal has to say why it
/// cannot work: generation is a pure function of the lock, and a template set
/// fetched while generating would make `--dry-run` a preview of whatever the
/// remote said at the time.
#[test]
fn a_fetched_template_set_is_refused_with_its_reason() {
    let (intent, codes, messages) = eval(&product(
        r#"templates = git(url = "https://example.com/house.git", tag = "v1"),"#,
    ));
    assert!(
        codes.contains(&DiagnosticCode::GdlEval),
        "{codes:?} {messages}"
    );
    assert!(messages.contains("git()"), "{messages}");
    // No intent: `eval_product` withholds the value whenever a diagnostic is an
    // error, so a description naming a template set nobody can read resolves to
    // nothing rather than to a product quietly using the builtins.
    assert!(intent.is_none());
}

#[test]
fn cargo_profile_is_recorded_on_self_hosted() {
    let src = r#"
product(
    id = "demo",
    name = "Demo",
    version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [self_hosted(
        id = "local",
        host = "gateway",
        worker_discovery = "static",
        cargo_profile = "release",
    )],
    default_profile = "local",
    gears = [],
)
"#;
    let (intent, codes, messages) = eval(src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let intent = intent.expect("evaluates");
    match intent.profiles.get(&ProfileId::new("local").unwrap()) {
        Some(DeploymentProfileDecl::SelfHosted { cargo_profile, .. }) => {
            assert_eq!(cargo_profile.as_deref(), Some("release"));
        }
        other => panic!("expected self_hosted, got {other:?}"),
    }
}

#[test]
fn cargo_profile_dotdot_is_refused() {
    let src = r#"
product(
    id = "demo",
    name = "Demo",
    version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [self_hosted(
        id = "local",
        host = "gateway",
        worker_discovery = "static",
        cargo_profile = "..",
    )],
    default_profile = "local",
    gears = [],
)
"#;
    let (intent, _codes, messages) = eval(src);
    assert!(messages.contains("cargo profile"), "{messages}");
    assert!(intent.is_none());
}

/// Every product the create wizard renders must evaluate, for every kind.
///
/// This is the check that was missing, and the gap it left was reachable.
/// `render_profile_entry` emitted `discovery = "dns"` for the two non-embedded
/// kinds -- not a discovery kind the lowering accepts -- so a product created
/// with a `kubernetes` or `self_hosted` profile evaluated straight to a
/// diagnostic. Nothing noticed because the renderer's output had never been fed
/// back through the evaluator that has to read it.
///
/// Driven off `PROFILE_KINDS` rather than a list of three, so a fourth kind is
/// covered the day it is added rather than the day someone remembers this test.
#[test]
fn rendered_product_templates_evaluate() {
    for kind in gearbox_gdl::edit::PROFILE_KINDS {
        let rendered =
            gearbox_gdl::edit::render_product_template(&gearbox_gdl::edit::CreateProductParams {
                id: "demo".to_owned(),
                name: "Demo".to_owned(),
                version: "0.1.0".to_owned(),
                sources: vec![("gears-rust".to_owned(), "../gears-rust".to_owned())],
                profile_kind: (*kind).to_owned(),
                profile_id: "first".to_owned(),
            });

        let (intent, codes, messages) = eval(&rendered);
        assert!(
            codes.is_empty(),
            "{kind}: a freshly created product must evaluate clean, got {codes:?}: \
             {messages}\n{rendered}"
        );
        let intent = intent.unwrap_or_else(|| panic!("{kind}: an intent"));
        assert!(
            intent
                .profiles
                .contains_key(&ProfileId::new("first").unwrap()),
            "{kind}: the profile the wizard was asked for must be declared"
        );
    }
}

/// `plugin(...)` records the line it was written on.
///
/// Every other span-carrying record has this test -- `use_gear` and `source` in
/// this file, the profiles beside them -- and without it a `call_location` that
/// returned the wrong line, or `None`, would look exactly like the file-level
/// fallback to every downstream test.
#[test]
fn plugin_on_the_fifth_line_records_line_four() {
    let src = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [
        use_gear("host", source = "s", plugins = [plugin("filler")]),
    ],
)
"#;
    let (intent, codes, messages) = eval(src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let intent = intent.expect("the description evaluates");
    let plugin = &intent.selected_gears[0].plugins[0];
    let at = plugin
        .declared_at
        .as_ref()
        .expect("`plugin(...)` records where it was written");
    let expected = src
        .lines()
        .position(|line| line.contains("plugin(\"filler\")"))
        .expect("the fixture writes one");
    assert_eq!(
        at.range.start.line as usize, expected,
        "the span must be the `plugin(...)` call, not the enclosing `use_gear`"
    );
    assert!(at.uri.contains("product.gdl"), "{}", at.uri);
}

/// `entry_index` is the position the entry is *written* at.
///
/// The editor addresses a connection by its place in the host's written
/// `plugins = [...]`, so this field is that address travelling to the client.
/// Pinned on its own because every other test here would pass just as well if it
/// were the position in the surviving vector -- the two only diverge when an
/// entry is dropped, which is the case below.
#[test]
fn entry_index_is_the_written_position_of_each_connection() {
    let src = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [embedded(id = "dev"), embedded(id = "prod")],
    default_profile = "dev",
    gears = [
        use_gear("host", source = "s", plugins = [
            plugin("first"),
            plugin("second", profiles = ["prod"]),
            plugin("third"),
        ]),
    ],
)
"#;
    let (intent, codes, messages) = eval(src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let intent = intent.expect("the description evaluates");
    let plugins = &intent.selected_gears[0].plugins;
    let written: Vec<(&str, usize)> = plugins
        .iter()
        .map(|plugin| (plugin.gear.as_str(), plugin.entry_index))
        .collect();
    assert_eq!(
        written,
        vec![("first", 0), ("second", 1), ("third", 2)],
        "each connection must carry where it is written"
    );
}

/// An entry that evaluation cannot use takes the whole intent down with it.
///
/// **This is what makes a positional address safe**, and it is worth pinning
/// precisely because nothing else states it. `build_plugins` skips an entry
/// whose id does not parse and an entry that duplicates a selection, which would
/// leave the surviving vector shorter than the written list -- but both skips
/// also push an *error*, and `eval_product` yields no intent when any error is
/// present. So a client never holds an intent whose plugins are missing one.
///
/// If that rule ever loosens -- either diagnostic downgraded to a warning, or a
/// partial intent handed out for a broken product -- positions and survivors
/// part company, and `PluginSelection::entry_index` is what keeps the editor
/// aiming at the right entry. This test is the tripwire for that change.
#[test]
fn an_unusable_plugin_entry_yields_no_intent_at_all() {
    let malformed = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [
        use_gear("host", source = "s", plugins = [
            plugin("Not A Gear Id"),
            plugin("good-plugin"),
        ]),
    ],
)
"#;
    let (intent, codes, _messages) = eval(malformed);
    assert!(
        intent.is_none(),
        "a plugin id that does not parse must not produce a partial intent"
    );
    assert!(codes.contains(&DiagnosticCode::GdlEval), "{codes:?}");

    let collided = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [
        use_gear("host", source = "s", plugins = [
            plugin("twice"),
            plugin("twice"),
            plugin("after"),
        ]),
    ],
)
"#;
    let (intent, codes, _messages) = eval(collided);
    assert!(
        intent.is_none(),
        "a duplicate selection must not produce a partial intent either"
    );
    assert!(
        codes.contains(&DiagnosticCode::GdlDuplicateProfileScoped),
        "{codes:?}"
    );
}

/// `crates = registry(...)` on a checkout says where the build takes its crates;
/// on anything but a `path(...)` it means nothing and is refused.
#[test]
fn a_checkout_can_take_its_crates_from_a_registry() {
    let src = product("").replace(
        r#"at = path("../gears-rust"))"#,
        r#"at = path("../gears-rust"), crates = registry("crates.io"))"#,
    );
    let (intent, codes, messages) = eval(&src);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let intent = intent.unwrap();
    let source = intent.sources.values().next().unwrap();
    assert!(
        matches!(source, gearbox_ir::SourceDecl::Path { crates: Some(r), .. } if r == "crates.io"),
        "{source:?}"
    );

    let git = product("").replace(
        r#"at = path("../gears-rust"))"#,
        r#"at = git(url = "https://x/y", tag = "v1"), crates = registry("crates.io"))"#,
    );
    let (_, codes, messages) = eval(&git);
    assert!(
        !codes.is_empty() && messages.contains("crates"),
        "{codes:?} {messages}"
    );
}
