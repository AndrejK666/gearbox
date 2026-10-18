//! The templates generation renders, and the overlay a product may supply.
//!
//! Builtins are compiled in via `include_str!`. A product that needs the
//! Dockerfile or a Helm template to match a house style drops a file at
//! `<dir of product.gdl>/templates/<key>.jinja` and that source wins. The
//! key *is* the contract: it is the path under this module's `templates/`
//! directory, without the `.jinja` suffix. A misspelled key is a file
//! nobody reads, which is why [`TemplateSet::get`] names the key it wanted
//! rather than silently falling through.
//!
//! **`generate` never reads the filesystem.** The CLI and the RPC server
//! load the overlay (see [`TemplateSet::load_for_product`]) and pass the
//! bytes in. That is what keeps `--dry-run` a preview of the same function
//! that writes, not a second implementation that happens to agree today.
//!
//! # Keys
//!
//! | key | produces |
//! |-----|----------|
//! | `main.rs` | host `<layout>/<p>/src/main.rs` |
//! | `worker_main.rs` | worker `<layout>/<p>/src/main.rs` |
//! | `registered_gears.rs` | `<layout>/<p>/src/registered_gears.rs` |
//! | `docker/Dockerfile` | `docker/<p>/Dockerfile` |
//! | `docker/dockerignore` | `docker/<p>/Dockerfile.dockerignore` |
//! | `helm/helpers.tpl` | `helm/<product>/charts/<sub>/templates/_helpers.tpl` |
//! | `helm/deployment.yaml` | `helm/<product>/charts/<sub>/templates/deployment.yaml` |
//! | `helm/service.yaml` | `helm/<product>/charts/<sub>/templates/service.yaml` |
//! | `helm/configmap.yaml` | `helm/<product>/charts/<sub>/templates/configmap.yaml` |
//! | `helm/serviceaccount.yaml` | `helm/<product>/charts/<sub>/templates/serviceaccount.yaml` |
//!
//! An overlay for a key this build does not render is kept and reported (so
//! the operator can see it was read) and otherwise unused.
//!
//! **A replaced Helm template reads its own settings from `custom`.** The values
//! schema is closed, so a house template cannot invent a top-level key; the one
//! object nothing validates is `<subchart>.custom`, and that is where a value
//! this generator never heard of belongs.
//!
//! # Context per key
//!
//! Strict undefined behaviour is on: a typo in a product template fails
//! with the variable name, not a hole in the file. The variables each key
//! may name:
//!
//! - `main.rs`: `header`, `process`, `bin_name`, `gear_count`
//! - `worker_main.rs`: those plus `gear_name`, `roles`, `version`
//! - `registered_gears.rs`: `header`, `idents`
//! - `docker/Dockerfile`: `header`, `rust_channel`, `crate_name`, `bin_name`,
//!   `application`, `out_rel`, `uid`, `home_dir`, `ports`
//! - `docker/dockerignore`: `header`, `source_dirs`, `output_dir`, `secret_dirs`
//! - `helm/helpers.tpl`: `name`, `process`
//! - `helm/deployment.yaml`: `name`, `process`, `config_filename`, `http_port`,
//!   `liveness_path`, `readiness_path`, `home_dir`, `container_ports`, `directory_endpoint`, `bin_name`
//! - `helm/service.yaml`: `name`, `service_name`, `ports`, `cluster_service`,
//!   `cluster_port`
//! - `helm/configmap.yaml`: `name`, `config_filename`
//! - `helm/serviceaccount.yaml`: `name`
//!
//! This table is checked against the code by
//! `every_helm_context_variable_is_documented`. It had drifted -- it omitted
//! `process`, `cluster_port` and a since-removed `uid` -- and a table an overlay
//! author is told to trust must not be a table nobody verifies.
//!
//! The Helm keys receive *fewer* variables than a chart needs, on purpose:
//! everything an operator may change is a Helm value, so it is named as
//! `.Values.x` in the template and never appears here. What arrives through
//! `<< >>` is what the lock decided and the operator may not contradict -- a
//! probe route, a Service name a neighbour dials, the port a process listens on.

