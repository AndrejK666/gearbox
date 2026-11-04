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
        &[(
            MFE_GDL,
            "mfe(requires = [\"studio-user\"], extensions = [])\n",
        )],
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

/// The image a shell with no passport builds: exactly what it was before
/// passports drove it.
const DEFAULT_DOCKERFILE: &str = "FROM node:24.21.0-alpine AS build
WORKDIR /app
COPY . .
# `npm install`, not `npm ci`: it starts from the shell's lock and resolves
# only what the product changed. A FrontX shell builds its own packages, each
# micro-frontend, the manifests it mounts them from, then the site: `npm run
# build` does all four.
RUN npm install --no-audit --no-fund && npm run build
FROM nginxinc/nginx-unprivileged:1.28-alpine
COPY --from=build /app/dist /usr/share/nginx/html
COPY nginx.conf /etc/nginx/nginx.conf
USER 65532:65532
EXPOSE 8080";

#[test]
fn a_shell_without_a_passport_builds_and_serves_as_before() {
    let tmp = Tmp::new("default-image");
    let files = generated(&tmp, &["api-gateway"], &[]).unwrap();
    assert_eq!(
        text(&files, "frontend/Dockerfile").trim_end(),
        DEFAULT_DOCKERFILE
    );
    let nginx = text(&files, "frontend/nginx.conf");
    for line in [
        "location = /env.js { add_header Cache-Control no-store; }",
        "location = /runtime-config.json { add_header Cache-Control no-store; }",
        "# FrontX micro-frontends call the gateway's own paths (`/cf/<gear>/v1`).",
        "location /cf/ {",
        "location / { try_files $uri /index.html; }",
    ] {
        assert!(nginx.contains(line), "{line}\n{nginx}");
    }
    let lock = lock_of(&files);
    assert_eq!(lock["shell"]["apiPrefix"], "/cf/");
    assert_eq!(lock["shell"]["output"], "dist");
    assert_eq!(lock["shell"]["base"], "/");
}

#[test]
fn a_shells_passport_drives_how_it_is_built_and_served() {
    let tmp = Tmp::new("passport-image");
    let (files, diagnostics) = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[(
            SHELL_GDL,
            r#"gdl_version = "1.5"
shell(
    signs_in = False,
    node = "22",
    build = "npm run build",
    output = "site",
    base = "/portal/",
    api_prefix = "/api/",
)
"#,
        )],
        false,
    )
    .unwrap();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let dockerfile = text(&files, "frontend/Dockerfile");
    for line in [
        "FROM node:22-alpine AS build",
        "# The shell's own build command, as its frontx.gdl declares it.",
        "RUN npm install --no-audit --no-fund && npm run build",
        "COPY --from=build /app/site /usr/share/nginx/html/portal",
    ] {
        assert!(dockerfile.contains(line), "{line}\n{dockerfile}");
    }
    let nginx = text(&files, "frontend/nginx.conf");
    for line in [
        "location = /portal/env.js {",
        "location /api/ {",
        "proxy_pass http://api-gateway:8087;",
        "location / { try_files $uri /portal/index.html; }",
    ] {
        assert!(nginx.contains(line), "{line}\n{nginx}");
    }
    assert!(!nginx.contains("location /cf/"), "{nginx}");
    let lock = lock_of(&files);
    assert_eq!(lock["shell"]["apiPrefix"], "/api/");
}

#[test]
fn a_node_range_keeps_the_pinned_image_and_another_manager_installs_its_way() {
    let tmp = Tmp::new("passport-pm");
    let (files, _) = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[(
            SHELL_GDL,
            "shell(signs_in = False, node = \">=24\", package_manager = \"pnpm\")\n",
        )],
        false,
    )
    .unwrap();
    let dockerfile = text(&files, "frontend/Dockerfile");
    assert!(
        dockerfile.contains("FROM node:24.21.0-alpine AS build"),
        "{dockerfile}"
    );
    assert!(
        dockerfile.contains("RUN corepack enable && pnpm install && pnpm run build"),
        "{dockerfile}"
    );
}

/// A full passport for the fixtures, each fact as their manifests say it.
const SHELL_PASSPORT: &str = r#"gdl_version = "1.5"
shell(
    name = "@gears-frontx/frontx-template-shell",
    version = "0.1.0-alpha.6",
    signs_in = False,
    build = "npm run build",
    carries = {
        "@gears-frontx/frontx-template-shell": "0.1.0-alpha.6",
        "@gears-frontx/mfes": "0.4.0",
        "@gears-frontx/react": "0.2.0-alpha.15",
    },
)
"#;
const MFE_PASSPORT: &str = r#"gdl_version = "1.5"
mfe(
    name = "@demo/people-mfe",
    version = "0.2.0",
    frontx = "0.2.0-alpha.0",
    screens = [],
    mounts = ["/people"],
    preview = ["/people"],
)
"#;

