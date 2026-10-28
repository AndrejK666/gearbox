# Gearbox

Gearbox is the product composition and resolution layer for the Gears platform. You declare
which gears belong to a product and what deployment shape you want; the resolver derives the
concrete implementation: application topology, local or remote contract bindings, transports,
cluster provider selections, and the build and deployment artefacts that realise them.

Today those decisions are made by hand, repeatedly, and are not checkable. Product composition
is spread across Cargo features, a hand-maintained `registered_gears.rs`, runtime YAML, one
hand-written Helm chart, and developer knowledge of which gears can and cannot be separated.
Whether a contract binding is local or remote depends on placement; whether a placement is
even legal depends on the contract's kind. Each of those is statically decidable. The principle
is **configure intent, derive implementation**.

Gearbox is not a replacement for Cargo, Kubernetes or Helm, and not a second implementation of
Gears runtime semantics. It sits above them and composes them. No language model takes part in
resolution or in any assertion path.

## Status

Prototype. [docs/plans/prototype.md](docs/plans/prototype.md) builds a working vertical slice
of the architecture under one rule: GDL may only express what the Gears runtime implements
today, and anything the vision assumes but the runtime lacks is either refused or downgraded to
a diagnostic carrying a `file:line` citation, never silently faked. The PRD is still a draft
written before implementation. There is no CI in this repository.

[docs/conformance.md](docs/conformance.md) checks each documented claim against a running
Studio: 207 claims, 206 built, 0 not built, 0 broken, 1 not observed.

## The pipeline

```
   gear.gdl        product.gdl
       \               /
        v             v
     Starlark evaluation        (a constrained domain API, no conditionals)
              |
              v
         typed Rust IR
       catalogue + intent
              |
              v
    deterministic resolver ------> explanation graph
              |
              v
          product.lock
              |
     +--------+--------+-----------------+
     |        |        |                 |
     v        v        v                 v
   Cargo    config   Dockerfiles      Helm umbrella
   crates    YAML                     + values.schema.json
```

The CLI, the JSON-RPC server and Studio are clients of the same engine. None of them holds
product semantics of its own.

## Vocabulary

These words are used precisely, and not the way most projects use them.

- **gear** is one unit of code: a Rust crate carrying `#[toolkit::gear(...)]` with a `gear.gdl`
  beside it.
- **application** is one generated binary, the co-location closure of an anchor gear, deployed
  as a unit with its own `replicas`. Not the same as a process: an application with three
  replicas is three of them.
- **product** is a composition one team owns and ships, declared in a `product.gdl`.
- **installation** is the running whole at one site, into which several products are deployed.
- **deployment profile** is exactly one of `embedded`, `self-hosted` or `kubernetes`, plus a
  closed record of that kind's fields. A Gearbox concept, not a runtime type.
- **projected and declared facts** are different. A fact a Rust attribute already carries is
  read out of Rust, and a `gear.gdl` restating one is rejected
  ([ADR-0002](docs/ADR/0002-cpt-gearbox-adr-macro-projected-catalogue.md)). A `gear.gdl`
  declares only what has no home in Rust: display name, description, category, visibility,
  the Cargo and SDK locators, cluster requirements.

Fuller definitions are in [docs/PRD.md](docs/PRD.md) §1.4; the argument for the whole model is
[docs/vision.md](docs/vision.md).

## Layout

| Path | |
|---|---|
| `crates/gearbox-ir` | Canonical typed model: catalogue, intent, resolved product, diagnostics, explanation graph. |
| `crates/gearbox-gdl` | Gears Description Language: a constrained Starlark-hosted domain API evaluated into that model. |
| `crates/gearbox-project` | Projects gear facts out of Rust attributes, which own them. |
| `crates/gearbox-engine` | Workspace discovery, catalogue assembly, resolve and generate. The only crate that touches the filesystem. |
| `crates/gearbox-lock` | Canonical `product.lock` serialization, hashing and structural diff. |
| `crates/gearbox-rpc` | JSON-RPC over stdio with LSP framing: the engine's wire surface. |
| `crates/gearbox-cli` | The `gearbox` command-line interface. |
| `ide/` | Gearbox Studio, a Theia application. See [ide/README.md](ide/README.md). |
| `products/<name>/product.gdl` | Product descriptions. One level deep, one fixed filename, which is what Studio's product picker scans. |
| `tools/oop-run.sh` | The out-of-process acceptance run. |
| `.gearbox/` | Generated output. Reproducible from `product.lock`, so not tracked. |

## Prerequisites

The gear corpus is a sibling checkout, and nothing works without it:

```
projects/
├── gearbox/       # this repository
└── gears-rust/    # the gears, on branch feature/gearbox
```

The branch matters: a `gears-rust` checkout on anything but `feature/gearbox` has no `gear.gdl`
in it at all. Engine tests that need the corpus skip themselves when it is missing, so
`make test` is only full signal with it present.

