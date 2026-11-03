//! A `FrontX` frontend: the shell's workspace with the product's micro-frontends
//! mounted under `src-app/mfe_packages/`. JSON is read and package.json pins
//! are rewritten; JavaScript is never run.
//!
//! What it checks is what assembling one by hand showed going wrong: the
//! shell is a `FrontX` workspace that builds its micro-frontends and the
//! manifests it reads; each micro-frontend declares where it mounts
//! (`mfe.json`); and a micro-frontend gets the `FrontX` packages the shell
//! carries, whatever versions it pinned. Those pins are set to the shell's and
//! recorded in `frontend/frontx.lock.json`, so a build that then fails on a
//! name the shell's version lacks is explained by the lock, not discovered in
//! a stack trace.
//!
//! A package may describe itself in a `frontx.gdl` (GDL 1.6): a shell whether
//! it signs users in and where it reads its runtime addresses, a
//! micro-frontend the backend gears it calls. A package without one is read
//! as before, and what it would have said is recorded as unknown.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use gearbox_gdl::{FRONTX_FILE, FileIdentity, GdlEngine};
use gearbox_ir::frontx::{FrontxPackage, FrontxShell};
use gearbox_ir::{Diagnostic, DiagnosticCode, Diagnostics, FileEntry, GearId, RelPath, SourceId};
use serde_json::{Value, json};

use super::{GenerateError, GenerateInput};

fn invalid(message: impl Into<String>) -> GenerateError {
    GenerateError::Ui(message.into())
}

/// The npm scope whose packages a `FrontX` shell carries for its micro-frontends.
const FRONTX_SCOPE: &str = "@gears-frontx/";

/// Directories a copied package never brings: install and build output, and
/// version control.
const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "dist",
    "dist-lib",
    ".git",
    ".gearbox",
    ".turbo",
];

/// Files at the shell's root that generation writes itself: a shell's own
/// (Studio's portal has a `Dockerfile`) gives way, and a product that wants
/// it overlays the `frontx/*` template instead.
const GENERATED: &[&str] = &[
    "Dockerfile",
    ".dockerignore",
    "nginx.conf",
    "frontx.lock.json",
];

/// The workspace glob a `FrontX` shell mounts micro-frontends from.
const MFE_WORKSPACE: &str = "src-app/mfe_packages/*";

/// What the package in `dir` declares in its `frontx.gdl`, when it has one.
///
/// A description that does not evaluate fails generation: a package that says
/// something about itself and says it wrongly is not one to guess about.
fn declared(
    dir: &Path,
    what: &str,
    diagnostics: &mut Diagnostics,
) -> Result<Option<FrontxPackage>, GenerateError> {
    let path = dir.join(FRONTX_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    let identity = FileIdentity {
        uri: gearbox_ir::file_uri(&path),
        source: SourceId::new("frontx").map_err(|e| invalid(e.to_string()))?,
        gdl_path: RelPath::new(FRONTX_FILE).map_err(|e| invalid(e.to_string()))?,
        load_paths: None,
    };
    let outcome = GdlEngine::new().eval_frontx(&identity, &text);
    let Some(package) = outcome.value else {
        let reasons: Vec<String> = outcome
            .diagnostics
            .iter()
            .map(|d| format!("{}: {}", d.code, d.message))
            .collect();
        return Err(invalid(format!(
            "{what}: {FRONTX_FILE} does not evaluate: {}",
            reasons.join("; ")
        )));
    };
    diagnostics.extend(outcome.diagnostics);
    Ok(Some(package))
}

/// How a `frontx.gdl` spells a kind, for a message that it is the wrong one.
fn spelled(package: &FrontxPackage) -> &'static str {
    match package {
        FrontxPackage::Shell(_) => "`shell(...)`",
        FrontxPackage::Mfe { .. } => "`mfe(...)`",
        FrontxPackage::Library => "`library()`",
    }
}

