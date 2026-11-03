//! A product's `FrontX` frontend generates as the shell's workspace with the
//! picked micro-frontends mounted in it, their `FrontX` pins set to the shell's
//! and recorded, and the same UI workload the `ui` block ships. What a
//! package's own `frontx.gdl` declares reaches the lock and is checked.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]
#[path = "support/resolve_fixtures.rs"]
mod fixtures;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gearbox_engine::TemplateSet;
use gearbox_engine::generate::GenerateInput;
use gearbox_ir::frontx::{FrontxLibrary, FrontxMfe, ProductFrontx};
use gearbox_ir::{DiagnosticCode, Diagnostics, Discovery, FileSet, ProfileId, RelPath, RuntimeCap};
use serde_json::{Value, json};

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "gearbox-frontx-generate-{name}-{}",
            std::process::id()
        ));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

fn write(path: &Path, body: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn json_file(path: &Path, value: &Value) {
    write(path, serde_json::to_vec_pretty(value).unwrap());
}

/// A `FrontX` template shell: a workspace that mounts `src-app/mfe_packages/*`,
/// carries `@gears-frontx/react` of its own, and ships an example
/// micro-frontend and a shared library beside it.
fn shell(dir: &Path) {
    json_file(
        &dir.join("package.json"),
        &json!({"name": "@gears-frontx/frontx-template-shell", "version": "0.1.0-alpha.6",
            "workspaces": ["packages/*", "src-app/mfe_packages/*"],
            "scripts": {"build": "npm run build:mfes && vite build"},
            "dependencies": {"@gears-frontx/mfes": "0.4.0", "@gears-frontx/api": ">=0.2.0-0"}}),
    );
    json_file(
        &dir.join("packages/react/package.json"),
        &json!({"name": "@gears-frontx/react", "version": "0.2.0-alpha.15"}),
    );
    write(&dir.join("src-app/main.tsx"), "export {};\n");
    write(&dir.join("Dockerfile"), "FROM the-shells-own\n");
    write(&dir.join("node_modules/x/index.js"), "never copied\n");
    json_file(
        &dir.join("src-app/mfe_packages/demo-mfe/package.json"),
        &json!({"name": "@gears-frontx/demo-mfe", "version": "0.1.0"}),
    );
    json_file(
        &dir.join("src-app/mfe_packages/demo-mfe/mfe.json"),
        &json!({"extensions": []}),
    );
    json_file(
        &dir.join("src-app/mfe_packages/shared/package.json"),
        &json!({"name": "@demo/shared", "version": "0.1.0"}),
    );
}

/// A micro-frontend written against older `FrontX` than the shell carries.
fn mfe(dir: &Path) {
    json_file(
        &dir.join("package.json"),
        &json!({"name": "@demo/people-mfe", "version": "0.2.0",
            "dependencies": {"@gears-frontx/react": "0.2.0-alpha.0", "@gears-frontx/ui-kit": "0.4.10"},
            "peerDependencies": {"@gears-frontx/mfes": "*"}}),
    );
    json_file(
        &dir.join("mfe.json"),
        &json!({"extensions": [{"id": "people", "domain": "screen", "route": "/people"}]}),
    );
    write(&dir.join("src/lifecycle.tsx"), "export {};\n");
}

/// A library the micro-frontends share, written for another shell than the
/// template: the template has its own `shared`, which this one replaces.
fn library(dir: &Path) {
    json_file(
        &dir.join("package.json"),
        &json!({"name": "@demo/shared", "version": "0.3.0",
            "dependencies": {"@gears-frontx/react": "0.2.0-alpha.0"}}),
    );
    write(&dir.join("src/index.ts"), "export const shared = 1;\n");
}

fn generated(tmp: &Tmp, requires: &[&str], libraries: &[FrontxLibrary]) -> Result<FileSet, String> {
    generate_with(tmp, requires, libraries, &[], false).map(|(files, _)| files)
}

/// The same product, with `described` files (path under the product
/// directory, contents) written over the fixtures -- a package's `frontx.gdl`
/// -- and, with `idp`, a gear that signs the product's users in.
fn generate_with(
    tmp: &Tmp,
    requires: &[&str],
    libraries: &[FrontxLibrary],
    described: &[(&str, &str)],
    idp: bool,
) -> Result<(FileSet, Diagnostics), String> {
    let product_dir = tmp.0.join("product");
    shell(&product_dir.join("template-shell"));
    mfe(&product_dir.join("screens/people-mfe"));
    library(&product_dir.join("screens/shared"));
    for (path, body) in described {
        write(&product_dir.join(path), body);
    }

    let deps: &[&str] = if idp {
        &["authn-resolver", "oidc-authn-plugin"]
    } else {
        &["authn-resolver"]
    };
    let mut gateway = fixtures::gear_with_caps(
        "api-gateway",
        &[RuntimeCap::Rest, RuntimeCap::RestHost],
        deps,
    );
    gateway.serves.push(gearbox_ir::EndpointDecl {
        name: "rest".to_owned(),
        config_key: Some("bind_addr".to_owned()),
        default_port: Some(8087),
        via: None,
    });
    let authn = fixtures::descriptor("authn-resolver");
    let mut gears = vec![gateway, authn];
    if idp {
        gears.push(fixtures::descriptor("oidc-authn-plugin"));
    }
    let catalogue = fixtures::catalogue_of(gears);

    let mut intent = fixtures::kubernetes(&["api-gateway"], Discovery::Static);
    let prod = ProfileId::new("prod").unwrap();
    if let Some(gearbox_ir::DeploymentProfileDecl::Kubernetes { development, .. }) =
        intent.profiles.get_mut(&prod)
    {
        *development = true;
    }
    intent.frontend = Some(ProductFrontx {
        shell: "template-shell".into(),
        source: None,
        mfes: vec![FrontxMfe {
            package: "screens/people-mfe".into(),
            source: None,
            requires: requires
                .iter()
                .map(|id| fixtures::gid(id))
                .collect::<BTreeSet<_>>(),
        }],
        libraries: libraries.to_vec(),
        entry_points: BTreeMap::from([("Portal".to_owned(), "/".to_owned())]),
    });
    let resolution = gearbox_engine::resolve::resolve(&catalogue, &intent, &prod);
    if resolution.has_errors() {
        return Err(format!("{:?}", resolution.diagnostics));
    }
    let lock = gearbox_engine::resolve::product::assemble(
        &catalogue,
        &intent,
        &resolution,
        BTreeMap::new(),
    );
    let source_roots = BTreeMap::from([(fixtures::source(), tmp.0.join("source"))]);
    gearbox_engine::generate(&GenerateInput {
        lock: &lock,
        source_roots: &source_roots,
        out_root: &tmp.0.join("out"),
        templates: TemplateSet::new(),
        product_dir: Some(&product_dir),
        catalogue: Some(&catalogue),
        registry: None,
    })
    .map(|out| (out.files, out.diagnostics))
    .map_err(|e| e.to_string())
}

fn text<'a>(files: &'a FileSet, path: &str) -> &'a str {
    files
        .get(&RelPath::new(path).unwrap())
        .unwrap_or_else(|| panic!("no generated file at `{path}`"))
        .as_text()
        .unwrap()
}

