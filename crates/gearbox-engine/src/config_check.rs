//! A product's config values against the types their gears declare.
//!
//! The join this performs has no resolution in it -- a product's selected gears
//! against the catalogue's projected fields -- which is why it is a pure
//! function with a `Diagnostics` out-parameter rather than a step of the
//! resolver.
//!
//! **It is called from two places, and both are necessary.** `validate` is where
//! a join like this belongs, but the Add Gear panel resolves rather than
//! validates, so a check placed only in `validate` would never appear where a
//! person is actually standing when they set a value. `Diagnostics::finish`
//! sorts and dedups, so a caller that ran both reports each finding once.
//!
//! **Unknown keys are errors.** Generated YAML is deserialized with
//! `deny_unknown_fields`, so a key the projected schema does not name produces
//! a file the runtime refuses at startup. A field the gear reads belongs in
//! the schema; a typo must not reach the file.

use gearbox_ir::{
    Catalogue, ConfigFieldType, Diagnostic, DiagnosticCode, Diagnostics, GearSelection, Location,
    ProductIntent,
};

/// What a JSON value is, named as a diagnostic should name it.
pub(crate) fn actual(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(n) if n.is_f64() => "a fractional number",
        serde_json::Value::Number(_) => "an integer",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "a list",
        serde_json::Value::Object(_) => "a map",
    }
}

/// Whether `value` is admissible for `ty`, and what to say when it is not.
///
/// Returns `None` when the value fits. `Complex` always fits: the projector
/// could not read a shape for it, and inventing an expectation from an absence
/// would refuse descriptions that were always correct.
pub(crate) fn mismatch(ty: &ConfigFieldType, value: &serde_json::Value) -> Option<String> {
    let ok = match ty {
        ConfigFieldType::Complex => true,
        ConfigFieldType::Str => value.is_string(),
        ConfigFieldType::Bool => value.is_boolean(),
        ConfigFieldType::Int => value.is_i64() || value.is_u64(),
        // An integer is a perfectly good float, and YAML and JSON both write
        // `1` for `1.0`, so refusing it would refuse the ordinary spelling.
        ConfigFieldType::Float => value.is_number(),
        ConfigFieldType::Enum { variants } => value
            .as_str()
            .is_some_and(|s| variants.iter().any(|v| v == s)),
    };
    if ok {
        return None;
    }
    Some(match ty {
        ConfigFieldType::Enum { variants } => {
            format!("one of {}", variants.join(", "))
        }
        ConfigFieldType::Str => "a string".to_owned(),
        ConfigFieldType::Bool => "a boolean".to_owned(),
        ConfigFieldType::Int => "an integer".to_owned(),
        ConfigFieldType::Float => "a number".to_owned(),
        ConfigFieldType::Complex => unreachable!("Complex always fits"),
    })
}

/// Check every config value a product sets against the field it names.
pub fn check(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for selection in &intent.selected_gears {
        let Some(gear) = catalogue.gears.get(&selection.gear) else {
            continue;
        };

        // Keys the generator derives from the resolved topology. Setting one is
        // not an error -- it is simply overwritten -- but it must not be
        // overwritten in silence, or an operator sets a bind address, finds the
        // generated file disagreeing, and has nothing to read that explains why.
        for endpoint in &gear.serves {
            let Some(derived) = endpoint.config_key.as_ref() else {
                continue;
            };
            if !selection.config.contains_key(derived) {
                continue;
            }
            // `new`, not `error`: the code's own default severity is a warning,
            // and the product still builds.
            let mut diagnostic = Diagnostic::new(
                DiagnosticCode::GdlConfigKeyDerived,
                format!(
                    "`{}` sets `{derived}`, which its `{}` endpoint derives from the port the resolver assigned",
                    selection.gear, endpoint.name
                ),
            )
            .with_help(format!(
                "remove `{derived}` from this gear's config; a value written here describes a \
                 product that was not resolved"
            ))
            .at(location(selection, uri))
            // Named for the same reason GBX0120 is: the fix is a control -- the
            // row's reset -- and without a subject and a key the Validation
            // stage could offer only the file and an explanation.
            .about_config(derived.clone());
            if let Some(node) = gearbox_ir::NodeKind::Gear.id_for(selection.gear.as_str()) {
                diagnostic = diagnostic.about(node);
            }
            diagnostics.push(diagnostic);
        }

        let Some(schema) = gear.config_schema.as_ref() else {
            continue;
        };

        for (key, value) in &selection.config {
            let Some(field) = schema.fields.iter().find(|f| &f.name == key) else {
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::GdlUnknownConfigKey,
                        format!(
                            "`{}` config key `{key}` is not declared by `{}`",
                            selection.gear, schema.rust
                        ),
                        format!(
                            "remove `{key}`, or add it to the gear's configuration schema so \
                             generated YAML will deserialize"
                        ),
                    )
                    .at(location(selection, uri))
                    .about_config(key.clone()),
                );
                continue;
            };
            if field.secret && !literal_secret_is_allowed(value) {
                diagnostics.push(literal_secret(&selection.gear, key, selection, uri));
                continue;
            }
            let Some(expected) = mismatch(&field.ty, value) else {
                continue;
            };
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlConfigTypeMismatch,
                    format!(
                        "`{}` config key `{key}` is {}, but `{}` declares it as {expected}",
                        selection.gear,
                        actual(value),
                        schema.rust
                    ),
                    format!("write `{key}` as {expected}"),
                )
                .at(location(selection, uri)),
            );
        }

        report_unset_required(gear, schema, selection, uri, diagnostics);

        check_plugin_config(catalogue, selection, uri, diagnostics);
    }
}