/// A gear that signs a product's users in: a browser in front of that product
/// has to sign in before its calls are let through. The same rule Studio's
/// frontend constructor applies: a gear id with `oidc-authn`, `keycloak` or
/// `idp` among its hyphen-separated words.
fn signs_users_in(gear: &GearId) -> bool {
    let words: Vec<&str> = gear.as_str().split('-').collect();
    words.iter().any(|w| matches!(*w, "keycloak" | "idp"))
        || words.windows(2).any(|w| w == ["oidc", "authn"])
}

/// Every file under `dir`, relative to it, without [`SKIPPED_DIRS`] or
/// `skip(rel)`. A symlink is refused: it could reach outside the package.
fn walk(
    dir: &Path,
    rel: &str,
    skip: &dyn Fn(&str) -> bool,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), GenerateError> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| invalid(format!("{}: {e}", dir.display())))?
        .collect::<Result<_, _>>()
        .map_err(|e| invalid(format!("{}: {e}", dir.display())))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let kind = entry
            .file_type()
            .map_err(|e| invalid(format!("{path}: {e}")))?;
        if kind.is_symlink() {
            return Err(invalid(format!(
                "{path}: symlinks are not copied into a frontend"
            )));
        }
        if skip(&path) {
            continue;
        }
        if kind.is_dir() {
            if SKIPPED_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(&entry.path(), &path, skip, out)?;
        } else {
            let bytes = std::fs::read(entry.path()).map_err(|e| invalid(format!("{path}: {e}")))?;
            out.push((path, bytes));
        }
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Value, GenerateError> {
    let bytes = std::fs::read(path).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    serde_json::from_slice(&bytes).map_err(|e| invalid(format!("{}: {e}", path.display())))
}

fn str_field<'a>(manifest: &'a Value, key: &str, what: &str) -> Result<&'a str, GenerateError> {
    manifest[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid(format!("{what}: package.json needs `{key}`")))
}

/// The `FrontX` packages a shell gives what it mounts, name to version: the ones
/// its own workspace carries under `packages/`, the `FrontX` it depends on, and
/// itself.
fn carried(shell_dir: &Path, manifest: &Value) -> Result<BTreeMap<String, String>, GenerateError> {
    let mut carries = BTreeMap::new();
    for section in ["dependencies", "devDependencies"] {
        if let Some(deps) = manifest[section].as_object() {
            for (name, spec) in deps {
                // A range (`*`, `>=0.2.0-0`) says what the shell accepts, not
                // what it carries.
                if let Some(spec) = spec.as_str()
                    && name.starts_with(FRONTX_SCOPE)
                    && is_exact(spec)
                {
                    carries.insert(name.clone(), spec.to_owned());
                }
            }
        }
    }
    let packages = shell_dir.join("packages");
    if packages.is_dir() {
        let mut dirs: Vec<_> = std::fs::read_dir(&packages)
            .map_err(|e| invalid(format!("{}: {e}", packages.display())))?
            .filter_map(Result::ok)
            .filter(|e| e.path().join("package.json").is_file())
            .collect();
        dirs.sort_by_key(std::fs::DirEntry::file_name);
        for dir in dirs {
            let package = read_json(&dir.path().join("package.json"))?;
            if let (Some(name), Some(version)) =
                (package["name"].as_str(), package["version"].as_str())
            {
                carries.insert(name.to_owned(), version.to_owned());
            }
        }
    }
    if let (Some(name), Some(version)) = (manifest["name"].as_str(), manifest["version"].as_str()) {
        carries.insert(name.to_owned(), version.to_owned());
    }
    Ok(carries)
}