fn has(files: &FileSet, path: &str) -> bool {
    files.get(&RelPath::new(path).unwrap()).is_some()
}

#[test]
fn a_frontx_frontend_mounts_its_micro_frontends_in_the_shell() {
    let tmp = Tmp::new("mount");
    let files = generated(&tmp, &["api-gateway"], &[]).unwrap();

    // The shell, without its own example, install output or root Dockerfile;
    // its shared library stays.
    text(&files, "frontend/package.json");
    text(&files, "frontend/src-app/main.tsx");
    text(&files, "frontend/src-app/mfe_packages/shared/package.json");
    assert!(!has(
        &files,
        "frontend/src-app/mfe_packages/demo-mfe/package.json"
    ));
    assert!(!has(&files, "frontend/node_modules/x/index.js"));
    assert!(!text(&files, "frontend/Dockerfile").contains("the-shells-own"));

    // The picked micro-frontend, under its directory name, its exact FrontX
    // pin set to the shell's; a package the shell does not carry and a peer
    // range are left as written.
    text(
        &files,
        "frontend/src-app/mfe_packages/people-mfe/src/lifecycle.tsx",
    );
    let manifest: Value = serde_json::from_str(text(
        &files,
        "frontend/src-app/mfe_packages/people-mfe/package.json",
    ))
    .unwrap();
    assert_eq!(
        manifest["dependencies"]["@gears-frontx/react"],
        "0.2.0-alpha.15"
    );
    assert_eq!(manifest["dependencies"]["@gears-frontx/ui-kit"], "0.4.10");
    assert_eq!(manifest["peerDependencies"]["@gears-frontx/mfes"], "*");

    let lock: Value = serde_json::from_str(text(&files, "frontend/frontx.lock.json")).unwrap();
    assert_eq!(lock["shell"]["name"], "@gears-frontx/frontx-template-shell");
    assert_eq!(
        lock["shell"]["carries"]["@gears-frontx/react"],
        "0.2.0-alpha.15"
    );
    assert_eq!(lock["shell"]["carries"]["@gears-frontx/mfes"], "0.4.0");
    // A range says what the shell accepts, not what it carries.
    assert!(lock["shell"]["carries"].get("@gears-frontx/api").is_none());
    assert_eq!(lock["entryPoints"]["Portal"], "/");
    let people = &lock["mfes"][0];
    assert_eq!(people["directory"], "people-mfe");
    assert_eq!(people["extensions"][0]["route"], "/people");
    assert_eq!(
        people["aligned"],
        json!([{"package": "@gears-frontx/react", "from": "0.2.0-alpha.0", "to": "0.2.0-alpha.15"}])
    );

    // The UI workload, under the umbrella's `ui` values.
    let nginx = text(&files, "frontend/nginx.conf");
    assert!(nginx.contains("location /cf/"), "{nginx}");
    assert!(
        nginx.contains("proxy_pass http://api-gateway:8087;"),
        "{nginx}"
    );
    let deployment = text(&files, "helm/fixture/templates/frontend-deployment.yaml");
    assert!(
        deployment.contains("name: fixture-frontend"),
        "{deployment}"
    );
    let values = text(&files, "helm/fixture/values.yaml");
    assert!(
        values.contains("image: gbx-fixture-frontend:0.0.0"),
        "{values}"
    );
}