- Rust 1.97.0 with `rustfmt` and `clippy`, pinned in `rust-toolchain.toml`. Edition 2024.
- `cargo-nextest` 0.9.130 or newer and `cargo-deny` 0.20.0 or newer. `make setup` installs both.
- Node `^24 || >=26` for Studio (`ide/.nvmrc` is 26). This repeats Theia 1.75's own tested
  matrix; 25 is excluded as a non-LTS line.
- `npx playwright install chromium`, once, for the conformance suite.

```sh
make setup
make build
```

## The CLI

`cargo build -p gearbox-cli` produces `target/debug/gearbox`.

```
catalogue   scan source roots for gear.gdl files and print the catalogue
validate    everything checkable without resolving
product     evaluate a product.gdl and print the intent it declares
plugins     extension points, and the implementations available for them
resolve     resolve one deployment profile and print the lock
generate    resolve and write artefacts under .gearbox/
lock        ask questions of a written product.lock
rpc         serve JSON-RPC over stdio, for Studio and the .gdl language client
```

Worked through the demo product, each command answering a different question:

```sh
cargo run -p gearbox-cli -- catalogue --root ../gears-rust
cargo run -p gearbox-cli -- validate  --root ../gears-rust --product products/payments-demo/product.gdl
cargo run -p gearbox-cli -- resolve   --root ../gears-rust --product products/payments-demo/product.gdl --profile dev
cargo run -p gearbox-cli -- generate  --root ../gears-rust --product products/payments-demo/product.gdl --profile local --dry-run
```

`--root` is repeatable, and the first root to declare a gear wins
([ADR-0012](docs/ADR/0012-cpt-gearbox-adr-multiple-source-roots.md)). `--profile` defaults to
the product's own `default_profile`, which is `dev` here. `resolve` writes nothing, and
`--format toml` prints byte for byte what a lock file would contain. `generate` writes to
`.gearbox/<product>/<profile>/` and never into a source root. Any error diagnostic means a
non-zero exit.

Structured output goes to stdout and everything else to stderr, because `rpc --stdio` uses
stdout as its JSON-RPC channel.

A generated tree is self-contained: its own `Cargo.toml`, `rust-toolchain.toml`, `.cargo/config.toml`,
one `config/<application>.yaml` per application, and the `product.lock` it was generated from.

```sh
cd .gearbox/payments-demo/local && cargo build
```

The binaries it produces are named `gbx-<application>`.

## The demo product

[products/payments-demo/product.gdl](products/payments-demo/product.gdl) declares all three
deployment profiles as data and selects one at resolve time. Abbreviated:

```python
product(
    id = "payments-demo", name = "Payments Demo", version = "0.1.0",
    sources = [source(id = "gears-rust", at = path("../../../gears-rust"))],
    profiles = [
        embedded(id = "dev"),
        self_hosted(id = "local", host = "gateway", worker_discovery = "directory"),
        kubernetes(id = "prod", discovery = "static", namespace = "payments"),
    ],
    default_profile = "dev",
    gears = [
        use_gear("api-gateway", source = "gears-rust"),
        # and five more, including the plugin choices authn-resolver routes to
    ],
    bindings = [
        bind(consumer = "api-contracts-consumer",
             contract = "api-contracts/PaymentApi@v1",
             mode = binding_mode.remote, transport = transport.rest,
             profiles = ["local", "prod"]),
    ],
    preferences = [prefer.existing_infrastructure(), prefer.fewer_applications()],
)
```

Profile scoping is a `profiles = [...]` field on `bind`, `plugin`, `cluster_profile` and
`application`, not an `if`. GDL has no conditionals by design: a description that could branch
on the resolve target would be a program whose output depends on how it was invoked.

One description therefore yields a different lock per profile. In `dev` the whole closure is
one process and every binding is local; the `bind` above applies only to `local` and `prod`,
where the gears end up in separate processes. The description states a request, and the
resolver records what it resolved to beside it. That is the promise the system rests on:
deployment topology changes composition and binding, not business code.

The GDL reference is [docs/gdl.md](docs/gdl.md): the grammar, every parameter signature for
both file kinds, and the two worked examples, which
`crates/gearbox-engine/tests/gdl_examples.rs` reads out of it and evaluates.

## Checks

```sh
make check    # fmt, clippy, lint, deny, test, and the three anti-drift guards
make test     # cargo nextest run --workspace
make dev      # fix what can be fixed: dev-fmt, dev-clippy, test
```

One convention trips everybody once: `fmt` and `clippy` **check**, while `dev-fmt` and
`dev-clippy` **fix**. `make check` does not run the browser suite.

Several tests read a real `gears-rust` checkout: the golden outputs of `products/`, the GDL
examples and the error-enum corpus. They find it beside this repository or at
`GEARBOX_CORPUS_ROOT`, and without one they print "skipping" and pass. Set
`GEARBOX_CORPUS_REQUIRED=1` to make a missing corpus a failure. CI
([.github/workflows/ci.yml](.github/workflows/ci.yml)) runs `make check` that way, against
`gears-rust`'s current `main`, on every pull request and nightly.