/// The shell's own micro-frontends: the directories under
/// `src-app/mfe_packages/` that hold an `mfe.json`.
fn own_mfes(shell_dir: &Path) -> Result<BTreeSet<String>, GenerateError> {
    let dir = shell_dir.join("src-app/mfe_packages");
    if !dir.is_dir() {
        return Ok(BTreeSet::new());
    }
    Ok(std::fs::read_dir(&dir)
        .map_err(|e| invalid(format!("{}: {e}", dir.display())))?
        .filter_map(Result::ok)
        .filter(|e| e.path().join("mfe.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect())
}

/// An exact version, `0.2.0` or `0.2.0-alpha.16`: not a range, tag or link.
fn is_exact(spec: &str) -> bool {
    let (core, pre) = spec.split_once('-').unwrap_or((spec, ""));
    let parts: Vec<_> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && pre
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// A dependency that points into the package's own workspace.
fn is_local(spec: &str) -> bool {
    ["file:", "workspace:", "link:"]
        .iter()
        .any(|p| spec.starts_with(p))
}

/// One pin of a micro-frontend set to the shell's.
struct Aligned {
    package: String,
    from: String,
    to: String,
}

/// Set every `FrontX` pin the shell carries to the shell's version; the rest is
/// left as written. Returns what changed.
fn align(manifest: &mut Value, carries: &BTreeMap<String, String>) -> Vec<Aligned> {
    let mut changed = Vec::new();
    for section in ["dependencies", "devDependencies", "peerDependencies"] {
        let Some(deps) = manifest.get_mut(section).and_then(Value::as_object_mut) else {
            continue;
        };
        for (name, spec) in deps.iter_mut() {
            let (Some(want), Some(have)) = (spec.as_str(), carries.get(name)) else {
                continue;
            };
            // A peer range (`*`, `>=0.2.0-0`) is the package saying it takes
            // what the host has; only an exact pin can disagree with it.
            if section == "peerDependencies" || want == have || is_local(want) {
                continue;
            }
            changed.push(Aligned {
                package: name.clone(),
                from: want.to_owned(),
                to: have.clone(),
            });
            *spec = Value::String(have.clone());
        }
    }
    changed
}

/// The directory a source or the product description locates packages under.
fn base<'a>(
    input: &'a GenerateInput<'_>,
    source: Option<&gearbox_ir::SourceId>,
    what: &str,
) -> Result<&'a Path, GenerateError> {
    match source {
        Some(source) => input
            .source_roots
            .get(source)
            .map(std::path::PathBuf::as_path)
            .ok_or_else(|| invalid(format!("{what}: source {source} is unavailable"))),
        None => input
            .product_dir
            .ok_or_else(|| invalid(format!("{what}: needs the product description directory"))),
    }
}

fn package_dir(base: &Path, rel: &str, what: &str) -> Result<std::path::PathBuf, GenerateError> {
    RelPath::new(rel).map_err(|e| invalid(format!("{what}: {e}")))?;
    base.join(rel)
        .canonicalize()
        .map_err(|e| invalid(format!("{what} {rel}: {e}")))
}

fn hash(files: &[(String, Vec<u8>)]) -> String {
    let mut hasher = blake3::Hasher::new();
    for (path, bytes) in files {
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hasher.finalize().to_hex().to_string()
}

/// A package mounted under `src-app/mfe_packages/<folder>`, its `FrontX` pins
/// set to the shell's.
struct Mounted {
    name: String,
    version: Value,
    folder: String,
    aligned: Vec<Aligned>,
    /// Of the package as written, before its pins were aligned.
    hash: String,
    files: Vec<(String, Vec<u8>)>,
}

impl Mounted {
    fn aligned_json(&self) -> Vec<Value> {
        self.aligned
            .iter()
            .map(|a| json!({"package": a.package, "from": a.from, "to": a.to}))
            .collect()
    }
}

fn mount(
    dir: &Path,
    what: &str,
    carries: &BTreeMap<String, String>,
) -> Result<Mounted, GenerateError> {
    let mut manifest = read_json(&dir.join("package.json"))?;
    let name = str_field(&manifest, "name", what)?.to_owned();
    let folder = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| invalid(format!("{what}: no directory name")))?
        .to_owned();
    let mut files = Vec::new();
    walk(dir, "", &|rel| rel == "package-lock.json", &mut files)?;
    let hash = hash(&files);
    let aligned = align(&mut manifest, carries);
    let mut mounted = Vec::with_capacity(files.len());
    for (path, bytes) in files {
        let bytes = if path == "package.json" && !aligned.is_empty() {
            let mut text =
                serde_json::to_vec_pretty(&manifest).map_err(|e| invalid(e.to_string()))?;
            text.push(b'\n');
            text
        } else {
            bytes
        };
        mounted.push((
            format!("frontend/src-app/mfe_packages/{folder}/{path}"),
            bytes,
        ));
    }
    Ok(Mounted {
        name,
        version: manifest["version"].clone(),
        folder,
        aligned,
        hash,
        files: mounted,
    })
}

/// What the shell's `frontx.gdl` declares, checked against the shell and the
/// product: a `runtime_env` it lacks fails, and a shell that does not sign in
/// on a product that does is reported (GBX0721).
fn shell_declaration(
    input: &GenerateInput<'_>,
    shell_dir: &Path,
    shell_name: &str,
    diagnostics: &mut Diagnostics,
) -> Result<Option<FrontxShell>, GenerateError> {
    let shell = match declared(shell_dir, shell_name, diagnostics)? {
        None => return Ok(None),
        Some(FrontxPackage::Shell(shell)) => shell,
        Some(other) => {
            return Err(invalid(format!(
                "{shell_name}: its {FRONTX_FILE} declares {}, not `shell(...)`",
                spelled(&other)
            )));
        }
    };
    if let Some(env) = &shell.runtime_env
        && !shell_dir.join(env).is_file()
    {
        return Err(invalid(format!(
            "{shell_name}: its {FRONTX_FILE} reads the product's addresses from `{env}`, \
             which it does not have"
        )));
    }
    if !shell.signs_in
        && let Some(idp) = input.lock.gears.keys().find(|id| signs_users_in(id))
    {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GenFrontxShellNoSignIn,
                format!(
                    "the product signs users in with `{idp}`, and the FrontX shell \
                     {shell_name} declares `signs_in = False`: its screens' calls will be refused"
                ),
            )
            .with_help("mount the screens in a shell that signs users in against the product"),
        );
    }
    Ok(Some(shell))
}

