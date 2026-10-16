//! The `textDocument/*` lifecycle, and the rule about which diagnostics may be
//! underlined.
//!
//! Driven through `document_notification` against an in-memory connection, which
//! is the whole handler: the only thing between it and `serve`'s loop is a match
//! on the method name.
//!
//! The rule tested last is the one this slice turns on
//! (`cpt-gearbox-adr-gdl-language-server`), and it is the kind that decays
//! quietly. Publishing everything would look like an improvement in a diff --
//! more diagnostics reach the editor -- and the damage would show up only as
//! squiggles under the first character of files whose real error is thirty lines
//! down.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use lsp_server::Connection;

use super::*;

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn scratch(label: &str) -> PathBuf {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives under the workspace");
    let nth = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = repo.join(format!(
        "target/gbx-document-{label}-{}-{nth}",
        std::process::id()
    ));
    drop(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// A session with no roots, which is what an editor that has opened a file
/// outside the corpus has.
fn state() -> State {
    State {
        roots: Vec::new(),
        registry_plans: BTreeMap::new(),
        catalogue: None,
        failed_roots: Vec::new(),
        creation_boundary: None,
        initialized: true,
        allow_writes: false,
        workspace: None,
        documents: BTreeMap::new(),
    }
}

/// A session with `root` open as a source root, which is what an editor working
/// inside the corpus has.
///
/// The root is canonicalized by `SourceRoot::open`, so the fixture canonicalizes
/// the path it builds URIs from too -- otherwise a symlink anywhere above the
/// scratch directory would make the two spellings different paths and the test
/// would fail for a reason that is not the one it is about.
fn state_with_root(root: &Path) -> State {
    State {
        roots: vec![
            SourceRoot::open(SourceId::new("fixture").unwrap(), root).expect("scratch root opens"),
        ],
        ..state()
    }
}

fn did_open(uri: &str, text: &str) -> Notification {
    Notification {
        method: method::DID_OPEN.to_owned(),
        params: serde_json::json!({
            "textDocument": { "uri": uri, "languageId": "gdl", "version": 1, "text": text }
        }),
    }
}

fn did_change(uri: &str, version: i32, text: &str) -> Notification {
    Notification {
        method: method::DID_CHANGE.to_owned(),
        params: serde_json::json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }]
        }),
    }
}

fn did_close(uri: &str) -> Notification {
    Notification {
        method: method::DID_CLOSE.to_owned(),
        params: serde_json::json!({ "textDocument": { "uri": uri } }),
    }
}

/// The next `publishDiagnostics` on the wire, decoded.
///
/// Every branch of the handler ends in one, so a test that finds none has found
/// a document the editor would be left holding stale markers for.
fn published(client: &Connection) -> PublishDiagnosticsParams {
    loop {
        let message = client
            .receiver
            .try_recv()
            .expect("the handler must publish, even when there is nothing to say");
        if let Message::Notification(notification) = message
            && notification.method == method::PUBLISH_DIAGNOSTICS
        {
            return serde_json::from_value(notification.params).expect("publish params");
        }
    }
}

/// The message of the next `gearbox/log` on the wire.
///
/// Anything this server has to say that is not a diagnostic arrives this way,
/// because the client cannot read stderr.
fn logged(client: &Connection) -> String {
    loop {
        let message = client
            .receiver
            .try_recv()
            .expect("the report must reach the client, not only the log");
        if let Message::Notification(notification) = message
            && notification.method == method::LOG
        {
            let params: LogParams =
                serde_json::from_value(notification.params).expect("log params");
            return params.message;
        }
    }
}

/// A gear description whose `name` is a number.
///
/// It parses, so the evaluator is what rejects it, and the span it reports
/// covers the whole `gear(...)` call -- a range with something in it, which is
/// what "source ranges" in the requirement asks for.
const MISTYPED_GEAR: &str = "gear(\n  maturity = \"stable\",\n  name = 5,\n  category = \"example\",\n  \
                             package = cargo(crate_name = \"demo\", lib = \"demo\", \
                             path = \".\"),\n)\n";

/// A description that stops in the middle: the call is never closed.
///
/// Its diagnostic is a *point*, not a span -- the place the expression should
/// have continued. Kept as a fixture because it is what a half-typed file looks
/// like, and because it is the case a "non-empty range" rule would wrongly drop.
const TRUNCATED_GEAR: &str = "gear(\n  maturity = \"stable\",\n  name = \"Demo\",\n";

