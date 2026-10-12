# GDL

GDL (Gears Description Language) is a declarative DSL. A file states facts. The resolver decides.

Two file kinds, two vocabularies. `gear()` is not callable in a product file; `product()` is not callable in a gear file.

| File | Declares | Required call |
|---|---|---|
| `gear.gdl` | one gear: facts Rust does not already own | exactly one `gear(...)` |
| `product.gdl` | one product: operator intent | exactly one `product(...)` |

Zero or two top-level declarations is **GBX0105**. Profile-specific behaviour is a `profiles = [...]` list on the declaration, not a branch.

Arguments are keyword-only except for eight positional ones: `feature` and `fail` in a `gear.gdl`, and `path`, `registry`, `plugin`, `use_gear`, `application`, `provider` in a `product.gdl`. An unknown **named** argument is **GBX0106**; a stray positional one is **GBX0102**, because it never reaches the argument name at all. Records returned by constructors (`cargo(...)`, `provide(...)`, …) are opaque: pass them on, do not inspect them.

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

**Strings.** `"..."` or `'...'`. Triple quotes `"""..."""` / `'''...'''` for multi-line. Prefix `r` for raw (backslashes are not escapes). Escapes in non-raw strings: `\\ \` \' \a \b \f \n \r \t \v`, octal, `\xHH`, `\uHHHH`, `\UHHHHHHHH`. No f-strings. `${NAME}` inside a string is not interpolation. GDL copies it through as text. What reads it later is a contract, not a coincidence: a config field Rust declares secret may carry nothing else (**GBX0116**).

**Numbers.** Decimal integers, `0x` / `0o` / `0b`, floats.

**Booleans / null.** `True`, `False`, `None`.

**The one keyword.** `load`. `crate` is not a legal parameter name, so cargo uses `crate_name`.

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

## `gear.gdl`

Constructors first; then the one `gear(...)` call.

Notation: `name` is required; `name = X` has a default; `name?` may be omitted.

### `cargo(...)` → cargo

```
cargo(crate_name, lib, path = ".", features = [], default_features = True, link = [], attr?)
```

`lib` is the Cargo library ident and is never derived (`cf-api-contracts` → `cf_api_contracts`). `path` is relative to the description. `link` is extra `use … as _` idents; empty means just `lib`. `attr` narrows the `#[toolkit::gear]` scan to one file, written relative to the crate root (`src/gear.rs`, not `gear.rs`); omit to scan `src/` and require exactly one.

### `docs(...)` → docs

```
docs(prd?, design?, adr = [], openapi?)
```

Paths relative to the description. Only needed when documents are not at the usual `docs/` beside the gear or one level up. A written path that does not exist is **GBX0109**; a merely absent conventional file is not.

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

### `fail(message)`

Positional. States that this description is invalid. Not a branch.

### `gear(...)`