Four things in the tree are generated rather than written. For the first three, `make check`
regenerates them and fails if anything changed, so the editor cannot drift from the engine:

| Output | Regenerate with |
|---|---|
| `docs/diagnostics.md` and Studio's diagnostic catalogue | `make diagnostics` |
| Studio's TypeScript bindings | `make ts` |
| Studio's `.gdl` grammar vocabulary | `make grammar` |
| `docs/conformance.md` | `cd ide && npm run conformance` |

`docs/conformance.md` is the exception: the Playwright run writes it, and `make check` does not
look at it.

### Studio

```sh
nvm use
cargo build -p gearbox-cli      # the engine the backend spawns
cd ide && npm ci
npm run plugins                 # once: fetches the VS Code git extension
npm run build
npm run start:browser           # http://127.0.0.1:3000
```

`GEARBOX_ENGINE` and `GEARBOX_ROOT` override the engine binary and the source root. Studio
opens a multi-root workspace, because the two repositories are siblings and no single folder
contains both. The AI chat takes its key from one of two places. Studio's own setting,
`gearbox.ai.apiKey` (**Gearbox: Settings**), wins; clearing it falls back to
`ANTHROPIC_API_KEY` in the backend's environment. `npm run start:browser` loads a `.env` at
the repo root (see [.env.example](.env.example)) for that fallback -- nothing else reads it,
and a variable already exported in the shell always beats the file. The chat is optional:
resolving, generating and every diagnostic work without a key, and the chat says so rather
than failing.

**A corporate CA is a runtime concern too, not just an `npm install` one.** The backend
reaches the API through Node's global `fetch`, so a broken trust store surfaces in the chat
as the bare string `Connection error.` -- the Anthropic SDK's message for a rejected
request, which carries no status to explain itself. Use `NODE_EXTRA_CA_CERTS`, which appends
to Node's bundled roots. Two ways to get this wrong, both of which empty or replace the
store rather than extending it, and both of which produce exactly that message:
`NODE_OPTIONS=--use-openssl-ca` on a Node that bundles its own OpenSSL with no CA store
configured, and `SSL_CERT_FILE` pointing at a single corporate root, which on Node >= 22
replaces the whole store. `npm run start:browser` detects the first and warns about the
second; **Gearbox: Check AI Connection** reports what the backend actually sees.

[ide/README.md](ide/README.md) covers the rest, including the two native modules that must
compile and the corporate CA `npm install` needs behind a TLS-intercepting proxy.

### The conformance suite

```sh
cd ide
npm run conformance   # Playwright; starts the app itself
npm run verify        # engine, smokes, build, and the suite
```

Two gates stop the run before any test does. The frontend bundle must be newer than
`gearbox-studio/src`, so run `npm run build` after touching either side (`make ts` rewrites
generated sources and trips this too). And `git status --porcelain -- products` must be empty:
the suite writes product descriptions and restores them, so it refuses to start over changes it
cannot tell apart from its own.

The run rewrites `docs/conformance.md`, then fails if the number of collected claims is not the
pinned count, because a claim that stops being collected proves nothing. A filtered run (`-g`)
leaves a partial table behind as a result; `--reporter=line` runs a subset without writing the
table at all.

### The out-of-process run

```sh
make oop-run
```

Generates the `local` profile, builds both binaries, starts the host and lets it spawn the
worker, then waits for `readiness: dependency resolved` in the log. Only a binding looked up in
the directory reaches that line, which is what makes it proof rather than a log message. No
database and no Docker. Ports 8087, 8090 and 50051 must be free; the log goes to
`/tmp/gearbox-oop-run.log`.

## Documents

| | |
|---|---|
| [docs/vision.md](docs/vision.md) | The architecture and the argument for it, with "as implemented" notes where the build went another way. |
| [docs/PRD.md](docs/PRD.md) | Requirements, actors, and the glossary. |
| [docs/plans/prototype.md](docs/plans/prototype.md) | The vertical slice: milestones, and a section of honest gaps. |
| [docs/gdl.md](docs/gdl.md) | The GDL reference: grammar, every parameter signature, and the examples the tests execute. |
| [docs/diagnostics.md](docs/diagnostics.md) | Every `GBX` code. Generated from the catalogue that declares them. |
| [docs/conformance.md](docs/conformance.md) | Documented claims against a running Studio. Generated by the Playwright run. |
| [docs/cargo-gears-comparison.md](docs/cargo-gears-comparison.md) | Why this is not `cargo-gears`. In Russian. |
| [docs/ADR/](docs/ADR/) | Decisions. Numbering starts at 0002: 0001 and 0003 to 0008 live in `gears-rust`. |

Licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE).