/// A file with no `gear()` in it at all.
///
/// The one diagnostic reachable from a description alone that is genuinely about
/// the file rather than a place in it, so it is the fixture for the rule.
const EMPTY_GEAR: &str = "";

const VALID_GEAR: &str = "gear(\n  maturity = \"stable\",\n  name = \"Demo\",\n  category = \"example\",\n  \
                          package = cargo(crate_name = \"demo\", lib = \"demo\", path = \".\"),\n)\n";

/// Opening a broken description underlines it, and underlines *something*.
///
/// The range assertion is the claim. A diagnostic that arrives with
/// `(0,0)-(0,0)` satisfies "a diagnostic was published" and fails the
/// requirement, which asks for "source ranges".
#[test]
fn opening_a_broken_description_publishes_a_range() {
    let dir = scratch("open");
    let path = dir.join("gear.gdl");
    let uri = gearbox_ir::file_uri(&path);
    let (server, client) = Connection::memory();
    let mut state = state();

    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, MISTYPED_GEAR)
    ));

    let params = published(&client);
    assert_eq!(params.uri, uri);
    assert_eq!(params.version, Some(1));
    assert!(
        !params.diagnostics.is_empty(),
        "a file that does not evaluate"
    );
    for diagnostic in &params.diagnostics {
        assert_ne!(
            diagnostic.range.start, diagnostic.range.end,
            "this fixture's error is a span, not a point: {diagnostic:?}"
        );
        assert_eq!(
            diagnostic.severity, 1,
            "a description that fails is an error"
        );
        assert_eq!(diagnostic.source, "gearbox");
    }
}

/// A description that stops halfway is underlined where it stops.
///
/// The companion to the test above, and the reason the rule is written against
/// `Range::whole_file` rather than against emptiness: this diagnostic's range has
/// nothing between its ends, and it is still a true statement about a position.
/// A rule that dropped empty ranges would leave every half-typed file unmarked --
/// which is most files, most of the time somebody is typing.
#[test]
fn a_truncated_description_is_underlined_where_it_stops() {
    let dir = scratch("truncated");
    let uri = gearbox_ir::file_uri(&dir.join("gear.gdl"));
    let (server, client) = Connection::memory();
    let mut state = state();

    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, TRUNCATED_GEAR)
    ));

    let published = published(&client);
    let [diagnostic] = published.diagnostics.as_slice() else {
        panic!("one parse error, got {:?}", published.diagnostics);
    };
    assert_eq!(
        diagnostic.range.start, diagnostic.range.end,
        "the fixture must really be a point, or this proves nothing"
    );
    assert_ne!(
        diagnostic.range.start,
        gearbox_ir::Position::origin(),
        "and it must be a point somewhere other than the origin, which is the \
         sentinel meaning `no position`"
    );
}

/// What the buffer says, not what the file says.
///
/// The point of holding documents at all. With the file on disk valid and the
/// buffer broken, anything that read the path would publish nothing -- so the
/// diagnostics here can only have come from the text the client sent.
#[test]
fn the_text_from_did_change_is_what_is_evaluated() {
    let dir = scratch("change");
    let path = dir.join("gear.gdl");
    std::fs::write(&path, VALID_GEAR).unwrap();
    let uri = gearbox_ir::file_uri(&path);
    let (server, client) = Connection::memory();
    let mut state = state();

    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, VALID_GEAR)
    ));
    assert!(
        published(&client).diagnostics.is_empty(),
        "the valid text is valid"
    );

    assert!(document_notification(
        &server,
        &mut state,
        did_change(&uri, 2, MISTYPED_GEAR)
    ));

    let params = published(&client);
    assert_eq!(params.version, Some(2));
    assert!(
        !params.diagnostics.is_empty(),
        "the buffer is broken, and the buffer is the question"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        VALID_GEAR,
        "and nothing was written: an editor's buffer is not a save"
    );
}