/// Say when a required field has nothing to supply it.
///
/// **The code the Studio's configurator already told the reader to expect.** Its
/// `valueMissing` marks a required field with nothing set and its comment says
/// what that buys: "`required` is visible before a GBX code explains it from the
/// other side of the screen". There was no code. `required` was projected out of
/// the gear's struct and read by a marker in a panel and by nothing else.
///
/// The cost is not hypothetical. `EventBrokerConfig.mode` is required with no
/// default, and the runtime's loader is strict, so a product selecting that gear
/// without setting `mode` generates a file the gear refuses at `init` with
/// `missing field`. Nothing between the description and that failure said
/// anything.
///
/// **A key some `serves` endpoint derives is not counted.**
/// `ApiGatewayConfig.bind_addr` is required, has no default, and is written from
/// the port the resolver assigned -- so counting it would turn every product in
/// the corpus red on a field that is always supplied. The set comes from the
/// gear's own `serves` declarations, the same set `GBX0114` refuses a value for
/// just above.
///
/// A warning, because a value can still arrive from a profile or from an
/// operator editing the generated file. What this can say is that the
/// description does not supply it.
fn report_unset_required(
    gear: &gearbox_ir::GearDescriptor,
    schema: &gearbox_ir::ConfigSchema,
    selection: &GearSelection,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let derived: std::collections::BTreeSet<&str> = gear
        .serves
        .iter()
        .filter_map(|endpoint| endpoint.config_key.as_deref())
        .collect();

    for field in &schema.fields {
        if !field.required || field.default.is_some() {
            continue;
        }
        if derived.contains(field.name.as_str()) || selection.config.contains_key(&field.name) {
            continue;
        }
        let mut diagnostic = Diagnostic::new(
            DiagnosticCode::GdlRequiredConfigUnset,
            format!(
                "`{}` requires `{}` and `{}` declares no default for it, and this \
                     description sets no value",
                selection.gear, field.name, schema.rust
            ),
        )
        .with_help(format!(
            "set `{}` in this gear's `config = {{...}}`, or the generated file will be \
                 missing a field the gear's loader refuses at startup",
            field.name
        ))
        .at(location(selection, uri))
        // The key *is* the subject here, and the Studio's answer to "set `mode`"
        // is a box called `mode`. Naming it is what lets a person reach the
        // control from the message rather than reading the name out of the
        // sentence and finding it themselves.
        .about_config(field.name.clone());
        // **Named, so a client can offer the control that fixes it.** The
        // location alone points at the `use_gear` line, which is where the value
        // would be *typed* and not where anyone sets it in the Studio: the
        // configurator is a form, reached from a gear. Without a subject the
        // Validation stage could only offer the description and an explanation,
        // and the way to the field named in this very message was to remember
        // the gear, change stage, and find it again.
        //
        // `if let` rather than an `about` taking an `Option`, matching
        // `resolve::bindings`: an id that cannot be built is a diagnostic
        // without a subject, which is the ordinary case, not an error.
        if let Some(node) = gearbox_ir::NodeKind::Gear.id_for(selection.gear.as_str()) {
            diagnostic = diagnostic.about(node);
        }
        diagnostics.push(diagnostic);
    }
}