#[test]
fn a_micro_frontend_that_requires_an_absent_gear_is_refused_at_resolve() {
    let tmp = Tmp::new("absent");
    let err = generated(&tmp, &["studio-user"], &[]).unwrap_err();
    assert!(err.contains("requires backend `studio-user`"), "{err}");
}

#[test]
fn a_library_the_product_brings_replaces_the_shells_and_is_aligned() {
    let tmp = Tmp::new("library");
    let shared = FrontxLibrary {
        package: "screens/shared".into(),
        source: None,
    };
    let files = generated(&tmp, &["api-gateway"], &[shared]).unwrap();

    text(&files, "frontend/src-app/mfe_packages/shared/src/index.ts");
    let manifest: Value = serde_json::from_str(text(
        &files,
        "frontend/src-app/mfe_packages/shared/package.json",
    ))
    .unwrap();
    assert_eq!(manifest["version"], "0.3.0");
    assert_eq!(
        manifest["dependencies"]["@gears-frontx/react"],
        "0.2.0-alpha.15"
    );

    let lock: Value = serde_json::from_str(text(&files, "frontend/frontx.lock.json")).unwrap();
    assert_eq!(lock["libraries"][0]["name"], "@demo/shared");
    assert_eq!(lock["libraries"][0]["directory"], "shared");
    assert_eq!(
        lock["libraries"][0]["aligned"],
        json!([{"package": "@gears-frontx/react", "from": "0.2.0-alpha.0", "to": "0.2.0-alpha.15"}])
    );
}

#[test]
fn a_micro_frontend_is_not_a_library() {
    let tmp = Tmp::new("not-library");
    let people = FrontxLibrary {
        package: "screens/people-mfe".into(),
        source: None,
    };
    let err = generated(&tmp, &["api-gateway"], &[people]).unwrap_err();
    assert!(err.contains("list it under `mfes`"), "{err}");
}

fn lock_of(files: &FileSet) -> Value {
    serde_json::from_str(text(files, "frontend/frontx.lock.json")).unwrap()
}

const SHELL_GDL: &str = "template-shell/frontx.gdl";
const MFE_GDL: &str = "screens/people-mfe/frontx.gdl";