/// Closing a document takes its markers with it.
///
/// An empty list is how LSP says "clean now". Staying silent would leave the
/// squiggles on a buffer nobody can see, which is the stale-marker failure
/// `cpt-gearbox-fr-editor-diagnostics` exists to prevent.
#[test]
fn closing_a_document_publishes_an_empty_list_and_forgets_it() {
    let dir = scratch("close");
    let uri = gearbox_ir::file_uri(&dir.join("gear.gdl"));
    let (server, client) = Connection::memory();
    let mut state = state();

    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, MISTYPED_GEAR)
    ));
    assert!(!published(&client).diagnostics.is_empty());

    assert!(document_notification(&server, &mut state, did_close(&uri)));

    let params = published(&client);
    assert_eq!(params.uri, uri);
    assert!(params.diagnostics.is_empty(), "closed means nothing to say");
    assert!(
        state.documents.is_empty(),
        "a document the editor closed must not be held for the life of the session"
    );
}

/// A diagnostic that names a file but no position is **not** underlined.
///
/// `no gear() declaration` is built with `Location::file`, which is
/// `Range::whole_file`. Many diagnostics are, and this is the one reachable from
/// a description alone -- see `lsp::publishable` for why no count is given.
///
/// Both halves matter. That the diagnostic exists is asserted directly, so this
/// cannot pass because the check stopped running; that it is not published is
/// the rule.
#[test]
fn a_diagnostic_with_no_position_is_not_underlined() {
    let dir = scratch("anchorless");
    let path = dir.join("gear.gdl");
    let uri = gearbox_ir::file_uri(&path);
    let diagnostics = gearbox_engine::check_description(&path, None, EMPTY_GEAR)
        .expect("`gear.gdl` is a description");
    assert!(
        diagnostics.iter().any(|d| d
            .location
            .as_ref()
            .is_some_and(|l| { l.uri == uri && l.range == gearbox_ir::Range::whole_file() })),
        "the fixture must actually produce an anchorless diagnostic, or this test \
         proves nothing: {diagnostics:?}"
    );

    let (server, client) = Connection::memory();
    let mut state = state();
    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, EMPTY_GEAR)
    ));

    assert!(
        published(&client).diagnostics.is_empty(),
        "underlining the start of the file is a false claim about where the error is"
    );
}

/// An incremental change against a server advertising `Full` is refused aloud.
///
/// Treating `text` as the whole document would replace the file with a fragment,
/// and every diagnostic after it would be confident and wrong.
#[test]
fn an_incremental_change_is_refused_rather_than_half_applied() {
    let dir = scratch("incremental");
    let uri = gearbox_ir::file_uri(&dir.join("gear.gdl"));
    let (server, client) = Connection::memory();
    let mut state = state();

    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, VALID_GEAR)
    ));
    drop(published(&client));

    let incremental = Notification {
        method: method::DID_CHANGE.to_owned(),
        params: serde_json::json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": { "start": { "line": 0, "character": 0 },
                           "end": { "line": 0, "character": 4 } },
                "text": "gearx"
            }]
        }),
    };
    assert!(document_notification(&server, &mut state, incremental));

    assert_eq!(
        state.documents.get(&uri).map(String::as_str),
        Some(VALID_GEAR),
        "the document keeps the last whole text the client actually sent"
    );
    let Ok(Message::Notification(logged)) = client.receiver.try_recv() else {
        panic!("the mismatch must be said out loud, not swallowed");
    };
    assert_eq!(logged.method, method::LOG);
}

/// The URI a client sends and the URI the engine builds are the same document.
///
/// Theia percent-encodes and `gearbox_ir::file_uri` does not, so a path with a
/// space in it is where the two spellings part company -- and a dropped
/// diagnostic there would look like "this file is fine".
#[test]
fn a_percent_encoded_uri_names_the_same_file() {
    let dir = scratch("encoded").join("my products");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gear.gdl");
    let encoded = gearbox_ir::file_uri(&path).replace(' ', "%20");
    assert!(
        encoded.contains("%20"),
        "the fixture must exercise encoding"
    );

    assert_eq!(lsp::path_from_uri(&encoded).local(), Some(path.as_path()));

    let (server, client) = Connection::memory();
    let mut state = state();
    assert!(document_notification(
        &server,
        &mut state,
        did_open(&encoded, MISTYPED_GEAR)
    ));

    let params = published(&client);
    assert_eq!(
        params.uri, encoded,
        "published under the spelling the client used, or it cannot match its own document"
    );
    assert!(
        !params.diagnostics.is_empty(),
        "a space in the path must not silently empty the diagnostics"
    );
}

