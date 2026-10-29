//! JSON-RPC 2.0 over stdio, with LSP `Content-Length` framing.
//!
//! One server backs both the Studio and the `.gdl` language client, which is why
//! the lifecycle is LSP-shaped (`initialize` / `shutdown` / `exit`) even though
//! most methods are Gearbox's own.
//!
//! **Nothing but JSON-RPC goes to stdout.** Every diagnostic, log line and panic
//! message goes to stderr or a `gearbox/log` notification. The CLI has been
//! written to that rule from the start; this crate is the reason it exists.
//!
//! Threads live here, not in the engine. `load_catalogue_staged` is synchronous
//! with a callback, and the callback writes notifications as events arrive, so
//! the engine keeps no dependency on parallelism (ADR
//! `cpt-gearbox-adr-staged-catalogue-loading`).

pub mod lsp;
pub mod protocol;

#[cfg(test)]
#[path = "apply_edits_tests.rs"]
mod apply_edits_tests;

#[cfg(test)]
#[path = "document_tests.rs"]
mod document_tests;

#[cfg(test)]
#[path = "initialize_tests.rs"]
mod initialize_tests;

#[cfg(test)]
#[path = "preview_tests.rs"]
mod preview_tests;

#[cfg(test)]
#[path = "serve_tests.rs"]
mod serve_tests;

#[cfg(test)]
#[path = "scaffold_tests.rs"]
mod scaffold_tests;

#[cfg(test)]
#[path = "write_gate_tests.rs"]
mod write_gate_tests;

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use gearbox_engine::{Continue, LoadEvent, SourceRoot, default_source_ids, load_catalogue_staged};
use gearbox_ir::{
    Diagnostic, ExplanationGraph, GearId, ProfileId, RelPath, ResolvedProduct, SourceId,
};
use lsp_server::{Connection, ExtractError, Message, Notification, Request, RequestId, Response};

use crate::lsp::{
    CompletionItem, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Hover, LspDiagnostic, PublishDiagnosticsParams,
    TextDocumentPositionParams,
};
use crate::protocol::{
    AddProfileParams, ApplyEditsParams, Capabilities, CatalogueChanged, CatalogueDiagnostics,
    CatalogueLoadResult, CreateProductParams, EditGearParams, EditGearResult, FailedRoot,
    GenerateApplyResult, GenerateFileParams, GenerateFileResult, GenerateParams,
    GeneratePlanResult, InitializeParams, InitializeResult, LockOnDisk, LockParams, LockResult,
    LogParams, PluginTarget, ProductEdit, ProductLoadParams, ProductLoadResult, ProgressParams,
    RemoveProfileParams, ResolveParams, ResolvePreviewParams, ResolveResult, ResolvedRoot,
    ScaffoldGearParams, ScaffoldGearResult, ServerInfo, SetConfigParams, SetFeaturesParams,
    SetProfileFieldParams, ValidateParams, ValidateResult, error_code, method,
};

/// Why the server could not run.
///
/// Each variant holds the error it was built from rather than a sentence made
/// out of it, so whoever prints a `ServeError` can walk the chain instead of
/// reading one flattened line.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("transport: {0}")]
    Transport(#[from] std::io::Error),
    /// The channel to the client is closed, so there is nothing left to send on
    /// and nothing left to report it to.
    ///
    /// Boxed because the real type is `crossbeam_channel::SendError<Message>`,
    /// which arrives through `lsp-server`'s public API without the crate itself
    /// being one of ours to name -- and the error carries the whole undelivered
    /// message, which is not something a caller should have to accept a
    /// dependency to see.
    #[error("cannot send to the client: {0}")]
    Send(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error("protocol: {0}")]
    Protocol(#[from] lsp_server::ProtocolError),
}

/// A `CreationBoundary` as this process holds it.
///
/// Paths rather than opened `SourceRoot`s: the only question asked of them is
/// whether a candidate path sits inside one, and `open_roots` would scan trees
/// that nothing is going to be read from.
struct CreationBoundaryState {
    roots: Vec<PathBuf>,
    workspace: Option<PathBuf>,
}

/// Everything the server knows between requests.
struct State {
    roots: Vec<SourceRoot>,
    /// Registry plans already computed, by product, sources and gear set; see
    /// `prepare_generate`. Emptied with the catalogue, whose roots they read.
    registry_plans: BTreeMap<
        String,
        (
            Option<gearbox_engine::published::RegistryPlan>,
            Vec<Diagnostic>,
        ),
    >,
    /// The catalogue from the last load.
    ///
    /// Cached because resolving re-reads nothing else: a load parses every gear
    /// crate in the tree, and doing that again per resolve would make the
    /// Resolve button cost a second on a slice and ten on a real registry.
    /// Filled by `gearbox/catalogue/load` and replaced by nothing else, which is
    /// the same contract the CLI has -- there is no file watch yet, and
    /// pretending otherwise would be worse than saying so. Dropped when
    /// `initialize` changes the roots, because a catalogue belongs to the roots
    /// it was scanned from.
    catalogue: Option<gearbox_ir::Catalogue>,
    /// The roots that could not be opened, kept so the client can be told which
    /// and why rather than being handed a shorter list.
    failed_roots: Vec<FailedRoot>,
    /// The boundary `create` is judged against, when the client named one.
    ///
    /// Separate from `roots`/`workspace` above, and unchanged by a later
    /// `initialize` that names a session -- which is the whole point: opening a
    /// product must not move where a product may be created. See
    /// `CreationBoundary` in `protocol.rs` for why the client has to tell us
    /// rather than the server remembering its own boot.
    ///
    /// `None` means "judge create by the session", which is what the CLI and
    /// every client that does not set it get.
    creation_boundary: Option<CreationBoundaryState>,
    initialized: bool,
    /// What the client declared at `initialize`.
    ///
    /// Recorded rather than inferred: the server cannot judge whether a caller
    /// should be allowed to write, so it holds the claim and refuses everything
    /// not claimed (`cpt-gearbox-fr-rpc-writes-opt-in`).
    allow_writes: bool,
    /// The directory the client declared as its workspace, if it declared one.
    workspace: Option<PathBuf>,
    /// Open descriptions, by URI, as the editor has them rather than as disk
    /// has them.
    ///
    /// The first thing in `State` that belongs to a *client* rather than to a
    /// workspace, and the reason it can be a plain map: the request loop is one
    /// thread, so `didChange` and the evaluation it triggers cannot interleave.
    ///
    /// Bounded by what the editor has open and emptied by `didClose`, which is
    /// the contract LSP already guarantees -- a client that opens a document
    /// closes it.
    ///
    /// **Uncapped on purpose**, and it is a trust decision rather than an
    /// oversight. The peer here is the process that spawned this one over its
    /// own stdio, which is the person's editor running as the person: a cap
    /// would not keep anything out that is not already in, and the size it
    /// would have to be guessed at is the size of a legitimate description,
    /// which nobody can guess. A cap that is ever reached refuses to diagnose a
    /// file the editor is showing, and says so nowhere the person is looking.
    documents: BTreeMap<String, String>,
}

/// Run the server on stdio until the client says `exit`.
///
/// `default_roots` come from `--root` on the command line and are used when
/// `initialize` names none, so the CLI stays usable without a client.
///
/// # Errors
/// Returns [`ServeError`] when the transport fails or the client sends something
/// the framing cannot parse.
pub fn serve_stdio(default_roots: &[PathBuf]) -> Result<(), ServeError> {
    let (connection, io_threads) = Connection::stdio();
    let mut state = new_state(default_roots);

    // **The connection is moved into the loop and dropped by it, before the
    // join.** `IoThreads::join` waits on lsp-server's message-dropper thread,
    // which waits on the writer thread, which is parked in
    // `writer_receiver.into_iter()` for as long as any sender is alive -- and
    // `connection.sender` is that sender. Joining with the connection still in
    // scope therefore never returned: after the client sent `exit` the process
    // hung here instead of exiting.
    let served = serve_connection(connection, &mut state);
    // Joined even when the loop failed, so the writer has finished flushing
    // whatever it had; the loop's own failure is the one worth reporting.
    let joined = io_threads.join();
    served?;
    joined.map_err(ServeError::from)
}

fn new_state(default_roots: &[PathBuf]) -> State {
    let (roots, failed_roots) = open_roots(default_roots);
    State {
        roots,
        registry_plans: BTreeMap::new(),
        catalogue: None,
        failed_roots,
        // Nothing to hold yet: the CLI's `--root` values are already `roots`
        // above, and a client that wants create judged separately says so at
        // `initialize`.
        creation_boundary: None,
        initialized: false,
        // Read-only until a client says otherwise, which is the posture
        // `cpt-gearbox-fr-rpc-writes-opt-in` asks for.
        allow_writes: false,
        workspace: None,
        documents: BTreeMap::new(),
    }
}

/// Serve `connection` until the client says `exit`, then drop it.
///
/// **By value.** The connection's sender is the only thing keeping lsp-server's
/// writer thread alive, so whoever joins the io threads must not be holding one;
/// taking ownership here is what makes that a property of the signature rather
/// than of where a `drop` happens to sit. See [`serve_stdio`].
fn serve_connection(connection: Connection, state: &mut State) -> Result<(), ServeError> {
    // Messages read off the channel while coalescing a burst of edits, to be
    // served before anything else is read. See `coalesce_did_change`.
    let mut queued: std::collections::VecDeque<Message> = std::collections::VecDeque::new();
    loop {
        let message = match queued.pop_front() {
            Some(message) => message,
            None => match connection.receiver.recv() {
                Ok(message) => message,
                // The peer is gone. Not an error: it is how a client that was
                // killed rather than shut down ends a session.
                Err(_) => break,
            },
        };
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                // `None` means the handler already answered: only the staged
                // load does that, and a second response with the same id would
                // be a protocol violation.
                if let Some(response) = dispatch(&connection, state, request) {
                    connection
                        .sender
                        .send(Message::Response(response))
                        .map_err(|e| ServeError::Send(Box::new(e)))?;
                }
            }
            Message::Notification(notification) => {
                if notification.method == method::EXIT {
                    break;
                }
                let notification = if notification.method == method::DID_CHANGE {
                    coalesce_did_change(notification, connection.receiver.try_iter(), &mut queued)
                } else {
                    notification
                };
                // A notification is never answered -- that is what makes it one --
                // so a handler that cannot reach the client has nowhere to report
                // it, and `false` ends the loop exactly as a failed send does for
                // a request.
                if !document_notification(&connection, state, notification) {
                    break;
                }
            }
            Message::Response(_) => {
                // The server issues no requests yet, so a response is unsolicited.
            }
        }
    }
    // Explicitly, because it is the whole reason this function owns it: the
    // writer thread ends when the last sender goes, and the caller joins on that.
    drop(connection);
    Ok(())
}

/// The newest `didChange` for the same document among those already queued.
///
/// Every `didChange` costs a full parse and evaluation on this thread, and only
/// the last text a client sent is the document -- so a burst of keystrokes that
/// arrived while the previous one was being evaluated is served once, by its
/// newest member, instead of once per event. Nothing else is dropped: a message
/// that is not a superseded edit to this document goes into `queued`, which the
/// loop serves before reading the channel again, so ordering is preserved for
/// everything that survives.
///
/// The URI is read out of the raw params rather than a decoded struct, because a
/// notification whose params do not decode is not a superseded edit -- it is a
/// client bug, and `document_notification` is the thing that reports it.
fn coalesce_did_change(
    mut newest: Notification,
    rest: impl Iterator<Item = Message>,
    queued: &mut std::collections::VecDeque<Message>,
) -> Notification {
    let Some(uri) = changed_uri(&newest).map(str::to_owned) else {
        // No document named, so nothing can be known to supersede it. The
        // handler reports the params it cannot read.
        queued.extend(rest);
        return newest;
    };
    for message in rest {
        match message {
            Message::Notification(notification)
                if notification.method == method::DID_CHANGE
                    && changed_uri(&notification) == Some(uri.as_str()) =>
            {
                newest = notification;
            }
            other => queued.push_back(other),
        }
    }
    newest
}

/// The document a `didChange` names, as its params spell it.
fn changed_uri(notification: &Notification) -> Option<&str> {
    notification
        .params
        .get("textDocument")?
        .get("uri")?
        .as_str()
}

/// Open every root, keeping the failures beside the successes.
///
/// A bad root is not fatal -- the client may correct it in `initialize`, and
/// refusing to start would leave no channel to say why -- but it is not silent
/// either. `.ok()` used to stand here, which turned a missing directory or an
/// unusable source id into a root that simply was not in the list, with the
/// cause discarded before anything could report it.
fn open_roots(paths: &[PathBuf]) -> (Vec<SourceRoot>, Vec<FailedRoot>) {
    let mut opened = Vec::with_capacity(paths.len());
    let mut failed = Vec::new();
    // Ids for the whole set at once, not one path at a time: the rule that makes
    // them distinct can only be applied to a set. See
    // `gearbox_engine::default_source_ids` for why two roots sharing an id is a
    // silent loss of gears rather than a cosmetic clash.
    for (path, id) in paths.iter().zip(default_source_ids(paths)) {
        let spelling = path.display().to_string();
        match SourceId::new(id) {
            Err(e) => failed.push(FailedRoot {
                path: spelling,
                error: e.to_string(),
            }),
            Ok(id) => match SourceRoot::open(id, path) {
                Ok(root) => opened.push(root),
                Err(e) => failed.push(FailedRoot {
                    path: spelling,
                    error: e.to_string(),
                }),
            },
        }
    }
    (opened, failed)
}

/// Handle one `textDocument/*` notification. `false` means the peer is gone.
///
/// Every branch ends in a `publishDiagnostics`, including the ones with nothing
/// to say. Publishing an empty list is how LSP spells "this document is clean
/// now"; staying silent would leave the last set of squiggles on screen, which is
/// the stale-marker failure `cpt-gearbox-fr-editor-diagnostics` names.
fn document_notification(
    connection: &Connection,
    state: &mut State,
    mut notification: Notification,
) -> bool {
    // Taken out before the match so the arms can consume it: the caller owns the
    // notification and drops it the moment this returns, and for `didOpen` and
    // `didChange` these params *are* the document text. Deserializing from a
    // clone would copy the whole buffer, per keystroke, to no end.
    let params = std::mem::take(&mut notification.params);
    let method = notification.method.as_str();
    match method {
        method::DID_OPEN => {
            let params = match document_params::<DidOpenTextDocumentParams>(method, params) {
                Ok(params) => params,
                Err(report) => return report_undecodable(connection, &report),
            };
            let document = params.text_document;
            let uri = document.uri;
            state.documents.insert(uri.clone(), document.text);
            publish_document_diagnostics(connection, state, &uri, document.version)
        }
        method::DID_CHANGE => {
            let params = match document_params::<DidChangeTextDocumentParams>(method, params) {
                Ok(params) => params,
                Err(report) => return report_undecodable(connection, &report),
            };
            let uri = params.text_document.uri;
            let version = params.text_document.version;
            // Full sync, so the last event carries the whole document. Taking the
            // last rather than the first is what keeps a client that sent several
            // from being half-applied.
            let Some(change) = params.content_changes.into_iter().next_back() else {
                // No changes at all. Nothing moved, so nothing is republished:
                // the markers already on screen are still the right ones.
                return true;
            };
            if change.range.is_some() {
                // An incremental edit against a server that advertised `Full`.
                // Applying `text` as if it were the document would replace the
                // file with a fragment, and the diagnostics that followed would
                // be confident and wrong -- so the document is left as it was and
                // the mismatch is said out loud.
                return log_to_client(
                    connection,
                    &format!(
                        "ignoring an incremental change to `{uri}`: this server advertises \
                         textDocumentSync = Full, so send the whole document"
                    ),
                );
            }
            // **Only a document the editor said it had opened.** The argument in
            // `State::documents` for leaving the map uncapped is that LSP
            // guarantees a `didClose` for every `didOpen`; an entry this
            // notification created is outside that guarantee -- nothing removes
            // it -- and both its key and its text are unbounded client strings.
            // Nothing is published either: an empty list would claim a document
            // this server was never given is clean.
            if !state.documents.contains_key(&uri) {
                return log_to_client(
                    connection,
                    &format!(
                        "ignoring a change to `{uri}`: it was never opened, so send a \
                         `textDocument/didOpen` for it first"
                    ),
                );
            }
            state.documents.insert(uri.clone(), change.text);
            publish_document_diagnostics(connection, state, &uri, version)
        }
        method::DID_CLOSE => {
            let params = match document_params::<DidCloseTextDocumentParams>(method, params) {
                Ok(params) => params,
                Err(report) => return report_undecodable(connection, &report),
            };
            let uri = params.text_document.uri;
            state.documents.remove(&uri);
            // What is on disk is not this server's to complain about once the
            // editor has stopped showing it, and a marker that outlived its
            // buffer would point at text nobody can see.
            notify(
                connection,
                method::PUBLISH_DIAGNOSTICS,
                &PublishDiagnosticsParams {
                    uri,
                    version: None,
                    diagnostics: Vec::new(),
                },
            )
            .peer_alive()
        }
        // `initialized`, `$/cancelRequest`, and anything else a client
        // volunteers: nothing to do, and answering a notification is a protocol
        // error.
        _ => true,
    }
}