```
gear(
  maturity,                         # required, no default -- see below
  package,                          # cargo, required unless maturity = "design"
  name?, description?, category?, visibility?,
  sdk?,                             # cargo, where this gear's own SDK crate lives
  extension_points = [],            # extension_point(...), points plugins fill
  implements?,                      # spec segment of the point this plugin implements
  docs?,                            # docs(...)
  provides = [],                    # provide(...)
  consumes = [],                    # consume(...)
  requires = [],                    # cluster.*(...)
  serves = [],                      # endpoint(...)
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

### `maturity`: how much a gear promises

Required on every `gear(...)`, with **no default**. A default of `stable` would turn a forgotten
field into a promise nobody made, and any lower default into an accusation, so the author says
which. Missing or misspelt, the description does not evaluate.

| `maturity` | Means | In the catalogue | In a product |
|---|---|---|---|
| `"design"` | documents (perhaps an SDK), no crate yet | `Catalogue.designs` | **GBX0321**, error |
| `"experimental"` | code exists; API and behaviour may change freely | `gears` | **GBX0322**, warning |
| `"preview"` | usable, not declared stable | `gears` | **GBX0323**, info |
| `"stable"` | supported for ordinary production use | `gears` | nothing |
| `"deprecated"` | still available, not for new products | `gears` | **GBX0324**, warning |

A product is told about every gear it links below `stable`, including the ones it did not choose:
a co-located dependency or a plugin is in the same binary, and the message says what pulled it in.
`validate` reports the selections only, since it does not resolve. None of the four blocks a lock
or generation; `design` does, because there is nothing to link.

A newly scaffolded gear is `"experimental"`. There is no second axis for "how much code exists":
a gear with only an SDK is `"design"` with `sdk = cargo(...)`.

### `maturity = "design"`: a gear with no code yet

A gear can be described before its crate exists: what it is for, where its documents are, and
the id it will have. It is listed in the catalogue apart from the gears that can be used
(`Catalogue.designs`), nothing is projected, and a product that names it is **GBX0321** -- not
GBX0301, because the gear is neither a typo nor in a closed source root.

```python
gear(
    maturity = "design",
    id = "approval-service",
    name = "Approval Service",
    description = "Multi-step approvals for tenant operations.",
    category = "core-functionality",
    sdk = cargo(crate_name = "cf-approval-sdk", lib = "approval_sdk", path = "approval-sdk"),  # when one exists
)
```

The rules:

- `id` is required, and must be a gear id. It is the one place an id is declared: a stable
  gear's id is projected, and restating it stays **GBX0210**.
- `name`, `description`, `category`, `sdk` and `docs` are the whole vocabulary. Everything else
  describes code -- `package`, `implements`, `extension_points`, `provides`, `serves`,
  `visibility` and the rest -- and is refused at evaluation.
- Documents are found by the same convention as for any gear, beside the description.
- A design id that also has a gear with code is **GBX0105**, the same code as a gear declared
  twice: the design description has been outgrown and should be deleted.

This is what replaced the platform's `gear.toml` for the gears that had only documents
(ADR-0002, Amendment 2026-10-02).

---

## `product.gdl`

Every deployment profile is data. Resolve chooses one with `--profile`. Scope a `bind` / `cluster_profile` / `application` / `plugin` with `profiles = ["dev", "prod"]`. Empty `profiles` means all declared profiles. Two declarations that cover the same subject in the same profile are **GBX0110**. A `profiles` entry naming an id the file does not declare is **GBX0102**; **GBX0111** is the other direction: `--profile` asking for a profile this product has none of.

Positional constructors in a product file: `path`, `registry`, `plugin`, `use_gear`, `application`, `provider`. Everything else is keyword-only. (`gear.gdl` adds `feature` and `fail`.)

### Sources

```
path("relative/dir") -> source-at
git(url, tag?, rev?, branch?) -> source-at          # at least one of tag / rev / branch
registry("crates.io", prefix?) -> source-at         # the registry, not one package
source(id, at, crates?) -> source                   # id is kebab-case
```

`crates = registry("crates.io")` on a `path(...)` source says its gears are also published. Descriptions still come from the checkout (a published crate carries no `gear.gdl`), and the generated build names each crate by its exact version instead of a path. Generation checks every crate against the commit its release was cut from (`.cargo_vcs_info.json`):

- as published: `version = "=X"`;
- changed since: the same version, plus a `[patch.crates-io]` entry pointing at the checkout, so one copy links (**GBX0709**);
- not published at that version: a path dependency (**GBX0710**);
- registry unreachable: every crate stays a path dependency (**GBX0711**).

Path dependencies reached from patched crates or from path gears, `workspace = true` ones included, are patched too, so each crate appears only once in the build. `crates` on a `git(...)` or `registry(...)` source is refused.

`git` and `source` are keyword-only. `path` / `registry` take the first string positionally. `path(...)` is relative to the product file. `git` with only `branch` pins a moving line; accepted, recorded as such.

**Several sources, and which root is which.** The engine reads directories, never fetches: a caller checks each source out and passes one root per source. `--root ID=DIR` names the `source(id = ...)` a checkout is, which is how a `git(...)` source arrives (`gearbox resolve --root platform=/co/platform --root org=/co/org --product product.gdl`). A bare `--root DIR` is named after its directory, unless the product declares that same directory as a `path(...)` source, in which case it takes the product's id. The lock records a `git(...)` source as what it is: `kind = "git"`, the URL with the declared reference (`https://.../platform.git?tag=v1.2.0`), and as its digest the commit the checkout is at (`git:<sha>`, read from its `.git`; a checkout without one keeps the content digest).

**One gear id in two sources.** An organization's fork of a platform gear keeps the platform's id, so both sources declare it. That is not an error: the catalogue lists the second copy under `alternatives`, and the product picks with `use_gear(id, source = ...)`. A product that uses such a gear without naming one of its sources is **GBX0325**, also for a gear the closure pulls in, which then needs a `use_gear` of its own; naming an open source that lacks the gear is **GBX0326**. The same id twice in *one* source stays **GBX0105**. A version per gear is a source per version: two tags of one repository are two sources.

A `registry` source is the registry itself rather than one package, so a product naming six gears from it fetches six packages and everything they depend on through one declaration. `prefix` turns a gear id into a package name: `api-gateway` under `prefix = "cf-gears-"` is `cf-gears-api-gateway`. `use_gear(package = ...)` is the exit for a gear that does not follow the house naming. The fetching is cargo's: a synthesised manifest plus `cargo metadata`, which is also what buys authentication, offline mode and whatever mirror the machine is configured with.

### Deployment profiles

```
embedded(id)
self_hosted(id, host, worker_discovery, target_dir?, cargo_profile?)
kubernetes(id, discovery, namespace?, image_registry?)
```