/// A description whose `load()` only resolves if the source root is known.
///
/// `//` addresses the source root explicitly, so this is the one thing a gear
/// can say that has a different answer depending on which directory the server
/// calls the root. That makes it the fixture for the `source_root` argument:
/// nothing else about evaluating a single description depends on it.
const ROOTED_GEAR: &str = "load(\"//shared.gdl\", \"SHARED\")\ngear(\n  maturity = \"stable\",\n  name = \"Demo\",\n  \
                           category = \"example\",\n  package = SHARED,\n)\n";

/// A source root laid out on disk, with the shared fragment at its top.
///
/// The gear sits two directories down, so the root is not the gear's own
/// directory and `strip_prefix` has more than one component to keep.
fn rooted_fixture(label: &str) -> (PathBuf, PathBuf) {
    let root = scratch(label)
        .canonicalize()
        .expect("scratch canonicalizes");
    std::fs::create_dir_all(root.join("gears/demo")).unwrap();
    std::fs::write(
        root.join("shared.gdl"),
        "SHARED = cargo(crate_name = \"cf-shared\", lib = \"shared\")\n",
    )
    .unwrap();
    let path = root.join("gears/demo/gear.gdl");
    (root, path)
}

/// The open source root is the `load()` boundary the description is evaluated
/// against, and it comes from the session rather than from the file's directory.
///
/// Both halves are the test. That the same text is *broken* without a root is
/// what proves the fixture depends on the root at all -- without it this would
/// pass against a server that ignored `state.roots` entirely, which is exactly
/// the bug it is here to catch. An editor that got the boundary wrong would
/// underline a `load()` that the catalogue loader accepts without complaint,
/// and the person would be told their working description is broken.
#[test]
fn a_gear_is_evaluated_against_the_source_root_the_session_has_open() {
    let (root, path) = rooted_fixture("rooted");
    let uri = gearbox_ir::file_uri(&path);

    let rootless = gearbox_engine::check_description(&path, None, ROOTED_GEAR)
        .expect("`gear.gdl` is a description");
    assert!(
        !rootless.is_empty(),
        "the fixture must really need the root, or this test proves nothing"
    );

    let (server, client) = Connection::memory();
    let mut state = state_with_root(&root);
    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, ROOTED_GEAR)
    ));

    let params = published(&client);
    assert_eq!(params.uri, uri);
    assert!(
        params.diagnostics.is_empty(),
        "`//shared.gdl` is inside the open root, so this description is clean: {:?}",
        params.diagnostics
    );
}

/// Between two nested roots, the description belongs to the one the catalogue
/// would attribute it to: the first in the roots list.
///
/// **Not the innermost, which is what this used to assert.** `load_catalogue`
/// walks the roots in order and attributes each description to the root it was
/// walked from, keeping the first declaration when two roots claim one gear
/// (`gearbox_engine::owning_source_root`, ADR
/// `cpt-gearbox-adr-multiple-source-roots`). Picking the deepest match here
/// meant the editor evaluated a file against a `load()` boundary no catalogue
/// entry had used -- so a `load("//...")` the catalogue resolved could be
/// underlined in the buffer while the same file loaded clean from disk, and
/// nothing about the file had changed between the two answers.
///
/// Order-dependent, therefore, and deliberately: with nested roots the answer is
/// a property of the list the person wrote, and the engine's rule is the one that
/// decides. Both orders are asserted so the dependence is stated rather than
/// discovered.
#[test]
fn the_first_of_two_nested_roots_is_the_one_used() {
    let (outer, _) = rooted_fixture("nested");
    let inner = outer.join("gears");
    // The inner root gets no `shared.gdl`, so a description resolving `//` against
    // it fails where the same text resolved against the outer root succeeds.
    let path = inner.join("demo/gear.gdl");
    let uri = gearbox_ir::file_uri(&path);

    let mut state = state_with_root(&outer);
    state
        .roots
        .push(SourceRoot::open(SourceId::new("inner").unwrap(), &inner).expect("inner root opens"));

    let (server, client) = Connection::memory();
    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, ROOTED_GEAR)
    ));
    assert!(
        published(&client).diagnostics.is_empty(),
        "the outer root is listed first, so it is the root the catalogue attributes this \
         description to, and `//shared.gdl` resolves under it"
    );

    // Listed the other way round, the inner root owns it -- and the same text is
    // broken there, which is what proves the boundary really came from the list.
    state.roots.reverse();
    let (server, client) = Connection::memory();
    let mut fresh = state;
    assert!(document_notification(
        &server,
        &mut fresh,
        did_open(&uri, ROOTED_GEAR)
    ));
    assert!(
        !published(&client).diagnostics.is_empty(),
        "`//shared.gdl` does not exist under the inner root, and with it listed first the \
         inner root is the one the catalogue would use"
    );
}