/// Decode a notification's params, or the sentence saying why not.
///
/// `Err` rather than an error response, because a notification has no id to
/// answer. A client sending params this server cannot read is a bug in the
/// client, and the loop's job is to survive it -- but not silently: for a
/// `didChange` the stored document keeps its previous text and nothing is
/// republished, so an editor left to infer this from stderr it cannot read goes
/// on showing markers computed from text its buffer no longer has. The document
/// is named when the params said which one, since that is the part a client
/// needs in order to resend it.
fn document_params<P: serde::de::DeserializeOwned>(
    method: &str,
    params: serde_json::Value,
) -> Result<P, String> {
    let named = params
        .get("textDocument")
        .and_then(|document| document.get("uri"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || "; no document was named, so none was updated".to_owned(),
            |uri| format!("; `{uri}` was not updated"),
        );
    serde_json::from_value(params).map_err(|e| format!("cannot read `{method}` params: {e}{named}"))
}

/// Report a notification this server could not read, on both channels.
///
/// stderr for whoever is reading the log, and `gearbox/log` for the client,
/// which cannot read stderr and is the only party able to send the notification
/// again.
fn report_undecodable(connection: &Connection, report: &str) -> bool {
    eprintln!("gearbox: {report}");
    log_to_client(connection, report)
}

/// Say something to the client that has no other channel to arrive on.
///
/// `LogParams` is one `String`, so this send cannot fail to serialize -- which
/// is what makes it usable as the report of a notification that did.
fn log_to_client(connection: &Connection, message: &str) -> bool {
    notify(
        connection,
        method::LOG,
        &LogParams {
            message: message.to_owned(),
        },
    )
    .peer_alive()
}

/// Evaluate one open document and send what is wrong with it.
///
/// The publication rule lives in `lsp::publishable`, and the two ways a document
/// yields nothing are deliberately not distinguished here: a file that is neither
/// a `product.gdl` nor a `gear.gdl`, and one that is a clean `product.gdl`, both
/// publish an empty list. The client's marker set matches the server's opinion
/// either way, which is the only property that matters to the person looking at
/// the editor.
fn publish_document_diagnostics(
    connection: &Connection,
    state: &State,
    uri: &str,
    version: Option<i32>,
) -> bool {
    let diagnostics = match state
        .documents
        .get(uri)
        .map(|text| document_diagnostics(state, uri, text))
    {
        Some(Ok(diagnostics)) => diagnostics.unwrap_or_default(),
        // A `file://` URI this server cannot decode names a document the editor
        // *is* showing, so publishing an empty list would report a real file as
        // clean. Said out loud instead, and no marker set is claimed.
        Some(Err(report)) => return log_to_client(connection, &report),
        None => Vec::new(),
    };

    match notify(
        connection,
        method::PUBLISH_DIAGNOSTICS,
        &PublishDiagnosticsParams {
            uri: uri.to_owned(),
            version,
            diagnostics,
        },
    ) {
        Delivery::Sent => true,
        // Nothing was published, so the markers the editor is showing are the
        // previous answer. It has no other way to learn that.
        Delivery::Dropped => log_to_client(
            connection,
            &format!(
                "diagnostics for `{uri}` could not be serialized and were not published; \
                 the markers on screen are from an earlier version of it"
            ),
        ),
        Delivery::PeerGone => false,
    }
}

/// What is wrong with `text`, read as the description `uri` names.
///
/// `Ok(None)` for a document there is nothing to say about -- not a description,
/// or not a local file at all. `Err` carries the sentence for a `file://` URI
/// this server could not decode, which is a document the editor really has open
/// and must not be reported as clean.
///
/// Split out so the rule can be tested without a connection.
fn document_diagnostics(
    state: &State,
    uri: &str,
    text: &str,
) -> Result<Option<Vec<LspDiagnostic>>, String> {
    let path = match lsp::path_from_uri(uri) {
        lsp::UriPath::Local(path) => path,
        lsp::UriPath::NotLocal => return Ok(None),
        lsp::UriPath::Undecodable => {
            return Err(format!(
                "cannot read `{uri}` as a path: its percent-escapes do not decode as UTF-8, \
                 so this document is not being diagnosed"
            ));
        }
    };
    // The source root this file sits in, when it sits in one: that is the
    // `load()` boundary `load_catalogue` gives a gear. `check_description`
    // ignores it for a product, which has none -- so a description is evaluated
    // the same way whether the question came from an editor or from the disk.
    //
    // **Which root owns a path is the engine's rule, and it answers it.** This
    // used to pick the deepest matching root, while `load_catalogue` attributes a
    // description to the first root in the list that contains it -- so with
    // nested roots the editor evaluated a file against a `load()` boundary no
    // catalogue entry had used, and a `load("//...")` the catalogue resolved was
    // underlined here.
    let source_root =
        gearbox_engine::owning_source_root(&state.roots, &path).map(|source| source.root.as_path());

    let Some(diagnostics) = gearbox_engine::check_description(&path, source_root, text) else {
        return Ok(None);
    };

    // Matched against the URI *this server* would have written, not against the
    // client's spelling: `gearbox_ir::file_uri` does not percent-encode and
    // Theia's `URI.toString()` does, so comparing the two raw would drop every
    // diagnostic on a path containing a space.
    let own = gearbox_ir::file_uri(&path);
    Ok(Some(
        diagnostics
            .into_iter()
            .filter(|diagnostic| lsp::publishable(diagnostic, &own))
            .map(lsp::to_lsp)
            .collect(),
    ))
}

#[allow(clippy::cognitive_complexity)]
fn dispatch(connection: &Connection, state: &mut State, request: Request) -> Option<Response> {
    let id = request.id.clone();
    match request.method.as_str() {
        method::INITIALIZE => Some(match cast::<InitializeParams>(request) {
            Ok((id, params)) => initialize(state, id, &params),
            Err(e) => invalid_params(id, &e),
        }),
        method::CATALOGUE_LOAD => {
            // **No root at all is an empty catalogue, not a refusal.** A fresh
            // install has none configured yet, and refusing made the Studio show
            // a disconnected engine where there is only nothing to read. A root
            // that was given and failed still refuses, by name.
            let refusal = if state.roots.is_empty() && state.failed_roots.is_empty() {
                require_initialized(state, &id)
            } else {
                require_ready(state, &id)
            };
            if let Some(refusal) = refusal {
                return Some(refusal);
            }
            catalogue_load(connection, state, id)
        }
        // Reading a description needs no source root: `product_load` evaluates
        // the file alone, and it is how a product's own `sources` are learned
        // before any root is open.
        method::PRODUCT_LOAD => Some(match require_initialized(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<ProductLoadParams>(request) {
                Ok((id, params)) => product_load(id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_RESOLVE => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<ResolveParams>(request) {
                Ok((id, params)) => resolve(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_RESOLVE_PREVIEW => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<ResolvePreviewParams>(request) {
                Ok((id, params)) => resolve_preview(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_LOCK => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<LockParams>(request) {
                Ok((id, params)) => lock(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_ADD_GEAR => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<EditGearParams>(request) {
                Ok((id, params)) => edit_gear(state, id, &params, true),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_REMOVE_GEAR => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<EditGearParams>(request) {
                Ok((id, params)) => edit_gear(state, id, &params, false),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_SET_CONFIG => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<SetConfigParams>(request) {
                Ok((id, params)) => edit_set_config(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_SET_FEATURES => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<SetFeaturesParams>(request) {
                Ok((id, params)) => edit_set_features(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_ADD_PROFILE => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<AddProfileParams>(request) {
                Ok((id, params)) => edit_add_profile(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_REMOVE_PROFILE => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<RemoveProfileParams>(request) {
                Ok((id, params)) => edit_remove_profile(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_SET_PROFILE_FIELD => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<SetProfileFieldParams>(request) {
                Ok((id, params)) => edit_set_profile_field(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::PRODUCT_APPLY_EDITS => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<ApplyEditsParams>(request) {
                Ok((id, params)) => edit_apply_edits(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        // **Initialized, and nothing about the session's roots.** `create` is
        // judged by its `creation_boundary` (ADR `cpt-gearbox-adr-create-product`),
        // which is exactly what `require_ready` would re-couple it to: a
        // start-screen session that declares a boundary and opens no product has
        // no `state.roots`, and got `WORKSPACE_NOT_OPEN` from the one method that
        // must not be judged by the session. `creation_out_root` decides.
        method::PRODUCT_CREATE => Some(match require_initialized(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<CreateProductParams>(request) {
                Ok((id, params)) => create_product(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::GEAR_SCAFFOLD => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<ScaffoldGearParams>(request) {
                Ok((id, params)) => scaffold_gear(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::VALIDATE => Some(match require_ready(state, &id) {
            Some(refusal) => refusal,
            None => match cast::<ValidateParams>(request) {
                Ok((id, params)) => validate(state, id, &params),
                Err(e) => invalid_params(id, &e),
            },
        }),
        method::GENERATE_PLAN | method::GENERATE_APPLY | method::GENERATE_FILE => {
            Some(dispatch_generate(state, request))
        }
        // **No `require_ready`, and that is deliberate.** These two answer from
        // the client's own buffer and the interpreter's vocabulary; neither needs
        // a source root, so the gate every other request arm uses would refuse
        // completion in exactly the session that wants it most -- a `gear.gdl`
        // opened on its own. They belong to the `textDocument/*` surface, which
        // is gated on whether the document is known rather than on `initialize`,
        // and `document_notification` skips the gate for the same reason.
        method::COMPLETION => Some(match cast::<TextDocumentPositionParams>(request) {
            Ok((id, params)) => ok(id, &completion(connection, state, &params)),
            Err(e) => invalid_params(id, &e),
        }),
        method::HOVER => Some(match cast::<TextDocumentPositionParams>(request) {
            Ok((id, params)) => ok(id, &hover(connection, state, &params)),
            Err(e) => invalid_params(id, &e),
        }),
        other => Some(error(
            id,
            lsp_server::ErrorCode::MethodNotFound as i32,
            &format!("unknown method `{other}`"),
        )),
    }
}

/// What can be typed where the caret is.
///
/// Answered from the buffer this server holds, not from disk: the question is
/// about text that has not been saved and usually does not parse
/// (`cpt-gearbox-adr-gdl-completion-and-hover`). The language's own answer is
/// `gearbox_gdl::assist`; this only turns a position into an offset and the
/// answer into LSP's shape.
///
/// An empty list for a document this server does not have. That is ordinary --
/// an editor may ask before its `didOpen` has been forwarded -- so it is not an
/// error.
fn completion(
    connection: &Connection,
    state: &State,
    params: &TextDocumentPositionParams,
) -> Vec<CompletionItem> {
    let uri = params.text_document.uri.as_str();
    let Some(source) = state.documents.get(uri) else {
        return Vec::new();
    };
    let Some(path) = assistable_path(connection, uri, "completion") else {
        return Vec::new();
    };
    let offset =
        gearbox_gdl::assist::offset_of(source, params.position.line, params.position.character);
    let answer = gearbox_gdl::assist::completion(&path, source, offset);
    let kind = if answer.in_call {
        lsp::COMPLETION_FIELD
    } else {
        lsp::COMPLETION_FUNCTION
    };
    answer
        .suggestions
        .into_iter()
        .map(|suggestion| CompletionItem {
            label: suggestion.label,
            kind,
            // `detail` is the one line an editor shows beside the label, and for
            // a parameter that is its type. A construct has no second line worth
            // spending there: putting the summary in both fields, which this did
            // at first, shows the same sentence twice in the same popup.
            detail: suggestion.type_name,
            documentation: suggestion.detail,
        })
        .collect()
}

/// The path behind a URI an assist request names, or `None` with the reason
/// said out loud when it deserves one.
///
/// **The three `UriPath` cases are not the same answer.** `NotLocal` is an
/// `untitled:` buffer or a scheme this server has no business reading -- nothing
/// to offer, and nothing worth saying. `Undecodable` is a `file://` URI whose
/// percent-escapes are not UTF-8: the editor really does have that document
/// open, so answering it as though there were nothing there is the same shape of
/// wrong answer that `document_diagnostics` refuses to give for the same case.
/// Collapsing the two was how the first version of these handlers lost that
/// distinction.
fn assistable_path(connection: &Connection, uri: &str, what: &str) -> Option<PathBuf> {
    match lsp::path_from_uri(uri) {
        lsp::UriPath::Local(path) => Some(path),
        lsp::UriPath::NotLocal => None,
        lsp::UriPath::Undecodable => {
            log_to_client(
                connection,
                &format!(
                    "cannot read `{uri}` as a path: its percent-escapes do not decode as \
                     UTF-8, so {what} is not being offered for this document"
                ),
            );
            None
        }
    }
}

/// The documentation for whatever the caret is inside.
fn hover(
    connection: &Connection,
    state: &State,
    params: &TextDocumentPositionParams,
) -> Option<Hover> {
    let uri = params.text_document.uri.as_str();
    let source = state.documents.get(uri)?;
    let path = assistable_path(connection, uri, "hover")?;
    let offset =
        gearbox_gdl::assist::offset_of(source, params.position.line, params.position.character);
    gearbox_gdl::assist::hover(&path, source, offset).map(|contents| Hover { contents })
}

fn initialize(state: &mut State, id: RequestId, params: &InitializeParams) -> Response {
    if !params.roots.is_empty() {
        let (roots, failed) =
            open_roots(&params.roots.iter().map(PathBuf::from).collect::<Vec<_>>());
        state.roots = roots;
        state.failed_roots = failed;
        // A catalogue is only ever the catalogue *of these roots*. Keeping it
        // across a re-`initialize` meant a second `initialize` naming different
        // roots -- which is exactly what a reconnecting client sends -- was
        // answered for the rest of the session out of a cache built from the
        // first set. Cheap to be wrong about, and impossible to notice.
        //
        // **Inside this `if`, deliberately.** `state.roots` is assigned here and
        // nowhere else, so the cache provably cannot outlive a change of roots,
        // and an `initialize` naming *no* roots changes none: what it keeps is
        // still the catalogue of exactly these roots. Clearing it there would
        // discard a valid cache and buy a full rescan with it. That is not
        // obvious from the outside -- it has been reported as a bug -- so
        // `initialize_tests.rs` pins all three cases rather than leaving the
        // invariant to be inferred from where two statements sit.
        state.catalogue = None;
        state.registry_plans.clear();
    }
    state.initialized = true;
    state.allow_writes = params.allow_writes;
    state.workspace = params.workspace.as_deref().map(PathBuf::from);
    // Replaced wholesale when named, and left alone when not: a client that
    // declares a creation boundary once, at boot, and then opens products
    // without repeating it keeps the boundary it declared.
    if let Some(boundary) = params.creation_boundary.as_ref() {
        state.creation_boundary = Some(CreationBoundaryState {
            roots: boundary.roots.iter().map(PathBuf::from).collect(),
            workspace: boundary.workspace.as_deref().map(PathBuf::from),
        });
    }

    ok(
        id,
        &InitializeResult {
            server_info: ServerInfo {
                name: "gearbox".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            capabilities: Capabilities {
                catalogue: true,
                staged_catalogue: true,
                // M4 and M5 landed, so both are true and the client's "needs
                // the resolver / generator" notices disappear on their own --
                // which is what the capabilities were for.
                resolve: true,
                generate: true,
                // Echoed back, so a client that forgot to ask for writes can see
                // that it forgot instead of finding out from a refusal later.
                writes: state.allow_writes,
                // The LSP half of the same object. See `Capabilities`.
                text_document_sync: crate::lsp::SYNC_FULL,
                completion_provider: crate::protocol::CompletionOptions {
                    resolve_provider: false,
                },
                hover_provider: true,
            },
            // `SourceRoot::root` is already canonicalized, which is what makes
            // it safe to join a `gdl_path` onto without `..` ambiguity.
            roots: state
                .roots
                .iter()
                .map(|root| ResolvedRoot {
                    id: root.id.as_str().to_owned(),
                    path: root.root.display().to_string(),
                })
                .collect(),
            failed_roots: state.failed_roots.clone(),
        },
    )
}

/// Refuse a request that arrived before `initialize`, or `None` to go ahead.
///
/// The half of [`require_ready`] that every method needs, including the ones
/// judged by something other than the session's roots.
fn require_initialized(state: &State, id: &RequestId) -> Option<Response> {
    (!state.initialized).then(|| {
        error(
            id.clone(),
            error_code::NOT_INITIALIZED,
            "`initialize` must come first",
        )
    })
}

/// Refuse a request that cannot be served yet, or `None` to go ahead.
///
/// Factored out because all three of the new methods need the same two guards
/// and a copy each would be three places for them to drift apart.
fn require_ready(state: &State, id: &RequestId) -> Option<Response> {
    if let Some(refusal) = require_initialized(state, id) {
        return Some(refusal);
    }
    if state.roots.is_empty() {
        // Naming the failures as well as the absence: a client that ignored the
        // `initialize` result would otherwise get "no source root is open" for a
        // root it did pass.
        let why = if state.failed_roots.is_empty() {
            "no source root is open; pass `roots` to `initialize` or `--root` to the CLI".to_owned()
        } else {
            format!(
                "no source root is open; every root given failed to open: {}",
                state
                    .failed_roots
                    .iter()
                    .map(|r| format!("{}: {}", r.path, r.error))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        };
        return Some(error(id.clone(), error_code::WORKSPACE_NOT_OPEN, &why));
    }
    None
}

/// Refuse a mutating method in a session that declared no write capability.
///
/// **One function, because five handlers spelled this gate out and the copies
/// diverged.** `create_product` and `scaffold_gear` had `!state.allow_writes &&
/// !params.dry_run`, which let a read-only session past: `create`'s clone branch
/// then read a `.gdl` and handed its whole text back in `after`, and `scaffold`
/// reached `writable_out_root` and `exists()`, so which destinations exist and
/// which sit inside a source root could be read off the refusals. A client
/// without write capability must not learn anything about the filesystem, which
/// is what `edit_gear` says above itself and what the two copies stopped doing.
///
/// **It takes no `dry_run`, on purpose.** The right answer does not depend on
/// one, so a parameter for it would be the same trap with one call site: the
/// dry run is decided after this gate, by whether anything is written.
fn require_writes(state: &State, id: &RequestId) -> Option<Response> {
    (!state.allow_writes).then(|| {
        error(
            id.clone(),
            error_code::WRITES_NOT_ALLOWED,
            "this session declared no write capability, so nothing will be written; \
             pass `allow_writes: true` to `initialize` if the client is meant to change files",
        )
    })
}

/// Evaluate a `product.gdl`.
fn product_load(id: RequestId, params: &ProductLoadParams) -> Response {
    let path = PathBuf::from(&params.path);
    let source = match gearbox_engine::product::read_product(&path) {
        Ok(source) => source,
        Err(diagnostics) => {
            return error_with_diagnostics(
                id,
                error_code::PRODUCT_LOAD_FAILED,
                &format!("`{}` could not be read", params.path),
                diagnostics.as_slice(),
            );
        }
    };
    let scan = gearbox_engine::product::eval_product_text(&path, None, &source);
    match scan.intent {
        Some(intent) => ok(
            id,
            &ProductLoadResult {
                source,
                intent,
                diagnostics: scan.diagnostics.as_slice().to_vec(),
            },
        ),
        // No intent means the file is not a product at all, so there is nothing
        // partial to hand back -- unlike a resolution, which is useful even when
        // it reports errors.
        None => error_with_diagnostics(
            id,
            error_code::PRODUCT_LOAD_FAILED,
            &format!("`{}` could not be evaluated", params.path),
            scan.diagnostics.as_slice(),
        ),
    }
}

/// Resolve a product for one profile.
/// One resolution, and everything two callers need from it.
struct Resolved {
    product: ResolvedProduct,
    /// `templates = path(...)`, as the description wrote it.
    ///
    /// Carried here rather than read off the lock because it is an input to
    /// generation, not a resolution decision: the lock says what the product
    /// resolves to, and which directory the chart templates came from does not
    /// change that answer.
    templates: Option<String>,
    /// Sources whose crates the build takes from a registry, by id -- also an
    /// input to generation rather than a resolution decision.
    registry_sources: BTreeMap<SourceId, String>,
    explanation: ExplanationGraph,
    /// The description's diagnostics plus the resolution's.
    diagnostics: Vec<Diagnostic>,
    profile: ProfileId,
}

/// Evaluate and resolve, or hand back the refusal to send.
///
/// Shared by `resolve` and `lock` so the two cannot answer about different
/// resolutions of the same request -- which is the whole reason the lock text is
/// not computed from a `ResolveResult` the client already has: the client would
/// then be re-serializing, and only the engine may decide the lock's bytes.
/// Resolve what is on disk, or what a caller proposes putting there.
///
/// `source` is the description's text when the answer is about text that does not
/// exist yet -- `gearbox/product/resolvePreview` supplies it. Everything else is
/// identical, deliberately: the roots, the catalogue and the sources are the real
/// ones, and the path is the real path, so a preview is an answer about *this*
/// product rather than about a hypothetical one somewhere else.
fn resolve_once(
    state: &mut State,
    id: &RequestId,
    path_str: &str,
    profile: Option<&str>,
    source: Option<&str>,
) -> Result<Resolved, Response> {
    let path = PathBuf::from(path_str);
    let scan = match source {
        Some(text) => gearbox_engine::product::eval_product_text(&path, None, text),
        None => gearbox_engine::product::load_product(&path, None),
    };
    let mut diagnostics = scan.diagnostics.as_slice().to_vec();
    let Some(intent) = scan.intent else {
        return Err(error_with_diagnostics(
            id.clone(),
            error_code::PRODUCT_LOAD_FAILED,
            &format!("`{path_str}` could not be evaluated"),
            &diagnostics,
        ));
    };

    let profile = match profile {
        Some(named) => match ProfileId::new(named) {
            Ok(profile) => profile,
            Err(e) => {
                return Err(error(
                    id.clone(),
                    error_code::RESOLVE_FAILED,
                    &format!("`{named}` is not a valid profile id: {e}"),
                ));
            }
        },
        None => intent.default_profile.clone(),
    };

    // Built before the catalogue borrow, not after: `catalogue_for` needs
    // The lock's `sources` need the catalogue *and* the roots at once: the digests
    // come from the catalogue, which is the thing that read the descriptions,
    // while the paths come from the roots. `catalogue_for` borrows `&mut state`
    // for as long as its result lives, so the roots are copied out first -- a
    // `SourceRoot` is an id, a path and a string, and there are one or two of
    // them.
    let roots = state.roots.clone();
    let (catalogue, loaded) = catalogue_for(state);
    let sources = gearbox_engine::lock_sources(&roots, catalogue, &path);
    let resolution = gearbox_engine::resolve::resolve_at(catalogue, &intent, &profile, Some(&path));
    let product =
        gearbox_engine::resolve::product::assemble(catalogue, &intent, &resolution, sources);
    let explanation = gearbox_engine::resolve::product::explain(catalogue, &intent, &resolution);

    // The product's own diagnostics are already inside it; the ones added here
    // are the description's, which resolution never sees, and the catalogue
    // load's when this request is what triggered it.
    diagnostics.extend(loaded);
    diagnostics.extend(resolution.diagnostics.as_slice().iter().cloned());
    Ok(Resolved {
        product,
        templates: intent.templates,
        registry_sources: gearbox_engine::published::registry_sources(&intent.sources),
        explanation,
        diagnostics,
        profile,
    })
}

fn resolve(state: &mut State, id: RequestId, params: &ResolveParams) -> Response {
    match resolve_once(state, &id, &params.path, params.profile.as_deref(), None) {
        Err(refusal) => refusal,
        Ok(resolved) => ok(
            id,
            &ResolveResult {
                product: Some(resolved.product),
                explanation: Some(resolved.explanation),
                diagnostics: resolved.diagnostics,
            },
        ),
    }
}

/// Resolve the description a configurator is about to write, without writing it.
///
/// Answers "what would this product become" before the person commits to finding
/// out. The edits are applied to the text in memory with the same functions the
/// write path uses -- `add_gear` then `apply_product_edits` -- so the preview and
/// the write cannot drift: they are the same transformation, resolved once and
/// applied once.
///
/// No write gate, because there is no write: the file is read, never opened for
/// writing, and the lock is not touched.
fn resolve_preview(state: &mut State, id: RequestId, params: &ResolvePreviewParams) -> Response {
    let path = PathBuf::from(&params.path);
    let uri = gearbox_ir::file_uri(&path);
    let before = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            return error(
                id,
                error_code::PRODUCT_LOAD_FAILED,
                &format!("cannot read `{}`: {e}", params.path),
            );
        }
    };

    let mut proposed = before;
    if let Some(add) = params.add.as_ref() {
        match gearbox_gdl::edit::add_gear(&uri, &proposed, &add.gear, &add.source) {
            Ok(edit) => {
                if let Some(next) = edit.changed() {
                    next.clone_into(&mut proposed);
                }
            }
            Err(diagnostics) => {
                return error_with_diagnostics(
                    id,
                    error_code::EDIT_REFUSED,
                    &format!("`{}` could not be added for the preview", add.gear),
                    diagnostics.as_slice(),
                );
            }
        }
    }
    if !params.edits.is_empty() {
        match apply_product_edits(&uri, &proposed, &params.edits) {
            Ok(edit) => {
                if let Some(next) = edit.changed() {
                    next.clone_into(&mut proposed);
                }
            }
            Err(diagnostics) => {
                return error_with_diagnostics(
                    id,
                    error_code::EDIT_REFUSED,
                    "the proposed edits could not be applied for the preview",
                    diagnostics.as_slice(),
                );
            }
        }
    }

    match resolve_once(
        state,
        &id,
        &params.path,
        params.profile.as_deref(),
        Some(&proposed),
    ) {
        Err(refusal) => refusal,
        Ok(resolved) => ok(
            id,
            &ResolveResult {
                product: Some(resolved.product),
                explanation: Some(resolved.explanation),
                diagnostics: resolved.diagnostics,
            },
        ),
    }
}

/// The canonical lock text for one profile.
fn lock(state: &mut State, id: RequestId, params: &LockParams) -> Response {
    let mut resolved = match resolve_once(state, &id, &params.path, params.profile.as_deref(), None)
    {
        Err(refusal) => return refusal,
        Ok(resolved) => resolved,
    };
    // The same rule generation uses, so the lock compared against is the lock in
    // the tree generation writes. A different directory would be a diff nobody
    // could act on.
    //
    // **And through the same gate.** `out` is a client field, `compare_to_disk`
    // reads `<out>/product.lock` and returns its bytes verbatim in
    // `on_disk.canonical`, so unchecked it read any `product.lock` on the machine
    // through a method that cannot write one. `prepare_generate` runs the same
    // field through `writable_out_root`; this is the same rule applied to the
    // same value, and its "a test can point somewhere else" docstring is not a
    // reason to accept it off the wire.
    let lock_root = match default_out_root(state, params.out.as_deref(), &resolved)
        .and_then(|root| writable_out_root(state, &root))
    {
        Ok(root) => root,
        Err(refusal) => return error(id, error_code::EDIT_REFUSED, &refusal),
    };
    let lock_path = lock_root.join("product.lock");

    // The same redaction generation runs, and for the same reason:
    // `write_canonical` renders `gears[].config` verbatim, so a lock written by
    // an earlier build that still carries a literal credential would have it
    // sent to the client and shown in the Lock widget. `GBX0705` says out loud
    // that a value was replaced.
    let product_dir = Path::new(&params.path).parent().map(Path::to_path_buf);
    let mut reported = gearbox_ir::Diagnostics::new();
    let (catalogue, loaded) = catalogue_for(state);
    let redacted = match gearbox_engine::secrets::redact_product(
        &resolved.product,
        Some(catalogue),
        product_dir.as_deref(),
        &mut reported,
    ) {
        std::borrow::Cow::Borrowed(_) => None,
        std::borrow::Cow::Owned(product) => Some(product),
    };
    if let Some(product) = redacted {
        resolved.product = product;
    }
    resolved.diagnostics.extend(loaded);
    resolved.diagnostics.extend(reported.iter().cloned());

    match gearbox_lock::write_canonical(&resolved.product) {
        Ok(canonical) => {
            let on_disk = compare_to_disk(&lock_path, &resolved.product);
            ok(
                id,
                &LockResult {
                    canonical,
                    lock_hash: resolved.product.product.lock_hash.clone(),
                    profile: resolved.profile.to_string(),
                    lock_path: lock_path.display().to_string(),
                    on_disk,
                    diagnostics: resolved.diagnostics,
                },
            )
        }
        // A product that resolved but cannot be written is a defect in the lock
        // writer, not in the description, so it carries the resolution's
        // diagnostics rather than pretending the description was at fault.
        Err(e) => error_with_diagnostics(
            id,
            error_code::RESOLVE_FAILED,
            &format!("the resolved product could not be written as a lock: {e}"),
            &resolved.diagnostics,
        ),
    }
}

/// Add or remove a gear in a product description.
///
/// Two gates before anything is read, let alone written, and they refuse in that
/// order because a client without write capability should not learn anything
/// about which paths exist:
///
/// 1. the client declared write capability at `initialize`;
/// 2. the path is inside a declared source root or beside a known product.
///
/// The write itself is temp-file-and-rename, so an interrupted run cannot leave a
/// truncated description behind -- ADR `cpt-gearbox-adr-authoring-ownership-tiers`
/// asks for transactional writes, and for one file that is what transactional
/// means.
fn edit_gear(state: &mut State, id: RequestId, params: &EditGearParams, add: bool) -> Response {
    if let Some(refusal) = require_writes(state, &id) {
        return refusal;
    }

    let requested = PathBuf::from(&params.path);
    let path = match writable_path(state, &requested) {
        Ok(path) => path,
        Err(refusal) => return error(id, error_code::EDIT_REFUSED, &refusal),
    };

    let before = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            return error(
                id,
                error_code::EDIT_REFUSED,
                &format!("cannot read `{}`: {e}", params.path),
            );
        }
    };

    let uri = gearbox_ir::file_uri(&path);
    let outcome = if add {
        let Some(source_id) = params.source.as_deref() else {
            return error(
                id,
                error_code::EDIT_REFUSED,
                "adding a gear needs `source`: which declared source it comes from",
            );
        };
        gearbox_gdl::edit::add_gear(&uri, &before, &params.gear, source_id)
    } else {
        gearbox_gdl::edit::remove_gear(&uri, &before, &params.gear)
    };

    let edit = match outcome {
        Ok(edit) => edit,
        Err(diagnostics) => {
            return error_with_diagnostics(
                id,
                error_code::EDIT_REFUSED,
                &format!("`{}` could not be edited", params.path),
                diagnostics.as_slice(),
            );
        }
    };

    let after = edit.changed().unwrap_or(&before).to_owned();
    let changed = edit.changed().is_some();

    if changed
        && !params.dry_run
        && let Err(e) = write_atomically(&path, &after)
    {
        return error(
            id,
            error_code::EDIT_REFUSED,
            &format!("cannot write `{}`: {e}", params.path),
        );
    }

    ok(
        id,
        &EditGearResult {
            changed,
            written: changed && !params.dry_run,
            before,
            after,
            diagnostics: Vec::new(),
        },
    )
}

fn edit_set_config(state: &mut State, id: RequestId, params: &SetConfigParams) -> Response {
    edit_with(state, id, &params.path, params.dry_run, |uri, before| {
        gearbox_gdl::edit::set_gear_config(
            uri,
            before,
            &params.gear,
            &params.key,
            params.value.as_ref(),
        )
    })
}

fn edit_set_features(state: &mut State, id: RequestId, params: &SetFeaturesParams) -> Response {
    edit_with(state, id, &params.path, params.dry_run, |uri, before| {
        gearbox_gdl::edit::set_gear_features(uri, before, &params.gear, &params.features)
    })
}

fn edit_add_profile(state: &mut State, id: RequestId, params: &AddProfileParams) -> Response {
    let fields = params
        .fields
        .iter()
        .map(|f| (f.name.clone(), f.value.clone()))
        .collect::<Vec<_>>();
    edit_with(state, id, &params.path, params.dry_run, |uri, before| {
        gearbox_gdl::edit::add_profile(uri, before, &params.kind, &params.id, &fields)
    })
}

fn edit_remove_profile(state: &mut State, id: RequestId, params: &RemoveProfileParams) -> Response {
    edit_with(state, id, &params.path, params.dry_run, |uri, before| {
        gearbox_gdl::edit::remove_profile(uri, before, &params.id)
    })
}

fn edit_set_profile_field(
    state: &mut State,
    id: RequestId,
    params: &SetProfileFieldParams,
) -> Response {
    edit_with(state, id, &params.path, params.dry_run, |uri, before| {
        gearbox_gdl::edit::set_profile_field(
            uri,
            before,
            &params.id,
            &params.field,
            params.value.as_deref(),
        )
    })
}

fn edit_apply_edits(state: &mut State, id: RequestId, params: &ApplyEditsParams) -> Response {
    edit_with(state, id, &params.path, params.dry_run, |uri, before| {
        if params
            .expected_before
            .as_deref()
            .is_some_and(|expected| expected != before)
        {
            let mut diagnostics = gearbox_ir::Diagnostics::new();
            diagnostics.push(gearbox_ir::Diagnostic::error(
                gearbox_ir::DiagnosticCode::GdlEval,
                "the document changed after preview",
                "reload the preview and confirm again",
            ));
            return Err(diagnostics);
        }
        let edited = apply_product_edits(uri, before, &params.edits)?;
        if let Some(after) = edited.changed() {
            let scan =
                gearbox_engine::product::eval_product_text(Path::new(&params.path), None, after);
            if scan.intent.is_none() {
                return Err(scan.diagnostics);
            }
        }
        Ok(edited)
    })
}

/// Refuse a batch whose own removals would move an entry a later edit addresses.
///
/// Every [`PluginTarget`] is a position in the text the *client* previewed, but
/// the fold below rewrites that text as it goes. Removing one entry shifts every
/// later entry under the same host down by one, so a batch that removes an entry
/// and then edits a second one under the same host would aim the second edit at
/// the wrong `plugin(...)` — and `names_entry` would only catch it when the two
/// happen to name different implementations.
///
/// Refused rather than rebased. Rebasing is a silent reinterpretation of what
/// was confirmed, and the preview a person agreed to was computed without it.
/// The client has no reason to build such a batch — removals are applied on
/// their own, drafts hold only config and profile edits — so this is a guard
/// against a future caller, not a case to be clever about.
fn refuse_shifted_targets(edits: &[ProductEdit]) -> Result<(), gearbox_ir::Diagnostics> {
    let target_of = |edit: &ProductEdit| match edit {
        ProductEdit::RemovePlugin { target }
        | ProductEdit::SetPluginConfig { target, .. }
        | ProductEdit::SetPluginProfiles { target, .. } => Some(target.clone()),
        _ => None,
    };
    let removed: Vec<PluginTarget> = edits
        .iter()
        .filter_map(|edit| match edit {
            ProductEdit::RemovePlugin { target } => Some(target.clone()),
            _ => None,
        })
        .collect();
    if removed.is_empty() {
        return Ok(());
    }
    for edit in edits {
        let Some(target) = target_of(edit) else {
            continue;
        };
        let shifted = removed.iter().any(|gone| {
            gone.gear == target.gear
                && (gone.entry_index < target.entry_index
                    // The same entry named twice in one batch is the same trap:
                    // the second edit would land on whatever slid into its place.
                    || (gone.entry_index == target.entry_index
                        && !matches!(edit, ProductEdit::RemovePlugin { .. })))
        });
        if shifted {
            let mut diagnostics = gearbox_ir::Diagnostics::new();
            diagnostics.push(gearbox_ir::Diagnostic::error(
                gearbox_ir::DiagnosticCode::GdlEval,
                format!(
                    "removing a plugin from `{}` moves the other connections this batch edits",
                    target.gear
                ),
                "apply the pending changes first, then remove the connection",
            ));
            return Err(diagnostics);
        }
    }
    Ok(())
}

/// Fold every edit onto the same text, in order. Fail the whole batch if any
/// step refuses — nothing is written until the fold succeeds.
fn apply_product_edits(
    uri: &str,
    before: &str,
    edits: &[ProductEdit],
) -> Result<gearbox_gdl::edit::Edit, gearbox_ir::Diagnostics> {
    refuse_shifted_targets(edits)?;
    let mut current = before.to_owned();
    let mut changed = false;
    for edit in edits {
        let step = match edit {
            ProductEdit::AddGear { gear, source } => {
                gearbox_gdl::edit::add_gear(uri, &current, gear, source)?
            }
            ProductEdit::RemoveGear { gear } => {
                gearbox_gdl::edit::remove_gear(uri, &current, gear)?
            }
            ProductEdit::AddSource { id, at } => {
                gearbox_gdl::edit::add_source(uri, &current, id, at)?
            }
            ProductEdit::RemoveSource { id } => {
                gearbox_gdl::edit::remove_source(uri, &current, id)?
            }
            ProductEdit::SetConfig { gear, key, value } => {
                gearbox_gdl::edit::set_gear_config(uri, &current, gear, key, value.as_ref())?
            }
            ProductEdit::SetFeatures { gear, features } => {
                gearbox_gdl::edit::set_gear_features(uri, &current, gear, features)?
            }
            ProductEdit::SetProviderOption {
                scope,
                entry_index,
                primitive,
                key,
                value,
            } => gearbox_gdl::edit::set_provider_option(
                uri,
                &current,
                scope,
                *entry_index,
                primitive,
                key,
                value.as_ref(),
            )?,
            ProductEdit::AddPlugin { gear, plugin } => {
                gearbox_gdl::edit::add_gear_plugin(uri, &current, gear, plugin)?
            }
            ProductEdit::AddPluginSelection {
                gear,
                plugin,
                profiles,
            } => gearbox_gdl::edit::add_plugin_selection(uri, &current, gear, plugin, profiles)?,
            ProductEdit::RemovePlugin { target } => gearbox_gdl::edit::edit_plugin_entry(
                uri,
                &current,
                &target.gear,
                target.entry_index,
                &target.plugin,
                None,
                None,
                true,
            )?,
            ProductEdit::SetPluginConfig { target, key, value } => {
                gearbox_gdl::edit::edit_plugin_entry(
                    uri,
                    &current,
                    &target.gear,
                    target.entry_index,
                    &target.plugin,
                    Some((key, value.as_ref())),
                    None,
                    false,
                )?
            }
            ProductEdit::SetPluginProfiles { target, profiles } => {
                gearbox_gdl::edit::edit_plugin_entry(
                    uri,
                    &current,
                    &target.gear,
                    target.entry_index,
                    &target.plugin,
                    None,
                    Some(profiles),
                    false,
                )?
            }
            ProductEdit::SetPlugins { gear, plugins } => {
                gearbox_gdl::edit::set_gear_plugins(uri, &current, gear, plugins)?
            }
            ProductEdit::SetProfileField {
                profile,
                field,
                value,
            } => gearbox_gdl::edit::set_profile_field(
                uri,
                &current,
                profile,
                field,
                value.as_deref(),
            )?,
        };
        if let Some(next) = step.changed() {
            next.clone_into(&mut current);
            changed = true;
        }
    }
    if changed {
        Ok(gearbox_gdl::edit::Edit::Changed { source: current })
    } else {
        Ok(gearbox_gdl::edit::Edit::Unchanged)
    }
}

fn edit_with(
    state: &mut State,
    id: RequestId,
    path_str: &str,
    dry_run: bool,
    apply: impl FnOnce(&str, &str) -> Result<gearbox_gdl::edit::Edit, gearbox_ir::Diagnostics>,
) -> Response {
    if let Some(refusal) = require_writes(state, &id) {
        return refusal;
    }
    let requested = PathBuf::from(path_str);
    let path = match writable_path(state, &requested) {
        Ok(path) => path,
        Err(refusal) => return error(id, error_code::EDIT_REFUSED, &refusal),
    };
    let before = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            return error(
                id,
                error_code::EDIT_REFUSED,
                &format!("cannot read `{path_str}`: {e}"),
            );
        }
    };
    let uri = gearbox_ir::file_uri(&path);
    let edit = match apply(&uri, &before) {
        Ok(edit) => edit,
        Err(diagnostics) => {
            return error_with_diagnostics(
                id,
                error_code::EDIT_REFUSED,
                &format!("`{path_str}` could not be edited"),
                diagnostics.as_slice(),
            );
        }
    };
    respond_edit(id, &path, dry_run, before, edit)
}

#[allow(clippy::needless_pass_by_value)]
fn respond_edit(
    id: RequestId,
    path: &Path,
    dry_run: bool,
    before: String,
    edit: gearbox_gdl::edit::Edit,
) -> Response {
    let after = edit.changed().unwrap_or(&before).to_owned();
    let changed = edit.changed().is_some();
    if changed
        && !dry_run
        && let Err(e) = write_atomically(path, &after)
    {
        return error(
            id,
            error_code::EDIT_REFUSED,
            &format!("cannot write `{}`: {e}", path.display()),
        );
    }
    ok(
        id,
        &EditGearResult {
            changed,
            written: changed && !dry_run,
            before,
            after,
            diagnostics: Vec::new(),
        },
    )
}

fn create_product(state: &mut State, id: RequestId, params: &CreateProductParams) -> Response {
    if let Some(refusal) = require_writes(state, &id) {
        return refusal;
    }

    // **The ids, before anything touches the filesystem.** This function stamped
    // whatever arrived: an empty box wrote `id = ""` and a destination of
    // `products//product.gdl`, and a single space wrote `id = " "` into a product
    // that then resolved clean with no diagnostics -- a product whose identity is
    // a space, and nothing on the way in said no. `SourceId::new` is built
    // eighty lines below for the literal `"product"`, so the validator was
    // already here for a value that could not be wrong, and absent for the two
    // that come from a person.
    if let Err(refusal) = gearbox_ir::ProductId::new(params.id.trim()) {
        return error(
            id,
            error_code::EDIT_REFUSED,
            &format!("{refusal}; a product id looks like `payments-demo`"),
        );
    }
    if let Err(refusal) = gearbox_ir::ProfileId::new(params.profile_id.trim()) {
        return error(
            id,
            error_code::EDIT_REFUSED,
            &format!("{refusal}; a profile id looks like `dev`"),
        );
    }
    // The third field a person types, and the one that had nothing checking it:
    // `version` is rendered into the template or stamped onto a clone, and
    // `product()` does not check it either, so whatever arrived reached a written
    // file. `scaffold_gear_files` already requires a triple of the same field.
    if !is_semver(params.version.trim()) {
        return error(
            id,
            error_code::EDIT_REFUSED,
            &format!(
                "`{}` is not a version; a product version is a semver triple like `0.1.0`",
                params.version
            ),
        );
    }

    let requested = PathBuf::from(&params.path);
    // **Absolute, or refused by name.** A relative path used to be joined to the
    // engine process's working directory -- which is wherever the backend was
    // started, `ide/browser-app` in practice -- so `relative/dir/product.gdl`
    // previewed cleanly and would have written inside the IDE's own package.
    // Nobody choosing a destination means that directory.
    if !requested.is_absolute() {
        return error(
            id,
            error_code::EDIT_REFUSED,
            &format!(
                "`{}` is not an absolute path; choose a folder, or give its full path",
                params.path
            ),
        );
    }
    let parent_input = requested
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // **The one method judged by a boundary rather than by the session.** See
    // `CreationBoundary`: opening a product whose `sources` contain the place
    // products live used to make every later create refuse. Falls back to the
    // session when no boundary was declared, so the CLI is unaffected.
    let parent = match creation_out_root(state, parent_input) {
        Ok(path) => path,
        Err(refusal) => return error(id, error_code::EDIT_REFUSED, &refusal),
    };
    let Some(file_name) = requested.file_name() else {
        return error(
            id,
            error_code::EDIT_REFUSED,
            &format!("`{}` does not name a file", params.path),
        );
    };
    let path = parent.join(file_name);
    if path.exists() {
        return error(
            id,
            error_code::EDIT_REFUSED,
            &format!(
                "`{}` already exists; create refuses to overwrite",
                path.display()
            ),
        );
    }

    let after = if let Some(clone_from) = params.clone_from.as_deref() {
        let requested = PathBuf::from(clone_from);
        let source_path = match writable_path(state, &requested) {
            Ok(path) => path,
            Err(refusal) => return error(id, error_code::EDIT_REFUSED, &refusal),
        };
        let source = match std::fs::read_to_string(&source_path) {
            Ok(text) => text,
            Err(e) => {
                return error(
                    id,
                    error_code::EDIT_REFUSED,
                    &format!("cannot read clone source `{clone_from}`: {e}"),
                );
            }
        };
        let uri = format!("file://{}", PathBuf::from(clone_from).display());
        let stamped = match gearbox_gdl::edit::clone_product_text(
            &uri,
            &source,
            &params.id,
            &params.name,
            Some(params.version.as_str()),
        ) {
            Ok(text) => text,
            Err(diagnostics) => {
                return error_with_diagnostics(
                    id,
                    error_code::EDIT_REFUSED,
                    &format!("`{clone_from}` could not be cloned"),
                    diagnostics.as_slice(),
                );
            }
        };
        match rebase_cloned_paths(
            &uri,
            &stamped,
            &source_path,
            &parent,
            params.rebase_relative_paths,
        ) {
            Ok(text) => text,
            Err(Rebase::Refused(reason)) => return error(id, error_code::EDIT_REFUSED, &reason),
            Err(Rebase::Unparsed(diagnostics)) => {
                return error_with_diagnostics(
                    id,
                    error_code::EDIT_REFUSED,
                    &format!("`{clone_from}` could not be cloned"),
                    diagnostics.as_slice(),
                );
            }
        }
    } else {
        gearbox_gdl::edit::render_product_template(&gearbox_gdl::edit::CreateProductParams {
            id: params.id.clone(),
            name: params.name.clone(),
            version: params.version.clone(),
            sources: params
                .sources
                .iter()
                .map(|s| (s.id.clone(), s.at.clone()))
                .collect(),
            profile_kind: params.profile_kind.clone(),
            profile_id: params.profile_id.clone(),
        })
    };

    // Refuse to write (or preview) text that does not evaluate as a product.
    // Escaping closes the practical hole; this catches a broken template itself.
    let uri = gearbox_ir::file_uri(&path);
    let identity = gearbox_gdl::FileIdentity {
        uri,
        source: SourceId::new("product").unwrap_or_else(|_| unreachable!("`product` is kebab")),
        gdl_path: RelPath::new("product.gdl")
            .unwrap_or_else(|_| unreachable!("`product.gdl` is a valid rel path")),
        load_paths: None,
    };
    let outcome = gearbox_gdl::GdlEngine::new().eval_product(&identity, &after);
    if outcome.value.is_none() {
        return error_with_diagnostics(
            id,
            error_code::EDIT_REFUSED,
            &format!(
                "`{}` does not evaluate as a product description",
                params.path
            ),
            outcome.diagnostics.as_slice(),
        );
    }

    if !params.dry_run {
        if let Err(e) = std::fs::create_dir_all(&parent) {
            return error(
                id,
                error_code::EDIT_REFUSED,
                &format!("cannot create `{}`: {e}", parent.display()),
            );
        }
        if let Err(e) = write_atomically(&path, &after) {
            return error(
                id,
                error_code::EDIT_REFUSED,
                &format!("cannot write `{}`: {e}", params.path),
            );
        }
    }

    ok(
        id,
        &EditGearResult {
            changed: true,
            written: !params.dry_run,
            before: String::new(),
            after,
            diagnostics: Vec::new(),
        },
    )
}

/// Scaffold a new gear crate: `gear.gdl`, `Cargo.toml`, and `src/lib.rs`.
///
/// ADR-0010 tier 0 / `GeneratedOnce`: preview via `FilePlan[]`, refuse when the
/// destination already exists, write only under `writable_out_root`.
fn scaffold_gear(state: &mut State, id: RequestId, params: &ScaffoldGearParams) -> Response {
    if let Some(refusal) = require_writes(state, &id) {
        return refusal;
    }

    let gear_id = match GearId::new(params.id.trim()) {
        Ok(gear_id) => gear_id,
        // The validator's own sentence, not a restatement of the rule: a client
        // needs to know what was wrong with the id it sent, not only what the
        // shape is. `create_product` formats the refusal the same way.
        Err(refusal) => {
            return error(
                id,
                error_code::EDIT_REFUSED,
                &format!(
                    "{refusal}; a gear id is kebab-case, a single path segment, and not `.` \
                     or `..`"
                ),
            );
        }
    };

    let dest_dir = PathBuf::from(&params.destination_dir);
    // Same rule as `create_product`: a relative destination resolved against the
    // engine's working directory, which is nobody's choice of folder.
    if !dest_dir.is_absolute() {
        return error(
            id,
            error_code::EDIT_REFUSED,
            &format!(
                "`{}` is not an absolute path; choose a folder, or give its full path",
                params.destination_dir
            ),
        );
    }
    let parent = match writable_out_root(state, &dest_dir) {
        Ok(path) => path,
        Err(refusal) => return error(id, error_code::EDIT_REFUSED, &refusal),
    };
    let out_root = parent.join(gear_id.as_str());
    if out_root.exists() {
        return error(
            id,
            error_code::EDIT_REFUSED,
            &format!(
                "`{}` already exists; scaffold refuses to overwrite",
                out_root.display()
            ),
        );
    }

    // A locator on a `service` or `minimal` scaffold has nowhere to be written,
    // so it is a client mistake rather than a field to ignore. Refused rather
    // than dropped: silently discarding half a request is how a client learns
    // the wrong contract.
    if params.plugin.is_some() && params.kind != crate::protocol::GearKind::Plugin {
        return error(
            id,
            error_code::EDIT_REFUSED,
            "`plugin` describes what a plugin implements, so it only applies to `kind = \"plugin\"`",
        );
    }

    // The platform SDK, from the session's own roots, so the skeleton can carry
    // a real `#[toolkit::gear]` rather than a comment saying one comes next.
    // Relative to the new crate, or absolute when no relative path exists.
    let toolkit = gearbox_engine::generate::locate_toolkit(
        state.roots.iter().map(|root| root.root.as_path()),
    )
    .map(|dir| {
        use gearbox_engine::generate::{relative, to_slash};
        relative(&out_root, &dir).map_or_else(
            || dir.to_string_lossy().replace('\\', "/"),
            |rel| to_slash(&rel),
        )
    });

    // The plugin's SDK path arrives relative to the destination *as the client
    // wrote it*, and `out_root` is canonical. Where the two differ -- `/tmp` is a
    // link to `/private/tmp` on macOS -- the path climbs the wrong number of
    // levels from where the crate really is. It only ever reached a comment
    // before; as a Cargo dependency it has to be right, so it is resolved from
    // the written destination and re-expressed from the real one.
    let plugin_dep = params.plugin.as_ref().map(|plugin| {
        use gearbox_engine::generate::{normalize, relative, to_slash};
        let written = normalize(&dest_dir.join(gear_id.as_str()).join(&plugin.path));
        let real = written.canonicalize().unwrap_or(written);
        relative(&out_root, &real).map_or_else(
            || real.to_string_lossy().replace('\\', "/"),
            |rel| to_slash(&rel),
        )
    });

    let files = match scaffold_gear_files(params, toolkit.as_deref(), plugin_dep.as_deref()) {
        Ok(files) => files,
        Err(message) => return error(id, error_code::EDIT_REFUSED, &message),
    };

    // Refuse to scaffold text that does not evaluate as a gear declaration.
    let gdl = files
        .iter()
        .find(|(rel, _, _)| rel.as_str() == "gear.gdl")
        .map_or("", |(_, body, _)| body.as_str());
    let uri = gearbox_ir::file_uri(&out_root.join("gear.gdl"));
    let identity = gearbox_gdl::FileIdentity {
        uri,
        source: SourceId::new("scaffold").unwrap_or_else(|_| unreachable!("`scaffold` is kebab")),
        gdl_path: RelPath::new("gear.gdl")
            .unwrap_or_else(|_| unreachable!("`gear.gdl` is a valid rel path")),
        load_paths: None,
    };
    let outcome = gearbox_gdl::GdlEngine::new().eval_gear(&identity, gdl);
    if outcome.value.is_none() {
        return error_with_diagnostics(
            id,
            error_code::EDIT_REFUSED,
            "scaffolded `gear.gdl` does not evaluate as a gear description",
            outcome.diagnostics.as_slice(),
        );
    }

    let mut plans = Vec::with_capacity(files.len());
    for (rel, body, kind) in &files {
        let entry = gearbox_ir::FileEntry::text(
            rel.clone(),
            body.clone(),
            *kind,
            gearbox_ir::Ownership::GeneratedOnce,
        );
        plans.push(gearbox_ir::FilePlan {
            path: rel.clone(),
            action: gearbox_ir::FileAction::Create,
            ownership: gearbox_ir::Ownership::GeneratedOnce,
            kind: *kind,
            blake3: entry.digest(),
            preview_available: true,
        });
    }

    if !params.dry_run {
        if let Err(e) = std::fs::create_dir_all(out_root.join("src")) {
            return error(
                id,
                error_code::EDIT_REFUSED,
                &format!("cannot create `{}`: {e}", out_root.join("src").display()),
            );
        }
        for (rel, body, _) in &files {
            let path = out_root.join(rel.as_str());
            if let Err(e) = write_atomically(&path, body) {
                return error(
                    id,
                    error_code::EDIT_REFUSED,
                    &format!("cannot write `{}`: {e}", path.display()),
                );
            }
        }
    }

    ok(
        id,
        &ScaffoldGearResult {
            plan: GeneratePlanResult {
                plans,
                diagnostics: Vec::new(),
                out_root: out_root.display().to_string().replace('\\', "/"),
                // Scaffolding renders no product templates, so nothing can be
                // overridden here -- an empty list is the truth, not a stub.
                overridden_templates: Vec::new(),
                templates: Vec::new(),
            },
            // The text this method already built and evaluated. Carried because
            // the three file *paths* are the same for all three kinds, so a
            // preview of paths alone showed the shape choice doing nothing.
            gear_gdl: gdl.to_owned(),
        },
    )
}

/// One file a scaffold will write: its path under the gear directory, its body,
/// and what kind of file it is.
///
/// Named rather than left as a triple because it is threaded through the plan,
/// the preview and the write, and `(RelPath, String, FileKind)` says nothing
/// about which field is the path.
///
/// A [`RelPath`] rather than a `String`, because the plan needs one: built here,
/// where a path `RelPath` rejects is a refusal this function can return, instead
/// of in the request handler, where it was an `unreachable!()` resting on an
/// invariant established in this function -- and a fourth scaffold file with an
/// unusable path would have turned one client request into a process panic,
/// taking the stdio server down for the whole editor session.
type ScaffoldFile = (RelPath, String, gearbox_ir::FileKind);

/// A scaffold's three files: the description, the manifest and the lib.
///
/// `toolkit` is the path from the new crate to the platform SDK, when the
/// session's roots have one. With it the crate is a real gear -- a
/// `#[toolkit::gear]` the projector reads and a dependency that makes it build
/// -- so a gear created for a product is in the catalogue the moment it exists.
/// Without it the lib stays the comments it always was: an attribute written
/// against a dependency nobody could add is a crate that does not compile.
fn scaffold_gear_files(
    params: &ScaffoldGearParams,
    toolkit: Option<&str>,
    plugin_dep: Option<&str>,
) -> Result<Vec<ScaffoldFile>, String> {
    let crate_name = GearId::new(params.id.trim())
        // The validator's sentence, not a substitute for it: the client needs to
        // know what was wrong with the id it sent.
        .map_err(|e| format!("{e}; a gear id is kebab-case"))?
        .to_string();
    let lib_name = crate_name.replace('-', "_");
    if !is_semver(&params.version) {
        return Err("version must be a semver triple like `0.1.0`".to_owned());
    }

    // **Comments, not values, in `gear.gdl`, and that is the whole design.**
    // Every field the template offers is either projected from Rust or checked
    // against it: a `category` invented here would draw the unknown-category
    // warning on the first load, an `implements` naming a spec no described gear
    // declares is refused (GBX0519), and an `sdk` locator pointing at a directory
    // that does not exist makes the gear fail to load. So the template puts the
    // next declaration where it goes, with the sentence that says what decides
    // it, and leaves it commented until there is something true to write. The
    // one exception is a host chosen from a loaded catalogue: its spec is a fact,
    // so `implements` is written live.
    //
    // Values reach the templates already escaped: GDL strings through
    // `quote_string` (the text is evaluated as GDL right after, and Rust's debug
    // escaping is not Starlark's), TOML strings as basic strings, and anything
    // shown inside a `#` or `//` comment with its line breaks removed.
    let kind = match params.kind {
        crate::protocol::GearKind::Minimal => "minimal",
        crate::protocol::GearKind::Service => "service",
        crate::protocol::GearKind::Plugin => "plugin",
    };
    let plugin = params.plugin.as_ref().map(|plugin| {
        minijinja::context! {
            spec => gearbox_gdl::edit::quote_string(
                plugin
                    .spec
                    .strip_prefix(gearbox_engine::plugin::PLUGIN_BASE)
                    .unwrap_or(&plugin.spec),
            ),
            trait_ident => comment_safe(&plugin.trait_ident),
            crate_name => comment_safe(&plugin.crate_name),
            lib => comment_safe(&plugin.lib_ident),
            path => comment_safe(&plugin.path),
        }
    });
    let render = |key: &str, ctx: minijinja::Value| -> Result<String, String> {
        let source = gearbox_engine::TemplateSet::builtin(key)
            .ok_or_else(|| format!("no builtin scaffold template `{key}`"))?;
        gearbox_engine::generate::render_template("scaffold", source, ctx)
            .map_err(|e| format!("cannot render the scaffold's {key}: {e}"))
    };

    let gdl = render(
        "scaffold/gear.gdl",
        minijinja::context! {
            comment => comment_safe(&params.name),
            reference => GDL_REFERENCE,
            version => gearbox_gdl::version::CURRENT.to_string(),
            name => gearbox_gdl::edit::quote_string(&params.name),
            crate_name => gearbox_gdl::edit::quote_string(&crate_name),
            lib => gearbox_gdl::edit::quote_string(&lib_name),
            kind => kind,
            plugin => &plugin,
        },
    )?;

    // The crate that declares the host's trait, which a plugin implements.
    let sdk = match (params.kind, &params.plugin, plugin_dep) {
        (crate::protocol::GearKind::Plugin, Some(plugin), Some(path)) => {
            Some(minijinja::context! {
                key => toml_basic_string(&plugin.lib_ident),
                package => toml_basic_string(&plugin.crate_name),
                path => toml_basic_string(path),
            })
        }
        _ => None,
    };
    let cargo = render(
        "scaffold/Cargo.toml",
        minijinja::context! {
            crate_name => toml_basic_string(&crate_name),
            version => toml_basic_string(&params.version),
            lib => toml_basic_string(&lib_name),
            toolkit => toolkit.map(|toolkit| minijinja::context! {
                alias => gearbox_engine::generate::TOOLKIT_ALIAS,
                package => toml_basic_string(gearbox_engine::generate::TOOLKIT_PACKAGE),
                path => toml_basic_string(toolkit),
            }),
            sdk => sdk,
        },
    )?;

    // With the toolkit the crate is a real gear: `#[toolkit::gear(name = ...)]`
    // on a `Default` struct and an `impl Gear` whose `init` does nothing. The id
    // is kebab-case by now, so it needs no escaping. A plugin's trait is named,
    // not implemented: its methods live in the host's SDK, so an `impl` would be
    // a body invented here. Without the toolkit the lib is a comment: a crate
    // that does not compile is worse than one that is empty.
    let type_name: String = crate_name
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect();
    let lib = render(
        "scaffold/lib.rs",
        minijinja::context! {
            toolkit => toolkit.is_some(),
            id => &crate_name,
            type_name => type_name,
            kind => kind,
            plugin => plugin,
        },
    )?;

    Ok(vec![
        (rel_path("gear.gdl")?, gdl, gearbox_ir::FileKind::Text),
        (rel_path("Cargo.toml")?, cargo, gearbox_ir::FileKind::Toml),
        (rel_path("src/lib.rs")?, lib, gearbox_ir::FileKind::Rust),
    ])
}

/// One scaffold path, or the refusal saying it is not a usable relative path.
fn rel_path(path: &str) -> Result<RelPath, String> {
    RelPath::new(path).map_err(|e| format!("`{path}` is not a usable path for a scaffold: {e}"))
}

/// A value from the wire, made safe to write inside a `#` comment.
///
/// These fields are shown, not evaluated, so escaping is not the hazard -- a line
/// break is: a path carrying one would end the comment and put the rest of the
/// string on a line of its own, evaluated as GDL.
fn comment_safe(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

/// Where a person reading a scaffolded `gear.gdl` finds every field.
const GDL_REFERENCE: &str = "https://github.com/constructorfabric/gearbox/blob/main/docs/gdl.md";

/// Whether a path may be written, and why not when it may not.
///
/// `cpt-gearbox-fr-rpc-writes-opt-in` requires rejecting "any path outside the
/// declared workspace or source roots". A product description lives in neither a
/// gear source root nor nowhere -- it sits beside the products -- which is why the
/// client declares a workspace at `initialize` and this checks against both.
///
/// If nothing was declared, nothing is writable. Failing closed is the only
/// defensible default for a method that changes files.
fn writable_path(state: &State, path: &Path) -> Result<PathBuf, String> {
    let Ok(canonical) = path.canonicalize() else {
        return Err(format!("`{}` does not exist", path.display()));
    };
    if canonical.extension().and_then(|e| e.to_str()) != Some("gdl") {
        return Err(format!(
            "`{}` is not a `.gdl` description; this method edits descriptions only",
            path.display()
        ));
    }

    let mut allowed = Vec::new();
    for root in &state.roots {
        match root.root.canonicalize() {
            Ok(path) => allowed.push(path),
            Err(e) => {
                return Err(format!(
                    "cannot canonicalize source root `{}`: {e}",
                    root.root.display()
                ));
            }
        }
    }
    if let Some(workspace) = state.workspace.as_ref() {
        match workspace.canonicalize() {
            Ok(path) => allowed.push(path),
            Err(e) => {
                return Err(format!(
                    "cannot canonicalize workspace `{}`: {e}",
                    workspace.display()
                ));
            }
        }
    }

    if allowed.is_empty() {
        return Err(
            "no workspace and no source root were declared, so no path is writable".to_owned(),
        );
    }
    if allowed.iter().any(|root| canonical.starts_with(root)) {
        Ok(canonical)
    } else {
        Err(format!(
            "`{}` is outside the declared workspace and every source root",
            path.display()
        ))
    }
}

/// Whether a generation output root may be written, and the resolved path when
/// it may.
///
/// Asymmetric with [`writable_path`], and the asymmetry is the point:
///
/// * a description edit requires the path to *exist* and to be a `.gdl` file,
///   because that method rewrites a file the operator already has;
/// * generation *creates* files that are not `.gdl`, so a missing path is
///   allowed -- the nearest existing ancestor is what gets checked -- and the
///   path must **not** sit inside a source root. Writing generated Rust next
///   to human-authored Rust is ADR-0010 tier 5.
///
/// The workspace is still required. Failing closed: if nothing was declared,
/// nothing is writable.
fn writable_out_root(state: &State, path: &Path) -> Result<PathBuf, String> {
    let roots: Vec<PathBuf> = state.roots.iter().map(|root| root.root.clone()).collect();
    writable_out_root_against(&roots, state.workspace.as_deref(), path)
}

/// Why a clone's relative paths could not be carried to its new place.
enum Rebase {
    Refused(String),
    Unparsed(gearbox_ir::Diagnostics),
}

/// A cloned description's relative paths, re-based onto where it is written.
///
/// **A clone keeps what its paths mean, not how they are spelled.** `sources`,
/// `templates` and `target_dir` are relative to the description's own folder, so
/// copying `path("../../../gears-rust")` one level deeper names a directory that
/// does not exist and the clone cannot be opened -- measured. Each is resolved
/// from the source's folder and re-expressed from the destination's; a path with
/// no relative form from there (another volume) is written absolute rather than
/// guessed at, and an absolute one is left as written.
///
/// A relative `load(...)` has no such answer: the fragment it names is not
/// copied, so the clone is refused -- before anything is written -- unless it
/// lands in the source's own folder, where the fragment is still beside it.
fn rebase_cloned_paths(
    uri: &str,
    stamped: &str,
    source_path: &Path,
    destination_dir: &Path,
    rebase: bool,
) -> Result<String, Rebase> {
    use gearbox_engine::generate::{normalize, relative, to_slash};

    let Some(source_dir) = source_path.parent() else {
        return Ok(stamped.to_owned());
    };
    let same_folder = normalize(source_dir) == normalize(destination_dir);
    // The load check runs whether or not paths are re-based: a git clone keeps
    // its paths as written, but the fragment a relative `load` names is not
    // copied by either kind of clone.
    let (text, found) = gearbox_gdl::edit::rebase_product_paths(uri, stamped, |written| {
        if !rebase || same_folder || Path::new(written).is_absolute() {
            return None;
        }
        let absolute = normalize(&source_dir.join(written));
        Some(match relative(destination_dir, &absolute) {
            Some(rebased) => to_slash(&rebased),
            // No relative form: another volume, so another platform's rules.
            // `to_slash` is for relative paths -- on an absolute Windows path it
            // renders the root component as an empty segment, `C://Users` -- so
            // the absolute fallback is spelled with separators swapped instead.
            None => absolute.to_string_lossy().replace('\\', "/"),
        })
    })
    .map_err(Rebase::Unparsed)?;

    if !same_folder && let Some(first) = found.relative_loads.first() {
        return Err(Rebase::Refused(format!(
            "`{}` loads `{first}` relative to its own folder, and a clone elsewhere would not have \
             it; clone into `{}`, or make the load `//`-rooted first",
            source_path.display(),
            source_dir.display(),
        )));
    }
    Ok(text)
}

/// `writable_out_root` for `create`, against the declared creation boundary.
///
/// The workspace falls back to the session's: a boundary that names roots but no
/// workspace still has to be measured against something, and a create with no
/// workspace at all is refused either way.
fn creation_out_root(state: &State, path: &Path) -> Result<PathBuf, String> {
    let Some(boundary) = state.creation_boundary.as_ref() else {
        return writable_out_root(state, path);
    };
    let workspace = boundary.workspace.as_deref().or(state.workspace.as_deref());
    writable_out_root_against(&boundary.roots, workspace, path)
}

/// The same rule, against a boundary the caller names.
///
/// **Split out for `create` alone.** Every other writer -- generate,
/// `scaffold_gear` -- is judged by the open session, and ADR-0013's 2026-09-07
/// amendment depends on that: a gear scaffolded for a product is refused inside
/// the product's own source root, and the flow works around it by declaring a new
/// source. Repointing the shared function would move those too. `create` is the
/// one method an ADR says must not be judged by the session, so it is the one
/// method that passes a different boundary.
fn writable_out_root_against(
    roots: &[PathBuf],
    workspace: Option<&Path>,
    path: &Path,
) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cannot resolve working directory: {e}"))?
            .join(path)
    };

    let existing = nearest_existing(&absolute)?;
    let canonical_existing = existing
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize `{}`: {e}", existing.display()))?;

    // **Source roots before the workspace, and this order is the whole point.**
    // A source root may sit *beside* the workspace -- the corpus is a sibling of
    // this repository, and `products/payments-demo` names `../../../gears-rust`
    // -- so generating into one is both "outside the declared workspace" and
    // "inside a source root". Both are refusals; only the second names the rule
    // (ADR-0010 tier 5, do not write next to human-authored crates) instead of
    // describing a boundary nobody meant to cross.
    //
    // The loop below the lexical join used to be the only one, under a comment
    // claiming exactly this precedence -- which the workspace check above it made
    // unreachable for the one layout the comment named. Checked here against the
    // nearest *existing* ancestor, because the joined path does not exist yet at
    // this point and cannot: the join needs the workspace this check precedes.
    if let Some(refusal) = inside_a_source_root(roots, &canonical_existing, path) {
        return Err(refusal);
    }

    let Some(raw_workspace) = workspace else {
        return Err("no workspace was declared, so no output root is writable".to_owned());
    };
    let workspace = raw_workspace.canonicalize().map_err(|e| {
        format!(
            "cannot canonicalize workspace `{}`: {e}",
            raw_workspace.display()
        )
    })?;
    if !canonical_existing.starts_with(&workspace) {
        return Err(format!(
            "`{}` is outside the declared workspace",
            path.display()
        ));
    }

    // Rebuild the full path under the canonical ancestor so `starts_with`
    // compares like-for-like. The suffix is the components that do not exist
    // yet -- the ones generation is about to create. Joining them raw would
    // let `missing/../../../outside` pass `starts_with` and then
    // `create_dir_all` walk out of the workspace; each `..` is applied
    // against the ancestor and refused if it would leave the workspace.
    let suffix = absolute.strip_prefix(&existing).unwrap_or(Path::new(""));
    let resolved = join_lexically(&workspace, canonical_existing, suffix)
        .ok_or_else(|| format!("`{}` is outside the declared workspace", path.display()))?;

    // Again, on the joined path. The check above sees the nearest existing
    // ancestor; this one sees where the `..` components actually land, which is a
    // different question -- `workspace/keep/missing/../../../gears-rust` starts
    // inside the workspace and ends inside a source root.
    if let Some(refusal) = inside_a_source_root(roots, &resolved, path) {
        return Err(refusal);
    }

    if !resolved.starts_with(&workspace) {
        return Err(format!(
            "`{}` is outside the declared workspace",
            path.display()
        ));
    }

    Ok(resolved)
}

/// The tier-5 refusal for `candidate`, or `None` when it is not in a source root.
///
/// One function because `writable_out_root` asks twice, about two different
/// paths: the nearest existing ancestor (before the workspace is known, so that a
/// source root beside the workspace gets the specific refusal) and the joined
/// path (after, so that a `..` chain landing in a root is caught too). Two copies
/// of the sentence would be two chances for them to drift, and the sentence is
/// what a person reads.
///
/// A root that cannot be canonicalized is skipped rather than refused: it is
/// already reported as a `FailedRoot` at `initialize`, and refusing every write
/// because an unrelated root went missing would be a second, worse answer to
/// that.
fn inside_a_source_root(roots: &[PathBuf], candidate: &Path, requested: &Path) -> Option<String> {
    roots.iter().find_map(|root| {
        let src = root.canonicalize().ok()?;
        candidate.starts_with(&src).then(|| {
            format!(
                "`{}` is inside a source root; generation must not write next to \
                 human-authored crates",
                requested.display()
            )
        })
    })
}

/// Join `suffix` onto `ancestor` without letting `..` leave `workspace`.
///
/// `Path::join` keeps `..` as a component, so a later `starts_with` on the
/// unresolved path is not a containment check. `normalize_lexically` is
/// unstable on the toolchain this crate builds with, so the walk is done here.
fn join_lexically(workspace: &Path, ancestor: PathBuf, suffix: &Path) -> Option<PathBuf> {
    let mut resolved = ancestor;
    for component in suffix.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => resolved.push(part),
            Component::ParentDir => {
                if !resolved.pop() || !resolved.starts_with(workspace) {
                    return None;
                }
            }
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    Some(resolved)
}

/// The lock at `path` against the one just resolved.
///
/// `None` only when there is no file. Everything else is an answer worth showing:
/// a file that will not parse is reported as unreadable rather than as an empty
/// diff, because "there is something there and it is not a lock" is neither
/// current nor stale.
///
/// The difference is `LockDiff::summary()`, which is *structured* comparison of
/// two parsed locks rather than a text diff -- so it says "this binding changed"
/// instead of "line 214 differs". Its own doc comment names this widget as the
/// consumer, which is why the client is sent the sentences instead of computing
/// them: a second implementation of "what changed" would be a second answer, and
/// a lock exists to have one.
fn compare_to_disk(path: &Path, resolved: &ResolvedProduct) -> Option<LockOnDisk> {
    let canonical = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(LockOnDisk {
                canonical: String::new(),
                lock_hash: String::new(),
                changes: Vec::new(),
                unreadable: Some(e.to_string()),
            });
        }
    };
    match gearbox_lock::read(&canonical) {
        Ok(on_disk) => {
            let diff = gearbox_lock::diff(&on_disk, resolved);
            Some(LockOnDisk {
                canonical,
                lock_hash: on_disk.product.lock_hash,
                changes: diff.summary(),
                unreadable: None,
            })
        }
        // **A lock that will not parse is not necessarily a lock somebody
        // edited**, and the widget's wording used to assume it was. A rename in
        // the lock's own shape -- `[[processes]]` became `[[applications]]` --
        // makes every previously written lock fail on a missing field, and the
        // operator who ran `gearbox generate` last month is then told their file
        // does not verify. That reads as an accusation and is a wrong one.
        //
        // `LOCK_SCHEMA_VERSION` cannot carry this: it is checked before the
        // parse and a rename that leaves the version alone sails past it. So the
        // sentence carries the alternative instead, and names the way out.
        Err(e) => Some(LockOnDisk {
            canonical,
            lock_hash: String::new(),
            changes: Vec::new(),
            unreadable: Some(format!(
                "{e}. A lock written by an older build can fail this way when the \
                 lock's shape has changed since; `gearbox generate` rewrites it."
            )),
        }),
    }
}

/// Where a product's generated tree lives: `out` if the caller named one, else
/// the engine's default layout under the declared workspace.
///
/// One function because two things need the same answer -- generation writes the
/// tree and `product/lock` compares against the lock inside it -- and a lock
/// compared against a different directory than the one generation writes would be
/// a diff nobody could act on. The layout itself is
/// `gearbox_engine::generate::default_out_root`, so this crate and the CLI cannot
/// disagree about where the tree is; it also validates the product id, which
/// reaches the lock as a plain `String` and would otherwise `join` its way out of
/// `.gearbox/`.
///
/// `Err` when there is nothing to compute an answer from, with the sentence
/// saying which declaration is missing.
fn default_out_root(
    state: &State,
    out: Option<&str>,
    resolved: &Resolved,
) -> Result<PathBuf, String> {
    if let Some(path) = out {
        return Ok(PathBuf::from(path));
    }
    let Some(workspace) = state.workspace.as_ref() else {
        return Err("no workspace was declared, so there is no default output root".to_owned());
    };
    let product = gearbox_ir::ProductId::new(&resolved.product.product.id).map_err(|e| {
        format!(
            "`{}` cannot name a directory under the generated tree: {e}",
            resolved.product.product.id
        )
    })?;
    Ok(workspace.join(gearbox_engine::generate::default_out_root(
        &product,
        &resolved.profile,
    )))
}

/// Where generation writes, and whether it is the person's own folder.
enum OutRoot {
    /// Under the declared workspace: the default `.gearbox/<product>/<profile>/`,
    /// or an `out` a test or the CLI's layout put there.
    Workspace(PathBuf),
    /// A folder the person chose outside the workspace, typically the root of a
    /// repository the product is shipped from (ADR-0010, Amendment 2026-09-29).
    Standalone(PathBuf),
}

/// The output root for `out`, judged by the rule that applies to it.
///
/// Inside the workspace nothing changes. **Outside it, only an explicit `out`
/// is considered, and only a folder that is new or already Gearbox's.** The
/// workspace boundary existed so that a client could not write just anywhere;
/// a folder chosen by name in a dialog is a different request from a default,
/// and the question that still matters is whether it holds anything generation
/// would clobber. An empty folder (dotfiles aside, so a fresh `git init` is
/// empty) or one with a `product.lock` in it is a generation target; any other
/// folder is somebody's, and is refused. Tier 5 holds either way.
fn generation_out_root(state: &State, requested: &Path, explicit: bool) -> Result<OutRoot, String> {
    let refusal = match writable_out_root(state, requested) {
        Ok(root) => return Ok(OutRoot::Workspace(root)),
        Err(refusal) => refusal,
    };
    if !explicit {
        return Err(refusal);
    }
    let roots: Vec<PathBuf> = state.roots.iter().map(|root| root.root.clone()).collect();
    standalone_out_root(&roots, requested).map(OutRoot::Standalone)
}

/// The rule for an output folder outside the workspace.
fn standalone_out_root(roots: &[PathBuf], path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "`{}` is outside the declared workspace; an output folder there must be an \
             absolute path",
            path.display()
        ));
    }
    let existing = nearest_existing(path)?;
    let canonical = existing
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize `{}`: {e}", existing.display()))?;
    // The part that does not exist yet is only ever new names. A `..` in it
    // would be judged here and land somewhere else.
    let suffix = path.strip_prefix(&existing).unwrap_or(Path::new(""));
    if !suffix
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!(
            "`{}` walks out of the folder it names; choose the folder itself",
            path.display()
        ));
    }
    let resolved = canonical.join(suffix);
    if let Some(refusal) = inside_a_source_root(roots, &resolved, path) {
        return Err(refusal);
    }
    if resolved.exists() && !resolved.is_dir() {
        return Err(format!("`{}` is not a folder", path.display()));
    }
    if resolved.is_dir() && !resolved.join("product.lock").is_file() {
        let entries = std::fs::read_dir(&resolved)
            .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        let foreign = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .find(|name| !name.starts_with('.'));
        if let Some(name) = foreign {
            return Err(format!(
                "`{}` already holds `{name}` and no `product.lock`, so Gearbox did not \
                 generate it; choose an empty folder or one Gearbox generated before",
                path.display()
            ));
        }
    }
    Ok(resolved)
}

/// The nearest ancestor of `path` that exists, including `path` itself.
fn nearest_existing(path: &Path) -> Result<PathBuf, String> {
    let mut cursor = path.to_path_buf();
    loop {
        if cursor.exists() {
            return Ok(cursor);
        }
        match cursor.parent() {
            Some(parent) => cursor = parent.to_path_buf(),
            None => {
                return Err(format!("`{}` has no existing ancestor", path.display()));
            }
        }
    }
}

/// Everything generation needs after the two gates (resolution is writable,
/// output root is allowed). Shared by plan, apply and file so they cannot
/// answer about different trees of the same request.
struct PreparedGenerate {
    files: gearbox_ir::FileSet,
    out_root: PathBuf,
    base_root: PathBuf,
    diagnostics: Vec<Diagnostic>,
    /// Keys whose builtin the product replaced.
    ///
    /// The CLI has always printed this and the RPC path dropped it, so a chart
    /// that came out looking wrong had a visible cause in one client and none in
    /// the other. An unexpected Dockerfile should say who wrote it.
    overridden_templates: Vec<String>,
    /// The product's templates as generation will leave them, and what an apply
    /// writes into the product for them.
    templates: Option<gearbox_engine::generate::product_templates::TemplateSync>,
}

fn prepare_generate(
    state: &mut State,
    id: &RequestId,
    path: &str,
    profile: Option<&str>,
    out: Option<&str>,
) -> Result<PreparedGenerate, Response> {
    let resolved = resolve_once(state, id, path, profile, None)?;
    if !resolved.product.is_writable() {
        return Err(error_with_diagnostics(
            id.clone(),
            error_code::GENERATE_REFUSED,
            "resolution reported errors; nothing was generated",
            &resolved.diagnostics,
        ));
    }

    let requested = match default_out_root(state, out, &resolved) {
        Ok(requested) => requested,
        Err(refusal) => {
            return Err(error(id.clone(), error_code::GENERATE_REFUSED, &refusal));
        }
    };
    let out_root = match generation_out_root(state, &requested, out.is_some()) {
        Ok(root) => root,
        Err(refusal) => {
            return Err(error(id.clone(), error_code::GENERATE_REFUSED, &refusal));
        }
    };
    let (out_root, standalone) = match out_root {
        OutRoot::Workspace(root) => (root, false),
        OutRoot::Standalone(root) => (root, true),
    };

    let source_roots: BTreeMap<_, _> = state
        .roots
        .iter()
        .map(|root| (root.id.clone(), root.root.clone()))
        .collect();
    // The product's own copy of every template, followed to this build's
    // builtins. Read here for the plan and the apply alike; only an apply writes.
    let template_sync = match gearbox_engine::TemplateSet::dir_for_product(
        Path::new(path),
        resolved.templates.as_deref(),
    )
    .and_then(|dir| {
        dir.map(|dir| gearbox_engine::generate::product_templates::sync(&dir))
            .transpose()
    }) {
        Ok(sync) => sync,
        Err(e) => {
            return Err(error(
                id.clone(),
                error_code::GENERATE_REFUSED,
                &format!("could not read the product's templates: {e}"),
            ));
        }
    };
    let templates = template_sync
        .as_ref()
        .map_or_else(gearbox_engine::TemplateSet::new, |sync| sync.set.clone());
    // What a standalone output root carries that the workspace tree does not.
    // The workspace's `.gitignore` already covers `.gearbox/`; a repository of
    // its own has none, and without one the first `git add .` commits the build
    // and the merge base. Offered once and then the operator's.
    let standalone_gitignore = match templates.get("standalone/gitignore").and_then(|source| {
        gearbox_engine::generate::render_template(
            "standalone/gitignore",
            source,
            minijinja::context! {},
        )
    }) {
        Ok(body) => body,
        Err(e) => {
            return Err(error(
                id.clone(),
                error_code::GENERATE_REFUSED,
                &format!("could not render the output folder's .gitignore: {e}"),
            ));
        }
    };
    // `crates = registry(...)`. Asking cargo takes seconds, and the Generate view
    // plans on every change it sees, so the answer is kept per product, sources
    // and gear set: none of those moving means cargo would answer the same.
    let registry_key = format!(
        "{path}|{:?}|{:?}|{:?}",
        resolved.registry_sources,
        resolved.product.sources,
        resolved.product.gears.keys().collect::<Vec<_>>()
    );
    let (registry, registry_diagnostics) =
        if let Some(cached) = state.registry_plans.get(&registry_key) {
            cached.clone()
        } else {
            let computed = gearbox_engine::published::for_product(
                &resolved.registry_sources,
                &resolved.product,
                &source_roots,
                &std::env::temp_dir()
                    .join("gearbox-published")
                    .join(&resolved.product.product.id),
            );
            state.registry_plans.insert(registry_key, computed.clone());
            computed
        };
    let generated = match gearbox_engine::generate(&gearbox_engine::generate::GenerateInput {
        lock: &resolved.product,
        source_roots: &source_roots,
        out_root: &out_root,
        templates,
        product_dir: Path::new(path).parent(),
        catalogue: state.catalogue.as_ref(),
        registry: registry.as_ref(),
    }) {
        Ok(generated) => generated,
        Err(e) => {
            return Err(error(
                id.clone(),
                error_code::GENERATE_REFUSED,
                &format!("generation failed: {e}"),
            ));
        }
    };

    // A standalone root keeps its merge base inside itself: its parent is the
    // person's, and `.gearbox/<product>/.base` next to a repository would be a
    // stray folder in their home directory.
    let base_root = if standalone {
        gearbox_engine::generate::standalone_base_root(&out_root)
    } else {
        gearbox_engine::generate::base_root_for(&out_root)
    };
    let mut files = generated.files;
    if standalone && let Ok(path) = RelPath::new(".gitignore") {
        drop(files.insert(gearbox_ir::FileEntry::text(
            path,
            standalone_gitignore,
            gearbox_ir::FileKind::Text,
            gearbox_ir::Ownership::GeneratedOnce,
        )));
    }
    let mut diagnostics = resolved.diagnostics;
    diagnostics.extend(registry_diagnostics);
    // See `Generated::diagnostics`: computing this and dropping it is exactly
    // what made a house template invisible in Studio for a milestone.
    diagnostics.extend(generated.diagnostics.as_slice().iter().cloned());
    if let Some(sync) = &template_sync {
        diagnostics.extend(sync.diagnostics.as_slice().iter().cloned());
    }

    Ok(PreparedGenerate {
        files,
        out_root,
        base_root,
        diagnostics,
        overridden_templates: generated.overridden_templates,
        templates: template_sync,
    })
}

fn dispatch_generate(state: &mut State, request: Request) -> Response {
    let id = request.id.clone();
    if let Some(refusal) = require_ready(state, &id) {
        return refusal;
    }
    match request.method.as_str() {
        method::GENERATE_PLAN => match cast::<GenerateParams>(request) {
            Ok((id, params)) => generate_plan(state, id, &params),
            Err(e) => invalid_params(id, &e),
        },
        method::GENERATE_APPLY => match cast::<GenerateParams>(request) {
            Ok((id, params)) => generate_apply(state, id, &params),
            Err(e) => invalid_params(id, &e),
        },
        method::GENERATE_FILE => match cast::<GenerateFileParams>(request) {
            Ok((id, params)) => generate_file(state, id, &params),
            Err(e) => invalid_params(id, &e),
        },
        other => error(
            id,
            lsp_server::ErrorCode::MethodNotFound as i32,
            &format!("unknown method `{other}`"),
        ),
    }
}

fn generate_plan(state: &mut State, id: RequestId, params: &GenerateParams) -> Response {
    let prepared = match prepare_generate(
        state,
        &id,
        &params.path,
        params.profile.as_deref(),
        params.out.as_deref(),
    ) {
        Ok(prepared) => prepared,
        Err(refusal) => return refusal,
    };

    match gearbox_engine::generate::plan(&prepared.files, &prepared.out_root, &prepared.base_root) {
        Ok((plans, apply_diagnostics)) => {
            let mut diagnostics = prepared.diagnostics;
            diagnostics.extend(apply_diagnostics.as_slice().iter().cloned());
            ok(
                id,
                &GeneratePlanResult {
                    plans,
                    diagnostics,
                    out_root: prepared.out_root.display().to_string(),
                    overridden_templates: prepared.overridden_templates,
                    templates: prepared
                        .templates
                        .map(|sync| sync.templates)
                        .unwrap_or_default(),
                },
            )
        }
        Err(e) => error(
            id,
            error_code::GENERATE_REFUSED,
            &format!("cannot plan generation: {e}"),
        ),
    }
}

fn generate_apply(state: &mut State, id: RequestId, params: &GenerateParams) -> Response {
    if let Some(refusal) = require_writes(state, &id) {
        return refusal;
    }

    let prepared = match prepare_generate(
        state,
        &id,
        &params.path,
        params.profile.as_deref(),
        params.out.as_deref(),
    ) {
        Ok(prepared) => prepared,
        Err(refusal) => return refusal,
    };

    // The templates directory is the product's, so it is judged as a write into
    // the product: inside the declared workspace or a source root.
    if let Some(sync) = &prepared.templates
        && let Some(refusal) = sync
            .templates
            .first()
            .and_then(|template| Path::new(&template.path).parent().map(Path::to_path_buf))
            .and_then(|dir| writable_templates_dir(state, &dir).err())
    {
        return error(id, error_code::GENERATE_REFUSED, &refusal);
    }

    match gearbox_engine::apply_generate(&prepared.files, &prepared.out_root, &prepared.base_root) {
        Ok(outcome) => {
            let mut diagnostics = prepared.diagnostics;
            diagnostics.extend(outcome.diagnostics.as_slice().iter().cloned());
            // Only beside a tree that was written: a refused apply leaves the
            // product exactly as it was, templates included.
            if !outcome.diagnostics.has_errors()
                && let Some(sync) = &prepared.templates
                && let Err(e) = sync.write()
            {
                return error(
                    id,
                    error_code::GENERATE_REFUSED,
                    &format!("generated, but could not write the product's templates: {e}"),
                );
            }
            ok(
                id,
                &GenerateApplyResult {
                    plans: outcome.plans,
                    diagnostics,
                    written: u32::try_from(outcome.written).unwrap_or(u32::MAX),
                    overridden_templates: prepared.overridden_templates,
                    templates: prepared
                        .templates
                        .map(|sync| sync.templates)
                        .unwrap_or_default(),
                },
            )
        }
        Err(e) => error(
            id,
            error_code::GENERATE_REFUSED,
            &format!("cannot apply generation: {e}"),
        ),
    }
}

/// Whether a product's templates directory may be written.
///
/// The directory belongs to the product -- beside its description, or the one
/// `product(templates = ...)` names -- so it gets [`writable_path`]'s rule: the
/// nearest existing ancestor must sit inside the declared workspace or a source
/// root. It may not exist yet; generation creates it.
fn writable_templates_dir(state: &State, dir: &Path) -> Result<(), String> {
    let existing = nearest_existing(dir)?;
    let canonical = existing
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize `{}`: {e}", existing.display()))?;
    let allowed = state
        .roots
        .iter()
        .map(|root| root.root.clone())
        .chain(state.workspace.clone())
        .filter_map(|root| root.canonicalize().ok());
    if allowed.into_iter().any(|root| canonical.starts_with(root)) {
        Ok(())
    } else {
        Err(format!(
            "the product's templates directory `{}` is outside the declared workspace and \
             every source root",
            dir.display()
        ))
    }
}

fn generate_file(state: &mut State, id: RequestId, params: &GenerateFileParams) -> Response {
    let file = match RelPath::new(&params.file) {
        Ok(file) => file,
        Err(e) => {
            return error(
                id,
                error_code::GENERATE_REFUSED,
                &format!("`{}` is not a usable generated path: {e}", params.file),
            );
        }
    };

    let prepared = match prepare_generate(
        state,
        &id,
        &params.path,
        params.profile.as_deref(),
        params.out.as_deref(),
    ) {
        Ok(prepared) => prepared,
        Err(refusal) => return refusal,
    };

    let Some(entry) = prepared.files.get(&file) else {
        return error(
            id,
            error_code::GENERATE_REFUSED,
            &format!("`{}` is not in the generated set", params.file),
        );
    };

    // **This one entry, not the whole tree.** `plan` stages every file it is
    // given: each target is read off disk and its bytes cloned, and an
    // `OperatorOwned` one is three-way merged against the base. The diff view
    // calls this method once per file a person opens, and all it needs is the
    // action and ownership of the file it asked about, so the set it plans is a
    // set of one.
    let mut requested = gearbox_ir::FileSet::new();
    drop(requested.insert(entry.clone()));
    let (plans, _) =
        match gearbox_engine::generate::plan(&requested, &prepared.out_root, &prepared.base_root) {
            Ok(planned) => planned,
            Err(e) => {
                return error(
                    id,
                    error_code::GENERATE_REFUSED,
                    &format!("cannot plan generation: {e}"),
                );
            }
        };
    let Some(plan) = plans.iter().find(|p| p.path == file) else {
        return error(
            id,
            error_code::GENERATE_REFUSED,
            &format!("`{}` is not in the generated set", params.file),
        );
    };

    let on_disk = prepared.out_root.join(file.as_str());
    let current = match std::fs::read(&on_disk) {
        Ok(bytes) => std::str::from_utf8(&bytes).ok().map(str::to_owned),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return error(
                id,
                error_code::GENERATE_REFUSED,
                &format!("cannot read `{}`: {e}", on_disk.display()),
            );
        }
    };

    ok(
        id,
        &GenerateFileResult {
            proposed: entry.as_text().map(str::to_owned),
            current,
            action: plan.action,
            ownership: plan.ownership,
        },
    )
}

/// Whether `value` is a semver triple: three dot-separated runs of digits.
fn is_semver(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    let Some(patch) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && [major, minor, patch]
            .into_iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

fn toml_basic_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Replace a file's contents without ever leaving it half-written.
///
/// Temp file in the same directory, then rename: a rename within one filesystem
/// is atomic, so a crash leaves either the old description or the new one and
/// never a truncated one. The same directory matters -- across filesystems a
/// rename is a copy, and the guarantee is gone.
fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;

    let directory = path.parent().unwrap_or(Path::new("."));
    let temporary = directory.join(temporary_name(path));

    // **`create_new`, not `write`.** The old name was
    // `.<file name>.gearbox-tmp`, fully predictable, and `fs::write` follows a
    // symlink at the path it is given -- so anything able to create that one name
    // in the product directory had this method write the description's new text
    // wherever the link pointed, outside every boundary `writable_path` had just
    // enforced. `create_new` refuses an existing path of any kind, symlink
    // included, and the name carries the process and a counter so two writes in
    // one directory cannot collide on it.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let written = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written {
        drop(std::fs::remove_file(&temporary));
        return Err(e);
    }

    // The destination's own permissions, carried onto the replacement. A fresh
    // temp file gets the process umask, and the rename would hand that to the
    // description -- so editing a `product.gdl` that was deliberately not 0644
    // silently widened it.
    if let Ok(existing) = std::fs::metadata(path)
        && let Err(e) = std::fs::set_permissions(&temporary, existing.permissions())
    {
        drop(std::fs::remove_file(&temporary));
        return Err(e);
    }

    if let Err(e) = std::fs::rename(&temporary, path) {
        drop(std::fs::remove_file(&temporary));
        return Err(e);
    }
    Ok(())
}

/// A temp name in the destination's directory that nothing can predict.
///
/// The directory has to be the destination's own: a rename within one filesystem
/// is atomic, and across two it is a copy with the guarantee gone.
fn temporary_name(path: &Path) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!(
        ".{}.{}-{}-{nanos}.gearbox-tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("description"),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    )
}

/// Everything checkable without resolving.
fn validate(state: &mut State, id: RequestId, params: &ValidateParams) -> Response {
    let mut diagnostics = Vec::new();
    let intent = params.product.as_deref().and_then(|path| {
        let scan = gearbox_engine::product::load_product(&PathBuf::from(path), None);
        diagnostics.extend(scan.diagnostics.as_slice().iter().cloned());
        scan.intent
    });

    let product_path = params.product.as_deref().map(PathBuf::from);
    let report = gearbox_engine::validate::validate_at(
        &state.roots,
        intent.as_ref(),
        product_path.as_deref(),
    );
    diagnostics.extend(report.diagnostics.as_slice().iter().cloned());

    let count = |error: bool| {
        u32::try_from(
            diagnostics
                .iter()
                .filter(|d| d.severity.is_error() == error)
                .count(),
        )
        .unwrap_or(u32::MAX)
    };
    // Counted before the move, so the struct can be built in declaration order.
    let errors = count(true);
    let warnings = u32::try_from(
        diagnostics
            .iter()
            .filter(|d| d.severity == gearbox_ir::Severity::Warning)
            .count(),
    )
    .unwrap_or(u32::MAX);
    ok(
        id,
        &ValidateResult {
            diagnostics,
            errors,
            warnings,
        },
    )
}

/// The catalogue, loaded once and reused, and the diagnostics of a load this
/// call had to do.
///
/// **The diagnostics are returned rather than kept.** A `product/resolve` that
/// arrives before any `gearbox/catalogue/load` triggers the load here, and
/// dropping what it reported answered as though the catalogue had loaded clean --
/// while the same load through `gearbox/catalogue/load` sent every one of those
/// diagnostics to the client. They are the load's, not the resolution's, so the
/// caller folds them into its own answer; an empty `Vec` means the cache
/// answered and nothing was read.
fn catalogue_for(state: &mut State) -> (&gearbox_ir::Catalogue, Vec<Diagnostic>) {
    let mut loaded = Vec::new();
    let catalogue = state.catalogue.get_or_insert_with(|| {
        let scan = gearbox_engine::load_catalogue(&state.roots);
        loaded = scan.catalogue.diagnostics.as_slice().to_vec();
        scan.catalogue
    });
    (catalogue, loaded)
}

/// Run a staged load, answering at the boundary and streaming the rest.
///
/// The response goes out when every description has been evaluated and no crate
/// has been parsed, which is exactly what a tree needs to render its shape. The
/// projections that follow arrive as notifications.
///
/// This runs on the request thread, so the connection's sender is used directly
/// for notifications. A long load therefore blocks further requests, which is
/// correct for now: cancellation is the answer to a slow load, and it needs the
/// request loop to be reading anyway. Moving the load to a worker is a change to
/// make when there is a second concurrent request worth serving.
fn catalogue_load(connection: &Connection, state: &mut State, id: RequestId) -> Option<Response> {
    /// How often a load in progress reports itself. Short enough to look live,
    /// long enough that a registry's worth of gears is tens of messages.
    const PROGRESS_STEP: std::time::Duration = std::time::Duration::from_millis(100);

    let mut last_progress = std::time::Instant::now();
    let mut pending = Vec::new();
    let mut designs = Vec::new();
    let mut total = 0_u32;
    let mut completed = 0_u32;
    // Whether a progress line has gone out yet. Not `completed == 1`: design
    // gears are counted during the first pass, so the first projection need
    // not be the first completion.
    let mut progressed = false;
    let mut answered = false;
    // How many diagnostics went out with the response, so the follow-up sends
    // the rest and not all of them again.
    let mut already_sent = 0_usize;
    let mut disconnected = false;

    let scan = load_catalogue_staged(&state.roots, &mut |event| {
        match event {
            LoadEvent::Discovered { total: n } => {
                total = u32::try_from(n).unwrap_or(u32::MAX);
            }
            LoadEvent::Declared(entry) => pending.push(entry.clone()),
            // Counted as done: `total` counts every description, and a design
            // one is complete when declared. Leaving it out held the bar short
            // of its denominator for the whole load.
            LoadEvent::Design(design) => {
                completed += 1;
                designs.push(design.clone());
            }
            LoadEvent::DeclarationComplete { diagnostics, .. } => {
                // The tree has its whole shape and none of its badges: answer.
                // The declaration diagnostics go with it -- an evaluation
                // failure is exactly what a client rendering the tree needs to
                // show, and there is no later response to carry it.
                already_sent = diagnostics.len();
                let result = CatalogueLoadResult {
                    total,
                    pending: std::mem::take(&mut pending),
                    designs: std::mem::take(&mut designs),
                    diagnostics: diagnostics.to_vec(),
                };
                // `answered` only when the send succeeded. Setting it
                // unconditionally left a disconnected client unanswered *and*
                // suppressed the fallback response below.
                match connection
                    .sender
                    .send(Message::Response(ok(id.clone(), &result)))
                {
                    Ok(()) => answered = true,
                    Err(e) => {
                        eprintln!("gearbox: cannot answer catalogue/load: {e}");
                        disconnected = true;
                    }
                }
            }
            // A plugin already sent, now joined to its host. The same
            // replacement a projection is, and not counted twice.
            LoadEvent::Joined(gear) => {
                disconnected |= !notify(
                    connection,
                    method::CATALOGUE_CHANGED,
                    &CatalogueChanged {
                        gear: gear.clone(),
                        replaces: gear.gdl_path.as_str().to_owned(),
                    },
                )
                .peer_alive();
            }
            LoadEvent::Projected(gear) => {
                completed += 1;
                disconnected |= !notify(
                    connection,
                    method::CATALOGUE_CHANGED,
                    &CatalogueChanged {
                        gear: gear.clone(),
                        replaces: gear.gdl_path.as_str().to_owned(),
                    },
                )
                .peer_alive();
                // **On a time step, not once per gear.** The stdio channel is
                // `bounded(0)`, so every send parks the load until the writer
                // thread has pushed it to stdout -- and a progress line per
                // projected gear doubled the message count during the most
                // expensive thing this server does, to tell a UI something it
                // cannot render that often anyway. The first one goes out
                // immediately so a bar appears at once, and `done: true` below is
                // unconditional, which is the message a client waits on.
                if !progressed || last_progress.elapsed() >= PROGRESS_STEP {
                    progressed = true;
                    last_progress = std::time::Instant::now();
                    disconnected |= !notify(
                        connection,
                        method::PROGRESS,
                        &ProgressParams {
                            token: "catalogue".to_owned(),
                            completed,
                            total,
                            done: false,
                        },
                    )
                    .peer_alive();
                }
            }
        }
        // A client that is gone will not read the rest of the load, and the
        // second pass is the expensive one.
        if disconnected {
            Continue::Stop
        } else {
            Continue::Yes
        }
    });

    // Everything the second pass produced. The response has already gone out, so
    // without this the projection and merge failures reach nobody.
    let remaining: Vec<gearbox_ir::Diagnostic> = scan
        .catalogue
        .diagnostics
        .iter()
        .skip(already_sent)
        .cloned()
        .collect();
    if answered && !remaining.is_empty() {
        notify_last(
            connection,
            method::CATALOGUE_DIAGNOSTICS,
            &CatalogueDiagnostics {
                diagnostics: remaining,
            },
        );
    }

    notify_last(
        connection,
        method::PROGRESS,
        &ProgressParams {
            token: "catalogue".to_owned(),
            completed,
            total,
            done: true,
        },
    );
    notify_last(
        connection,
        method::LOG,
        &LogParams {
            message: format!(
                "catalogue: {} gear(s), {} crate(s) parsed for {} request(s)",
                scan.catalogue.gears.len(),
                scan.crates_scanned,
                scan.scan_requests
            ),
        },
    );

    let diagnostics = scan.catalogue.diagnostics.as_slice().to_vec();
    // The whole point of the cache, and it was never being filled.
    //
    // A staged load parses every gear crate in the tree -- it is the most
    // expensive thing the server does. Dropping the result on the floor meant
    // the first `product/resolve` after a load called `catalogue_for`, found
    // `None`, and did the entire scan again *non-staged*: the same seconds of
    // work, this time with no progress notifications and with the request thread
    // blocked, for a catalogue the client already had on screen.
    //
    // **Only a load that ran to the end.** A stopped one is missing most of its
    // gears, and caching it would have every later `product/resolve` answer out
    // of a tree with holes in it -- `GBX0301`, "unknown gear", for gears that
    // are on disk and were simply never reached.
    //
    // The stop today means the client is gone, and a gone client ends the
    // request loop, so nothing would ever read this. The guard is for the
    // sentence twenty lines above this one: cancellation is the answer to a slow
    // load. When that exists, stopping early stops being fatal and starts being
    // routine, and this becomes the line that poisons the session.
    if disconnected {
        state.catalogue = None;
        state.registry_plans.clear();
    } else {
        state.catalogue = Some(scan.catalogue);
    }

    if answered {
        return None;
    }
    // No description evaluated (or the boundary send failed), so nothing has
    // answered this request. Answer with an empty tree rather than leaving the
    // client waiting -- if it is still there, the send loop reports the failure.
    Some(ok(
        id,
        &CatalogueLoadResult {
            total,
            pending: Vec::new(),
            designs: Vec::new(),
            diagnostics,
        },
    ))
}

/// What became of a notification.
///
/// Three states rather than a `bool`, because two of them used to answer
/// `true`: a notification whose params could not be serialized was logged to
/// stderr and reported to the caller as delivered. For `publishDiagnostics` that
/// meant the client kept the markers it already had, with the only trace on a
/// channel it cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    Sent,
    /// Nothing went out, and the peer is still there to be told so.
    Dropped,
    /// The channel is closed: nothing will go out, now or later.
    PeerGone,
}

impl Delivery {
    /// Whether there is still somebody to talk to, which is what the request
    /// loop turns on.
    const fn peer_alive(self) -> bool {
        !matches!(self, Self::PeerGone)
    }
}

/// Send one notification.
///
/// A serialization failure drops the notification rather than encoding it as
/// `Null`: a notification whose params are `null` is one the client cannot tell
/// from a well-formed empty one, so it would read as "nothing happened".
fn notify<T: serde::Serialize>(connection: &Connection, method: &str, params: &T) -> Delivery {
    let params = match serde_json::to_value(params) {
        Ok(params) => params,
        Err(e) => {
            eprintln!("gearbox: cannot serialize `{method}` params: {e}");
            return Delivery::Dropped;
        }
    };
    match connection.sender.send(Message::Notification(Notification {
        method: method.to_owned(),
        params,
    })) {
        Ok(()) => Delivery::Sent,
        Err(e) => {
            eprintln!("gearbox: cannot send `{method}`: {e}");
            Delivery::PeerGone
        }
    }
}

/// Send a notification after the work it is about is over.
///
/// The one place a [`Delivery`] is genuinely unusable: these go out once the
/// load has finished, so there is no request left to refuse, nothing left to
/// re-send, and a peer that has gone will be noticed by the request loop on its
/// next read. `notify` has already said so on stderr.
fn notify_last<T: serde::Serialize>(connection: &Connection, method: &str, params: &T) {
    let _ = notify(connection, method, params);
}

fn cast<P: serde::de::DeserializeOwned>(
    request: Request,
) -> Result<(RequestId, P), ExtractError<Request>> {
    let method = request.method.clone();
    request.extract::<P>(&method)
}

fn ok<T: serde::Serialize>(id: RequestId, value: &T) -> Response {
    match serde_json::to_value(value) {
        Ok(result) => Response {
            id,
            response_result: Ok(result),
        },
        // **`InternalError`, not `LOAD_FAILED`.** A result that cannot be
        // serialized is a defect in this server, on whatever method was called;
        // answering with the catalogue-load code told a client mapping codes to
        // causes that its load had failed. `protocol.rs` makes this exact
        // argument about not reusing `-32002`.
        Err(e) => error(
            id,
            lsp_server::ErrorCode::InternalError as i32,
            &format!("cannot serialize result: {e}"),
        ),
    }
}

fn error(id: RequestId, code: i32, message: &str) -> Response {
    Response {
        id,
        response_result: Err(lsp_server::ResponseError {
            code,
            message: message.to_owned(),
            data: None,
        }),
    }
}

/// An error that carries the diagnostics explaining it.
///
/// A refusal whose message is "could not be evaluated" tells a client that
/// something is wrong and nothing about what. The diagnostics are the answer,
/// and they exist -- they were just being dropped on the floor with the failed
/// result. Serialization failure falls back to the plain error rather than
/// losing the refusal itself.
fn error_with_diagnostics(
    id: RequestId,
    code: i32,
    message: &str,
    diagnostics: &[gearbox_ir::Diagnostic],
) -> Response {
    let data = serde_json::json!({ "diagnostics": diagnostics });
    Response {
        id,
        response_result: Err(lsp_server::ResponseError {
            code,
            message: message.to_owned(),
            data: Some(data),
        }),
    }
}

fn invalid_params(id: RequestId, e: &ExtractError<Request>) -> Response {
    error(
        id,
        lsp_server::ErrorCode::InvalidParams as i32,
        &e.to_string(),
    )
}