fn codes_of(diagnostics: &Diagnostics, code: DiagnosticCode) -> Vec<String> {
    diagnostics
        .iter()
        .filter(|d| d.code == code)
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn a_passport_that_agrees_with_its_manifests_says_nothing_more() {
    let tmp = Tmp::new("passport-agrees");
    let (_, diagnostics) = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[(SHELL_GDL, SHELL_PASSPORT), (MFE_GDL, MFE_PASSPORT)],
        false,
    )
    .unwrap();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn a_passport_whose_identity_pins_or_screens_drifted_is_an_error() {
    let shell = SHELL_PASSPORT
        .replace(
            "0.1.0-alpha.6\",\n    signs_in",
            "0.1.0-alpha.5\",\n    signs_in",
        )
        .replace(
            "\"@gears-frontx/mfes\": \"0.4.0\"",
            "\"@gears-frontx/mfes\": \"0.3.0\"",
        );
    let mfe = MFE_PASSPORT
        .replace("\"@demo/people-mfe\"", "\"@demo/persons-mfe\"")
        .replace("frontx = \"0.2.0-alpha.0\"", "frontx = \"0.2.0-alpha.15\"")
        .replace("screens = []", "screens = [\"People\"]")
        .replace("mounts = [\"/people\"]", "mounts = [\"/persons\"]")
        .replace("preview = [\"/people\"]", "preview = [\"/persons\"]");
    let tmp = Tmp::new("passport-errors");
    let (_, diagnostics) = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[(SHELL_GDL, &shell), (MFE_GDL, &mfe)],
        false,
    )
    .unwrap();
    let errors = codes_of(&diagnostics, DiagnosticCode::GenFrontxPassportMismatch);
    let all = errors.join("\n");
    for expected in [
        "version = \"0.1.0-alpha.5\", its package.json \"0.1.0-alpha.6\"",
        "@gears-frontx/mfes 0.3.0 (it carries 0.4.0)",
        "name = \"@demo/persons-mfe\"",
        "frontx = \"0.2.0-alpha.15\", its package.json pins @gears-frontx/react \"0.2.0-alpha.0\"",
        "screens = [\"People\"], its mfe.json []",
        "mounts = [\"/persons\"], its mfe.json [\"/people\"]",
    ] {
        assert!(all.contains(expected), "{expected}\n{all}");
    }
    assert_eq!(errors.len(), 6, "{all}");
    assert!(diagnostics.has_errors());
    // The preview names a route the passport mounts: consistent with itself.
    assert!(
        codes_of(&diagnostics, DiagnosticCode::GenFrontxPassportDrift).is_empty(),
        "{diagnostics:?}"
    );
}

#[test]
fn a_shell_that_does_not_list_all_it_carries_is_an_error() {
    let shell = SHELL_PASSPORT.replace("        \"@gears-frontx/mfes\": \"0.4.0\",\n", "");
    let tmp = Tmp::new("passport-carries");
    let (_, diagnostics) =
        generate_with(&tmp, &["api-gateway"], &[], &[(SHELL_GDL, &shell)], false).unwrap();
    let errors = codes_of(&diagnostics, DiagnosticCode::GenFrontxPassportMismatch);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("@gears-frontx/mfes 0.4.0 is carried and not listed"),
        "{}",
        errors[0]
    );
}

#[test]
fn a_passport_that_drifted_on_a_softer_fact_is_a_warning() {
    let shell = SHELL_PASSPORT.replace(
        "build = \"npm run build\"",
        "build = \"npm run build:site\", description = \"Another\", package_manager = \"yarn\"",
    );
    let mfe = MFE_PASSPORT.replace(
        "preview = [\"/people\"]",
        "preview = [\"/nobody\"], fixtures = \"preview/fixtures.json\"",
    );
    let tmp = Tmp::new("passport-warnings");
    let (_, diagnostics) = generate_with(
        &tmp,
        &["api-gateway"],
        &[],
        &[(SHELL_GDL, &shell), (MFE_GDL, &mfe)],
        false,
    )
    .unwrap();
    assert!(!diagnostics.has_errors(), "{diagnostics:?}");
    let warnings = codes_of(&diagnostics, DiagnosticCode::GenFrontxPassportDrift).join("\n");
    for expected in [
        "has no `build:site` script",
        "says package_manager = \"yarn\", and the package has no yarn.lock",
        "previews `/nobody`, which it does not mount",
        "names fixtures `preview/fixtures.json`, which the package does not have",
    ] {
        assert!(warnings.contains(expected), "{expected}\n{warnings}");
    }
    // The fixture shell has no description, so there is nothing to disagree with.
    assert!(!warnings.contains("description"), "{warnings}");
}

#[test]
fn a_package_checks_its_own_passport_outside_a_product() {
    let tmp = Tmp::new("check-package");
    let dir = tmp.0.join("people-mfe");
    mfe(&dir);
    write(
        &dir.join("frontx.gdl"),
        MFE_PASSPORT.replace("version = \"0.2.0\"", "version = \"9.9.9\""),
    );
    let (package, diagnostics) = gearbox_engine::generate::check_frontx_package(&dir).unwrap();
    assert!(package.is_some());
    assert_eq!(
        codes_of(&diagnostics, DiagnosticCode::GenFrontxPassportMismatch).len(),
        1,
        "{diagnostics:?}"
    );
}

/// Every `frontx.gdl` in a `FrontX` templates checkout evaluates and agrees
/// with its manifests. Set `GEARBOX_FRONTX_TEMPLATES` to the checkout to run
/// it; without one it says so and passes.
#[test]
fn the_frontx_templates_passports_agree_with_their_manifests() {
    let Some(root) = std::env::var_os("GEARBOX_FRONTX_TEMPLATES").map(PathBuf::from) else {
        eprintln!("skipping: GEARBOX_FRONTX_TEMPLATES is not set");
        return;
    };
    let mut dirs = vec![root.join("template-shell")];
    let mfes = root.join("template-mfe/src-app/mfe_packages");
    let mut entries: Vec<_> = std::fs::read_dir(&mfes)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("frontx.gdl").is_file())
        .collect();
    entries.sort();
    dirs.extend(entries);
    for dir in dirs {
        let (package, diagnostics) = gearbox_engine::generate::check_frontx_package(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        assert!(package.is_some(), "{}: no frontx.gdl", dir.display());
        assert!(diagnostics.is_empty(), "{}: {diagnostics:?}", dir.display());
        eprintln!(
            "{}: {}",
            dir.display(),
            package
                .unwrap()
                .identity()
                .name
                .as_deref()
                .unwrap_or("(unnamed)")
        );
    }
}