/// The gears a micro-frontend requires: what the product says, and what the
/// package's own `frontx.gdl` says. The union, because a product may know of a
/// call the package does not (one made through a shared library), and neither
/// may drop the other's. Also whether the package has a `frontx.gdl`.
fn mfe_requires(
    input: &GenerateInput<'_>,
    mfe: &gearbox_ir::frontx::FrontxMfe,
    dir: &Path,
    what: &str,
    diagnostics: &mut Diagnostics,
) -> Result<(BTreeSet<GearId>, bool), GenerateError> {
    let mut requires = mfe.requires.clone();
    match declared(dir, what, diagnostics)? {
        None => Ok((requires, false)),
        Some(FrontxPackage::Mfe { requires: own }) => {
            if let Some(absent) = own.iter().find(|id| !input.lock.gears.contains_key(*id)) {
                return Err(invalid(format!(
                    "{what} requires absent backend {absent} (its {FRONTX_FILE} says so)"
                )));
            }
            requires.extend(own);
            Ok((requires, true))
        }
        Some(other) => Err(invalid(format!(
            "{what}: its {FRONTX_FILE} declares {}, not `mfe(...)`",
            spelled(&other)
        ))),
    }
}

/// A package the product brings as a library is not a micro-frontend: it has
/// no `mfe.json`, and its `frontx.gdl`, if any, says `library()`.
fn check_library(
    dir: &Path,
    what: &str,
    diagnostics: &mut Diagnostics,
) -> Result<(), GenerateError> {
    if dir.join("mfe.json").is_file() {
        return Err(invalid(format!(
            "{what} has an mfe.json: list it under `mfes`"
        )));
    }
    match declared(dir, what, diagnostics)? {
        None | Some(FrontxPackage::Library) => Ok(()),
        Some(other) => Err(invalid(format!(
            "{what}: its {FRONTX_FILE} declares {}, not `library()`",
            spelled(&other)
        ))),
    }
}

