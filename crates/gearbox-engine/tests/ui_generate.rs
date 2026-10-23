//! A product's UI generates from templates: the build files, the nginx front and
//! the Kubernetes workload beside the other Helm templates.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]
#[path = "support/resolve_fixtures.rs"]
mod fixtures;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gearbox_engine::TemplateSet;
use gearbox_engine::generate::GenerateInput;
use gearbox_ir::{Discovery, FileSet, ProfileId, RelPath, RuntimeCap};

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("gearbox-ui-generate-{name}-{}", std::process::id()));
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

/// A UI package on disk: `package.json` and the source files its exports name.
fn package(dir: &Path, manifest: &serde_json::Value) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("package.json"),
        serde_json::to_vec_pretty(manifest).unwrap(),
    )
    .unwrap();
    for entry in manifest["exports"].as_object().unwrap().values() {
        let path = dir.join(entry.as_str().unwrap().trim_start_matches("./"));
        std::fs::write(path, "export {};\n").unwrap();
    }
}

fn generated(tmp: &Tmp, templates: TemplateSet) -> FileSet {
    let product_dir = tmp.0.join("product");
    let source_root = tmp.0.join("source");
    let peer = serde_json::json!({"@gears/ui-sdk": "^1.0.0"});
    package(
        &product_dir.join("ui/sdk"),
        &serde_json::json!({"name": "@gears/ui-sdk", "version": "1.0.0",
            "gearsUi": {"apiVersion": 1, "kind": "sdk"}, "exports": {".": "./src/index.ts"}}),
    );
    package(
        &product_dir.join("ui/shell"),
        &serde_json::json!({"name": "@gears/ui-shell", "version": "1.0.0",
            "gearsUi": {"apiVersion": 1, "kind": "shell"}, "peerDependencies": peer,
            "dependencies": {"react": "19.0.0"}, "devDependencies": {"vite": "7.0.0"},
            "exports": {".": "./src/index.tsx", "./theme.css": "./src/theme.css"}}),
    );
    package(
        &source_root.join("front/ui"),
        &serde_json::json!({"name": "@gears/front-ui", "version": "1.0.0",
            "gearsUi": {"apiVersion": 1, "kind": "module"}, "peerDependencies": peer,
            "exports": {"./plugin": "./src/plugin.tsx"}}),
    );

    let mut front = fixtures::gear_with_caps("front", &[RuntimeCap::Rest], &[]);
    front.package.path = RelPath::new("front").unwrap();
    front.ui = Some(gearbox_ir::ui::GearUi {
        package: RelPath::new("front/ui").unwrap(),
        requires: BTreeSet::new(),
    });
    // A process serving REST needs a tenant authenticator (GBX0417).
    let mut gateway = fixtures::gear_with_caps(
        "api-gateway",
        &[RuntimeCap::Rest, RuntimeCap::RestHost],
        &["authn-resolver"],
    );
    // The listener nginx proxies `/api/` to.
    gateway.serves.push(gearbox_ir::EndpointDecl {
        name: "rest".to_owned(),
        config_key: Some("bind_addr".to_owned()),
        default_port: Some(8087),
        via: None,
    });
    let authn = fixtures::descriptor("authn-resolver");
    let catalogue = fixtures::catalogue_of(vec![gateway, authn, front]);

    let mut intent = fixtures::kubernetes(&["api-gateway", "front"], Discovery::Static);
    let prod = ProfileId::new("prod").unwrap();
    if let Some(gearbox_ir::DeploymentProfileDecl::Kubernetes { development, .. }) =
        intent.profiles.get_mut(&prod)
    {
        *development = true;
    }
    intent.ui = Some(gearbox_ir::ui::ProductUi {
        entry_points: BTreeMap::default(),
        shell: "ui/shell".into(),
        sdk: "ui/sdk".into(),
        source: None,
        npm_lock: None,
        modules: BTreeSet::from([fixtures::gid("front")]),
    });
    let resolution = gearbox_engine::resolve::resolve(&catalogue, &intent, &prod);
    assert!(!resolution.has_errors(), "{:?}", resolution.diagnostics);
    let lock = gearbox_engine::resolve::product::assemble(
        &catalogue,
        &intent,
        &resolution,
        BTreeMap::new(),
    );

    let source_roots = BTreeMap::from([(fixtures::source(), source_root)]);
    gearbox_engine::generate(&GenerateInput {
        lock: &lock,
        source_roots: &source_roots,
        out_root: &tmp.0.join("out"),
        templates,
        product_dir: Some(&product_dir),
        catalogue: Some(&catalogue),
        registry: None,
    })
    .expect("generation succeeds")
    .files
}

fn text<'a>(files: &'a FileSet, path: &str) -> &'a str {
    files
        .get(&RelPath::new(path).unwrap())
        .unwrap_or_else(|| panic!("no generated file at `{path}`"))
        .as_text()
        .unwrap()
}

#[test]
fn a_ui_generates_every_file_from_a_template() {
    let tmp = Tmp::new("builtin");
    let files = generated(&tmp, TemplateSet::new());

    let registry = text(&files, "ui/src/module-registry.ts");
    assert!(
        registry.contains(r#"import { plugin as module0 } from "@gears/front-ui/plugin";"#),
        "{registry}"
    );
    assert!(
        registry.contains("export const modules = [module0];"),
        "{registry}"
    );
    let main = text(&files, "ui/src/main.tsx");
    assert!(
        main.contains(r#"import {ProductShell} from "@gears/ui-shell";"#),
        "{main}"
    );
    assert!(
        main.contains("import '@gears/ui-shell/theme.css';"),
        "{main}"
    );
    for path in [
        "ui/index.html",
        "ui/tsconfig.json",
        "ui/vite.config.ts",
        "ui/.dockerignore",
        "ui/Dockerfile",
    ] {
        text(&files, path);
    }
    let nginx = text(&files, "ui/nginx.conf");
    assert!(nginx.contains("proxy_pass http://api-gateway:"), "{nginx}");

    let deployment = text(&files, "helm/fixture/templates/ui-deployment.yaml");
    assert!(deployment.contains("name: fixture-ui"), "{deployment}");
    assert!(
        deployment.contains("image: {{ .Values.ui.image | quote }}"),
        "{deployment}"
    );
    assert!(
        deployment.contains("imagePullPolicy: {{ .Values.ui.pullPolicy }}"),
        "{deployment}"
    );
    let service = text(&files, "helm/fixture/templates/ui-service.yaml");
    assert!(service.contains("name: fixture-ui"), "{service}");

    let values = text(&files, "helm/fixture/values.yaml");
    assert!(values.contains("image: gbx-fixture-ui:0.0.0"), "{values}");
    assert!(values.contains("pullPolicy: IfNotPresent"), "{values}");
    let schema = text(&files, "helm/fixture/values.schema.json");
    assert!(schema.contains("\"pullPolicy\""), "{schema}");

    for entry in &files {
        if let Some(body) = entry.as_text() {
            assert!(
                !body.contains("demo-ui") && !body.contains("demoUiImage"),
                "{}",
                entry.path.as_str()
            );
        }
    }
}

#[test]
fn a_product_template_replaces_the_builtin_ui_file() {
    let tmp = Tmp::new("overlay");
    let templates = TemplateSet::from_overrides(BTreeMap::from([(
        "ui/Dockerfile".to_owned(),
        "FROM house/nginx:1\n".to_owned(),
    )]));
    let files = generated(&tmp, templates);
    assert_eq!(text(&files, "ui/Dockerfile"), "FROM house/nginx:1\n");
}
