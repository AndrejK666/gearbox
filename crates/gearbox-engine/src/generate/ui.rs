//! Build-time UI packages. JSON and source files are read; JavaScript is never run.
use std::{collections::BTreeSet, path::Path};

use gearbox_ir::{FileEntry, FileKind, Ownership, RelPath};
use serde_json::{Value, json};

use super::{GenerateError, GenerateInput};

fn invalid(message: impl Into<String>) -> GenerateError {
    GenerateError::Ui(message.into())
}

fn file(path: &str, bytes: impl Into<Vec<u8>>) -> Result<FileEntry, GenerateError> {
    let bytes = bytes.into();
    let binary_extension = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "woff"
                    | "woff2"
                    | "ttf"
                    | "otf"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "webp"
                    | "gif"
                    | "ico"
                    | "bin"
            )
        });
    let kind = if !binary_extension && std::str::from_utf8(&bytes).is_ok() {
        FileKind::Text
    } else {
        FileKind::Binary
    };
    Ok(FileEntry {
        path: RelPath::new(path).map_err(|e| invalid(e.to_string()))?,
        bytes,
        kind,
        ownership: Ownership::Generated,
    })
}

struct Package {
    name: String,
    manifest: Value,
    files: Vec<FileEntry>,
    hash: String,
}

fn npm_name(name: &str) -> bool {
    let scoped = name.starts_with('@');
    let parts: Vec<_> = name.strip_prefix('@').unwrap_or(name).split('/').collect();
    !name.is_empty()
        && name.len() <= 214
        && parts.len() == if scoped { 2 } else { 1 }
        && parts.iter().all(|part| {
            !part.is_empty()
                && !part.starts_with(['.', '_'])
                && !matches!(*part, "node_modules" | "favicon.ico")
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        })
}