/// A URI with `..` in it is classified by where it resolves, not by how it is
/// spelled.
///
/// `Path::starts_with` compares components, so an unresolved `..` makes the
/// boundary check a spelling test: it answers wrongly in both directions. The
/// direction that lets a path out of a root it is not in is
/// `lsp_tests::a_dot_dot_uri_is_collapsed`; this is the other one, which is the
/// direction a person actually meets. The climb here lands back on the same
/// file -- a real spelling for a real document -- and unresolved it shares only
/// two components with the root, so the file would be evaluated with no source
/// root at all and its legal `load()` underlined as an escape.
#[test]
fn a_uri_that_climbs_is_classified_by_where_it_resolves() {
    let (root, path) = rooted_fixture("climbing");
    let leaf = root.file_name().unwrap().to_string_lossy().into_owned();
    let climbing = root
        .parent()
        .unwrap()
        .join("..")
        .join(root.parent().unwrap().file_name().unwrap())
        .join(&leaf)
        .join("gears/demo/gear.gdl");
    assert!(
        climbing.components().any(|c| c == Component::ParentDir),
        "the fixture must really contain a `..`"
    );

    let uri = gearbox_ir::file_uri(&climbing);
    assert_eq!(
        lsp::path_from_uri(&uri).local(),
        Some(path.as_path()),
        "the `..` names the same file, and the decode must say so"
    );

    let (server, client) = Connection::memory();
    let mut state = state_with_root(&root);
    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, ROOTED_GEAR)
    ));
    assert!(
        published(&client).diagnostics.is_empty(),
        "resolved, this file is inside the root and its `load()` is legal"
    );
}

/// A `didChange` carrying no changes changes nothing.
///
/// Republishing would be harmless; *not* republishing is the claim, and it is
/// the one worth pinning, because the way this branch breaks is by falling
/// through to an evaluation of a document the client never sent -- which for an
/// empty `contentChanges` on a URI that was never opened means publishing
/// diagnostics for an empty buffer.
#[test]
fn a_change_with_no_changes_publishes_nothing() {
    let dir = scratch("empty-change");
    let uri = gearbox_ir::file_uri(&dir.join("gear.gdl"));
    let (server, client) = Connection::memory();
    let mut state = state();

    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, VALID_GEAR)
    ));
    drop(published(&client));

    let empty = Notification {
        method: method::DID_CHANGE.to_owned(),
        params: serde_json::json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": []
        }),
    };
    assert!(document_notification(&server, &mut state, empty));

    assert!(
        client.receiver.try_recv().is_err(),
        "nothing moved, so the markers already on screen are still the right ones"
    );
    assert_eq!(
        state.documents.get(&uri).map(String::as_str),
        Some(VALID_GEAR),
        "and the stored document is untouched"
    );
}

/// Params this server cannot read end the notification, not the session.
///
/// A notification has no id to answer, so the only thing the loop can do is say
/// so and carry on -- and carrying on is the part that matters: a panic here
/// would take down a language server over one malformed message from a client
/// that is free to send another.
///
/// **Said to the client, not only to stderr.** For a `didChange` the stored
/// document keeps its previous text and nothing is republished, so an editor told
/// nothing goes on showing markers computed from text its buffer no longer has --
/// and stderr is not a channel it can read. The report names the document when
/// the params said which one, because resending it is the only thing the client
/// can do about it.
#[test]
fn a_malformed_notification_is_reported_and_survived() {
    let (server, client) = Connection::memory();
    let mut state = state();

    let no_text = Notification {
        method: method::DID_OPEN.to_owned(),
        params: serde_json::json!({
            "textDocument": { "uri": "file:///a/gear.gdl", "version": 1 }
        }),
    };
    assert!(
        document_notification(&server, &mut state, no_text),
        "the peer is not gone; only the message was unreadable"
    );

    let report = logged(&client);
    assert!(
        report.contains("file:///a/gear.gdl") && report.contains("not updated"),
        "the report must name the document that was left as it was: {report}"
    );

    let not_an_object = Notification {
        method: method::DID_CHANGE.to_owned(),
        params: serde_json::json!("nonsense"),
    };
    assert!(document_notification(&server, &mut state, not_an_object));
    let report = logged(&client);
    assert!(
        report.contains("no document was named"),
        "params that name nothing are still reported, and say so: {report}"
    );

    assert!(
        client.receiver.try_recv().is_err(),
        "there is no document to publish about: the server was never told which one"
    );
    assert!(state.documents.is_empty(), "and nothing was stored");
}