pub(super) fn files(
    input: &GenerateInput<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<Vec<FileEntry>, GenerateError> {
    let Some(frontend) = &input.lock.frontend else {
        return Ok(Vec::new());
    };
    if input.lock.ui.is_some() {
        return Err(invalid(
            "a product has `ui` or `frontend`, not both: they ship the same UI image",
        ));
    }

    // The shell: a FrontX workspace that mounts micro-frontends and builds them.
    let shell_dir = package_dir(
        base(input, frontend.source.as_ref(), "FrontX shell")?,
        &frontend.shell,
        "FrontX shell",
    )?;
    let shell_manifest = read_json(&shell_dir.join("package.json"))?;
    let shell_name = str_field(&shell_manifest, "name", "FrontX shell")?.to_owned();
    let shell_version = str_field(&shell_manifest, "version", &shell_name)?.to_owned();
    let mounts = shell_manifest["workspaces"]
        .as_array()
        .is_some_and(|w| w.iter().any(|g| g.as_str() == Some(MFE_WORKSPACE)));
    if !mounts {
        return Err(invalid(format!(
            "{shell_name}: a FrontX shell lists `{MFE_WORKSPACE}` in its workspaces"
        )));
    }
    if shell_manifest["scripts"]["build"].as_str().is_none() {
        return Err(invalid(format!(
            "{shell_name}: a FrontX shell has a `build` script"
        )));
    }
    let carries = carried(&shell_dir, &shell_manifest)?;
    let shell_declares = shell_declaration(input, &shell_dir, &shell_name, diagnostics)?;
    // The shell's npm lock is kept: `npm install` starts from it, so what the
    // shell resolved stays resolved, and only what the product changed (its
    // micro-frontends, the pins aligned) is resolved anew. Without it a range
    // such as `"*"` takes whatever is newest that day.
    let npm_lock = std::fs::read(shell_dir.join("package-lock.json"))
        .ok()
        .map(|bytes| blake3::hash(&bytes).to_hex().to_string());
    // The libraries the product brings, each under its own directory name;
    // one the shell has there gives way.
    let mut names = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    let mut libraries = Vec::new();
    for library in &frontend.libraries {
        let what = format!("library {}", library.package);
        let dir = package_dir(
            base(input, library.source.as_ref(), &what)?,
            &library.package,
            &what,
        )?;
        check_library(&dir, &what, diagnostics)?;
        let mounted = mount(&dir, &what, &carries)?;
        if !names.insert(mounted.name.clone()) || !dirs.insert(mounted.folder.clone()) {
            return Err(invalid(format!(
                "{}: mounted twice, or two packages share `{}`",
                mounted.name, mounted.folder
            )));
        }
        libraries.push(mounted);
    }

    // The shell's own micro-frontends (a template's examples) are not the
    // product's: only the ones it picks are mounted. A package there without
    // an `mfe.json` is a library the shell's screens share, and stays unless
    // the product brings its own.
    let own_mfes = own_mfes(&shell_dir)?;
    let mut shell_files = Vec::new();
    walk(
        &shell_dir,
        "",
        &|rel| {
            rel.strip_prefix("src-app/mfe_packages/")
                .and_then(|r| r.split('/').next())
                .is_some_and(|dir| own_mfes.contains(dir) || dirs.contains(dir))
                || rel == "public/generated-mfe-manifests.json"
                || GENERATED.contains(&rel)
        },
        &mut shell_files,
    )?;

    let mut out_files: Vec<(String, Vec<u8>)> = shell_files
        .iter()
        .map(|(p, b)| (format!("frontend/{p}"), b.clone()))
        .collect();
    for library in &mut libraries {
        out_files.append(&mut library.files);
    }

    // The micro-frontends, each under its own directory name.
    let mut mounted = Vec::new();
    for mfe in &frontend.mfes {
        let what = format!("micro-frontend {}", mfe.package);
        if let Some(absent) = mfe
            .requires
            .iter()
            .find(|id| !input.lock.gears.contains_key(*id))
        {
            return Err(invalid(format!("{what} requires absent backend {absent}")));
        }
        let dir = package_dir(
            base(input, mfe.source.as_ref(), &what)?,
            &mfe.package,
            &what,
        )?;
        let (requires, described) = mfe_requires(input, mfe, &dir, &what, diagnostics)?;
        let mfe_json = read_json(&dir.join("mfe.json"))
            .map_err(|_| invalid(format!("{what}: a micro-frontend has an mfe.json")))?;
        let extensions: Vec<Value> = mfe_json["extensions"]
            .as_array()
            .map(|all| {
                all.iter()
                    .map(|e| json!({"id": e["id"], "domain": e["domain"], "route": e["route"]}))
                    .collect()
            })
            .unwrap_or_default();
        let mut package = mount(&dir, &what, &carries)?;
        if !names.insert(package.name.clone()) || !dirs.insert(package.folder.clone()) {
            return Err(invalid(format!(
                "{}: mounted twice, or two packages share `{}`",
                package.name, package.folder
            )));
        }
        out_files.append(&mut package.files);
        mounted.push(json!({
            "name": package.name,
            "version": package.version,
            "directory": package.folder,
            "requires": requires,
            "described": described,
            "extensions": extensions,
            "aligned": package.aligned_json(),
            "blake3": package.hash,
        }));
    }

    let lock = json!({
        "schemaVersion": 1,
        "productLockHash": input.lock.product.lock_hash,
        "entryPoints": frontend.entry_points,
        "shell": {
            "name": shell_name,
            "version": shell_version,
            "carries": carries,
            "blake3": hash(&shell_files),
            "npmLock": npm_lock,
            // `null` when the shell has no `frontx.gdl` to say.
            "signsIn": shell_declares.as_ref().map(|s| s.signs_in),
            "runtimeEnv": shell_declares.as_ref().and_then(|s| s.runtime_env.clone()),
            "described": shell_declares.is_some(),
        },
        "mfes": mounted,
        "libraries": libraries.iter().map(|l| json!({
            "name": l.name,
            "version": l.version,
            "directory": l.folder,
            "aligned": l.aligned_json(),
            "blake3": l.hash,
        })).collect::<Vec<_>>(),
    });
    let mut out = Vec::new();
    for (path, bytes) in out_files {
        out.push(super::ui::file(&path, bytes)?);
    }
    let mut lock_bytes = serde_json::to_vec_pretty(&lock).map_err(|e| invalid(e.to_string()))?;
    lock_bytes.push(b'\n');
    out.push(super::ui::file("frontend/frontx.lock.json", lock_bytes)?);

    let render = |key: &str, ctx: minijinja::Value| -> Result<Vec<u8>, GenerateError> {
        Ok(super::templates::render("FrontX", input.templates.get(key)?, ctx)?.into_bytes())
    };
    out.push(super::ui::file(
        "frontend/Dockerfile",
        render("frontx/Dockerfile", minijinja::context! {})?,
    )?);
    out.push(super::ui::file(
        "frontend/.dockerignore",
        render("frontx/dockerignore", minijinja::context! {})?,
    )?);
    if input.lock.kubernetes.is_none() {
        return Ok(out);
    }
    let gateway = input
        .lock
        .applications
        .iter()
        .find(|a| a.gears.iter().any(|id| id.as_str() == "api-gateway"))
        .ok_or_else(|| invalid("a FrontX frontend needs an API Gateway application"))?;
    let port = gateway
        .service_port
        .ok_or_else(|| invalid("the API Gateway application has no service port"))?;
    out.push(super::ui::file(
        "frontend/nginx.conf",
        render(
            "frontx/nginx.conf",
            minijinja::context! { gateway => gateway.name.as_str(), gateway_port => port },
        )?,
    )?);
    // The same deployment as `ui`'s: one UI image per product, under `ui` values.
    let product = input.lock.product.id.as_str();
    let helm_ctx =
        minijinja::context! { name => format!("{product}-frontend"), port => super::ui::UI_PORT };
    for (key, name) in [
        ("helm/ui/deployment.yaml", "frontend-deployment.yaml"),
        ("helm/ui/service.yaml", "frontend-service.yaml"),
    ] {
        let body = super::templates::render_helm(
            "FrontX chart",
            input.templates.get(key)?,
            helm_ctx.clone(),
        )?;
        out.push(super::ui::file(
            &format!("helm/{product}/templates/{name}"),
            body.into_bytes(),
        )?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_the_shell_carries_is_set_to_the_shells_and_recorded() {
        let carries = BTreeMap::from([
            (
                "@gears-frontx/react".to_owned(),
                "0.2.0-alpha.15".to_owned(),
            ),
            ("@gears-frontx/mfes".to_owned(), "0.4.0".to_owned()),
        ]);
        let mut manifest = json!({
            "dependencies": {"@gears-frontx/react": "0.2.0-alpha.0", "@gears-frontx/ui-kit": "0.4.10"},
            "devDependencies": {"@gears-frontx/mfes": "0.4.0", "@gears-frontx/shared": "file:../shared"},
            "peerDependencies": {"@gears-frontx/react": "*"}
        });
        let changed = align(&mut manifest, &carries);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].package, "@gears-frontx/react");
        assert_eq!(changed[0].from, "0.2.0-alpha.0");
        assert_eq!(
            manifest["dependencies"]["@gears-frontx/react"],
            "0.2.0-alpha.15"
        );
        assert_eq!(manifest["dependencies"]["@gears-frontx/ui-kit"], "0.4.10");
        assert_eq!(manifest["peerDependencies"]["@gears-frontx/react"], "*");
    }

    #[test]
    fn only_an_exact_version_is_carried() {
        assert!(is_exact("0.4.0"));
        assert!(is_exact("0.2.0-alpha.16"));
        assert!(!is_exact("*"));
        assert!(!is_exact(">=0.2.0-0"));
        assert!(!is_exact("^0.4.0"));
        assert!(!is_exact("file:."));
    }

    #[test]
    fn workspace_links_are_not_pins() {
        assert!(is_local("file:."));
        assert!(is_local("workspace:*"));
        assert!(!is_local("0.4.0"));
    }

    #[test]
    fn a_shell_carries_its_workspace_packages_its_frontx_and_itself() {
        let dir =
            std::env::temp_dir().join(format!("gearbox-frontx-carries-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("packages/react")).unwrap();
        std::fs::write(
            dir.join("packages/react/package.json"),
            r#"{"name":"@gears-frontx/react","version":"0.2.0-alpha.15"}"#,
        )
        .unwrap();
        let manifest = json!({
            "name": "@gears-frontx/frontx-template-shell", "version": "0.1.0-alpha.6",
            "dependencies": {"@gears-frontx/mfes": "0.4.0", "@gears-frontx/frontx-template-shell": "file:."}
        });
        let carries = carried(&dir, &manifest).unwrap();
        assert_eq!(
            carries,
            BTreeMap::from([
                (
                    "@gears-frontx/frontx-template-shell".to_owned(),
                    "0.1.0-alpha.6".to_owned()
                ),
                ("@gears-frontx/mfes".to_owned(), "0.4.0".to_owned()),
                (
                    "@gears-frontx/react".to_owned(),
                    "0.2.0-alpha.15".to_owned()
                ),
            ])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