fn package(directory: &Path, folder: &str, role: &str) -> Result<Package, GenerateError> {
    let directory = directory
        .canonicalize()
        .map_err(|e| invalid(format!("UI package {}: {e}", directory.display())))?;
    let bytes = std::fs::read(directory.join("package.json"))
        .map_err(|e| invalid(format!("{}: {e}", directory.display())))?;
    let manifest: Value =
        serde_json::from_slice(&bytes).map_err(|e| invalid(format!("UI package.json: {e}")))?;
    let name = manifest["name"]
        .as_str()
        .filter(|s| npm_name(s))
        .ok_or_else(|| invalid("UI package.json needs a valid npm name"))?
        .to_owned();
    let version = manifest["version"]
        .as_str()
        .ok_or_else(|| invalid(format!("{name} needs version")))?;
    if version.split('.').count() != 3
        || !version.split('.').all(|n| {
            !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
                && n.parse::<u32>().is_ok()
                && (n == "0" || !n.starts_with('0'))
        })
    {
        return Err(invalid(format!(
            "{name}: use a numeric major.minor.patch package version"
        )));
    }
    if manifest["gearsUi"]["apiVersion"] != 1 || manifest["gearsUi"]["kind"] != role {
        return Err(invalid(format!(
            "{name}: expected GearUiPlugin API 1 and kind {role}"
        )));
    }
    let exports = manifest["exports"]
        .as_object()
        .ok_or_else(|| invalid(format!("{name}: string exports required")))?;
    let primary = if role == "module" { "./plugin" } else { "." };
    if !exports.contains_key(primary) || (role == "shell" && !exports.contains_key("./theme.css")) {
        return Err(invalid(format!("{name}: missing required exports")));
    }
    let mut entry_paths = Vec::new();
    for (key, value) in exports {
        if key != "." && !key.starts_with("./") {
            return Err(invalid(format!("{name}: invalid export name")));
        }
        let entry = value
            .as_str()
            .ok_or_else(|| invalid(format!("{name}: export must be a string")))?;
        let entry_path = entry
            .strip_prefix("./")
            .and_then(|s| RelPath::new(s).ok())
            .filter(|p| p.as_str().starts_with("src/"))
            .ok_or_else(|| invalid(format!("{name}: export must point into src/")))?;
        let actual = directory
            .join(entry_path.as_str())
            .canonicalize()
            .map_err(|e| invalid(format!("{name} export: {e}")))?;
        if !actual.starts_with(&directory) || !actual.is_file() {
            return Err(invalid(format!("{name}: export escapes its package")));
        }
        entry_paths.push(entry_path);
    }
    let mut files = Vec::new();
    let mut hasher = blake3::Hasher::new();
    let mut paths = Vec::new();
    for entry in walkdir::WalkDir::new(&directory)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !matches!(
                    e.file_name().to_str(),
                    Some("node_modules" | "dist" | ".git" | ".gearbox")
                )
        })
    {
        let entry = entry.map_err(|e| invalid(e.to_string()))?;
        if entry.file_type().is_symlink() {
            return Err(invalid(format!(
                "{name}: package symlinks are not supported"
            )));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&directory)
            .map_err(|e| invalid(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        // Deliberate package boundary: no dotfiles, credentials, local caches or private assets.
        if relative == "package.json" || relative == "LICENSE" || relative.starts_with("src/") {
            if relative.split('/').any(|part| part.starts_with('.')) {
                continue;
            }
            paths.push((relative, entry.path().to_path_buf()));
        }
    }
    paths.sort();
    if entry_paths.iter().any(|entry_path| {
        !paths
            .iter()
            .any(|(relative, _)| relative == entry_path.as_str())
    }) {
        return Err(invalid(format!(
            "{name}: UI exports must point into the copied src/ tree"
        )));
    }
    for (relative, path) in paths {
        let bytes = std::fs::read(path).map_err(|e| invalid(e.to_string()))?;
        hasher.update(&(relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
        files.push(file(&format!("ui/packages/{folder}/{relative}"), bytes)?);
    }
    Ok(Package {
        name,
        manifest,
        files,
        hash: hasher.finalize().to_hex().to_string(),
    })
}

pub(super) fn files(input: &GenerateInput<'_>) -> Result<Vec<FileEntry>, GenerateError> {
    let Some(ui) = &input.lock.ui else {
        return Ok(Vec::new());
    };
    // The UI's values live under the umbrella's `ui` key, beside each subchart's.
    if input.lock.applications.iter().any(|application| {
        application
            .subchart
            .as_deref()
            .unwrap_or(application.name.as_str())
            == "ui"
    }) {
        return Err(invalid(
            "an application named `ui` would share the UI's Helm values key",
        ));
    }
    let base = if let Some(source) = &ui.source {
        let root = input
            .source_roots
            .get(source)
            .ok_or_else(|| invalid(format!("UI package source {source} is unavailable")))?;
        for path in [&ui.sdk, &ui.shell] {
            RelPath::new(path).map_err(|e| invalid(e.to_string()))?;
        }
        root.as_path()
    } else {
        input
            .product_dir
            .ok_or_else(|| invalid("UI generation needs the product description directory"))?
    };
    let mut packages = vec![
        package(&base.join(&ui.sdk), "sdk", "sdk")?,
        package(&base.join(&ui.shell), "shell", "shell")?,
    ];
    let sdk_name = packages[0].name.clone();
    let sdk_major = packages[0].manifest["version"]
        .as_str()
        .and_then(|v| v.split('.').next())
        .unwrap_or("")
        .to_owned();
    if sdk_major == "0" {
        return Err(invalid("UI SDK major version must be at least 1"));
    }
    let shell_name = packages[1].name.clone();
    let mut imports = Vec::new();
    for (index, (id, module)) in ui.modules.iter().enumerate() {
        let root = input
            .source_roots
            .get(&module.source)
            .ok_or_else(|| invalid(format!("UI module {id} has an unavailable source")))?;
        if !input.lock.gears.contains_key(id)
            || module
                .module
                .requires
                .iter()
                .any(|id| !input.lock.gears.contains_key(id))
        {
            return Err(invalid(format!("UI module {id} has an absent backend")));
        }
        let package = package(
            &root.join(module.module.package.as_str()),
            id.as_str(),
            "module",
        )?;
        imports.push(minijinja::context! {
            index => index,
            from => serde_json::to_string(&format!("{}/plugin", package.name))
                .map_err(|e| invalid(e.to_string()))?,
        });
        packages.push(package);
    }
    let mut names = BTreeSet::new();
    for package in &packages {
        if !names.insert(&package.name) {
            return Err(invalid(format!("duplicate UI package {}", package.name)));
        }
        if package.name != sdk_name {
            let required = package.manifest["peerDependencies"][&sdk_name]
                .as_str()
                .unwrap_or("");
            if required != format!("^{sdk_major}.0.0") {
                return Err(invalid(format!(
                    "{} must declare peerDependency {sdk_name}: ^{sdk_major}.0.0",
                    package.name
                )));
            }
        }
    }
    let root = json!({"name":format!("{}-ui",input.lock.product.id),"version":input.lock.product.version,"private":true,"type":"module",
        "workspaces":["packages/*"],"scripts":{"build":"tsc --noEmit && vite build","dev":"vite --host 127.0.0.1","test":"vitest run"},
        "dependencies":packages[1].manifest["dependencies"],"devDependencies":packages[1].manifest["devDependencies"]});
    let npm_lock = if let Some(path) = &ui.npm_lock {
        let relative = RelPath::new(path).map_err(|e| invalid(e.to_string()))?;
        let product_dir = input
            .product_dir
            .ok_or_else(|| invalid("npm_lock needs the product directory"))?
            .canonicalize()
            .map_err(|e| invalid(e.to_string()))?;
        let actual = product_dir
            .join(relative.as_str())
            .canonicalize()
            .map_err(|e| invalid(format!("npm lock: {e}")))?;
        if !actual.starts_with(&product_dir) {
            return Err(invalid("npm lock escapes the product directory"));
        }
        let bytes = std::fs::read(actual).map_err(|e| invalid(e.to_string()))?;
        let npm: Value =
            serde_json::from_slice(&bytes).map_err(|e| invalid(format!("npm lock JSON: {e}")))?;
        if npm["lockfileVersion"] != 3
            || npm["packages"][""]["name"] != root["name"]
            || npm["packages"][""]["version"] != root["version"]
            || npm["packages"][""]["dependencies"] != root["dependencies"]
            || npm["packages"][""]["devDependencies"] != root["devDependencies"]
        {
            return Err(invalid(
                "npm lock does not match the generated root dependencies; update the tracked npm lock",
            ));
        }
        Some((
            json!({"path":path,"blake3":blake3::hash(&bytes).to_hex().to_string()}),
            bytes,
        ))
    } else {
        None
    };
    let lock = json!({"schemaVersion":1,"productLockHash":input.lock.product.lock_hash,"entryPoints":ui.entry_points,"npmLock":npm_lock.as_ref().map(|(metadata,_)|metadata),
        "packages":packages.iter().map(|p|json!({"name":p.name,"version":p.manifest["version"],"exports":p.manifest["exports"],"blake3":p.hash})).collect::<Vec<_>>()});
    let render = |key: &str, ctx: minijinja::Value| -> Result<Vec<u8>, GenerateError> {
        Ok(super::templates::render("UI", input.templates.get(key)?, ctx)?.into_bytes())
    };
    let shell = serde_json::to_string(&shell_name).map_err(|e| invalid(e.to_string()))?;
    let mut out = vec![
        file(
            "ui/package.json",
            serde_json::to_vec_pretty(&root).map_err(|e| invalid(e.to_string()))?,
        )?,
        file(
            "ui/ui.lock.json",
            serde_json::to_vec_pretty(&lock).map_err(|e| invalid(e.to_string()))?,
        )?,
        file(
            "ui/src/module-registry.ts",
            render(
                "ui/src/module-registry.ts",
                minijinja::context! { modules => imports },
            )?,
        )?,
        file(
            "ui/src/main.tsx",
            render(
                "ui/src/main.tsx",
                minijinja::context! { shell => shell, shell_name => &shell_name },
            )?,
        )?,
        file(
            "ui/index.html",
            render(
                "ui/index.html",
                minijinja::context! { product_id => &input.lock.product.id },
            )?,
        )?,
        file(
            "ui/tsconfig.json",
            render("ui/tsconfig.json", minijinja::context! {})?,
        )?,
        file(
            "ui/vite.config.ts",
            render("ui/vite.config.ts", minijinja::context! {})?,
        )?,
        file(
            "ui/.dockerignore",
            render("ui/dockerignore", minijinja::context! {})?,
        )?,
        file(
            "ui/Dockerfile",
            render("ui/Dockerfile", minijinja::context! {})?,
        )?,
    ];
    if let Some((_, bytes)) = npm_lock {
        out.push(file("ui/package-lock.json", bytes)?);
    }
    for package in packages {
        out.extend(package.files);
    }
    if input.lock.kubernetes.is_none() {
        return Ok(out);
    }
    let gateway = input
        .lock
        .applications
        .iter()
        .find(|a| a.gears.iter().any(|id| id.as_str() == "api-gateway"))
        .ok_or_else(|| invalid("UI requires an API Gateway application"))?;
    let port = gateway
        .service_port
        .ok_or_else(|| invalid("UI Gateway has no service port"))?;
    out.push(file(
        "ui/nginx.conf",
        render(
            "ui/nginx.conf",
            minijinja::context! { gateway => gateway.name.as_str(), gateway_port => port },
        )?,
    )?);
    // Beside the other Helm templates, rendered into the umbrella chart.
    let product = input.lock.product.id.as_str();
    let helm_ctx = minijinja::context! { name => format!("{product}-ui"), port => UI_PORT };
    for (key, name) in [
        ("helm/ui/deployment.yaml", "ui-deployment.yaml"),
        ("helm/ui/service.yaml", "ui-service.yaml"),
    ] {
        let body =
            super::templates::render_helm("UI chart", input.templates.get(key)?, helm_ctx.clone())?;
        out.push(file(
            &format!("helm/{product}/templates/{name}"),
            body.into_bytes(),
        )?);
    }
    Ok(out)
}

/// The port the UI's nginx listens on inside its container.
const UI_PORT: u16 = 8080;

#[cfg(test)]
mod tests {
    use super::*;

    struct TestPackage(std::path::PathBuf);
    impl TestPackage {
        fn new() -> Self {
            // A per-process counter, not the clock: macOS clocks tick in
            // microseconds, so two tests starting together got the same path.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("gearbox-ui-{}-{nth}", std::process::id()));
            std::fs::create_dir(&dir).unwrap();
            std::fs::create_dir(dir.join("src")).unwrap();
            std::fs::write(dir.join("src/index.ts"), "export const plugin = {};").unwrap();
            Self(dir)
        }
        fn write(&self, api_version: u32, export: &str) {
            std::fs::write(
                self.0.join("package.json"),
                serde_json::to_vec(&json!({
                    "name":"@gears/test-ui","version":"1.0.0","exports":{"./plugin":export},
                    "gearsUi":{"apiVersion":api_version,"kind":"module"}
                }))
                .unwrap(),
            )
            .unwrap();
        }
    }
    impl Drop for TestPackage {
        fn drop(&mut self) {
            // Only the three explicitly created files/directories; no recursive removal.
            drop(std::fs::remove_file(self.0.join("src/index.ts")));
            drop(std::fs::remove_file(self.0.join("package.json")));
            drop(std::fs::remove_dir(self.0.join("src")));
            drop(std::fs::remove_dir(&self.0));
        }
    }
    #[test]
    fn package_identity_is_projected_without_executing_the_entrypoint() {
        let dir = TestPackage::new();
        dir.write(1, "./src/index.ts");
        let package = package(&dir.0, "test-ui", "module").unwrap();
        assert_eq!(package.name, "@gears/test-ui");
        assert_eq!(package.files.len(), 2);
        assert_eq!(package.hash.len(), 64);
    }
    #[test]
    fn an_incompatible_ui_api_is_rejected_before_building() {
        let dir = TestPackage::new();
        dir.write(2, "./src/index.ts");
        assert!(matches!(
            package(&dir.0, "test-ui", "module"),
            Err(GenerateError::Ui(_))
        ));
    }
    #[test]
    fn missing_and_escaping_exports_are_rejected_before_building() {
        let dir = TestPackage::new();
        for export in ["./src/missing.ts", "./../private.ts", "/absolute.ts"] {
            dir.write(1, export);
            assert!(
                matches!(
                    package(&dir.0, "test-ui", "module"),
                    Err(GenerateError::Ui(_))
                ),
                "{export}"
            );
        }
    }

    #[test]
    fn secondary_exports_and_non_numeric_versions_are_rejected() {
        let dir = TestPackage::new();
        for (version, extra) in [
            ("1.0.0", json!({"browser":"./src/index.ts"})),
            ("+1.0.0", json!("./src/index.ts")),
            ("1.0.0", json!("./../private.ts")),
        ] {
            dir.write(1, "./src/index.ts");
            let path = dir.0.join("package.json");
            let mut manifest: Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            manifest["version"] = json!(version);
            manifest["exports"]["./secondary"] = extra;
            std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            assert!(matches!(
                package(&dir.0, "test-ui", "module"),
                Err(GenerateError::Ui(_))
            ));
        }
    }

    #[test]
    fn npm_names_cannot_smuggle_package_path_segments() {
        for name in ["@gears/ui", "settings-ui", "@gears/ui-sdk"] {
            assert!(npm_name(name));
        }
        for name in [
            "@",
            "@@gears/ui",
            "@gears",
            "gear/../../private",
            "@gears/ui/extra",
            "_private",
            "BadName",
        ] {
            assert!(!npm_name(name), "{name}");
        }
    }
}