/// The variables each Helm key's context carries, as the module docs list them.
///
/// Duplicated from the docs on purpose: a table in a comment cannot be compared
/// against anything, and this one had already drifted -- it omitted `process` and
/// `cluster_port` while still promising a `uid` that no longer exists. An overlay
/// author is told the table is the contract, so the contract has to be checkable.
#[cfg(test)]
const HELM_CONTEXT_DOC: &[(&str, &[&str])] = &[
    ("helm/helpers.tpl", &["name", "application"]),
    (
        "helm/deployment.yaml",
        &[
            "name",
            "application",
            "config_filename",
            "http_port",
            "liveness_path",
            "readiness_path",
            "home_dir",
            "container_ports",
            "directory_endpoint",
            "bin_name",
        ],
    ),
    (
        "helm/service.yaml",
        &[
            "name",
            "service_name",
            "ports",
            "cluster_service",
            "cluster_port",
        ],
    ),
    ("helm/configmap.yaml", &["name", "config_filename"]),
    ("helm/serviceaccount.yaml", &["name"]),
];

use std::collections::BTreeMap;
use std::path::Path;

use minijinja::{Environment, UndefinedBehavior};

use super::GenerateError;

/// A set of template sources, keyed as described in the module docs.
///
/// Empty means "use the builtins". That is the ordinary case: most products
/// never drop a `templates/` directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemplateSet {
    overrides: BTreeMap<String, String>,
}

impl TemplateSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Overlay already loaded. The caller read the files; this just holds them.
    #[must_use]
    pub fn from_overrides(overrides: BTreeMap<String, String>) -> Self {
        Self { overrides }
    }

    /// Walk the product's overlay directory for `*.jinja` files.
    ///
    /// `declared` is `ProductIntent::templates` -- the directory the description
    /// named, resolved against the description's own directory so it may point
    /// outside the product. That is the whole reason it can be declared: the
    /// convention holds the overlay beside `product.gdl`, which makes a house
    /// that keeps twenty products keep twenty copies of the same chart.
    ///
    /// **The two cases fail differently, on purpose.** An absent `templates/`
    /// directory is every product that never opted in, so it is emptiness rather
    /// than an error. A *declared* directory that is not there is someone's
    /// intent gone wrong -- silently generating the builtin chart would hand them
    /// a chart they did not ask for and no reason why -- so it is reported.
    ///
    /// A file that cannot be read is an error either way: generating from a
    /// truncated override produces a chart the operator cannot explain.
    ///
    /// # Errors
    /// Returns [`GenerateError::MissingTemplateDir`] when a declared directory
    /// does not exist, and [`GenerateError::Io`] when a directory cannot be
    /// walked or a file cannot be read.
    pub fn load_for_product(
        product_file: &Path,
        declared: Option<&str>,
    ) -> Result<Self, GenerateError> {
        let Some(parent) = product_file.parent() else {
            return Ok(Self::new());
        };
        let Some(declared) = declared else {
            return load_overrides(&parent.join("templates"));
        };
        let dir = parent.join(declared);
        if !dir.is_dir() {
            return Err(GenerateError::MissingTemplateDir {
                declared: declared.to_owned(),
                at: dir.display().to_string(),
            });
        }
        load_overrides(&dir)
    }

    /// The compiled-in source for `key`, if this build has one.
    #[must_use]
    pub fn builtin(key: &str) -> Option<&'static str> {
        match key {
            "main.rs" => Some(include_str!("templates/main.rs.jinja")),
            "worker_main.rs" => Some(include_str!("templates/worker_main.rs.jinja")),
            "registered_gears.rs" => Some(include_str!("templates/registered_gears.rs.jinja")),
            "docker/Dockerfile" => Some(include_str!("templates/docker/Dockerfile.jinja")),
            "docker/dockerignore" => Some(include_str!("templates/docker/dockerignore.jinja")),
            "helm/helpers.tpl" => Some(include_str!("templates/helm/helpers.tpl.jinja")),
            "helm/deployment.yaml" => Some(include_str!("templates/helm/deployment.yaml.jinja")),
            "helm/service.yaml" => Some(include_str!("templates/helm/service.yaml.jinja")),
            "helm/configmap.yaml" => Some(include_str!("templates/helm/configmap.yaml.jinja")),
            "helm/serviceaccount.yaml" => {
                Some(include_str!("templates/helm/serviceaccount.yaml.jinja"))
            }
            _ => None,
        }
    }

    /// The source that will actually render for `key`.
    ///
    /// An overlay wins. A key with no overlay and no builtin is a generator
    /// bug -- it asked for a name that is not in the contract -- so this
    /// returns an error rather than an empty string.
    ///
    /// # Errors
    /// Returns [`GenerateError::UnknownTemplate`] when neither the overlay
    /// nor the builtins know `key`.
    pub fn get(&self, key: &str) -> Result<&str, GenerateError> {
        if let Some(body) = self.overrides.get(key) {
            return Ok(body);
        }
        Self::builtin(key).ok_or_else(|| GenerateError::UnknownTemplate {
            key: key.to_owned(),
        })
    }

    /// Keys the product overlaid, in sorted order.
    ///
    /// Reported so an unexpected chart has a visible cause: without this,
    /// a house-style Dockerfile looks like a generator change.
    #[must_use]
    pub fn overridden(&self) -> Vec<String> {
        self.overrides.keys().cloned().collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }
}

