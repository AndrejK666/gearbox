# GDL

GDL (Gears Description Language) is a declarative DSL. A file states facts. The resolver decides.

Four file kinds, four vocabularies. `gear()` is not callable in a product file; `product()` is not callable in a gear file; neither is callable in a `frontx.gdl` or a `service.gdl`.

| File | Declares | Required call |
|---|---|---|
| `gear.gdl` | one gear: facts Rust does not already own | exactly one `gear(...)` |
| `product.gdl` | one product: operator intent | exactly one `product(...)` |
| `frontx.gdl` | one FrontX package's passport: everything about it, its `package.json` and `mfe.json` facts included and checked against them (GDL 1.5) | exactly one `shell(...)`, `mfe(...)` or `library()` |
| `service.gdl` | one service that is not a gear: an own service, or an infra service a product runs beside its gears (GDL 1.6) | exactly one `service(...)` |

Zero or two top-level declarations is **GBX0105**. Profile-specific behaviour is a `profiles = [...]` list on the declaration, not a branch.

Arguments are keyword-only, with these exceptions. In a `gear.gdl`, the first argument of `extension_point`, `feature`, `scenario`, `path` and `fail` is positional. In a `product.gdl`, the first argument of `path`, `registry`, `plugin`, `use_gear`, `use_service`, `application` and `provider` is positional. An unknown **named** argument is **GBX0106**. A stray positional argument is **GBX0102**, because it does not get to an argument name. Records returned by constructors (`cargo(...)`, `provide(...)`, …) are opaque: pass them on, do not inspect them.

---

## Grammar

```
file        = { stmt } ;
stmt        = load_stmt | assignment | call ;

assignment  = ident "=" expr ;
load_stmt   = "load" "(" string { "," load_item } [ "," ] ")" ;
load_item   = string | ident "=" string ;

call        = callee "(" [ arg { "," arg } [ "," ] ] ")" ;
callee      = ident | ident "." ident ;
arg         = ident "=" expr | expr ;          (* positional only where listed *)

expr        = literal | ident | ident "." ident | call | list | map ;
list        = "[" [ expr { "," expr } [ "," ] ] "]" ;
map         = "{" [ string ":" expr { "," string ":" expr } [ "," ] ] "}" ;

literal     = string | number | "True" | "False" | "None" ;
ident       = ( letter | "_" ) { letter | digit | "_" } ;
letter      = "A".."Z" | "a".."z" ;
digit       = "0".."9" ;
```

Refused: `if`, `for`, `def`, `lambda`, `and`, `or`, `not`, `in`, comprehensions, conditional expressions. Every occurrence is **GBX0103**, with its own span.

Not refused, but outside the grammar above: arithmetic (`"a" + "b"`, `8000 + 80`), indexing, tuples, string and dict methods, non-string map keys. The host evaluates them; GDL states no fact that needs any of them, and a description reaching for one is computing rather than stating.

`#` comments run to end of line.

---

## Lexicon

**Identifiers** (language names: `use_gear`, `crate_name`): ASCII, no leading digit.

**Kebab-case ids** (string values: gear, source, profile, application): lowercase letters, digits, single interior hyphens; start with a letter; no trailing hyphen; no `_`; no `--`. Example: `api-gateway`, `gears-rust`.

**Strings.** `"..."` or `'...'`. Triple quotes `"""..."""` / `'''...'''` for multi-line. Prefix `r` for raw (backslashes are not escapes). Escapes in non-raw strings: `\\ \` \' \a \b \f \n \r \t \v`, octal, `\xHH`, `\uHHHH`, `\UHHHHHHHH`. No f-strings. `${NAME}` inside a string is not interpolation. GDL copies it through as text. What reads it later is a contract, not a coincidence: a config field Rust declares secret may carry nothing else (**GBX0116**), and a gear's configuration reads it as a variable the generated process substitutes when it starts (GDL 1.6, see [Infra services and variables](#infra-services-and-variables-gdl-16)).

**Numbers.** Decimal integers, `0x` / `0o` / `0b`, floats.

**Booleans / null.** `True`, `False`, `None`.

**The one keyword.** `load`. `crate` is not a legal parameter name, so cargo uses `crate_name`.

---

## UI composition (GDL 1.2)

UI composition is optional and available in GDL 1.2. A gear locates its UI
source package and states the backends it needs:

```python
gear(
    # Existing Rust package and other declarations...
    ui = ui_module(package = path("../ui")),
)
```

The owning gear is required automatically. `requires` lists only additional
backend gears the UI calls; it neither adds them to the product nor declares
Rust linking dependencies. Product-specific screens belong to the product's
adapter gear, rather than a reusable backend service's UI.

A product explicitly selects modules and locates its shell and shared SDK:

```python
ui = ui_product(
    source = "gears-rust",
    npm_lock = path("ui-package-lock.json"),
    shell = path("ui/product-shell"),
    sdk = path("ui/ui-sdk"),
    modules = ["settings-service"],
)
```

With `source`, package paths resolve through the supplied source root, so checkout
folder names do not enter UI locators. Without `source`, paths are relative to
the product description. These are directory locators. npm names, versions and exports come from each
`package.json`. Packages declare `gearsUi.apiVersion = 1` and kind `sdk`, `shell`
or `module`. Modules export `./plugin` from a file under `src/`; SDK and shell
export `.`. Shell and modules declare a compatible SDK peer (`^1.0.0` for SDK
1.x). The current source-package format accepts numeric `major.minor.patch`
versions and string exports. Runtime module federation is not used.

The resolver rejects missing backend gears. Generation validates package
metadata and export files without executing JavaScript, then emits the frontend
workspace, module registry, nginx, Dockerfile and
Kubernetes UI resources. `product.lock` schema 3 records the UI composition;
`ui/ui.lock.json` records projected package identities, exports and content
hashes beside the parent product lock hash. npm's generated `package-lock.json`
pins third-party dependencies for `npm ci` in the prepared bundle. Optional
`npm_lock = path(...)` copies a tracked lock from within the product directory,
validates its root dependencies and records its content hash in `ui.lock.json`. Product preparation
supplies `public/runtime-config.json`; generation does not overwrite it. Products
without UI declarations continue to generate only their existing backend
artifacts. Resolve older locks again to write schema 3.

Firebase Web SDK configuration belongs in the prepared product's public runtime
configuration. User mappings, bearer tokens and private assets do not belong in
GDL or UI source packages.

## FrontX frontend (GDL 1.5)

A product whose screens are FrontX micro-frontends describes them with
`frontx_product(...)` instead of `ui_product(...)`. A product has one or the
other: they ship the same UI image.

```python
frontend = frontx_product(
    source = "frontx-templates",          # optional: `shell` is under this source's root
    shell = path("template-shell"),       # a FrontX template, or a product's own fork of one
    mfes = [
        frontx_mfe(
            package = path("studio-frontend/src-app/mfe_packages/people-mfe"),
            requires = ["studio-user", "account-management"],
        ),
    ],
    # optional: packages the micro-frontends share that are not ones themselves
    libraries = [frontx_library(package = path("studio-frontend/src-app/mfe_packages/shared"))],
    entry_points = {"Portal": "/"},
),
```

FrontX packages keep FrontX's own contract, so none of `ui_product`'s package
rules apply: versions may be prereleases, there is no `gearsUi` metadata, and
micro-frontends are module-federation remotes the shell mounts from `mfe.json`.

- **The shell** is an npm workspace that lists `src-app/mfe_packages/*` and
  has a `build` script; a FrontX template's does (it builds the packages the
  shell carries, each micro-frontend, the manifests it mounts them from, then
  the site). The FrontX packages it **carries** are its own `packages/*`, the
  `@gears-frontx/*` it depends on, and itself.
- **A micro-frontend** has a `package.json` and an `mfe.json`. Without `source`
  its path is relative to the product description. `requires` names the
  backend gears its screens call; resolution reports one the profile does not
  have (GBX0720). It neither adds the gear nor links anything. What the
  package's own `frontx.gdl` requires is added to it (see below).
- **A library** is a package the micro-frontends depend on that is not one
  itself: a `package.json` and no `mfe.json`. A shell keeps its own libraries;
  `libraries` brings one the shell lacks (micro-frontends written for another
  shell), and replaces one the shell has under the same directory. Its FrontX
  pins are aligned like a micro-frontend's.
- **Versions are aligned to the shell.** A micro-frontend gets the FrontX the
  shell carries, whatever it pinned. Generation sets each exact pin of a
  carried package to the shell's version (peer ranges are left alone) and
  records every change in `frontend/frontx.lock.json`. A package that uses
  something the shell's version lacks then fails to build, and the lock says
  which pin moved.

Generation writes `frontend/`: the shell's files (without its own
micro-frontends (a library beside them, with no `mfe.json`, stays), or
install and build output; its `package-lock.json` is kept, and the image's
`npm install` starts from it),
each micro-frontend and library under `src-app/mfe_packages/<its directory
name>`, `frontx.lock.json` (the shell's name, version and carried packages,
and what its `frontx.gdl` says: `signsIn`, `runtimeEnv`, `described`; each
micro-frontend's name, version, extensions, required gears, whether it has a
`frontx.gdl`, aligned pins and content hash; each library's name, version,
aligned pins and content hash), a `Dockerfile` and `.dockerignore`. A kubernetes profile adds
`frontend/nginx.conf`, which proxies the shell's `api_prefix` (`/cf/` by
default) to the API Gateway application unchanged and serves `env.js` uncached, and the deployment and service under
the umbrella chart's `ui` values, image `gbx-<product>-frontend:<version>`.
Templates: `frontx/Dockerfile`, `frontx/dockerignore`, `frontx/nginx.conf`.

### A package's passport: `frontx.gdl`

A FrontX package may carry a passport, a `frontx.gdl` at its root beside its
`package.json`, the way a gear has its `gear.gdl`. It is the one file to read
the package from: who it is, how a shell is built and served, what it reads at
start, what a micro-frontend calls and mounts. It deliberately restates what
`package.json` and `mfe.json` already say, and generation checks each restated
fact against the manifest it restates (below), so the copies cannot drift
unnoticed. Every argument but a shell's `signs_in` is optional; one left out is
read from the manifests, as without a passport. One call says what the package
is:

```python
# template-shell/frontx.gdl
gdl_version = "1.5"

shell(
    name = "@acme/shell",            # = package.json name
    version = "1.2.0",               # = package.json version
    description = "The Acme portal", # = package.json description
    node = ">=24",                   # = engines.node; "24" or "24.21.0" picks the build image
    package_manager = "npm",         # "npm", "pnpm" or "yarn": the one whose lock it keeps
    build = "npm run build",         # the build step, after the install its package manager does
    output = "dist",                 # the directory the build writes the site to
    base = "/",                      # the path it is served under
    api_prefix = "/cf/",             # the path its screens call the backend under, proxied to the gateway
    signs_in = True,                 # required: does it sign users in before mounting a screen?
    sign_in_redirect = "/callback",  # or a list; only with signs_in = True
    runtime_env = "public/env.js",   # or None: addresses fixed at build
    env = {"apiBase": "gateway.url", "oidcIssuer": "oidc.issuer"},  # keys it reads -> what fills them
    build_env = ["VITE_API_BASE"],   # variables needed at build when runtime_env is None
    carries = {"@gears-frontx/react": "0.2.0-alpha.15"},  # every FrontX package it gives what it mounts
)
```

```python
# src-app/mfe_packages/people-mfe/frontx.gdl
gdl_version = "1.5"

mfe(
    name = "@acme/people-mfe", version = "0.3.0", description = "People screens",
    requires = ["account-management", "studio-user"],   # backend gears its screens call; [] calls none
    screens = ["People"],               # = mfe.json extensions' presentation.label, in order
    mounts = ["/people"],               # = mfe.json extensions' route, in order
    frontx = "0.2.0-alpha.15",          # = its @gears-frontx/react pin
    roles = ["people.read"],            # permissions its screens need, when known
    preview = ["/people"],              # routes worth a screenshot
    fixtures = "preview/fixtures.json", # mock data for them
)
```

```python
# src-app/mfe_packages/shared/frontx.gdl
gdl_version = "1.5"

library(name = "@acme/shared", version = "0.1.0", description = "Shared components")
```

The calls are not the product's `frontx_mfe(...)` and `frontx_library(...)`:
those locate a package (`package = path(...)`); here the file is the locator.
`runtime_env`, `output` and `fixtures` are paths inside the package; `base` and
`api_prefix` start and end with `/` (and `api_prefix` is not `/` alone);
`mounts`, `preview` and `sign_in_redirect` are routes; `build_env` holds
variable names; `requires` takes gear ids. A file with two calls, or none, is
**GBX0105**; an argument outside the passport is **GBX0106**.

**What the generator uses.** The shell's passport drives its image; with the
argument left out, the output is what it was before passports:

| Argument | Drives | Default |
|---|---|---|
| `node` | the build image `node:<v>-alpine`, when it is an exact or major version; a range keeps the default | `node:24.21.0-alpine` |
| `package_manager` | the install step: `npm install --no-audit --no-fund`, or `corepack enable && pnpm install` / `yarn install` | npm |
| `build` | the build step, run after the install | `npm run build` |
| `output` | the directory copied into the server | `dist` |
| `base` | where the site is copied under the server's root, and where `env.js` and the fallback `index.html` are | `/` |
| `api_prefix` | the nginx location proxied to the API Gateway, unchanged | `/cf/` |

`frontx.lock.json` records the effective `output`, `base` and `apiPrefix`.

**The consistency check.** At generation, for each package the product mounts
(and for one package alone through `gearbox_engine::generate::check_frontx_package`):