#[test]
fn what_the_packages_declare_reaches_the_lock() {
    let tmp = Tmp::new("described");
    let (files, diagnostics) = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[
            (
                SHELL_GDL,
                "gdl_version = \"1.5\"\nshell(signs_in = True, runtime_env = \"public/env.js\")\n",
            ),
            ("template-shell/public/env.js", "window.env = {};\n"),
            (MFE_GDL, "mfe(requires = [\"authn-resolver\"])\n"),
        ],
        false,
    )
    .unwrap();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let lock = lock_of(&files);
    assert_eq!(lock["shell"]["signsIn"], true);
    assert_eq!(lock["shell"]["runtimeEnv"], "public/env.js");
    assert_eq!(lock["shell"]["described"], true);
    // The product's requirement and the package's, together.
    assert_eq!(
        lock["mfes"][0]["requires"],
        json!(["api-gateway", "authn-resolver"])
    );
    assert_eq!(lock["mfes"][0]["described"], true);
    // The descriptions travel with their packages.
    text(&files, "frontend/frontx.gdl");
    text(
        &files,
        "frontend/src-app/mfe_packages/people-mfe/frontx.gdl",
    );
}

#[test]
fn a_gear_a_micro_frontend_declares_must_be_in_the_product() {
    let tmp = Tmp::new("declared-absent");
    let err = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[(MFE_GDL, "mfe(requires = [\"studio-user\"])\n")],
        false,
    )
    .unwrap_err();
    assert!(err.contains("requires absent backend studio-user"), "{err}");
    assert!(err.contains("frontx.gdl"), "{err}");
}

#[test]
fn a_shell_that_does_not_sign_in_is_reported_on_a_product_that_does() {
    let tmp = Tmp::new("no-sign-in");
    let (_, diagnostics) = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[(SHELL_GDL, "shell(signs_in = False)\n")],
        true,
    )
    .unwrap();
    let warned: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::GenFrontxShellNoSignIn)
        .collect();
    assert_eq!(warned.len(), 1, "{diagnostics:?}");
    assert!(
        warned[0].message.contains("oidc-authn-plugin"),
        "{}",
        warned[0].message
    );
}

#[test]
fn a_shell_that_signs_in_or_says_nothing_is_not_reported() {
    for (name, shell_gdl) in [
        ("signs-in", Some("shell(signs_in = True)\n")),
        ("silent", None),
    ] {
        let tmp = Tmp::new(name);
        let files: Vec<_> = shell_gdl
            .map(|body| (SHELL_GDL, body))
            .into_iter()
            .collect();
        let (_, diagnostics) = generate_with(&tmp, &["api-gateway"], &[], &files, true).unwrap();
        assert!(
            !diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::GenFrontxShellNoSignIn),
            "{name}: {diagnostics:?}"
        );
    }
}

#[test]
fn a_shell_that_does_not_sign_in_on_a_product_that_does_not_is_fine() {
    let tmp = Tmp::new("no-idp");
    let (_, diagnostics) = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[(SHELL_GDL, "shell(signs_in = False)\n")],
        false,
    )
    .unwrap();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn a_runtime_env_the_shell_lacks_is_refused() {
    let tmp = Tmp::new("no-env");
    let err = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[(
            SHELL_GDL,
            "shell(signs_in = True, runtime_env = \"public/env.js\")\n",
        )],
        false,
    )
    .unwrap_err();
    assert!(err.contains("public/env.js"), "{err}");
}

#[test]
fn a_package_mounted_as_another_kind_than_it_declares_is_refused() {
    for (name, path, body, expected) in [
        ("mfe-as-library", MFE_GDL, "library()\n", "not `mfe(...)`"),
        ("shell-as-mfe", SHELL_GDL, "mfe()\n", "not `shell(...)`"),
    ] {
        let tmp = Tmp::new(name);
        let err = generate_with(&tmp, &["api-gateway"], &[], &[(path, body)], false).unwrap_err();
        assert!(err.contains(expected), "{name}: {err}");
    }
}

#[test]
fn a_package_description_that_does_not_evaluate_fails_generation() {
    let tmp = Tmp::new("bad-gdl");
    let err = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[(MFE_GDL, "mfe(requires = [\"studio-user\"], screens = [])\n")],
        false,
    )
    .unwrap_err();
    assert!(err.contains("frontx.gdl does not evaluate"), "{err}");
    assert!(err.contains("GBX0106"), "{err}");
}

#[test]
fn a_package_without_a_description_has_said_nothing() {
    let tmp = Tmp::new("undescribed");
    let lock = lock_of(&generated(&tmp, &["api-gateway"], &[]).unwrap());
    assert_eq!(lock["shell"]["signsIn"], Value::Null);
    assert_eq!(lock["shell"]["runtimeEnv"], Value::Null);
    assert_eq!(lock["shell"]["described"], false);
    assert_eq!(lock["mfes"][0]["described"], false);
    assert_eq!(lock["mfes"][0]["requires"], json!(["api-gateway"]));
}