/// A `file://` URI that cannot be decoded is reported, not called clean.
///
/// `%FF` is a valid escape and not valid UTF-8, so the decode has no path to
/// answer with -- and it used to answer the same `None` an `untitled:` buffer
/// gets, which this handler reads as "not a local file" and turns into an empty
/// diagnostic list. That is a document the editor is showing, reported as having
/// nothing wrong with it.
#[test]
fn a_document_whose_uri_cannot_be_decoded_is_reported_rather_than_published() {
    let (server, client) = Connection::memory();
    let mut state = state();
    let uri = "file:///a/%FF.gdl";

    assert!(document_notification(
        &server,
        &mut state,
        did_open(uri, MISTYPED_GEAR)
    ));

    let report = logged(&client);
    assert!(
        report.contains(uri) && report.contains("UTF-8"),
        "the client is told which document is not being diagnosed, and why: {report}"
    );
    assert!(
        client.receiver.try_recv().is_err(),
        "no diagnostic set may be claimed for it, and an empty one claims it is clean"
    );
}

/// A `didChange` for a document that was never opened is refused, not stored.
///
/// `State::documents` is uncapped on the argument that LSP guarantees a
/// `didClose` for every `didOpen`; an entry `didChange` inserted is outside that
/// guarantee -- nothing ever removes it -- and both the key and the text are
/// unbounded client strings, so a client that only ever sends `didChange` grew
/// the map for the life of the process. Nothing is published either: an empty
/// list would report a document this server has never been given as clean.
#[test]
fn a_change_to_a_document_that_was_never_opened_is_refused() {
    let dir = scratch("unopened");
    let uri = gearbox_ir::file_uri(&dir.join("gear.gdl"));
    let (server, client) = Connection::memory();
    let mut state = state();

    assert!(document_notification(
        &server,
        &mut state,
        did_change(&uri, 1, MISTYPED_GEAR)
    ));

    assert!(
        state.documents.is_empty(),
        "a document the editor never opened must not be held for the life of the session"
    );
    let report = logged(&client);
    assert!(
        report.contains("never opened") && report.contains(&uri),
        "and the client is told which document to open first: {report}"
    );
    assert!(
        client.receiver.try_recv().is_err(),
        "no diagnostics are claimed for a document that was never sent"
    );
}

/// A `didOpen` with no version publishes without one, rather than claiming zero.
///
/// LSP requires the field, so this is a misbehaving client -- but the text it
/// sent is perfectly good and the diagnostics are true. Echoing `0` would be the
/// server inventing the one number the client uses to decide whether an answer
/// is stale.
#[test]
fn a_document_opened_without_a_version_is_published_without_one() {
    let dir = scratch("versionless");
    let uri = gearbox_ir::file_uri(&dir.join("gear.gdl"));
    let (server, client) = Connection::memory();
    let mut state = state();

    let versionless = Notification {
        method: method::DID_OPEN.to_owned(),
        params: serde_json::json!({
            "textDocument": { "uri": uri, "languageId": "gdl", "text": MISTYPED_GEAR }
        }),
    };
    assert!(document_notification(&server, &mut state, versionless));

    let params = published(&client);
    assert_eq!(
        params.version, None,
        "no version was given, so none is claimed"
    );
    assert!(
        !params.diagnostics.is_empty(),
        "and the diagnostics are published anyway: the text is what they are about"
    );
}

// --------------------------------------------------------------------------
// Completion and hover. Requests, unlike everything above, so they go through
// `dispatch` and come back as a `Response` rather than a notification.

