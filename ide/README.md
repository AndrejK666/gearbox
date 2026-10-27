# Gearbox Studio (Eclipse Theia)

## Native macOS ARM64 application

From this directory, use Node 24 and run `npm ci`, then `npm run package:mac`.
The local application is `../dist/desktop/mac-arm64/Gearbox.app`. It contains
production Theia/Electron, the optimized release Gearbox CLI, and the official
standalone Darwin ARM64 Node build matching the packaging Node version. Packaging
checks the Node archive against the vendor's published SHA256 list. Native npm
modules are rebuilt for Electron. The local app is unsigned; distribution
signing/notarization is a separate release step.

## Native Windows x64 application

From an *x64 Native Tools Command Prompt* (node-gyp finds a standalone Windows
SDK only through `vcvars64.bat`'s `WindowsSDKVersion`), with Node 24 and the
MSVC Spectre-mitigated libraries installed: `npm ci`, `npm run plugins`, then
`npm run package:win`. The installer is `../dist/desktop/Gearbox Setup 0.1.0.exe`
(NSIS, per-user, unsigned) and `../dist/desktop/win-unpacked/Gearbox.exe` runs
without installing. It carries the same parts as the macOS app, with Windows'
native title bar.

## No machine paths in a packaged app

Neither desktop build carries a path from the machine that built it. On first
start Studio asks for a gears checkout -- or clones
`gearbox.defaultSourceRepository` -- and a workspace, and keeps them in the
`gearbox.sourceRoots` and `gearbox.workspace` settings; **Gearbox: Configure
Sources** asks again. Run from a checkout, Studio still uses the repository it
runs in and `../gears-rust` (or `GEARBOX_ROOT`) unless those settings are set.

The editor front end for the Gearbox engine. Domain views today: **Start**, **Catalogue**,
**Product**, **Inspector**, **Graph**, **Conflicts**, **Lock**, **Generate** and the **AI
Chat** (ADR-0017), which answers from the resolver's own output through typed read-only tools. Two working
contexts are live — **Home** and **Product** — derived from what is actually open; **Gear** is
declared and reserved until New Gear lands. The terminal is withdrawn (ADR-0011): the package
stays for dependents, the shell asserts its absence. Conformance prefers visible Start and header
buttons over palette-only command paths.

## Running it

Node 26 (see `.nvmrc`); 24 is supported too. Theia 1.75 asks for `>= 24` in its own `doc/Developing.md`, and its
`ci-cd.yml` builds only `[24.x, 26.x]`, which is what `engines.node` here repeats. The `@theia/*`
packages declare no `engines` of their own, so nothing catches a wrong runtime for you.

```bash
nvm use                               # or any Node 26 (24 also works)
cargo build -p gearbox-cli            # the engine the backend spawns
cd ide && npm ci
npm run plugins                       # once: fetches the VS Code git extension
npm run build
npm run start:browser                 # http://127.0.0.1:3000
```

`make ide` from the repository root wraps that sequence (engine build, frontend build, start),
prints the URL and opens it in a browser once the server actually answers (`tools/ide-open.sh`
polls it in the background). `make ide-release` does the same with a minified production bundle
(`theia build --mode production`, via `npm run build:release`) instead of the development one
`build` produces; `start:browser` is otherwise identical, since `theia start` just serves whichever
bundle is on disk. Both pass `ANTHROPIC_API_KEY` through if it is set, e.g.
`make ide ANTHROPIC_API_KEY=sk-...`.

`start:browser` goes through `scripts/start-studio.mjs`, which loads a `.env` at the repository
root (see `.env.example`) for `ANTHROPIC_API_KEY` and refuses to pass on a `NODE_OPTIONS` that
leaves Node with no CA store. A variable already exported in the shell always wins over the file.

The AI chat is optional: resolving, generating and diagnostics work without
it. When an installed Claude Code is signed in, the Gearbox chat uses
that existing login through Anthropic's Agent SDK. Gearbox never reads or copies
the OAuth token; Claude Code manages Keychain access and refresh. The CLI is found
in the usual native installation or the installed Anthropic VS Code extension;
`GEARBOX_CLAUDE_BIN` can select an absolute CLI path. Built-in filesystem/shell
tools, user/project settings and persisted SDK sessions are disabled. Only the
current request's Gearbox tools are exposed through an in-process MCP server;
Theia's existing confirmation and product-edit preview still govern writes.

The existing Anthropic API provider remains a fallback. Its key comes from
**Gearbox: Settings** (`gearbox.ai.apiKey`) or `ANTHROPIC_API_KEY` in the backend
environment. **Gearbox: Check AI Connection** still probes that API transport,
not Claude Code's subscription login. For an explicit live SDK/tool integration
check, run `node scripts/claude-code-smoke.cjs --live`; it consumes a model request
through the existing login and prints no credential or account details.

Studio opens its own workspace: the repository root plus every source root the engine reports. That
is what gives the Explorer something to browse, lets a generated `product.lock` be opened at all, and
lets git find any repositories -- the extension walks workspace folders. It is **multi-root** because
the two repositories are siblings, so no single folder contains both.

The backend finds the engine at `<repo>/target/debug/gearbox` and the source root at
`../gears-rust`, both overridable:

```bash
GEARBOX_ENGINE=/path/to/gearbox GEARBOX_ROOT=/path/to/gears npm run start:browser
```

## Checking it

```bash
npm run engine       # cargo build -p gearbox-cli -- the binary the backend spawns
npm run smoke        # JSON-RPC over real vscode-jsonrpc framing, no browser
npm run conformance  # Playwright; starts the app itself
npm run wedge        # Playwright again, on a second server whose engine can be wedged
npm run verify       # all of the above, plus the build and two more smokes
```

The browser is one explicit step, once: `npx playwright install chromium`. Playwright has no
postinstall of its own, so npm's install-script gate does not apply to it -- and nothing fetches
100 MB behind your back either.

**`npm run conformance` is organised by document, not by feature.** Each test in
`tests/conformance/` is named for one claim in the PRD, an ADR or §9 of the plan, and carries its
source in the title; a documented claim that is not implemented is a `test.fixme` with the same
name, so it is present, counted and traceable rather than absent. The run writes
`docs/conformance.md`. The number of collected claims is pinned: a claim that stops being collected
fails the run, because the script this replaced had eleven checks inside `if` guards and reported a
smaller denominator as "all passed".

**`npm run wedge` is a second configuration, on its own port, and it is not part of the claim
count.** One thing the suite could not reach is the engine's own deadline: `PRODUCT_TIMEOUT_MS` was
a 60-second constant, so everything downstream of missing it -- the handle being disposed, the child
process ending, every later call refusing, `initialize` being the only way back -- had only ever
been reasoned about. Three pieces make it reachable, and each is a seam rather than a behaviour
change:

* `GEARBOX_PRODUCT_TIMEOUT_MS`, read per call and **validated** -- a malformed value is refused by
  name rather than falling back, because a silent fallback makes a wedge test pass for the wrong
  reason. Unset, which is every other run, the cap is the same 60 seconds it always was.
* `scripts/wedging-engine.mjs`, a pass-through over the real `gearbox rpc` that withholds the answer
  to one chosen method while a sentinel file exists. A stub that answered nothing could only show the
  *first* call failing; what has to be shown is a working session losing its engine and getting
  another.
* `scripts/start-studio-wedge.mjs`, a backend of its own with its own Theia configuration directory.
  The claim ends an engine on purpose, so it never runs against a server somebody else is using --
  and `reuseExistingServer: false` means a busy port fails the run instead of quietly testing
  something else.

What that reaches, beyond the timeout itself: **a write the engine finished and never reported.**
The proxy forwards the request it withholds the answer to, so the description on disk really does
change while the panel really does not hear about it -- which is the one state a recovery path
cannot be designed for by reasoning, because the tempting answer (re-send the write) is wrong
precisely there. `tests/wedge/engine-recovery.spec.ts` holds what a person is offered afterwards:
that the panel says the screen is no longer current, that the way back re-establishes the session
rather than re-reading through an engine that is not there, that the profile, selection and draft
survive it, that nothing re-sends the write, and that a draft the description already contains is
ended rather than left pending for ever.

The staged-loading tests sample the DOM on a timeline rather than after loading finishes, because
the claim under test is that a row is useful *before* it is complete. A snapshot taken at the end
would pass even if the tree had appeared all at once. The sampler is installed with
`addInitScript`, so it starts before Theia's own scripts.

**Every test gets a fresh browser context, and that is load-bearing.** Theia persists its layout,
and `initializeLayout` runs only when there is no saved layout -- deliberately, so that a closed
panel stays closed. An empty `localStorage` is what makes the panels open as designed, so setting
`storageState` or reusing a profile would fail half the suite for a reason unrelated to any claim.

**`npm run build` does not rebuild the engine.** The backend spawns
`../target/debug/gearbox`, so a change on the Rust side is invisible to the
TypeScript build -- and the symptom is a client that reports something the engine
was already taught to send. The suite's `globalSetup` builds it, and also fails if the frontend
bundle is older than `gearbox-studio/src` -- testing a stale bundle reports on code that is not
there, and reports it as success.

## What is in the shell, and what was taken out

Explorer, Search, Source Control, Problems and the editor stack, plus the Gearbox views above. The
Explorer shows both repositories, and git decorates it. The **terminal** is suppressed: no shell tab
at startup, no `Terminal:` command in the palette (ADR-0011 amendment 2026-09-01). Taken out of the
menu bar and panels: the **Selection** menu (`@theia/monaco`), **Go** (`@theia/editor`), **Run**
(`@theia/debug`), and the **Debug** and **Testing** views.

The removed menus and hidden views come from packages this application did not choose:
`@theia/plugin-ext` needs `@theia/debug` and `@theia/test` to implement the VS Code debug and testing
APIs, and Monaco and the editor bring their own menus. So the packages stay and only their
presentation goes -- `initializeLayout(): NOOP` for a view, `unregisterMenuAction` for a menu -- which
keeps the command and the keybinding where that is intended, respects a saved layout, and leaves the
view one command away only when the whitelist allows it. ADR 0011 is the argument;
`tests/conformance/adr-0011-ide-shell.spec.ts` is the check that a Theia upgrade putting any of them
back gets caught by a test rather than by someone noticing.

**Git is not `@theia/git`.** That package stopped being released after `1.61.0-next.8`. In 1.75 the
Source Control *view* is `@theia/scm` and git itself is the VS Code `vscode.git` extension running in
the plugin host, which is why `--plugins=local-dir:../plugins` is in the start script.

`npm run plugins` fetches it. Versions are pinned in the URLs under `theiaPlugins` in
`browser-app/package.json`, so an upgrade is an edit somebody makes on purpose; `git-base` is not
optional, because `vscode.git` depends on it. The download is deliberately **not** part of `build`:
it pulls a third-party artefact over the network, and the precedent here is
`npx playwright install chromium` -- an explicit step, documented, never a postinstall. `ide/plugins/`
is entirely untracked, including any placeholder: the plugin deployer treats every entry as a plugin
candidate and warns about the ones it cannot unpack.

Writing this ourselves was the alternative, and it was measured before being rejected: an `ScmProvider`
over `simple-git` came to roughly a thousand lines for a subset of what the extension does -- no
history, no blame, no conflict resolution, no gutter diffs.

**Workspace trust is settled narrowly, not switched off.** An untrusted workspace restricts
extensions, so git would be present and inert, and Theia puts a modal dialog over the application on
first launch. `DomainWorkspace` adds exactly the roots it derived to
`security.workspace.trust.trustedFolders`; it does not set `security.workspace.trust.enabled: false`,
which would trust every folder anyone ever opens in an application that runs third-party extension
code. Two traps, both survived: trusted folders are compared as URIs, so a bare `/Users/...` entry
never matches a `file:///Users/...` root; and Theia requires its *own* generated
`Untitled-NN.theia-workspace` to be trusted, which no setting can express, so
`StudioWorkspaceTrustService` drops it from the set.

**New Product writes only inside the repository.** Create and Clone are bounded by the workspace the
Studio declared, so a product cannot be created in a folder of your own yet. It can be opened from
anywhere. This is a recorded limit (`docs/plans/prototype.md` §13), not a design goal.

## The `.gdl` language

Syntax highlighting is a native Theia contribution
(`gearbox-studio/src/browser/gdl/`), not a bundled VS Code extension for the grammar itself: the
plugin host exists for git, while `.gdl` colouring stays a native
`LanguageGrammarDefinitionContribution` so the vocabulary can be generated from the engine.

**The vocabulary it colours is generated, not written.**
`gearbox-studio/src/browser/gdl/generated/vocabulary.ts` comes from
`cargo test -p gearbox-gdl --test export_grammar`, which reads the globals the
interpreter actually evaluates against -- so the editor cannot colour a function
the engine does not have, nor miss one it does. Add a function to
`gdl_vocabulary` and `make grammar-check` fails until you run `make grammar` and
commit the result, exactly like `make ts-check`.

The regexes in `gdl-grammar.ts` are hand-written and stay that way: the volatile
part of a grammar is its word lists, the hard part is its patterns, and only the
first can drift.

## Three things that will bite

**One pinned `@theia/*` version, repeated in the root `overrides`.** The version lives in
`theia-version.txt`. A transitive `^` pulls a second `@theia/core` copy, which breaks inversify
identity -- the most common Theia build failure. Check with `npm ls @theia/core`: exactly one entry.

**The corporate CA is needed at install time *and* at run time, and the root README points
here for both.** At run time the backend reaches the AI provider through Node's global `fetch`, so
a broken trust store surfaces in the chat as the bare string `Connection error.` -- the SDK's
message for a request that never got an answer. `NODE_EXTRA_CA_CERTS` is the variable that
*appends* to Node's bundled roots. Two ways to get it wrong, both of which empty or replace the
store: `NODE_OPTIONS=--use-openssl-ca` on a Node whose bundled OpenSSL has no CA store configured
(the flag then discards the bundled roots and reads nothing), and `SSL_CERT_FILE` pointing at a
single corporate root, which on Node >= 22 replaces the whole store so public hosts stop
verifying. `npm run start:browser` detects the first and warns about the second.

**`npm install` needs the same CA in `NODE_EXTRA_CA_CERTS`.** Nine packages have install
scripts, and two of them -- `drivelist` and `@theia/ffmpeg` -- compile from source through
`node-gyp`, which downloads Node headers over TLS and does *not* pick up the `cafile` npm itself
uses. Behind a TLS-intercepting proxy the download fails with `unable to get local issuer
certificate`, npm aborts, and the tree is left incomplete:

```bash
export NODE_EXTRA_CA_CERTS=/path/to/corp-ca.pem
```

**Switching Node major version means rebuilding the native modules**, because they are compiled
against one ABI: `npm ci` under the new version, then `node-pty` by hand as below. `node-gyp` first
downloads that version's headers from nodejs.org. On one machine the download failed inside
`npm ci` with `unable to get local issuer certificate` while a plain `fetch` to the same URL
worked; running npm's own `node-gyp` directly primed the cache and `npm ci` then went through
(the exact cause inside npm was not established):

```bash
node "$(dirname "$(which node)")/../lib/node_modules/npm/node_modules/node-gyp/bin/node-gyp.js" install "$(node -p 'process.versions.node')"
```

`SSL_CERT_FILE` pointing at a single root breaks this too, and on Node >= 22 only: it *replaces* the
trust store, where `NODE_EXTRA_CA_CERTS` appends to it.

**Two of those native modules are load-bearing.** `@theia/core`'s backend requires
`drivelist/build/Release/drivelist.node` unconditionally, browser target or not, and
`@theia/terminal` needs `node-pty/build/Release/pty.node` for its package even though Studio
suppresses the terminal UI. Skipping install scripts -- with `ignore-scripts`, or by leaving npm's
`allowScripts` gate unapproved -- produces a backend that dies at startup with `Cannot find module`.
Neither publishes a darwin-arm64 prebuild, so on Apple Silicon both are compiled.

`node-pty` has a trap of its own: its install script is
`node scripts/prebuild.js || node-gyp rebuild`, and `prebuild.js` **exits 0 without producing a
binary**, so the fallback never runs. `npm rebuild node-pty` therefore reports success and leaves
nothing behind. Build it directly:

```bash
(cd node_modules/node-pty && ../.bin/node-gyp rebuild)
```

`keytar` really is unused and `esbuild`'s binary really does arrive through its platform package;
the mistake to avoid is generalising from those to the whole list.

npm 11 warns that these scripts are "not yet covered by allowScripts" and then runs them anyway --
the warning is advice to codify the approvals, not a statement that nothing executed.


### Integrator UX acceptance

The product workspace now keeps Composition, Topology, Checks and Generate under one header.
Product/profile management and source tools live in the Product menu. Existing generation commands
open the embedded section. Pending edits are reviewed and applied from one fixed draft bar.

`npm run test:ux` starts an isolated browser server on 127.0.0.1:3014 and creates temporary products
and generation output. It uses `GEARBOX_ENGINE` (or a local debug/release binary) and `GEARBOX_ROOT`
(default: the sibling `gears-rust` checkout). It does not rewrite the real products or source corpus.
The standard conformance suite still has its documented corpus and generated-output prerequisites.

If the Playwright Chromium download is absent, set `GEARBOX_UX_BROWSER=chrome` to use installed Chrome.