/// A plugin's configuration: credentials, and keys its schema does not declare.
///
/// The unknown-key rule matters more here than anywhere: the generator writes a
/// plugin's configuration through the same schema filter as a gear's
/// (`generate::config::drop_unknown_keys`), so an undeclared key -- a CA path, a
/// field a schema does not expose yet -- would vanish from the generated file
/// without a word. It used to stop at credentials while the demo product set an
/// undeclared `issuer`; the corpus declares the plugin's real surface now.
///
/// A plugin's configuration never reaches the lock as its own entry --
/// `resolve::product` merges it into the implementing gear's -- so the rule
/// serves the description, which is where the key was typed.
fn check_plugin_config(
    catalogue: &Catalogue,
    selection: &GearSelection,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for plugin in &selection.plugins {
        let Some(schema) = catalogue
            .gears
            .get(&plugin.gear)
            .and_then(|gear| gear.config_schema.as_ref())
        else {
            continue;
        };
        for (key, value) in &plugin.config {
            let Some(field) = schema.fields.iter().find(|field| &field.name == key) else {
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::GdlUnknownConfigKey,
                        format!(
                            "plugin `{}` config key `{key}` is not declared by `{}`",
                            plugin.gear, schema.rust
                        ),
                        format!(
                            "remove `{key}`, or add it to the plugin's configuration schema \
                             (`config_schema = config(exposes = [...])` in its gear.gdl); the \
                             generator writes only declared keys, so this one would be dropped"
                        ),
                    )
                    .at(Location::or_file(plugin.declared_at.as_ref(), uri))
                    .about_config(key.clone()),
                );
                continue;
            };
            if field.secret && !literal_secret_is_allowed(value) {
                diagnostics.push(literal_secret(&plugin.gear, key, selection, uri));
            }
        }
        // A required field the plugin's loader refuses without: the generated
        // file carries only what the description sets.
        for field in &schema.fields {
            if !field.required || field.default.is_some() || plugin.config.contains_key(&field.name)
            {
                continue;
            }
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::GdlRequiredConfigUnset,
                    format!(
                        "plugin `{}` requires `{}` and `{}` declares no default for it, and this \
                         description sets no value",
                        plugin.gear, field.name, schema.rust
                    ),
                )
                .with_help(format!(
                    "set `{}` in the plugin's `config = {{...}}`, or the generated file will be \
                     missing a field the plugin's loader refuses at startup",
                    field.name
                ))
                .at(Location::or_file(plugin.declared_at.as_ref(), uri))
                .about_config(field.name.clone()),
            );
        }
    }
}

/// Only `${VAR}` may stand where a credential is declared.
///
/// A non-string is never a reference, so it is always a literal -- an integer
/// token is still a token.
fn literal_secret_is_allowed(value: &serde_json::Value) -> bool {
    matches!(value, serde_json::Value::String(raw) if crate::secrets::is_env_placeholder(raw))
}

/// The refusal, naming the variable the generator would read.
///
/// The help spells the exact environment name rather than describing the shape,
/// because the author's next action is to type it and a wrong guess produces a
/// product that resolves and then starts with an empty password.
fn literal_secret(
    gear: &gearbox_ir::GearId,
    key: &str,
    selection: &GearSelection,
    uri: &str,
) -> Diagnostic {
    let name = crate::secrets::secret_env_name(gear, key);
    Diagnostic::error(
        DiagnosticCode::GdlLiteralSecret,
        format!("`{gear}` config key `{key}` is a credential and must not be written here"),
        format!(
            "write `{key} = \"${{{name}}}\"` and put the value in the environment or a Secret; \
             a description is committed, and `product.lock` is generated from it"
        ),
    )
    .at(location(selection, uri))
}

/// Where a diagnostic about one selection points.
///
/// The `use_gear(...)` span when the description recorded one; claiming
/// byte-zero precision would be worse than claiming none. The rule itself is
/// `Location::or_file`, next to the sentinel it falls back to: this file was the
/// first of about thirty across two crates to need it.
fn location(selection: &gearbox_ir::GearSelection, uri: &str) -> Location {
    gearbox_ir::Location::or_file(selection.declared_at.as_ref(), uri)
}

#[cfg(test)]
#[path = "config_check_tests.rs"]
mod config_check_tests;