| The package's `frontx.gdl` | Result |
|---|---|
| absent | read as before; the lock records `signsIn: null`, `runtimeEnv: null`, `described: false` |
| declares another kind than the product mounts it as (`library()` listed under `mfes`, …) | **GBX0720** |
| does not evaluate | **GBX0720**, naming the description's own diagnostics |
| `mfe(requires = [...])` | added to the product's `frontx_mfe(requires = ...)` (the union: either may know of a call the other does not); a gear the product lacks is **GBX0720** |
| `shell(runtime_env = "...")` naming a file the shell lacks | **GBX0720** |
| `shell(signs_in = False)`, on a product with a gear that signs users in (`oidc-authn`, `keycloak` or `idp` among its id's words) | **GBX0721**, a warning: the screens' calls will be refused |
| `name` or `version` other than `package.json`'s; `frontx` other than its `@gears-frontx/react` pin; `carries` other than what the shell's workspace carries (a missing, extra or different entry); `screens` or `mounts` other than `mfe.json`'s | **GBX0722**, an error |
| `description` other than `package.json`'s; `node` other than its `engines.node`; `package_manager` other than its `packageManager`, or without that manager's lock file; a `build` running a script `package.json` lacks; a `preview` route it does not mount; a `fixtures` file it lacks | **GBX0723**, a warning |

A fact the manifest does not state (no `description`, no `engines`) has nothing
to disagree with and is not reported.

## Infra services and variables (GDL 1.6)

A product's infrastructure services -- its own Keycloak, its PostgreSQL -- are
third-party services it runs beside its gears. Each is described by a passport,
a `service.gdl` with `kind = "infra"`, in one of the product's sources; the
product chooses them with `product(infra = [use_service(...)])`. A gear's
configuration reads what they provide as environment variables:
`"${OIDC_ISSUER}"`.

**Gearbox installs none of them.** It reads the passports, checks the product's
choice against them, binds each capability to a variable, records it all in the
lock, and makes each generated process substitute the variables when it starts.
The deployment (Studio's) installs the services and sets the variables. What a
product says is *what* it runs; *where* -- with the product, or in the
organization's shared place -- its public address, the values of a ready
chart, and the copy of an image an environment's registry holds belong to the
environment, so one revision of the description deploys the same way to both.

### A service's passport: `service.gdl`

One `service(...)` call, in a file beside the service (one per directory, like
`gear.gdl`). The catalogue finds every `service.gdl` under a source root; a
second passport of the same `name` in one source is **GBX0105**. It is the
vocabulary Studio's catalogue reads too.

```python
# infra/keycloak/service.gdl
gdl_version = "1.6"

service(
    name = "keycloak",                       # required: kebab-case
    kind = "infra",                          # "own" (the default) or "infra"
    version = "26.0.7",
    description = "Keycloak: the product's own OIDC identity provider",
    image = "quay.io/keycloak/keycloak:26.0.7",   # the publisher's, with a tag or a digest
    args = ["start-dev"],
    ports = {"http": 8080, "management": 9000},
    health = "/health/ready",
    config = {"KC_HEALTH_ENABLED": "true", "KC_BOOTSTRAP_ADMIN_USERNAME": "admin"},
    secrets = ["KC_BOOTSTRAP_ADMIN_PASSWORD"],    # the deployment makes and keeps them
    provides = {
        "oidc.issuer": "{public_url}/realms/product",
        "keycloak.admin_url": "http://{host}:{port.http}",
    },
    stateful = False,                        # True: needs data_path; storage defaults to 10Gi
    chart = "oci://registry-1.docker.io/bitnamicharts/keycloak",
    chart_version = "24.4.0",
)
```

- An **infra** service is pulled, never built: `dockerfile` and `context` are
  refused, and so is `env`. It names an `image`, a ready `chart`, or both;
  `chart_version` goes with `chart`, which is `oci://...` or `https://...`.
- `config` holds literal variables, `secrets` the variables the deployment makes;
  a name is in one or the other.
- `provides` is a dict of capability (`oidc.issuer`: lowercase, dotted) to the
  template of its value, resolved at deploy: `{host}`, `{namespace}`,
  `{public_url}`, `{port.<name>}`, `{config.<KEY>}`, `{secret.<KEY>}`. Every name
  a template uses is declared by the passport (`{public_url}` needs a port). A
  list of keys is read too: values the shared place or the project gives.
- `stateful = True` needs `data_path`, an absolute path; `storage` is a size
  (`10Gi`, `512Mi`) and both go with `stateful`.
- An **own** service (`kind` left out) is built from its `dockerfile` and
  `context`, reads `env` (variable to what fills it), and lists `provides` as
  capability keys. The infra-only arguments are refused on it. A product does not
  run an own service through `infra`.

A wrong passport is reported when the catalogue loads, with the usual
evaluation codes (**GBX0102**, **GBX0106**).

### `use_service(...)` and `secrets`

```
use_service("name", source, image?, chart?, chart_version?, config = {}, profile_config = {})
product(..., infra = [use_service(...)], secrets = ["NAME", ...])
```

```python
gdl_version = "1.6"

product(
    id = "studioweb", version = "0.4.0",
    sources = [
        source(id = "gears-rust", at = git(url = "https://github.com/constructorfabric/gears-rust", tag = "v0.12.0")),
        source(id = "studio-web", at = path("..")),
    ],
    infra = [
        use_service("keycloak", source = "studio-web",
            image = "quay.io/keycloak/keycloak:26.0.7@sha256:4f3c...",   # optional: else the passport's
            config = {"KC_PROXY_HEADERS": "xforwarded"},
            profile_config = {"dev": {"KC_HEALTH_ENABLED": "false"}}),
        use_service("postgresql", source = "studio-web"),
    ],
    secrets = ["SMTP_PASSWORD"],
    gears = [
        use_gear("authn-resolver", source = "gears-rust", plugins = [
            plugin("oidc-authn-plugin", config = {
                "jwt": {"trusted_issuers": [{"issuer": "${OIDC_ISSUER}"}]},
                "s2s_oauth": {"discovery_url": "${OIDC_ISSUER}/.well-known/openid-configuration"},
            }),
        ]),
        use_gear("mailer", source = "studio-web", config = {"smtp_password": "${SMTP_PASSWORD}"}),
    ],
    profiles = [kubernetes(id = "dev", discovery = "static", development = True)],
    default_profile = "dev",
)
```

- `name` is the passport's, found in the named `source`. None there, or one that
  is not `kind = "infra"`, is **GBX0127**. The same name twice is **GBX0110**.
- `image` is the publisher's reference -- a version choice, not an environment's
  mirror. A digest in it (`...@sha256:...`) is recorded in the lock. `chart` and
  `chart_version` choose a ready chart instead of the passport's. Each is
  optional; left out, the passport's is used.
- `config` sets keys of the passport's `config`, each to a string literal;
  `profile_config` maps a declared profile to values merged over `config` for it.
  A key the passport's `config` does not declare, one of its `secrets`, or a
  `${...}` value is **GBX0129**.
- Two services that provide the same capability are **GBX0128**.
- `secrets` lists the variables the product's configuration reads that no infra
  service provides; the deployment sets them.

### Variables in a gear's configuration

Any string in a `use_gear` or `plugin` `config` or `profile_config` may read a
variable, whole (`"${OIDC_ISSUER}"`) or inside a string
(`"${OIDC_ISSUER}/protocol"`). A capability is read as its name upper-cased with
every other character `_`: `oidc.issuer` is `OIDC_ISSUER`, `keycloak.admin_url`
is `KEYCLOAK_ADMIN_URL`.

A product that declares `infra` or `secrets` names every variable its
configuration reads; one it does not declare is **GBX0130**. Declared are: each
capability of its infra services, each profile's database password
(`postgres(password = "${...}")`), and each of `secrets`. A product that
declares neither keeps the rule it had before 1.6 and is not checked.
`${VAR:-default}` is not a variable here and stays refused where GBX0116 applies:
the default half is a live value.

A `${VAR}` is substituted as text, so it fits a string field and an enum field
(the gear checks the variant it reads at start). In a boolean or number field it
stays **GBX0113**.

**Substituted when the process starts.** A generated application whose gears'
configuration reads a variable gets `src/config_env.rs` (template
`config_env.rs`) and a `serde_json` dependency, and its entry point substitutes
every `${NAME}` in each `gears.<id>.config` section from the process
environment before the runtime reads it, in one pass (a value is not expanded
again). The runtime's own `database` section is left to it. A variable that is
not set stops the process with an error naming each missing variable, where it
is read (`gears.oidc-authn-plugin.config.jwt.trusted_issuers[0].issuer`) and the
configuration file; a value is never logged. A host substitutes in memory, after
`--print-config` and the dump flags, so those never show a value. A worker, whose
runtime loads the file itself, writes the substituted sections under
`server.home_dir/.gearbox-rendered-gears/` (readable by its user alone) and hands
them to the runtime as `APP__GEARS_DIR` before its runtime starts. Being in the
binary, it works the same in every profile. An application whose gears read no
variable is generated as before.

**On Kubernetes** every `${NAME}` in a process's generated configuration is
already in its subchart's `secretKeys`, injected from the Secret its
`existingSecret` names: the deployment writes each capability's value and each
of `secrets` into that Secret under the variable's name.

### What the lock and the JSON say

`gearbox product --format json` (evaluation only) carries the choice:

```json
"infra": [{"name": "keycloak", "source": "studio-web",
           "image": "quay.io/keycloak/keycloak:26.0.7@sha256:4f3c...",
           "config": {"KC_PROXY_HEADERS": "xforwarded"},
           "profile_config": {"dev": {"KC_HEALTH_ENABLED": "false"}}}],
"secrets": ["SMTP_PASSWORD"]
```

`gearbox resolve --format json` (and `product.lock`) carries it resolved for the
profile, with each passport:

```json
"infra": {
  "services": [{
    "name": "keycloak", "source": "studio-web",
    "gdl_path": "infra/keycloak/service.gdl", "passport_hash": "<blake3 of the file>",
    "passport": {"name": "keycloak", "kind": "infra", "ports": {...}, "provides": {...}, ...},
    "image": {"reference": "quay.io/keycloak/keycloak:26.0.7@sha256:4f3c...", "digest": "sha256:4f3c..."},
    "chart": "oci://...", "chart_version": "24.4.0",
    "config": {"KC_HEALTH_ENABLED": "false", "KC_PROXY_HEADERS": "xforwarded", ...},
    "overridden": ["KC_HEALTH_ENABLED", "KC_PROXY_HEADERS"]
  }],
  "capabilities": {
    "oidc.issuer": {"service": "keycloak", "env": "OIDC_ISSUER", "template": "{public_url}/realms/product"}
  },
  "needs": [
    {"need": "oidc", "capability": "oidc.issuer", "gears": ["oidc-authn-plugin"], "met_by": "keycloak"},
    {"need": "postgres", "capability": "postgres", "gears": ["settings-service"], "met_by": "postgresql"}
  ]
},
"secrets": ["SMTP_PASSWORD"]
```

- `image.digest` is the digest the reference names. Gearbox reaches no registry,
  so a reference without one is locked without a digest; the deployment that
  pulls the image records it.
- `passport_hash` and `gdl_path` say which passport the lock was resolved
  against.
- `needs` says what the profile's gears need running beside them and which
  declared service meets each: a gear that keeps data needs `postgres`; the
  plugins `*oidc-authn-plugin`, `*keycloak-idp-plugin`,
  `*clickhouse-usage-collector-plugin` and `*timescaledb-usage-collector-plugin`
  need `oidc.issuer`, `keycloak.admin_url`, `clickhouse.url` and
  `timescaledb.url`. An unmet need has no `met_by` and is not an error: it may be
  met outside the product.
- A product without `infra` has no `infra` section, and its lock is what it was.

The lock diff (`gearbox_lock::diff`) names a change by service: `infra.keycloak.image`,
`infra.keycloak.image.digest`, `infra.keycloak.config.<KEY>`,
`infra.capabilities.<capability>`, `infra.needs.<need>`, and `secrets`.

## External database (GDL 1.1)

Each `embedded`, `self_hosted` and `kubernetes` profile accepts an optional
`database = postgres(...)`. The connection is deployment intent, independent of
cluster provider options and of each gear's own `config` bag.

```python
embedded(
    id = "dev",
    database = postgres(
        host = "127.0.0.1",
        user = "app",
        password = "${GEARS_PG_PASSWORD}",
        port = 5432,
        params = {"sslmode": "disable"},
        database_names = {"resource-group": "resource_group"},
    ),
)
```

`host`, `user` and `password` are required. The port defaults to 5432 and must be
1..65535. Passwords must be a single `${ENV_NAME}` reference (uppercase ASCII,
digits and underscores, not starting with a digit); literals and `${VAR:-default}`
are refused. Resolution and generation never expand the reference.

`params` is a string-to-string map of runtime Postgres connection parameters.
`database_names` overrides database names by selected DB-gear id; entries for
unselected or non-DB gears are errors. Otherwise the name replaces the gear id's
hyphens with underscores. Names must be unique, 1..63 lowercase ASCII letters,
digits or underscores and start with a letter or underscore. Long ids require an
override; names are never silently truncated or shared.

A selected gear with projected `cap.db` requires the selected profile to configure
a database (**GBX0306**). This includes gears pulled in by dependency closure.
The lock records every resolved database name; generation writes
`database.servers.pg` and `gears.<id>.database = {server: pg, dbname: ...}` only in
processes that execute the DB gear. A host entry that only spawns a DB worker
gets no connection for that worker.

For Kubernetes add `existing_secret = "app-db"` to `postgres(...)`. The existing
Secret must contain a key with the same name as the password environment variable.
Each DB-using subchart exposes `database.existingSecret` and `database.passwordEnv`
in its values/schema and injects that key through `secretKeyRef`, independently
of cluster-provider `existingSecret`/`secretKeys`. Generated files contain only
references. The operator provisions Postgres, each named database and the Secret;
Gearbox creates none of them and bundles no database chart.

### Per-gear SQLite platform databases

Every profile also accepts `sqlite_databases`, mapping selected DB-gear ids to
relative files. This explicitly selects SQLite for those gears; remaining DB
gears use the profile's Postgres connection. A SQLite-only product needs no
Postgres declaration.

```python
kubernetes(
    id = "demo", discovery = "static", namespace = "gearbox-demo",
    database = postgres(
        host = "host.minikube.internal", user = "demo",
        password = "${GEARS_PG_PASSWORD}", existing_secret = "demo-db",
    ),
    sqlite_databases = {"event-broker": "metadata.sqlite3"},
)
```

The file lives below `server.home_dir/<gear-id>/`: in this example,
`/var/lib/gearbox/event-broker/metadata.sqlite3` in Kubernetes. Absolute paths,
parent traversal, DSNs, environment references and `:memory:` are refused.
The gear's generated database section contains only `engine: sqlite` and `file`;
it inherits no Postgres connection fields. SQLite gears are excluded from
`database_names` and Postgres password injection. Explicitly naming the same
gear in both mappings is an error. Cargo enables the drivers actually used by
each application. IR, canonical locks, lock diffs and generated TypeScript
record the SQLite selection. Empty mappings are omitted, preserving old locks.

A SQLite gear must have exactly one application and one replica. Its Helm
values default to `Recreate` and `homeVolume.type: persistentVolumeClaim`.
Set `homeVolume.claimName` to an existing claim before rendering or deployment.
The generated values schema rejects `emptyDir`, a missing claim, multiple
replicas and a rolling update strategy for that application. The operator
provisions and retains the PVC; Gearbox never removes it on regeneration.

This extends GDL 1.0 additively: older descriptions still parse; DB-backed products
must add a connection before they can resolve successfully. Generated locks use
schema 2. Schema-1 locks are refused with instructions to run `gearbox resolve`;
there is no implicit upgrade or change to their recorded hash.

---

## Namespaces

A missing member is an attribute error, not a string.

Namespaces of values. Both file kinds see all five:

```
cap.db | cap.rest | cap.rest_host | cap.stateful | cap.system | cap.grpc_hub | cap.grpc
transport.local | transport.rest | transport.grpc
contract_kind.api | contract_kind.embedded | contract_kind.backend | contract_kind.extension
cluster_cap.linearizable | cluster_cap.watch | cluster_cap.prefix_watch
binding_mode.auto | binding_mode.local | binding_mode.remote
```

Namespaces of functions, one per file kind. `cluster.*` in `gear.gdl`, `prefer.*` in `product.gdl`:

```
cluster.cache(...) | cluster.leader_election(...) | cluster.lock(...)
prefer.existing_infrastructure() | prefer.fewer_applications() | prefer.isolate(...)
```

`cluster_cap.watch` and `cluster_cap.prefix_watch` apply only to `cluster.cache`; a lock and an election take `linearizable` and nothing else. Asking for either on either is an error, and the message names what does apply.

`cap.*` and `contract_kind.*` have no legal use site. The three arguments that would take them, `gear(runtime_caps = ...)`, `provide(kind = ...)` and `consume(kind = ...)`, are accepted only to be refused by name (**GBX0210**), because those facts are projected from Rust. The members resolve so that the refusal is about the argument rather than about the attribute.

---

## Versions

A file can declare the version of GDL that it uses. Write a literal assignment at the top of the file:

```python
gdl_version = "1.2"

gear(...)  # or product(...)
```

The value is `"MAJOR.MINOR"`. If a file does not declare a version, the version is `1.0`. This Gearbox reads GDL 1.6: 1.5 added the FrontX frontend and `frontx.gdl`; 1.6 adds `service.gdl`, `product(infra = ..., secrets = ...)` and variables in a gear's configuration. The legacy `gear(gdl_version=...)` and `product(gdl_version=...)` arguments remain accepted; conflicting declarations are rejected.

- A minor version only adds: optional fields, constructors and values. A file for an older minor version reads with no change.
- A major version can remove a field, rename a field or change what a field means.

Gearbox reads the version before it evaluates the file. Starlark finds unknown names before any call runs, so `gear()` cannot read the version itself.

| The file declares | Result |
|---|---|
| a different major version | **GBX0123**. Gearbox does not evaluate the file. |
| the same major version, the same or an older minor version | Gearbox evaluates the file. |
| the same major version, a newer minor version | Gearbox evaluates the file. If the evaluation fails, the help text tells you to update Gearbox. |
| a value that is not `MAJOR.MINOR`, for example `"1"` or `"v1.0"` | **GBX0123**. |

---

## `gear.gdl`

Constructors first; then the one `gear(...)` call.

A `gear.gdl` can be the only file that a reader has: in Studio, in a registry, or in a different repository. Thus it tells where everything is:

- the code: `package = cargo(...)`, and `source = git(...)` for the repository;
- the SDKs, in all languages: `sdk = [...]`;
- the documents and the tests: `docs(...)` and `tests = [...]`;
- the status: `maturity` and `implementation`.

Gearbox projects the other facts from Rust. Do not write them here (**GBX0210**).

To see all the fields, use the [full template](#full-template). In an editor, completion shows the fields that you did not write yet, and hover shows what each field does.

Notation: `name` is required; `name = X` has a default; `name?` may be omitted.

### `cargo(...)` → cargo

```
cargo(crate_name, lib, path = ".", features = [], default_features = True, link = [], attr?)
```

`lib` is the Cargo library ident and is never derived (`cf-api-contracts` → `cf_api_contracts`). `path` is relative to the description. `link` is extra `use … as _` idents; empty means just `lib`. `attr` narrows the `#[toolkit::gear]` scan to one file, written relative to the crate root (`src/gear.rs`, not `gear.rs`); omit to scan `src/` and require exactly one.

### `npm(...)`, `nuget(...)`, `go(...)` → package in another language

```
npm(name, path = ".")
nuget(id, path = ".")
go(module, path = ".")
```

These constructors tell where an SDK in another language is. Each one has the name that its ecosystem uses and a path relative to the description. Gearbox does not read these packages. It records them in the catalogue, and Studio shows them.

A gear's `sdk` accepts one package or a list of packages, in any language:

```python
sdk = [
    cargo(crate_name = "cf-pricing-sdk", lib = "pricing_sdk", path = "../pricing-sdk"),
    npm(name = "@cf/pricing-sdk", path = "../pricing-sdk-ts"),
    nuget(id = "Cf.Pricing.Sdk", path = "../pricing-sdk-dotnet"),
    go(module = "github.com/cf/pricing-sdk-go"),
]
```

Rules:

- A list can have one `cargo(...)` at most. The projector reads that crate for the traits of the extension points. Two `cargo(...)` packages are an error.
- `package` is always `cargo(...)`. Gearbox reads the code of a gear only from Rust. If `package` has a package in another language, the error tells you to put it in `sdk`.
- There is no `language` field. The language comes from the constructor.
- To add an ecosystem, add one constructor and one value of `Ecosystem`.
### `docs(...)` → docs

```
docs(prd?, design?, adr = [], openapi?)
```

Write `docs(...)` only when the documents are not at the usual `docs/` beside the gear or one level up. Each value is a path relative to the description, or a URL (`https://...`).

Gearbox finds a written value in this order:

1. A URL. Gearbox keeps the URL as it is.
2. A file on disk, relative to the description. A file on disk always wins.
3. The same path in the gear's `source = git(...)`. Gearbox makes a link to it.

If Gearbox cannot find the value, the result is **GBX0109**. A file that the `docs/` convention does not find is not an error.

### `config(...)` → config

```
config(rust?, exposes = [])
```

`rust` is the config struct path, and it is optional because the projector can usually find the struct itself, from the single `ctx.config*()` call in `impl Gear::init`. Declare it only when that search is ambiguous. `exposes` is which fields to show, in order. A name that is not a field of the struct is an error later, at projection.

### `feature(...)` → cargo feature

```
feature(name, kinds = [])
```

One Cargo feature the gear offers, for `gear(cargo_features = [...])`. `name` is positional and must be a key of the crate's own `[features]` table. A name that is not is **GBX0213**, the same check `exposes` gets against its struct.

`kinds` says which deployment kinds the feature belongs to: `embedded`, `self_hosted`, `kubernetes`. Empty (the usual case) means every kind. A non-empty list says two things at once: the feature is offered for those kinds and **refused** for the rest, which is **GBX0316** when a product selects it anyway. `k8s-auth` is why: a Kubernetes deployment needs it and a local one must not have it, and `Cargo.toml` has no way to say so.

`cargo_features` distinguishes absent from empty. Absent means nobody has curated this gear and a client falls back to the crate's whole `[features]` table, labelled uncurated. `cargo_features = []` is a curation that offers nothing, which is the right answer for a crate whose only feature gates its own tests.

### `helm_chart(...)` → unit chart

```
helm_chart(path, image, gears)
```

For `gear(deploy = ...)` (GDL 1.3): the hand-written chart of the executable unit
this gear anchors. `path` is `path(...)`, relative to the description; `image` is
the repository the unit's Dockerfile builds, without a registry or tag; `gears`
are the non-plugin gear ids its binary links, this gear included.

On a `kubernetes` profile, an application anchored on this gear that links
exactly `gears` (plugins aside) and needs no database **ships this chart**: the
generator copies it, with its `file://` dependencies, into the product's umbrella
under the application's name as an alias, and writes no crate or Dockerfile for
it -- the unit's own image runs it, tagged with the product version. Its values
are the image, the description's configuration of its gears as `configOverlay`,
and `secretEnv` for `${VAR}` placeholders backed by the description's existing
Secret. Ports, listeners and hardening are the chart's, and the chart checks its
production rules after merging the overlay. Any other application anchored on the
gear gets a generated chart.

A `path` with no `Chart.yaml` is reported when the catalogue loads (GBX0126) and
the chart is not used: the unit gets a generated chart, as if none were declared.

### `endpoint(...)` → endpoint

```
endpoint(name, config_key?, default_port?, via?)
```

`default_port` is 0–65535. `via` marks a gear mounted on another host (usually `"rest_host"`) rather than binding itself. `config_key` is the field the gear reads its listen address from: a REST host reads `bind_addr`, a gRPC hub reads `listen_addr`. Generation writes the resolved address there.

### `rest(...)` → rest

```
rest(base_path, require_full_coverage = False, visibility?)
```

`visibility` is `"exposed"` or `"internal"`; omit means exposed.

### `grpc(...)` → grpc

```
grpc(package, service, stubs_module)
```

### `provide(...)` → provide

```
provide(contract, rust, sdk, local?, rest?, grpc?, policies = [])
```

`contract` is the trait name (join key against `#[toolkit::contract]`). `rust` is the versioned trait path (`payments_audit_sdk::PaymentsAuditApi`). `sdk` is a `cargo(...)`. Do not pass `version`, `kind`, or `transports`; that is **GBX0210**.

### `consume(...)` → consume

```
consume(contract, rust, sdk, critical = False, resolving_client?)
```

`critical` gates readiness. Which gear provides it is **not** written here: `#[toolkit::consumes(from = "...")]` owns that, and the runtime reads the attribute's spelling as a directory key. Do not pass `from_`, `version` or `kind`; that is **GBX0210**.

### `cluster_plugin(...)` → cluster_plugin

```
cluster_plugin(package, process_local = False, needs_credentials = False, backend?,
               cache_options?, leader_election_options?, lock_options?, credential_option?)
```

`package` is a `cargo(...)`: a locator plus the flags Rust does not state. Providers themselves are projected. `backend` is an optional path to the impl, relative to the plugin crate.

`*_options` names the struct a primitive's options are deserialized into — a **join key**, not a schema. The struct is already the authority (`#[serde(deny_unknown_fields)]`); what Rust has no way to state is which type the `&Map` handed to `build_cache` is read as, because that is named only inside the function body. Given, the fields, their types, their enum variants and their defaults are projected and an option key is checked against them (**GBX0521** if the name resolves to nothing, **GBX0522** for a key the backend does not read, **GBX0523** for a value the field cannot take, **GBX0524** for a required option nobody supplied). Omitted, the options stay the untyped bag they always were.

`credential_option` names the option that carries the backend's credential, for a plugin that needs one. Declared for the same reason: `connection_string` and `url` are plain `String`, so nothing in the type marks them. A literal under it is **GBX0116**; write `secret_ref = "..."` on the `provider(...)`, or a value with `${VAR}` the backend expands at startup.

### `cluster.*` → cluster require

```
cluster.cache(profile, capabilities = [])
cluster.leader_election(profile, capabilities = [])
cluster.lock(profile, capabilities = [])
```

`profile` has no default. It must match `impl ClusterProfile { const NAME }` on the gear. A name no marker supplies is **GBX0508**, caught here rather than at startup, where it would be `ProfileNotBound`. The scope belongs to the product's coordination domain, not to the gear: the examples below have a gear requiring `event-broker` and a product binding it, which is the same join key seen from both sides.

### `role(...)` → role

```
role(name, directory_name?, labels = [])
```

`name` is the value the gear's own mode selector accepts, the only spelling checkable against a projected enum. `directory_name` is what an instance of this role registers under; omit it and lowering defaults to `<gear-id>-<name>`, where the id is known. `labels` are the label *keys* an instance registers under, not values: a value is per-instance and belongs to the deployment, which the runtime sources from configuration or the environment.

Parsed and stored. The runtime takes whatever directory name it is given; what cannot express a second role is the model here, one application per anchor gear, so a role nothing anchors has nowhere to go. That is **GBX0318** at resolution. Sharding and per-instance addressing are a separate gap, **GBX0602**. Both are warnings: the description is not wrong, it describes a shape this tool does not build yet.

### `extension_point(spec, trait = ..., sdk = ..., selector = ...)`

One point a host lets plugins fill. `spec` is positional and is the identity: the GTS spec's own
segment, without the `cf.toolkit.plugins.plugin.v1~` base every plugin spec shares. `trait` is the
interface plugins register under. `sdk` is written only when that trait lives outside the gear's
own `sdk`.

`selector` names the host's config field it picks a plugin by, as a dotted path: `selector =
"idp.vendor"`. Its default is read at that path, and a product overrides it there --
`config = {"idp": {"vendor": "keycloak"}}`. Omitted, the selector is the config's top-level
`vendor`. Write it when the config has more than one `vendor`: account-management selects its IdP
plugin by `idp.vendor` and registers itself as a tenant-resolver plugin under
`tr_plugin.vendor`. A path that leads nowhere, or two points naming different selectors, is
**GBX0516**.

```python
extension_points = [
    extension_point("cf.core.authn_resolver.plugin.v1~", trait = "AuthNResolverPluginClient"),
],
```

A plugin names the same segment: `implements = "cf.core.authn_resolver.plugin.v1~"`, and declares no
`sdk` -- the host's SDK is the host's.

Both are checked, not trusted:

- the spec must be a `PluginV1`-derived GTS type the gear's `sdk` declares, and the trait a
  `pub trait` in the crate the point names (GBX0516);
- an `implements` no described gear declares is GBX0519;
- a plugin whose crate implements none of the point's trait gets a warning (GBX0526).

Why declared: see ADR-0002, Amendment 2026-09-24.

### `git(...)` and `path(...)` → where something is

```
git(url, path?, tag?, rev?, branch?)
path("relative/dir")
```

`git(...)` tells a repository. `url` is `https://...` or `git@host:org/repo`. `path` is a directory in the repository, from its root, with `/` and without `..`. `tag`, `rev` and `branch` tell the version. Both file kinds use these constructors. Each file has its own rules, see `source` below and [Sources](#sources).

### `source = git(...)` on a gear

```python
source = git(url = "https://github.com/constructorfabric/gears-rust", path = "gears/bss/pricing/pricing"),
```

`source` tells the repository and the directory of the gear. A reader that has only the description uses it to find the code and the documents. `source` is optional. A pin (`tag`, `rev`, `branch`) is optional too, because `source` is an address and not a lock.

Gearbox finds each path in the description in this order:

1. **On disk**, relative to the description. This is the usual case: the description is in a checkout of its repository.
2. **In the git source.** If the path is not on disk, Gearbox makes a link to the same path in `source`.

If neither finds the path, the error names the two places.

Only `git(...)` is a source. `source = path(...)` is an error: paths are already relative to the description, and a checkout in a different directory is different on each computer.

When the description is in a git checkout, Gearbox compares `source` with the checkout:

- One remote of the checkout must be the repository of `url`, or a fork of it. A fork has the same host and repository name and a different owner: a checkout of `me/gears-rust` agrees with `url = "https://github.com/constructorfabric/gears-rust"`. Gearbox compares `https://host/org/repo`, `git@host:org/repo.git` and `ssh://...` as the same repository. An SSH remote through a host alias from `~/.ssh/config` (`git@github-me:me/gears-rust.git`, a host with no dot) is matched by repository name alone, since the alias does not say which host it dials.
- The directory of the description must be `path`.

A difference is **GBX0121**, a warning. The help text gives the correct `path`. A wrong `url` is reported once per checkout, not once per description. A tree without `.git` is not checked.

### `tests = [...]` on a gear

```python
tests = ["tests", "https://ci.example.com/pricing"],
```

Each value is a path relative to the description, or a URL. Gearbox finds a value in the same order as `docs(...)`. A path can be a file or a directory. A value that Gearbox cannot find is **GBX0109**.

### `scenario(...)` → scenario

```
scenario(text, priority?, done = False)
```

One thing that a user can do with the gear, and whether it is written. This is the same data as a line in `GEARS.md`: `- [x] p1 - enqueue and execute jobs` is `scenario("enqueue and execute jobs", priority = "p1", done = True)`.

`text` is positional and must not be empty. `priority` is `"p1"` (first) to `"p5"`. `done` is `False` until the scenario is written.

### `fail(message)`

Positional. States that this description is invalid. Not a branch.

### `gear(...)`

```
gear(
  gdl_version?,                     # legacy argument; prefer the file header
  deps?,                            # gear IDs; exact checked mirror of Rust deps
  maturity,                         # required, no default; refused at implementation = "design"
  implementation?,                  # "design" | "in-progress" | "done"
  package,                          # cargo, required unless implementation = "design"
  name?, description?, category?, visibility?,
  sdk?,                             # one package or a list, any language; one cargo at most
  source?,                          # git(url, path), where this gear lives
  scenarios = [],                   # scenario(...)
  tests = [],                       # paths or URLs
  extension_points = [],            # extension_point(...), points plugins fill
  implements?,                      # spec segment of the point this plugin implements
  docs?,                            # docs(...)
  provides = [],                    # provide(...)
  consumes = [],                    # consume(...)
  requires = [],                    # cluster.*(...)
  serves = [],                      # endpoint(...)
  rest_planes?,                     # "tenant" | "platform"; absent is read as ["tenant"]
  deploy?,                          # helm_chart(...), the unit this gear anchors
  cluster_plugins = [],             # cluster_plugin(...)
  roles = [],                       # role(...), recorded but not resolved
  config_schema?,                   # config(...)
  cargo_features?,                  # feature(...), curated; absent is not []
)
```

`visibility` is `"public"` or `"internal"`; omit means internal. `category` should be one of:

```
api-ingress, bss, core-functionality, core-platform-integration,
example, gen-ai, oss, serverless
```

Anything else is **GBX0108** (warning, not error).

`deps = ["types-registry", ...]` explicitly documents co-location dependencies.
It must exactly match `#[toolkit::gear(deps = [types_registry, ...])]`, using gear
IDs in kebab-case. Ordering is immaterial; duplicate/invalid IDs are rejected.
Missing or extra dependencies are **GBX0214** and block generation. Rust still
supplies the actual linking edges. Omission remains supported for legacy
projection-only descriptions; `deps = []` explicitly asserts no dependencies.
Contract consumption (`consumes`) remains separate and may cross processes.

`rest_planes` (GDL 1.3) says which authentication planes the gear's REST operations use:
`"tenant"` (a tenant bearer token) and `"platform"` (the platform token,
`.platform()`). Anonymous operations need no validator and are not a plane.
`rest_planes = []` is a gear whose REST capability serves only probes, as
cluster's does. Absent is not empty: it is read as `["tenant"]`, because the
runtime refuses to start a process that serves tenant operations with no tenant
authenticator, and an undeclared gear may. An application whose REST gears are
tenant (declared or not) and that links no `authn-resolver` is **GBX0417**. The
field is an authored claim; the runtime's start check is what verifies it.

These fields are accepted only to be refused by name (**GBX0210**). They live in Rust:

| Field | Owner |
|---|---|
| `id` | `#[toolkit::gear(name = ...)]` |
| `runtime_caps` | `#[toolkit::gear(capabilities = [...])]` |
| `colocated_deps` | `#[toolkit::gear(deps = [...])]` |
| `lifecycle` | `#[toolkit::gear(lifecycle(...))]` |
| `client` | `#[toolkit::gear(client = ...)]` |
| `cluster_providers` | `ClusterGear::provider_registry()` |

`lifecycle(...)` exists as a constructor (`entry?`, `stop_timeout?`, `await_ready = False`) so a restatement can be named. Passing it to `gear()` is still **GBX0210**.

### `maturity` and `implementation`: two axes of status

A gear has two statuses. They are independent.

| Field | Tells | Values |
|---|---|---|
| `maturity` | what the gear promises to its users | `experimental`, `preview`, `stable`, `deprecated` |
| `implementation` | how much of the gear is written | `design`, `in-progress`, `done` |

A `stable` gear can be `in-progress`: it is safe to use, and more work is planned. An `experimental` gear can be `done`.

`maturity` is required on a gear with code. It has **no default**. A default of `stable` makes a forgotten field into a promise that nobody made. A lower default makes it into an accusation. Thus the author writes the value. If the value is missing or wrong, the description does not evaluate.

| `maturity` | Means | In a product |
|---|---|---|
| `"experimental"` | code exists; the API and the behaviour can change | **GBX0322**, warning |
| `"preview"` | usable, not declared stable | **GBX0323**, info |
| `"stable"` | supported for usual production use | nothing |
| `"deprecated"` | still available, not for new products | **GBX0324**, warning |

A product gets a message for each gear below `stable` that it links, also for the gears that it did not select. A co-located dependency or a plugin is in the same binary, and the message tells what pulled it in. `validate` reports only the selections, because it does not resolve. No level stops a lock or generation.

`implementation` is optional. If it is not written, the description makes no statement about it.

| `implementation` | Means | In the catalogue |
|---|---|---|
| `"design"` | documents, possibly an SDK; no crate yet | `Catalogue.designs`; a product that selects it is **GBX0321**, error |
| `"in-progress"` | code exists, and work continues | `gears` |
| `"done"` | all that the gear sets out to do is written | `gears` |

Scenarios give the detail of `implementation`. If a gear is `"done"` and a scenario is not `done`, the result is **GBX0122**, a warning. One of the two is out of date, and only the author knows which.

A new gear from the scaffold is `maturity = "experimental"`, `implementation = "in-progress"`.

`maturity = "design"` is an error. Write `implementation = "design"` and no `maturity`.

### `implementation = "design"`: a gear with no code yet

You can describe a gear before its crate exists: what it is for, where its documents are, and its id. The catalogue lists it apart from the gears that a product can use (`Catalogue.designs`). Gearbox does not project it. A product that names it is **GBX0321**, not GBX0301, because the gear is not a typo and not in a closed source root.

```python
gear(
    implementation = "design",
    id = "approval-service",
    name = "Approval Service",
    description = "Multi-step approvals for tenant operations.",
    category = "core-functionality",
    sdk = cargo(crate_name = "cf-approval-sdk", lib = "approval_sdk", path = "approval-sdk"),  # when one exists
    scenarios = [scenario("approve a tenant operation in several steps", priority = "p1")],
)
```

The rules:

- `id` is required, and must be a gear id. This is the only place where you write an id. A gear with code gets its id from Rust, and an id written there is **GBX0210**.
- `maturity` is an error. A gear with no code promises nothing.
- A design gear can write `name`, `description`, `category`, `sdk`, `source`, `docs` and `scenarios`. All other fields tell about code, for example `package`, `tests`, `implements`, `extension_points`, `provides`, `serves` and `visibility`. They are errors.
- Gearbox finds the documents with the same convention as for a gear with code.
- If a gear with code has the same id as a design, the result is **GBX0105**, the same code as for a gear declared two times. Delete the design description.

This is what replaced the platform's `gear.toml` for the gears that had only documents
(ADR-0002, Amendment 2026-10-02).

### Full template

This template shows all the fields of a `gear.gdl`. A field that the gear does not use stays as a comment. To use a field, remove the `#` at the start of its lines. Do not write an empty field as `None`: the file stays short, and this template, completion and hover show what you can add.

```python
# Every field: https://github.com/constructorfabric/gearbox/blob/main/docs/gdl.md

gdl_version = "1.2"

gear(
    # What it is.
    name = "Pricing",
    deps = [],  # Must match #[toolkit::gear(deps = [...])].
    # description = "What this gear does, in one sentence.",
    # category = "bss",
    # visibility = "public",

    # Status: two axes.
    maturity = "experimental",            # experimental | preview | stable | deprecated
    implementation = "in-progress",       # design | in-progress | done

    # Where it is.
    package = cargo(crate_name = "cf-bss-pricing", lib = "bss_pricing", path = "."),
    # source = git(url = "https://github.com/<org>/<repo>", path = "gears/bss/pricing/pricing"),
    # sdk = [
    #     cargo(crate_name = "cf-bss-pricing-sdk", lib = "bss_pricing_sdk", path = "../pricing-sdk"),
    #     npm(name = "@cf/bss-pricing-sdk", path = "../pricing-sdk-ts"),
    # ],
    # docs = docs(prd = "docs/PRD.md", design = "docs/DESIGN.md", openapi = "https://..."),
    # tests = ["tests"],

    # What a user can do with it.
    # scenarios = [
    #     scenario("calculate a price for a subscription", priority = "p1", done = True),
    # ],

    # Plugins: a host declares points; a plugin implements one.
    # extension_points = [extension_point("cf.bss.pricing.plugin.v1~", trait = "PricingPluginClient")],
    # implements = "cf.bss.rate_provider_source.plugin.v1~",

    # Contracts, cluster, endpoints, roles.
    # provides = [provide(contract = "PricingApi", rust = "bss_pricing_sdk::PricingApi", sdk = ...)],
    # consumes = [consume(contract = "TenantApi", rust = "...", sdk = ...)],
    # requires = [cluster.cache(profile = "event-broker")],
    # serves = [endpoint(name = "rest", via = "rest_host")],
    # cluster_plugins = [cluster_plugin(package = cargo(...))],
    # roles = [role(name = "worker")],

    # What an integrator sees.
    # config_schema = config(exposes = ["bind_addr"]),
    # cargo_features = [feature("otel")],
)
```

A design gear uses `implementation = "design"`, an `id`, and no `maturity` and no `package`. See [`implementation = "design"`](#implementation--design-a-gear-with-no-code-yet).

---

## `product.gdl`

Every deployment profile is data. Resolve chooses one with `--profile`. Scope a `bind` / `cluster_profile` / `application` / `plugin` with `profiles = ["dev", "prod"]`. Empty `profiles` means all declared profiles. Two declarations that cover the same subject in the same profile are **GBX0110**. A `profiles` entry naming an id the file does not declare is **GBX0102**; **GBX0111** is the other direction: `--profile` asking for a profile this product has none of.

Positional constructors in a product file: `path`, `registry`, `plugin`, `use_gear`, `use_service`, `application`, `provider`. Everything else is keyword-only. (`gear.gdl` adds `feature` and `fail`.)

### Sources

```
path("relative/dir") -> source-at
git(url, tag?, rev?, branch?) -> source-at          # a product source needs one of tag / rev / branch
registry("crates.io", prefix?) -> source-at         # the registry, not one package
source(id, at, crates?) -> source                   # id is kebab-case
```

A product source is a full repository, and Gearbox builds a lock from it. Thus `source(at = git(...))` must have a pin, and it must not have `path`. A `path(...)` source is found from the description: `resolve`, `generate` and `validate --product` scan a `--root` at that directory under the declared id (not its directory's name, unless `--source-id` named it), and open the directory themselves when no `--root` is at it, inside it or around it. A `git(...)` source still arrives as a `--root`. A gear's `source = git(...)` has different rules, see [`source = git(...)` on a gear](#source--git-on-a-gear).

`crates = registry("crates.io")` on a `path(...)` source says its gears are also published. Descriptions still come from the checkout (a published crate carries no `gear.gdl`), and the generated build names each crate by its exact version instead of a path. Generation checks every crate against the commit its release was cut from (`.cargo_vcs_info.json`):

- as published: `version = "=X"`;
- changed since: the same version, plus a `[patch.crates-io]` entry pointing at the checkout, so one copy links (**GBX0709**);
- not published at that version: a path dependency (**GBX0710**);
- registry unreachable: every crate stays a path dependency (**GBX0711**).

Path dependencies reached from patched crates or from path gears, `workspace = true` ones included, are patched too, so each crate appears only once in the build. `crates` on a `git(...)` or `registry(...)` source is refused.

**The product's own `Cargo.lock` comes first.** A product whose own gears depend on published platform crates has already chosen the platform release: its lock pins it. `gearbox generate` names every crate it writes (a gear, the toolkit, a client SDK) that this lock pins from crates.io at exactly one version as `version = "=X"`, and every one it pins from a git repository at one commit as `git = "<url>", rev = "<commit>"`, with no `[patch]` back to a checkout, and seeds the tree with that lock. The checkout the gears were described from is never the build's copy of a crate the lock pins. A gear the lock does not pin that would reach a pinned crate by path is taken from where those crates come from (crates.io, or that repository and commit) at the checkout's version, when the checkout's requirements on them are met by the locked versions; otherwise it keeps its path dependency (or what `crates = registry(...)` decided). The lock is the one `--cargo-lock` or `GEARBOX_CARGO_LOCK` names, else the output root's `Cargo.lock`, else that of the product's one `path(...)` source that has one, else the one lock nearest above a gear's crate, in any source, that pins a crate the build names from crates.io or git: a build service fetches every source by `git(...)`, the product's own code included, and the platform's own workspace lock pins none of its members from outside, so only the product code's lock qualifies. `--gears-from source` (or `GEARBOX_GEARS_FROM=source`) ignores it; `--gears-from registry` also warns about every gear it could not pin. **GBX0713** reports how many gears came from where; **GBX0714** a crate locked at two versions, or a gear left on source that reaches a pinned crate by path and would link a second copy.

`git` and `source` are keyword-only. `path` / `registry` take the first string positionally. `path(...)` is relative to the product file. `git` with only `branch` pins a moving line; accepted, recorded as such.

A `registry` source is the registry itself rather than one package, so a product naming six gears from it fetches six packages and everything they depend on through one declaration. `prefix` turns a gear id into a package name: `api-gateway` under `prefix = "cf-gears-"` is `cf-gears-api-gateway`. `use_gear(package = ...)` is the exit for a gear that does not follow the house naming. The fetching is cargo's: a synthesised manifest plus `cargo metadata`, which is also what buys authentication, offline mode and whatever mirror the machine is configured with.

### Deployment profiles

```
embedded(id, database?, development = False)
self_hosted(id, host, worker_discovery, target_dir?, cargo_profile?, database?, platform_auth?, development = False)
kubernetes(id, discovery, namespace?, image_registry?, database?, platform_auth?, development = False)
```

Every profile is a **production** target unless it says `development = True`
(GDL 1.3). A production profile is held to the production rules before anything
is generated (**GBX0325**, gears-rust ADR-0011 §1/§3): the gateway's public listener
stays exposed-only, a multi-process profile has its kind's platform auth
(`"kube"` on `kubernetes`, `"shared_secret"` on `self_hosted`), an `embedded` hub binds no TCP, a hub socket is not in `/tmp`,
a hub enforces platform auth, and tenant AuthN is not `static-authn-plugin` in
`accept_all` mode. `development = True` reports the same findings as warnings and
keeps the generator's development defaults.

`platform_auth` (GDL 1.3) is how the processes of a multi-process profile
authenticate each other on the platform plane: `"kube"` (a projected
ServiceAccount token validated through TokenReview; `kubernetes` only) or
`"shared_secret"` (one secret, one identity). Gearbox writes it into
`api-gateway`'s `internal_auth`, which the generator carries to every hub and
worker:

- `"kube"`: `{provider = "kube", audiences = ["toolkit-internal"], token_path =
  "/var/run/secrets/tokens/<audience>"}`; the generated chart mounts that token in
  every unit and grants the validators TokenReview.
- `"shared_secret"`: `{provider = "shared_secret", secret =
  "${TOOLKIT_PLATFORM_SECRET}", peer_name = "<product>-platform"}`, and that label
  joins `service-discovery`'s `trusted_registrars`, since every holder of the secret
  is the same identity. Every `internal_auth` block of every process names the same
  variable, which the toolkit reads from the environment when the configuration
  loads: a self-hosted host sets it once and the workers it spawns inherit it. An
  unset variable fails startup. On Kubernetes a generated chart maps it from the
  Secret its `existingSecret` value names, like any other `${VAR}`.

Fields the gateway's own `internal_auth` sets (`audiences`, `peer_name`, ...) are
kept over these defaults. A `provider` there that names another kind, or a
product without `api-gateway`, is **GBX0326**: the description gives one answer.
A profile without `platform_auth` keeps whatever the gateway's configuration
says, as before.

`id` is kebab-case. Duplicate ids are **GBX0110**. `worker_discovery` / `discovery` is `"static"` or `"directory"`, and nothing else. `host` is an application name, kebab-case. `cargo_profile` is a single path segment: `dev`, `release`, or a custom Cargo profile. `dev` writes under `target/debug`. `target_dir` is the Cargo target directory the host spawns workers out of, **as written** and relative to the description; a `self_hosted` profile without one cannot say where a worker binary is, which is **GBX0310**. On `kubernetes`, `namespace` and `image_registry` prefix what the chart deploys and where it pulls from; both are optional, recorded in the lock and used to derive the generated deployment artifacts.

### Gears

```
plugin("gear-id", config = {}, profiles = [])
use_gear("gear-id", source, version?, package?, features = [], config = {}, profile_config = {}, plugins = [])
```

Only the gear id is positional: `use_gear("api-gateway", source = "gears-rust")`. `source` names a `source(id = ...)`. `version` and `package` are registry-only; on `path`/`git` they are an error. `config` is a map of JSON-shaped values (string, number, bool, list, map; no `None`, no records; nesting ≤ 32). Keys must be fields the gear actually declares (**GBX0115**) and types must match (**GBX0113**). Both are checked against `config_schema`, so a gear that declares none has no field list to check against. A field Rust declares secret (`secrecy::SecretString`) may hold only `"${UPPER_NAME}"`, naming an environment variable the generator reads; a literal is **GBX0116**, and so is `"${VAR:-default}"`, because the default half is a live value. Any other string may read a variable too, substituted when the process starts (GDL 1.6, [Variables in a gear's configuration](#variables-in-a-gears-configuration)).

`profile_config` (GDL 1.3) maps a declared profile id to configuration merged over `config` when that profile is resolved: maps merge key by key, any other value (a list included) replaces. `use_gear("api-gateway", config = {"public": {...}}, profile_config = {"prod": {"internal_auth": {"audiences": ["payments"]}}})`. The merged result is what the production rules, the config checks and the lock see; an id that is not a declared profile is reported like any other undeclared profile.

`plugin` names an implementing gear. Which extension point it fills comes from the catalogue. The same implementation twice for one host in one profile is **GBX0110**.

Selected plugins follow their host into every application that contains that
host, including hosts pulled in as dependencies. Attachment determines process
membership; initialization order still follows the gears' actual dependencies.

### Bindings

```
bind(consumer, contract, mode, transport?, endpoint?, profiles = [])
```

`consumer` is a kebab gear id. `contract` looks like `api-contracts/PaymentApi@v1`. `mode` is a `binding_mode.*` member. `transport` is a `transport.*` member.

### Cluster

```
provider("name", secret_ref?, **options) -> provider
cluster_profile(name, cache, leader_election?, lock?, profiles = [])
```

`provider` is the only `**options` function. Options become a JSON map, keys sorted. `cache` is required. `name` is the operator side of `impl ClusterProfile { const NAME }`. Omit `leader_election` / `lock` to take the SDK compare-and-swap default over the cache. `secret_ref` says where a credential comes from: a provider the catalogue marks as needing one and given none is **GBX0506**, and the reference reaches the generated configuration, never the lock.

### Topology and preferences

```
application("name", anchor, include = [], role?, replicas = 1, profiles = [])
prefer.existing_infrastructure()
prefer.fewer_applications()
prefer.isolate(gear = "api-gateway")
```

Only the application name is positional: `application("audit", anchor = "api-contracts-consumer")`. `anchor` is a kebab gear id. `role` names one of the anchor gear's `role(...)` declarations; omit it for the gear's default shape. `replicas` must be ≥ 1.

For a REST host anchor, a pin without `role` names and configures the host
application itself. It does not create a second worker containing that host.

`include` co-locates additional gears already present in the product closure,
including their dependencies and selected plugins. For example,
`application("settings-worker", anchor = "settings-service", include = ["authn-resolver"])`
gives the worker its own authentication stack for bearer re-validation. This
does not add a lifecycle dependency to the Settings gear. An unknown or
unselected included gear is refused before generation.
An explicitly included worker gear is not also seeded into the default host.
It can still appear in both processes when a host dependency or host `include`
requires it there.
An included gear is placed by the `include`: a consumer elsewhere does not also
give it a process of its own (only its own `application(...)` pin or a
`prefer.isolate` does). That consumer reaches it in the including worker, which
keeps a directory record under the gear's own name (`oop_http.directory_gears`;
the directory admits the worker for it through `registrar_gears`).

### `product(...)`

```
product(
  id, version,
  sources,                          # source(...), at least the ones use_gear names
  profiles,                         # embedded / self_hosted / kubernetes; at least one
  default_profile,                  # must name a declared profile
  gears,                            # use_gear(...)
  name?,                            # display name; omit → id
  templates?,                       # path("...") only; git/registry refused
  layout?,                          # one directory name; omit → "apps"
  bindings = [],
  cluster_profiles = [],
  applications = [],
  preferences = [],
  permissions = {},                 # GDL 1.4: permission -> scopes that grant it
  infra = [],                       # GDL 1.6: use_service(...), infra services beside the gears
  secrets = [],                     # GDL 1.6: variables the configuration reads that no service provides
)
```

Omit `templates` to use a `templates/` directory beside the file. A declared
directory that does not exist is refused; the conventional one is created.

**Generation writes every template it renders into that directory**, as
`<key>.jinja` -- `helm/deployment.yaml.jinja`, `docker/Dockerfile.jinja`,
`ui/nginx.conf.jinja` and the rest of the keys in
`crates/gearbox-engine/src/generate/templates.rs` -- and renders from those copies.
To change a generated file, edit its template; to go back to the builtin, delete
the copy. Beside each copy, `.base/<key>.jinja` is the builtin it was last written
from, which is how a later build tells an edit from its own change:

| the copy | a new build's builtin |
|---|---|
| is missing | is written |
| is its base: nobody edited it | replaces it |
| was edited | is merged into it three ways; the edit stays |
| was edited where the builtin changed too | does not reach it: the copy is kept and renders as it is, **GBX0712** |
| has no base, and is not the builtin | does not reach it, **GBX0712** |

A copy that differs from the current builtin is reported as an override
(**GBX0708**), in the CLI's output and in Studio's Generate view, which lists every
template with its state. `--dry-run` and Studio's plan write nothing; an apply
writes the copies after the tree, and not at all when the apply is refused. Keep
`.base/` with the copies: without it an edited copy can no longer follow the
builtin. A file for a key this build does not render is kept and unused.

`permissions` (GDL 1.4) maps each abstract permission a gear's operations declare
(`.permission("settings.values.write")`, gears-rust `cpt-cf-adr-operation-permissions`)
to the token scopes that grant it, any of them:
`permissions = {"settings.values.write": ["operator", "bootstrap"], "lab.view": ["*"]}`.
`["*"]` grants the permission to every authenticated caller, `[]` to nobody. The
generator writes the map into every process's configuration, and the runtime refuses
a call to an operation whose permission is not in it. A gear in the product that
declares a permission the map does not name is **GBX0124** (an error on a production
profile, a warning under `development = True`); a mapped permission no gear declares
is **GBX0125**; a gear that names a permission other than as a string literal is
**GBX0609**, because its set cannot be read. A name is dot-separated segments of
`[a-z0-9_-]`, at least two; a malformed name, or a scope that is not a non-empty
string, is refused when the file is evaluated.

`layout` names the directory the generated application crates go under, one
path segment: `apps/<name>/Cargo.toml` by default, `layout = "processes"` for a
checkout generated before it was configurable. It is recorded in the lock, so
changing it changes `lock_hash`. Generation has no delete path, so the old
directory stays where it is and **GBX0706** names it.

---

## `load()`

```
load("path.gdl", "NAME")
load("//path.gdl", LOCAL = "NAME")
```

`//...` is from the source root. Anything else is relative to the loading file. No absolute paths. `..` may not climb out of the root (**GBX0104**); a symlink that lands outside is the same error.

A fragment is ordinary GDL: same syntax, same forbidden constructs. It binds names (`SDK = cargo(...)`). It must not call `gear()` / `product()`; that is **GBX0102**, reported against the fragment. Names starting with `_` are private and cannot be loaded. Loaded names are not re-exported: a third file cannot `load` them from the file that loaded them.

Cycles are an error. A fragment larger than 1 MiB is an error. The evaluator is capped too, at 64 call frames, 4 MiB of heap and 100 000 ticks, which a declarative description cannot reach and a runaway `load()` can.

---

## Not GDL

Refused as **GBX0103**:

```
if  elif  else  for  def  lambda  and  or  not  in
break  continue  return  pass
```

Refused as a parse error **GBX0101** (reserved):

```
as assert async await class del except finally from global
import is nonlocal raise try while with yield
```

`len`, `dict`, `range`, string methods and the arithmetic operators are Starlark's own, and the host keeps its standard set, so they remain callable. They are not GDL vocabulary: nothing refuses them, and nothing needs them either.

---

## Diagnostics a description can raise

The right-hand column is the stage that reports it. Only the first group needs nothing but the file; the rest need the projected catalogue, which is why a bad `config` key evaluates clean and is refused later, where the gear's schema is in scope.

This table is the authoring subset, curated and hand-written, because the stage that reports a code is not something the catalogue knows. Every code the engine can emit is in [the generated reference](diagnostics.md), which cannot fall behind the declaration it is generated from.

| Code | When | Reported by |
|---|---|---|
| GBX0101 | syntax error | evaluating the file |
| GBX0102 | evaluation error (bad type, missing required arg, …) | evaluating the file |
| GBX0103 | forbidden construct | evaluating the file |
| GBX0104 | `load()` leaves the source root | evaluating the file |
| GBX0105 | no `gear()`/`product()`, or more than one | evaluating the file |
| GBX0106 | unknown argument | evaluating the file |
| GBX0110 | two profile-scoped declarations collide | evaluating the file |
| GBX0210 | restates a fact Rust already owns | evaluating the file |
| GBX0108 | `category` is not one the platform uses (warning) | building the catalogue |
| GBX0123 | `gdl_version` is not `MAJOR.MINOR`, or has a different major version | evaluating the file |
| GBX0124 | a gear in the product declares a permission `product(permissions = ...)` does not map (warning under `development = True`) | resolving |
| GBX0125 | `product(permissions = ...)` maps a permission no gear in the product declares (warning) | resolving |
| GBX0109 | a `docs(...)` or `tests` path does not exist, and no `source = git(...)` | building the catalogue |
| GBX0121 | `source = git(...)` is different from the checkout (warning) | building the catalogue |
| GBX0122 | `implementation = "done"` with a scenario that is not done (warning) | building the catalogue |
| GBX0112 | `config_schema` names no usable struct | building the catalogue |
| GBX0212 | `exposes` names a field the struct does not declare | building the catalogue |
| GBX0213 | `cargo_features` names a feature the crate does not declare | building the catalogue |
| GBX0113 | `config` value has the wrong type | validate / resolve |
| GBX0114 | `config` key is derived from topology (warning) | validate / resolve |
| GBX0115 | `config` key the gear does not declare | validate / resolve |
| GBX0116 | a credential is written into the file | validate / resolve |
| GBX0127 | `use_service` names no `kind = "infra"` passport in its source | resolve |
| GBX0128 | two infra services provide the same capability | resolve |
| GBX0129 | an infra service's `config` sets a key its passport does not declare, a secret, or a `${...}` | resolve |
| GBX0130 | a gear's configuration reads a `${NAME}` the product does not declare (once it declares `infra` or `secrets`) | validate / resolve |
| GBX0111 | `--profile` asked for a profile the file does not declare | resolve |
| GBX0316 | a selected feature belongs to another deployment kind | resolve |
| GBX0506 | a provider needs a credential and the scope names no source | resolve |
| GBX0508 | a `cluster_profile` name no selected gear implements | resolve |
| GBX0318 | a gear's roles cannot all be deployed (warning) | resolve |
| GBX0602 | sharding and per-instance addressing are not generated (warning) | resolve |
| GBX0706 | a generated crate directory the product no longer builds (warning) | generate |
| GBX0722 | a FrontX package's `frontx.gdl` disagrees with its `package.json` or `mfe.json` on identity, pins or screens | generate |
| GBX0723 | a FrontX package's `frontx.gdl` drifts from its manifests on a softer fact (warning) | generate |

---

## Examples

`gear.gdl`:

```
AUDIT_SDK = cargo(
    crate_name = "cf-gears-payments-audit-sdk",
    lib = "payments_audit_sdk",
    path = "../payments-audit-sdk",
    features = ["rest-client"],
)
PAYMENT_SDK = cargo(
    crate_name = "cf-api-contracts-sdk",
    lib = "api_contracts_sdk",
    path = "../../../examples/toolkit/api-contracts/api-contracts-sdk",
    features = ["rest-client"],
)

gear(
    name = "Payments Audit",
    maturity = "experimental",
    implementation = "in-progress",
    category = "example",
    visibility = "public",
    package = cargo(crate_name = "cf-gears-payments-audit", lib = "payments_audit", path = "."),
    sdk = [AUDIT_SDK, npm(name = "@cf/payments-audit-sdk", path = "../payments-audit-sdk-ts")],
    source = git(url = "https://github.com/constructorfabric/gears-rust",
                 path = "examples/payments-audit/payments-audit"),
    tests = ["tests"],
    scenarios = [
        scenario("record every payment for audit", priority = "p1", done = True),
        scenario("export the audit trail", priority = "p2"),
    ],
    provides = [
        provide(
            contract = "PaymentsAuditApi",
            rust = "payments_audit_sdk::PaymentsAuditApi",
            sdk = AUDIT_SDK,
            local = "Self::build_local",
            rest = rest(base_path = "/api/v1/payments-audit"),
        ),
    ],
    consumes = [
        consume(
            contract = "PaymentApi",
            rust = "api_contracts_sdk::PaymentApi",
            sdk = PAYMENT_SDK,
            critical = False,
        ),
    ],
    requires = [
        cluster.cache(profile = "event-broker", capabilities = [cluster_cap.linearizable]),
        cluster.leader_election(profile = "event-broker"),
    ],
    serves = [endpoint(name = "rest", via = "rest_host")],
    cargo_features = [
        feature("otel"),
        feature("k8s-auth", kinds = ["kubernetes"]),
    ],
)
```

`product.gdl`:

```
gdl_version = "1.4"

product(
    id = "payments-demo",
    name = "Payments Demo",
    version = "0.1.0",
    layout = "apps",
    sources = [source(id = "gears-rust", at = path("../../../gears-rust"))],
    # What each permission a gear declares is granted to (GDL 1.4): here the
    # Directory listing, to any authenticated caller.
    permissions = {"directory.read": ["*"]},
    # `dev` and `local` are development profiles: accept-all AuthN and no
    # platform auth are allowed (GBX0325 as warnings). `prod` is a production
    # profile, its platform auth given by the profile -- see "Deployment profiles".
    profiles = [
        embedded(id = "dev", development = True,
                 database = postgres(host = "127.0.0.1", user = "postgres",
                    password = "${GEARS_PG_PASSWORD}", params = {"sslmode": "disable"})),
        self_hosted(id = "local", development = True, host = "gateway", worker_discovery = "directory",
                    target_dir = "../../../gears-rust/target",
                    database = postgres(host = "127.0.0.1", user = "postgres",
                        password = "${GEARS_PG_PASSWORD}", params = {"sslmode": "disable"})),
        kubernetes(id = "prod", platform_auth = "kube", discovery = "static", namespace = "payments",
                    database = postgres(host = "postgres.example.com", user = "payments",
                        password = "${GEARS_PG_PASSWORD}", existing_secret = "payments-db")),
    ],
    default_profile = "dev",
    gears = [
        use_gear("api-gateway", source = "gears-rust"),
        use_gear("service-discovery", source = "gears-rust"),
        use_gear("api-contracts", source = "gears-rust"),
        use_gear("api-contracts-consumer", source = "gears-rust"),
        use_gear("cluster", source = "gears-rust"),
        use_gear("authn-resolver", source = "gears-rust", plugins = [
            plugin("static-authn-plugin", profiles = ["dev", "local"],
                   config = {"mode": "accept_all"}),
            plugin("oidc-authn-plugin", profiles = ["prod"],
                   config = {"jwt": {"trusted_issuers": [{"issuer": "https://id.example.com"}],
                                     "expected_audience": ["payments"],
                                     "claim_mapping": {"subject_id": "sub",
                                                       "subject_tenant_id": "tenant_id",
                                                       "token_scopes": "scope"}},
                             "s2s_oauth": {"discovery_url": "https://id.example.com",
                                           "default_subject_type": "gts.cf.core.security.subject_user.v1~"}}),
        ]),
    ],
    bindings = [
        bind(
            consumer = "api-contracts-consumer",
            contract = "api-contracts/PaymentApi@v1",
            mode = binding_mode.remote,
            transport = transport.rest,
            profiles = ["local", "prod"],
        ),
    ],
    cluster_profiles = [
        cluster_profile(name = "event-broker", cache = provider("standalone"),
                        profiles = ["dev"]),
        cluster_profile(
            name = "event-broker",
            profiles = ["local", "prod"],
            cache = provider("postgres", connection_string = "postgres://${PG_HOST}/payments",
                             schema = "cluster", secret_ref = "env:PG_PASSWORD"),
        ),
    ],
    applications = [
        application("api-contracts", anchor = "api-contracts", include = ["authn-resolver"],
                    profiles = ["local", "prod"]),
        application("audit", anchor = "api-contracts-consumer", include = ["authn-resolver"],
                    replicas = 2, profiles = ["prod"]),
    ],
    preferences = [prefer.existing_infrastructure(), prefer.fewer_applications()],
)
```

---

Evaluated today by a locked-down host. This file is the language, not the host. The
vocabulary it describes lives in code beside it:

| | |
|---|---|
| `crates/gearbox-gdl/src/globals.rs` | the `gear.gdl` vocabulary |
| `crates/gearbox-gdl/src/product.rs` | the `product.gdl` vocabulary |
| `crates/gearbox-gdl/src/frontx.rs` | the `frontx.gdl` vocabulary |
| `crates/gearbox-gdl/src/service.rs` | the `service.gdl` vocabulary |
| `crates/gearbox-gdl/src/vocabulary.rs` | the closed value sets |
| `crates/gearbox-gdl/src/declarative.rs` | the locked-down Starlark dialect and the refused keywords |

The editor's syntax vocabulary is generated from those same globals into
`ide/gearbox-studio/src/browser/gdl/generated/vocabulary.ts` by `make grammar`, so the
highlighter cannot colour a function the engine does not have.

The two worked examples above are executed, not just printed:
`crates/gearbox-engine/tests/gdl_examples.rs` reads them out of this file and evaluates
them, resolving the product one against the `gears-rust` corpus when a checkout is there.

### UI deployment entry points

`ui_product(...)` accepts an optional `entry_points` dictionary of display names
and same-origin UI paths, for example `{"Operator": "/#operator", "My Lab": "/#my-lab"}`.
These paths are carried through the product lock and generated `ui/ui.lock.json`.
A registered deployment adapter supplies the actual endpoint origin after
readiness; Studio combines that origin with these declared paths. The paths do
not grant authorization or select a tenant.