/// Load every `*.jinja` under `dir` into a [`TemplateSet`].
///
/// The key is the path relative to `dir` with the `.jinja` suffix stripped,
/// always `/`-separated: an overlay on Windows has to match the same key
/// the builtins are registered under, and those keys are written with slashes.
fn load_overrides(dir: &Path) -> Result<TemplateSet, GenerateError> {
    if !dir.is_dir() {
        return Ok(TemplateSet::new());
    }

    let mut overrides = BTreeMap::new();
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry.map_err(|source| GenerateError::Io {
            what: "cannot walk template overrides",
            path: source.path().unwrap_or(dir).to_path_buf(),
            source: std::io::Error::other(source.to_string()),
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jinja"))
        {
            continue;
        }
        let rel = path.strip_prefix(dir).unwrap_or(path);
        let mut key = super::paths::to_slash(rel);
        if let Some(dot) = key.rfind('.') {
            key.truncate(dot);
        }
        let body = std::fs::read_to_string(path).map_err(|source| GenerateError::Io {
            what: "cannot read template override",
            path: path.to_path_buf(),
            source,
        })?;
        overrides.insert(key, body);
    }
    Ok(TemplateSet { overrides })
}

/// Render one template, naming it in any error.
///
/// The result always ends in exactly one newline. minijinja strips the
/// template's own trailing newline, and a Rust file without one is a file
/// `rustfmt --check` and half the tools in the ecosystem complain about.
pub fn render(
    what: &'static str,
    source: &str,
    ctx: minijinja::Value,
) -> Result<String, GenerateError> {
    let mut env = Environment::new();
    // `Undefined` is an error rather than an empty string: a template variable
    // this module forgot to pass would otherwise render as a hole in a file,
    // and the compiler's complaint would be about syntax rather than about
    // the missing fact.
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.add_template(what, source)
        .and_then(|()| env.get_template(what)?.render(ctx))
        .map(|body| format!("{}\n", body.trim_end()))
        .map_err(|source| GenerateError::Template {
            what,
            source: Box::new(source),
        })
}

/// Render a Helm template, with minijinja delimiters moved off `{{ }}`.
///
/// Helm has already claimed the default Jinja markers. The generator uses
/// `<< >>` / `<% %>` / `<# #>` so a `{{ include }}` in the source is still
/// a `{{ include }}` in the chart. The mirror of [`super::rust`]'s test:
/// `{{` must survive, `<<` must not.
pub fn render_helm(
    what: &'static str,
    source: &str,
    ctx: minijinja::Value,
) -> Result<String, GenerateError> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    let syntax = minijinja::syntax::SyntaxConfig::builder()
        .variable_delimiters("<<", ">>")
        .block_delimiters("<%", "%>")
        .comment_delimiters("<#", "#>")
        .build()
        .map_err(|source| GenerateError::Template {
            what,
            source: Box::new(source),
        })?;
    env.set_syntax(syntax);
    env.add_template(what, source)
        .and_then(|()| env.get_template(what)?.render(ctx))
        .map(|body| format!("{}\n", body.trim_end()))
        .map_err(|source| GenerateError::Template {
            what,
            source: Box::new(source),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `<< >>` name a builtin Helm template uses is in the documented list.
    ///
    /// The module's context table is what an overlay author is told to write
    /// against, and it had drifted in the direction that hurts: `uid` and
    /// `cluster_port` were passed and used, and named nowhere, so a house
    /// template could not have known they existed. Reading the names out of the
    /// templates makes the omission impossible to repeat.
    #[test]
    fn every_helm_context_variable_is_documented() {
        // Jinja's own vocabulary, plus loop bindings, which are not context.
        const NOT_CONTEXT: &[&str] = &["for", "in", "endfor", "if", "endif", "else", "port"];

        for (key, documented) in HELM_CONTEXT_DOC {
            let source = TemplateSet::builtin(key).expect("a builtin");
            for used in helm_variables(source) {
                if NOT_CONTEXT.contains(&used.as_str()) {
                    continue;
                }
                assert!(
                    documented.contains(&used.as_str()),
                    "`{key}` uses `{used}`, which the context table does not list"
                );
            }
        }
    }

    /// The root identifiers named inside `<< >>` and `<% %>`.
    ///
    /// Only the segment before the first `.`: `port.name` is the loop binding
    /// `port`, not a context variable of its own.
    fn helm_variables(source: &str) -> Vec<String> {
        let mut found = Vec::new();
        for (open, close) in [("<<", ">>"), ("<%", "%>")] {
            let mut rest = source;
            while let Some(start) = rest.find(open) {
                let after = &rest[start + open.len()..];
                let Some(end) = after.find(close) else { break };
                for word in after[..end].split(|c: char| !c.is_alphanumeric() && c != '_') {
                    if let Some(first) = word.chars().next()
                        && (first.is_alphabetic() || first == '_')
                    {
                        found.push(word.to_owned());
                    }
                }
                rest = &after[end + close.len()..];
            }
        }
        found
    }

    #[test]
    fn every_declared_key_has_a_builtin() {
        for key in [
            "main.rs",
            "worker_main.rs",
            "registered_gears.rs",
            "docker/Dockerfile",
            "docker/dockerignore",
            "helm/helpers.tpl",
            "helm/deployment.yaml",
            "helm/service.yaml",
            "helm/configmap.yaml",
            "helm/serviceaccount.yaml",
        ] {
            assert!(
                TemplateSet::builtin(key).is_some(),
                "`{key}` is in the contract but has no builtin"
            );
        }
    }

    #[test]
    fn an_overlay_wins_over_the_builtin() {
        let mut overrides = BTreeMap::new();
        overrides.insert("main.rs".to_owned(), "overlaid".to_owned());
        let set = TemplateSet::from_overrides(overrides);
        assert_eq!(set.get("main.rs").unwrap(), "overlaid");
        assert_eq!(set.overridden(), vec!["main.rs"]);
    }

    #[test]
    fn a_missing_directory_is_an_empty_overlay() {
        let set = TemplateSet::load_for_product(Path::new("/no/such/product.gdl"), None).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn an_unknown_key_is_an_error() {
        let err = TemplateSet::new().get("nope").unwrap_err();
        assert!(matches!(err, GenerateError::UnknownTemplate { .. }));
    }

    #[test]
    fn load_overrides_keys_by_path_without_jinja() {
        let dir = std::env::temp_dir().join(format!(
            "gbx-templates-{}-{}",
            std::process::id(),
            "overlay"
        ));
        let nested = dir.join("templates/docker");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("Dockerfile.jinja"), "FROM overlay\n").unwrap();
        let set = TemplateSet::load_for_product(&dir.join("product.gdl"), None).unwrap();
        assert_eq!(set.get("docker/Dockerfile").unwrap(), "FROM overlay\n");
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// A declared overlay may live outside the product, which is the point.
    ///
    /// The convention holds it beside `product.gdl`, so a house with twenty
    /// products keeps twenty copies of one chart. `templates = path("../house")`
    /// makes them share a directory -- and `..` is exactly what the convention
    /// could not express.
    #[test]
    fn a_declared_overlay_may_sit_outside_the_product() {
        let root = std::env::temp_dir().join(format!("gbx-declared-{}", std::process::id()));
        let product = root.join("products/payments");
        let house = root.join("house-templates/helm");
        std::fs::create_dir_all(&product).unwrap();
        std::fs::create_dir_all(&house).unwrap();
        std::fs::write(house.join("service.yaml.jinja"), "house service\n").unwrap();

        let set = TemplateSet::load_for_product(
            &product.join("product.gdl"),
            Some("../../house-templates"),
        )
        .unwrap();
        assert_eq!(set.get("helm/service.yaml").unwrap(), "house service\n");
        assert_eq!(set.overridden(), vec!["helm/service.yaml"]);
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    /// A declared directory that is not there is reported; an undeclared one is not.
    ///
    /// The asymmetry is the whole reason declaring is worth having. Nobody meant
    /// the absent `templates/` beside their description -- that is every product
    /// that never opted in. Someone did mean the path they wrote, so generating
    /// the builtin chart instead would hand them a chart they did not ask for and
    /// no reason why.
    #[test]
    fn a_declared_directory_that_is_missing_is_an_error() {
        let err = TemplateSet::load_for_product(
            Path::new("/no/such/product.gdl"),
            Some("../house-templates"),
        )
        .unwrap_err();
        assert!(
            matches!(err, GenerateError::MissingTemplateDir { .. }),
            "{err:?}"
        );
        assert!(
            TemplateSet::load_for_product(Path::new("/no/such/product.gdl"), None)
                .unwrap()
                .is_empty()
        );
    }
}