`id` is kebab-case. Duplicate ids are **GBX0110**. `worker_discovery` / `discovery` is `"static"` or `"directory"`, and nothing else. `host` is an application name, kebab-case. `cargo_profile` is a single path segment: `dev`, `release`, or a custom Cargo profile. `dev` writes under `target/debug`. `target_dir` is the Cargo target directory the host spawns workers out of, **as written** and relative to the description; a `self_hosted` profile without one cannot say where a worker binary is, which is **GBX0310**. On `kubernetes`, `namespace` and `image_registry` prefix what the chart deploys and where it pulls from; both are optional and both end up in the generated values rather than in the lock.

### Gears

```
plugin("gear-id", config = {}, profiles = [])
use_gear("gear-id", source?, version?, package?, features = [], config = {}, plugins = [])
```

Only the gear id is positional: `use_gear("api-gateway", source = "gears-rust")`. `source` names a `source(id = ...)`, and decides which copy is used when several open sources declare the id. Omitted, or naming a source no open root carries, the gear comes from the one open source that declares it; two candidates are **GBX0325**. `version` and `package` are registry-only; on `path`/`git` they are an error. `config` is a map of JSON-shaped values (string, number, bool, list, map; no `None`, no records; nesting ≤ 32). Keys must be fields the gear actually declares (**GBX0115**) and types must match (**GBX0113**). Both are checked against `config_schema`, so a gear that declares none has no field list to check against. A field Rust declares secret (`secrecy::SecretString`) may hold only `"${UPPER_NAME}"`, naming an environment variable the generator reads; a literal is **GBX0116**, and so is `"${VAR:-default}"`, because the default half is a live value.

`plugin` names an implementing gear. Which extension point it fills comes from the catalogue. The same implementation twice for one host in one profile is **GBX0110**.

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
application("name", anchor, role?, replicas = 1, profiles = [])
prefer.existing_infrastructure()
prefer.fewer_applications()
prefer.isolate(gear = "api-gateway")
```

Only the application name is positional: `application("audit", anchor = "api-contracts-consumer")`. `anchor` is a kebab gear id. `role` names one of the anchor gear's `role(...)` declarations; omit it for the gear's default shape. `replicas` must be ≥ 1.

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
)
```

Omit `templates` to use a `templates/` directory beside the file.

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
| GBX0109 | a `docs(...)` path does not exist | building the catalogue |
| GBX0112 | `config_schema` names no usable struct | building the catalogue |
| GBX0212 | `exposes` names a field the struct does not declare | building the catalogue |
| GBX0213 | `cargo_features` names a feature the crate does not declare | building the catalogue |
| GBX0113 | `config` value has the wrong type | validate / resolve |
| GBX0114 | `config` key is derived from topology (warning) | validate / resolve |
| GBX0115 | `config` key the gear does not declare | validate / resolve |
| GBX0116 | a credential is written into the file | validate / resolve |
| GBX0111 | `--profile` asked for a profile the file does not declare | resolve |
| GBX0316 | a selected feature belongs to another deployment kind | resolve |
| GBX0325 | several sources declare a gear and the product names none of them | validate / resolve |
| GBX0326 | `use_gear(source = ...)` names an open source that lacks the gear | validate / resolve |
| GBX0506 | a provider needs a credential and the scope names no source | resolve |
| GBX0508 | a `cluster_profile` name no selected gear implements | resolve |
| GBX0318 | a gear's roles cannot all be deployed (warning) | resolve |
| GBX0602 | sharding and per-instance addressing are not generated (warning) | resolve |
| GBX0706 | a generated crate directory the product no longer builds (warning) | generate |

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
    category = "example",
    visibility = "public",
    package = cargo(crate_name = "cf-gears-payments-audit", lib = "payments_audit", path = "."),
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
product(
    id = "payments-demo",
    name = "Payments Demo",
    version = "0.1.0",
    layout = "apps",
    sources = [source(id = "gears-rust", at = path("../../../gears-rust"))],
    profiles = [
        embedded(id = "dev"),
        self_hosted(id = "local", host = "gateway", worker_discovery = "directory",
                    target_dir = "../../../gears-rust/target"),
        kubernetes(id = "prod", discovery = "static", namespace = "payments"),
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
                   config = {"issuer": "https://id.example.com"}),
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
    applications = [application("audit", anchor = "api-contracts-consumer", replicas = 2,
                         profiles = ["prod"])],
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
| `crates/gearbox-gdl/src/vocabulary.rs` | the closed value sets |
| `crates/gearbox-gdl/src/declarative.rs` | the locked-down Starlark dialect and the refused keywords |

The editor's syntax vocabulary is generated from those same globals into
`ide/gearbox-studio/src/browser/gdl/generated/vocabulary.ts` by `make grammar`, so the
highlighter cannot colour a function the engine does not have.

The two worked examples above are executed, not just printed:
`crates/gearbox-engine/tests/gdl_examples.rs` reads them out of this file and evaluates
them, resolving the product one against the `gears-rust` corpus when a checkout is there.
