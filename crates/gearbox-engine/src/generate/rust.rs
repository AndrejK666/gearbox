//! The two generated Rust files: the host entry point and the link file.
//!
//! Rendered with minijinja and its default delimiters, which is safe for Rust
//! because Rust's own brace pairs never sit adjacent on one line. That is a
//! property of the templates as written rather than of Rust, so the tests below
//! assert it instead of trusting it. The Helm templates M7 adds cannot use
//! these delimiters -- Helm has already claimed them -- which is what the
//! `custom_syntax` feature is enabled for.

use std::collections::BTreeSet;

use gearbox_ir::{FileEntry, FileKind, Ownership, ResolvedApplication};
use minijinja::context;

use super::templates;
use super::{GenerateError, GenerateInput, header, paths};

/// `<layout>/<p>/src/main.rs`.
///
/// # Errors
/// Returns [`GenerateError::Template`] if the template cannot be rendered.
pub fn host_main(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<FileEntry, GenerateError> {
    let body = templates::render(
        "main.rs",
        input.templates.get("main.rs")?,
        context! {
            header => header("//").trim_end(),
            application => application.name.as_str(),
            bin_name => application.bin_name.as_str(),
            gear_count => application.gears.len(),
            render_env => renders_env(input, application),
        },
    )?;

    Ok(FileEntry::text(
        paths::rel(&[input.layout(), application.name.as_str(), "src", "main.rs"])?,
        body,
        FileKind::Rust,
        Ownership::Generated,
    ))
}

/// The roles the anchor gear declares, for the table the worker compiles in.
///
/// Read from the catalogue rather than from the lock: the lock records which
/// role a deployment *starts* in, and the binary needs every role it could be
/// started in. Empty when the catalogue is out of scope, which is the ordinary
/// case for a generation driven from a lock alone -- and then `--role` refuses
/// everything, which is the right answer for a binary that cannot say what its
/// roles are.
fn roles_of(input: &GenerateInput<'_>, application: &ResolvedApplication) -> Vec<RoleContext> {
    input
        .catalogue
        .and_then(|c| c.gears.get(&application.anchor))
        .map(|gear| {
            gear.declared_roles
                .iter()
                .map(|r| RoleContext {
                    name: r.name.clone(),
                    directory_name: r.directory_name.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the worker's compiled-in role table.
#[derive(serde::Serialize)]
struct RoleContext {
    name: String,
    directory_name: String,
}

/// `<layout>/<w>/src/main.rs` for a worker.
///
/// A second function rather than a branch inside [`host_main`]: this module's
/// shape is one function per file it produces, and the two entry points share
/// no context beyond the header -- a worker needs its directory identity and
/// its version, which a host has no use for.
///
/// # Errors
/// Returns [`GenerateError::Template`] if the template cannot be rendered.
pub fn worker_main(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<FileEntry, GenerateError> {
    let body = templates::render(
        "worker_main.rs",
        input.templates.get("worker_main.rs")?,
        context! {
            header => header("//").trim_end(),
            application => application.name.as_str(),
            bin_name => application.bin_name.as_str(),
            gear_count => application.gears.len(),
            gear_name => application.registers_as(),
            // The anchor's own id, and deliberately not `registers_as()`. A role
            // changes the name an instance registers under, not the section the
            // gear reads its configuration from -- `GearCtx::config()` keys that
            // by the `#[toolkit::gear(name = ...)]` attribute. Passing one string
            // for both filed the host's rendered config under a key the gear
            // never looks up.
            config_gear_name => application.anchor.as_str(),
            roles => roles_of(input, application),
            version => input.lock.product.version.as_str(),
            render_env => renders_env(input, application),
        },
    )?;

    Ok(FileEntry::text(
        paths::rel(&[input.layout(), application.name.as_str(), "src", "main.rs"])?,
        body,
        FileKind::Rust,
        Ownership::Generated,
    ))
}

/// Whether this process's gears read variables in their configuration
/// (`${OIDC_ISSUER}`), which its entry point then substitutes at start.
///
/// Decided per process, from the lock: a process whose gears read none gets
/// the entry point it always had, and no `config_env.rs`.
#[must_use]
pub fn renders_env(input: &GenerateInput<'_>, application: &ResolvedApplication) -> bool {
    fn reads(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(s) => !crate::config_check::variables_in(s).is_empty(),
            serde_json::Value::Array(items) => items.iter().any(reads),
            serde_json::Value::Object(map) => map.values().any(reads),
            _ => false,
        }
    }
    application.gears.iter().any(|id| {
        input
            .lock
            .gears
            .get(id)
            .is_some_and(|gear| gear.config.values().any(reads))
    })
}

/// `<layout>/<p>/src/config_env.rs`: the substitution of `${NAME}` in the
/// gears' configuration at start, for a process whose gears read variables.
///
/// # Errors
/// Returns [`GenerateError::Template`] if the template cannot be rendered.
pub fn config_env(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<Option<FileEntry>, GenerateError> {
    if !renders_env(input, application) {
        return Ok(None);
    }
    let body = templates::render(
        "config_env.rs",
        input.templates.get("config_env.rs")?,
        context! {},
    )?;
    Ok(Some(FileEntry::text(
        paths::rel(&[
            input.layout(),
            application.name.as_str(),
            "src",
            "config_env.rs",
        ])?,
        format!("{}\n{body}", header("//").trim_end()),
        FileKind::Rust,
        Ownership::Generated,
    )))
}

/// `<layout>/<p>/src/registered_gears.rs`.
///
/// One `use <ident> as _;` per [`CargoRef::link`](gearbox_ir::CargoRef::link)
/// entry, so a gear whose plugins are separate registrations inside one crate
/// gets a line each. Sorted and deduplicated: two gears in a process can
/// legitimately name the same crate.
///
/// # Errors
/// Returns [`GenerateError::UnlinkableIdent`] when a link entry's crate root is
/// not a dependency the manifest declares, which would be a file that does not
/// compile; [`GenerateError::UnknownGear`] when the process names a gear the
/// lock does not describe.
pub fn registered_gears(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<FileEntry, GenerateError> {
    // The dependency keys the manifest will emit. Built from the same field the
    // manifest keys on, so the two files cannot disagree.
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for id in &application.gears {
        let gear = input
            .lock
            .gears
            .get(id)
            .ok_or_else(|| GenerateError::UnknownGear {
                application: application.name.to_string(),
                gear: id.to_string(),
            })?;
        declared.insert(gear.package.lib_ident.as_str());
    }

    let mut idents: BTreeSet<&str> = BTreeSet::new();
    for id in &application.gears {
        let Some(gear) = input.lock.gears.get(id) else {
            continue;
        };
        for ident in gear.package.link_idents() {
            // `mini_chat::infra::plugins::static_audit` links through the
            // `mini_chat` dependency; the root segment is what Cargo has to
            // know about, and the rest is a module path inside it.
            let root = ident.split("::").next().unwrap_or(ident);
            if !declared.contains(root) {
                return Err(GenerateError::UnlinkableIdent {
                    gear: id.to_string(),
                    ident: ident.to_owned(),
                    root: root.to_owned(),
                });
            }
            idents.insert(ident);
        }
    }
    // A client SDK carries the registration that wires its remote client; it
    // stays linked by its own line.
    for (sdk, _) in super::manifest::client_sdks(input, application) {
        idents.insert(sdk.lib_ident.as_str());
    }

    let body = templates::render(
        "registered_gears.rs",
        input.templates.get("registered_gears.rs")?,
        context! {
            header => header("//").trim_end(),
            idents => idents.iter().collect::<Vec<_>>(),
        },
    )?;

    Ok(FileEntry::text(
        paths::rel(&[
            input.layout(),
            application.name.as_str(),
            "src",
            "registered_gears.rs",
        ])?,
        body,
        FileKind::Rust,
        Ownership::Generated,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No Jinja delimiter may survive into the rendered Rust.
    ///
    /// Rust and Jinja share `{` and `}`, and the only thing keeping them apart
    /// is that these templates never write `{{` except where Jinja should read
    /// it. A future edit adding a `format!("{{literal}}")` would produce a file
    /// whose failure mode is a rustc parse error at a line the template does
    /// not have, so the property is asserted rather than assumed.
    #[test]
    fn no_delimiters_survive_rendering() {
        let rendered = templates::render(
            "registered_gears.rs",
            templates::TemplateSet::builtin("registered_gears.rs").expect("builtin"),
            context! {
                header => "// generated",
                idents => vec!["api_gateway", "mini_chat::infra::plugins::static_audit"],
            },
        )
        .expect("render the link file");

        for delimiter in ["{{", "}}", "{%", "%}"] {
            assert!(
                !rendered.contains(delimiter),
                "`{delimiter}` survived into the generated Rust:\n{rendered}"
            );
        }
        assert!(rendered.contains("use api_gateway as _;"));
        assert!(rendered.contains("use mini_chat::infra::plugins::static_audit as _;"));
    }

    /// A missing variable must fail loudly.
    ///
    /// The default would render it as an empty string, which in a Rust template
    /// means a hole the compiler reports as a syntax error somewhere unrelated.
    #[test]
    fn an_undefined_variable_is_an_error() {
        let result = templates::render(
            "registered_gears.rs",
            templates::TemplateSet::builtin("registered_gears.rs").expect("builtin"),
            context! { header => "// generated" },
        );
        assert!(result.is_err(), "a missing `idents` should not render");
    }

    /// The substitution a generated entry point runs at start, compiled here
    /// from the very template it is rendered from.
    #[allow(
        dead_code,
        reason = "the template's whole surface, not all of it exercised"
    )]
    mod config_env {
        include!("templates/config_env.rs.jinja");
    }

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn the_config_env_template_renders_to_itself() {
        let source = templates::TemplateSet::builtin("config_env.rs").expect("builtin");
        let rendered =
            templates::render("config_env.rs", source, context! {}).expect("render config_env.rs");
        assert_eq!(rendered.trim_end(), source.trim_end());
    }

    #[test]
    fn a_variable_is_substituted_whole_or_embedded() {
        let lookup = env(&[("OIDC_ISSUER", "https://id.example.com/realms/product")]);
        assert_eq!(
            config_env::render_str("${OIDC_ISSUER}", &lookup),
            Ok(Some("https://id.example.com/realms/product".to_owned()))
        );
        assert_eq!(
            config_env::render_str("${OIDC_ISSUER}/protocol", &lookup),
            Ok(Some(
                "https://id.example.com/realms/product/protocol".to_owned()
            ))
        );
        assert_eq!(config_env::render_str("plain", &lookup), Ok(None));
    }

    #[test]
    fn a_default_and_a_lowercase_name_are_left_as_written() {
        let lookup = env(&[]);
        assert_eq!(config_env::render_str("${PORT:-8080}", &lookup), Ok(None));
        assert_eq!(config_env::render_str("${tenant}/x", &lookup), Ok(None));
        assert_eq!(config_env::render_str("${UNCLOSED", &lookup), Ok(None));
    }

    #[test]
    fn a_value_holding_a_reference_is_not_expanded_again() {
        let lookup = env(&[("A", "${B}"), ("B", "secret")]);
        assert_eq!(
            config_env::render_str("${A}", &lookup),
            Ok(Some("${B}".to_owned()))
        );
    }

    #[test]
    fn only_the_gears_config_sections_are_rendered() {
        let mut gears: std::collections::HashMap<String, serde_json::Value> = [
            (
                "oidc-authn-plugin".to_owned(),
                serde_json::json!({
                    "config": {"jwt": {"trusted_issuers": [{"issuer": "${OIDC_ISSUER}"}]}},
                    "database": {"dsn": "${PG_PASSWORD}"},
                }),
            ),
            (
                "api-gateway".to_owned(),
                serde_json::json!({"config": {"port": 8080}}),
            ),
        ]
        .into_iter()
        .collect();
        let lookup = env(&[("OIDC_ISSUER", "https://id")]);
        let changed =
            config_env::render_gears_with(&mut gears, None, &lookup).expect("all variables set");
        assert!(changed);
        assert_eq!(
            gears["oidc-authn-plugin"]["config"]["jwt"]["trusted_issuers"][0]["issuer"],
            "https://id"
        );
        // The runtime's own section is left to the runtime.
        assert_eq!(
            gears["oidc-authn-plugin"]["database"]["dsn"],
            "${PG_PASSWORD}"
        );
    }

    #[test]
    fn a_missing_variable_is_named_with_where_it_is_read_and_no_value() {
        let mut gears: std::collections::HashMap<String, serde_json::Value> = [(
            "smtp".to_owned(),
            serde_json::json!({"config": {
                "password": "${SMTP_PASSWORD}",
                "issuer": "${OIDC_ISSUER}",
                "user": "${SMTP_USER}",
            }}),
        )]
        .into_iter()
        .collect();
        let lookup = env(&[("SMTP_USER", "do-not-print-me")]);
        let error = config_env::render_gears_with(
            &mut gears,
            Some(std::path::Path::new("/etc/gearbox/app.yaml")),
            &lookup,
        )
        .expect_err("two variables are not set");
        assert!(error.contains("/etc/gearbox/app.yaml"), "{error}");
        assert!(
            error.contains("not set: OIDC_ISSUER, SMTP_PASSWORD"),
            "{error}"
        );
        assert!(
            error.contains("gears.smtp.config.password reads ${SMTP_PASSWORD}"),
            "{error}"
        );
        assert!(!error.contains("do-not-print-me"), "{error}");
    }

    #[test]
    fn rendered_sections_are_written_as_a_gears_directory() {
        let dir = std::env::temp_dir().join(format!("gearbox-config-env-{}", std::process::id()));
        let gears: std::collections::HashMap<String, serde_json::Value> = [(
            "api-gateway".to_owned(),
            serde_json::json!({"config": {"a": "b"}}),
        )]
        .into_iter()
        .collect();
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("stale.yaml"), "{}").expect("stale file");
        config_env::write_gears_dir(&dir, &gears).expect("write");
        assert!(!dir.join("stale.yaml").exists());
        let written = std::fs::read_to_string(dir.join("api-gateway.yaml")).expect("written");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&written).expect("json"),
            gears["api-gateway"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn host(render_env: bool) -> String {
        templates::render(
            "main.rs",
            templates::TemplateSet::builtin("main.rs").expect("builtin"),
            context! {
                header => "// generated",
                application => "app",
                bin_name => "app",
                gear_count => 1,
                render_env => render_env,
            },
        )
        .expect("render main.rs")
        // A checkout with CRLF line ends renders CRLF.
        .replace("\r\n", "\n")
    }

    fn worker(render_env: bool) -> String {
        templates::render(
            "worker_main.rs",
            templates::TemplateSet::builtin("worker_main.rs").expect("builtin"),
            context! {
                header => "// generated",
                application => "w",
                bin_name => "w",
                gear_count => 1,
                gear_name => "w",
                config_gear_name => "w",
                roles => Vec::<RoleContext>::new(),
                version => "0.1.0",
                render_env => render_env,
            },
        )
        .expect("render worker_main.rs")
        .replace("\r\n", "\n")
    }

    #[test]
    fn an_entry_point_substitutes_variables_only_when_its_gears_read_some() {
        let plain = host(false);
        assert!(!plain.contains("config_env"), "{plain}");
        assert!(plain.contains("mod registered_gears;\n\nuse std::path::PathBuf;"));
        assert!(plain.contains("        return Ok(());\n    }\n\n    match cli.command"));

        let rendering = host(true);
        assert!(rendering.contains("mod registered_gears;\nmod config_env;\n\nuse"));
        // After the flags that print the configuration, before it runs.
        let render_at = rendering.find("config_env::render_gears").unwrap();
        assert!(rendering.find("dump_gears_config_json {").unwrap() < render_at);
        assert!(render_at < rendering.find("match cli.command").unwrap());
        assert!(rendering.contains(".map_err(anyhow::Error::msg)?;\n\n    match cli.command"));
    }

    #[test]
    fn a_worker_renders_before_its_runtime_starts() {
        let plain = worker(false);
        assert!(!plain.contains("config_env"), "{plain}");
        assert!(plain.contains("#[tokio::main]\nasync fn main() -> Result<()> {"));
        assert!(plain.contains("    }\n\n    // `..Default::default()` is load-bearing"));

        let rendering = worker(true);
        assert!(!rendering.contains("#[tokio::main]"), "{rendering}");
        assert!(rendering.contains("fn main() -> Result<()> {"));
        assert!(rendering.contains("APP__GEARS_DIR"));
        assert!(
            rendering
                .contains("async fn run(cli: Cli) -> Result<()> {\n    // `..Default::default()`")
        );
        assert!(rendering.contains(".block_on(run(cli))"));
    }
}