/// Ask one request against a state holding `text` for `uri`.
fn answer<T: serde::de::DeserializeOwned>(
    method: &str,
    uri: &str,
    text: &str,
    line: u32,
    character: u32,
) -> T {
    let (server, _client) = Connection::memory();
    let mut state = state();
    assert!(document_notification(
        &server,
        &mut state,
        did_open(uri, text)
    ));
    let request = lsp_server::Request {
        id: RequestId::from(1),
        method: method.to_owned(),
        params: serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    };
    let response = dispatch(&server, &mut state, request).expect("a request is answered");
    let value = match response.response_result {
        Ok(value) => value,
        Err(e) => panic!("{method} failed: {e:?}"),
    };
    serde_json::from_value(value).expect("the answer decodes")
}

/// Completion inside a half-written call offers that call's parameters.
///
/// The end-to-end shape of the decision: the buffer does not parse, the server
/// answers anyway, and what it offers comes from the interpreter's own
/// vocabulary (`cpt-gearbox-adr-gdl-completion-and-hover`).
#[test]
fn completion_inside_an_unparseable_call_offers_its_parameters() {
    let dir = scratch("completion");
    let uri = gearbox_ir::file_uri(&dir.join("gear.gdl"));
    // Deliberately unfinished: this is the state a person completes from.
    let text = "gear(\n  name = \"demo\",\n  ";
    let items: Vec<CompletionItem> = answer(method::COMPLETION, &uri, text, 2, 2);

    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"package"), "{labels:?}");
    assert!(
        !labels.contains(&"name"),
        "`name` is already written, so offering it restates the file: {labels:?}"
    );
    assert!(
        items.iter().all(|i| i.kind == lsp::COMPLETION_FIELD),
        "a parameter is a Field, not a Function: {items:?}"
    );
    // Accepted only to be refused by name: offering them invites the mistake.
    for refused in ["fills", "runtime_caps", "cluster_providers"] {
        assert!(
            !labels.contains(&refused),
            "`{refused}` is offered: {labels:?}"
        );
    }
    assert!(
        labels.contains(&"maturity"),
        "the required field is offered: {labels:?}"
    );
}

/// At the top of a file, the constructs that file kind admits.
#[test]
fn completion_at_the_top_level_offers_the_file_s_own_constructs() {
    let dir = scratch("completion-top");
    let uri = gearbox_ir::file_uri(&dir.join("product.gdl"));
    let items: Vec<CompletionItem> = answer(method::COMPLETION, &uri, "", 0, 0);

    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"product"), "{labels:?}");
    assert!(labels.contains(&"use_gear"), "{labels:?}");
    assert!(
        !labels.contains(&"gear"),
        "a product description cannot declare a gear: {labels:?}"
    );
    assert!(items.iter().all(|i| i.kind == lsp::COMPLETION_FUNCTION));
}

/// Hover returns the doc comment the builtin carries.
#[test]
fn hover_inside_a_call_explains_that_call() {
    let dir = scratch("hover");
    let uri = gearbox_ir::file_uri(&dir.join("gear.gdl"));
    let text = "gear(\n  package = cargo(crate_name = \"c\", \n";
    let hover: Option<Hover> = answer(method::HOVER, &uri, text, 1, 38);
    let hover = hover.expect("the caret is inside `cargo(...)`");
    assert!(
        hover
            .contents
            .contains("where a gear's or SDK's crate lives"),
        "the summary from globals.rs must reach the editor: {}",
        hover.contents
    );
}

/// A document this server has never been told about answers empty, not an error.
///
/// An editor can ask before its `didOpen` has crossed the wire, and a failed
/// request there would surface to the person as a broken feature rather than as
/// the ordinary race it is.
#[test]
fn a_document_the_server_does_not_hold_answers_empty() {
    let (server, _client) = Connection::memory();
    let mut state = state();
    let uri = gearbox_ir::file_uri(&scratch("unknown").join("gear.gdl"));
    let request = lsp_server::Request {
        id: RequestId::from(1),
        method: method::COMPLETION.to_owned(),
        params: serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 0 },
        }),
    };
    let response = dispatch(&server, &mut state, request).expect("answered");
    let value = response.response_result.expect("not an error");
    let items: Vec<CompletionItem> = serde_json::from_value(value).expect("decodes");
    assert!(items.is_empty());
}

/// Hover answers a successful `null` rather than an error in both empty cases.
///
/// A document the server does not hold, and a caret at the top level with no
/// enclosing call. Both are ordinary, and an error in either would surface to
/// the person as a broken feature rather than as "nothing to say here".
#[test]
fn hover_answers_null_rather_than_failing_when_it_has_nothing() {
    let dir = scratch("hover-empty");
    let held = gearbox_ir::file_uri(&dir.join("gear.gdl"));

    // Held, but the caret is outside every call.
    let outside: Option<Hover> = answer(method::HOVER, &held, "gear(\n)\n", 1, 1);
    assert_eq!(outside, None, "the top level is inside no call");

    // Not held at all.
    let (server, _client) = Connection::memory();
    let mut state = state();
    let unknown = gearbox_ir::file_uri(&scratch("hover-unknown").join("gear.gdl"));
    let request = lsp_server::Request {
        id: RequestId::from(1),
        method: method::HOVER.to_owned(),
        params: serde_json::json!({
            "textDocument": { "uri": unknown },
            "position": { "line": 0, "character": 0 },
        }),
    };
    let response = dispatch(&server, &mut state, request).expect("answered");
    let value = response
        .response_result
        .expect("an unopened document is not an error");
    assert!(
        serde_json::from_value::<Option<Hover>>(value)
            .expect("decodes")
            .is_none()
    );
}

/// A parameter carries its type in `detail` and its prose in `documentation`.
///
/// Both fields, because swapping them would leave every other test green: the
/// three above check `label` and `kind` only. A construct carries no `detail` at
/// all -- putting the summary in both fields showed the same sentence twice.
#[test]
fn a_completion_item_separates_its_type_from_its_prose() {
    let dir = scratch("completion-detail");
    let gear_uri = gearbox_ir::file_uri(&dir.join("gear.gdl"));
    let items: Vec<CompletionItem> = answer(method::COMPLETION, &gear_uri, "cargo(", 0, 6);
    let param = items
        .iter()
        .find(|i| i.label == "crate_name")
        .expect("`cargo` takes `crate_name`");
    assert!(
        param.detail.as_deref().is_some_and(|d| d.contains("str")),
        "`detail` is the parameter's type: {:?}",
        param.detail
    );

    let top: Vec<CompletionItem> = answer(method::COMPLETION, &gear_uri, "", 0, 0);
    let construct = top
        .iter()
        .find(|i| i.label == "cargo")
        .expect("`cargo` is a top-level construct");
    assert_eq!(
        construct.detail, None,
        "a construct has no type to show beside its name"
    );
    assert!(
        construct
            .documentation
            .as_deref()
            .is_some_and(|d| d.contains("crate lives")),
        "`documentation` is the summary from globals.rs: {:?}",
        construct.documentation
    );
}

/// Completion and hover answer before `initialize`, deliberately.
///
/// Every other request arm goes through `require_ready`, which needs a source
/// root. These two need none -- they read the client's buffer and the
/// interpreter's vocabulary -- so the gate would refuse completion in exactly
/// the session that wants it most, a `gear.gdl` opened on its own. Pinned so the
/// asymmetry reads as a decision rather than an omission.
#[test]
fn the_assist_requests_need_no_initialize_and_no_source_root() {
    let (server, _client) = Connection::memory();
    let mut state = state();
    state.initialized = false;
    assert!(state.roots.is_empty(), "the fixture opens no root");

    let dir = scratch("uninitialised");
    let uri = gearbox_ir::file_uri(&dir.join("gear.gdl"));
    assert!(document_notification(
        &server,
        &mut state,
        did_open(&uri, "cargo(")
    ));

    let request = lsp_server::Request {
        id: RequestId::from(1),
        method: method::COMPLETION.to_owned(),
        params: serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 6 },
        }),
    };
    let response = dispatch(&server, &mut state, request).expect("answered");
    let value = response
        .response_result
        .expect("not refused for want of `initialize`");
    let items: Vec<CompletionItem> = serde_json::from_value(value).expect("decodes");
    assert!(
        items.iter().any(|i| i.label == "crate_name"),
        "the vocabulary answers without a root: {items:?}"
    );
}
