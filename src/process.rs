//! One language server process, and the JSON-RPC that goes over its stdio.
//!
//! Plain [`std::process::Command`] with a thread reading its stdout. Every call here is
//! synchronous and not necessarily made from inside an async runtime, so an async LSP
//! framework would have to be bridged at every call anyway - and the whole of what it would
//! buy is the framing in [`crate::framing`].

use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::{framing::Frames, languages::ServerSpec, protocol::ProgressNote};

/// How long a request waits for its answer. A server that has stopped answering must not
/// take the thread that asked with it: the window's worker threads are few, and a stuck one
/// is a pane that never draws again.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Starting up is allowed longer: a server reads the project's manifest before it replies,
/// and on a cold cache that is not instant.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long `shutdown` is given before the process is killed anyway. Being polite is worth
/// a moment; waiting on it is not.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(300);
/// How long a server has to stay quiet about starting up before it is taken to have
/// finished - see [`Readiness`].
pub(crate) const SETTLING: Duration = Duration::from_secs(2);
/// How long a server is allowed to be starting before readiness stops saying so, however
/// little it has finished.
///
/// Being wrong about readiness has to degrade into "ask anyway and see", never into a pane
/// that says "try again in a moment" for the rest of the session. Five minutes is longer
/// than a cold rust-analyzer takes on a large workspace - measured at just under three on
/// this one - so a server that has passed it is a server whose progress this client has
/// misread, and the honest thing left is to let the question through.
pub(crate) const STARTING_CEILING: Duration = Duration::from_secs(300);
/// How long a request that came back `content modified` waits before it is asked again -
/// see [`CONTENT_MODIFIED`].
const RETRY_PAUSE: Duration = Duration::from_millis(250);
/// What a server answers when the document moved under a request: JSON-RPC's
/// `ContentModified`.
///
/// The protocol says this one is transient by definition - the question was about a document
/// that has since changed, so the same question asked again is a different question and may
/// well have an answer. It is retried once rather than shown, which is what makes readiness
/// a hint rather than a gate.
pub(crate) const CONTENT_MODIFIED: i64 = -32801;
const READ_CHUNK: usize = 16 * 1024;

/// Progress work whose token or title holds one of these is the server keeping a project it
/// has already loaded up to date, not the server starting up.
///
/// A table rather than a chain of tests, and matched on rather than named exactly, because
/// the tokens carry an index or a run number: rust-analyzer's checks arrive under
/// `rust-analyzer/flycheck/0`, `.../1` and so on, under the title `cargo check`. The title is
/// matched as well as the token, since the protocol lets a token be a bare number that says
/// nothing about what it is.
///
/// Only what has actually been seen on the wire is listed - the two names above are one
/// server's, and typescript-language-server announces no work at all. Anything unlisted
/// counts as starting up, which is the safe way round: unknown work delays readiness rather
/// than declaring it early, and [`STARTING_CEILING`] is what stops that delay being forever.
const BACKGROUND_WORK: &[&str] = &["flycheck", "cargo check"];

/// Which units the server counts a column in - see [`crate::protocol::lsp_character`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PositionEncoding {
    /// Bytes, which is what the editor already has: nothing to convert.
    Utf8,
    /// UTF-16 code units, the protocol's default and the only one a server must support.
    Utf16,
}

/// Whether the server has finished starting, told from its progress notifications.
///
/// This is the honest answer and the initialize reply is not: rust-analyzer answers
/// `initialize` in milliseconds and then reads, loads and indexes the project for minutes,
/// and a request in between comes back empty or refused - which reads in a pane as "no
/// definition found" rather than as "not yet".
///
/// The rule is in three parts, and each part is there for a failure that was actually seen:
///
/// - **Starting up is not the same as working.** Only work this client takes to be part of
///   starting up counts, by its token - see [`BACKGROUND_WORK`]. rust-analyzer runs
///   `cargo check` over a workspace for as long as the workspace is open, re-running it on
///   every edit, and counting that as starting up is how a large project stays "still
///   indexing" for the whole session.
/// - **Quiet, not just an empty count.** A server announces the pieces of starting up one
///   after another - fetch, scan the roots, load the proc-macros, prime the cache - and the
///   outstanding work passes through empty in the gaps between them. So readiness also wants
///   [`SETTLING`] of quiet, or a request lands in one of those gaps and comes back
///   [`CONTENT_MODIFIED`].
/// - **Once finished, finished.** Readiness latches. A server that has started does not go
///   back to starting because it picked up some background work, and nothing a server says
///   later can take a pane that was answering questions and stop it.
///
/// And over all three, [`STARTING_CEILING`]: past it the server is called ready whatever its
/// progress said, because a misread of readiness must cost a request that comes back empty,
/// not a session that never asks.
pub(crate) struct Readiness {
    /// When `initialized` went out, which is when starting up begins as far as this client
    /// can see. What [`STARTING_CEILING`] is measured from.
    initialized: Instant,
    /// When the server last said anything about *starting up*. The clock the quiet is
    /// measured on, and untouched by background work so that a project being checked in the
    /// background can still fall quiet.
    last_spoke: Instant,
    /// The starting-up work the server began and has not ended, by its progress token.
    starting_work: HashSet<String>,
    /// The background work the server began and has not ended, by its progress token. Kept
    /// because only a `begin` carries a title, so the reports and the end of a piece of work
    /// can only be placed by remembering what its token was taken to be.
    background_work: HashSet<String>,
    /// Whether the server has finished starting. The latch: written once, and true from
    /// then on.
    finished_starting: bool,
}

impl Readiness {
    /// A server that has just been told `initialized` and has announced nothing yet.
    pub(crate) fn new() -> Self {
        Self {
            initialized: Instant::now(),
            last_spoke: Instant::now(),
            starting_work: HashSet::new(),
            background_work: HashSet::new(),
            finished_starting: false,
        }
    }

    /// Fold one `$/progress` notification into what is known about starting up.
    ///
    /// A notification with no token names no piece of work, and there is nothing honest to
    /// do with it: it cannot be paired with the `begin` that would say what it is, so it is
    /// left out of the reckoning rather than made into work with no identity.
    pub(crate) fn follow(&mut self, message: &Value) {
        let Some(token) = crate::protocol::progress_token(message) else {
            return;
        };
        let kind = crate::protocol::progress_kind(message);
        let title = crate::protocol::progress_note(message).title;

        match kind {
            Some("begin") if is_background_work(&token, title.as_deref()) => {
                self.background_work.insert(token);
            }
            Some("begin") => {
                self.starting_work.insert(token);
                self.last_spoke = Instant::now();
            }
            Some("report") if !self.background_work.contains(&token) => {
                // Part way through starting up is not finished starting up, so a report
                // restarts the quiet just as a begin does.
                self.last_spoke = Instant::now();
            }
            Some("end") => {
                if self.background_work.remove(&token) {
                    return;
                }
                self.starting_work.remove(&token);
                self.last_spoke = Instant::now();
            }
            _ => {}
        }
    }

    /// Whether the server has finished starting, latching the answer the first time it has.
    pub(crate) fn is_ready(&mut self) -> bool {
        if self.finished_starting {
            return true;
        }
        let settled = self.starting_work.is_empty() && self.last_spoke.elapsed() >= SETTLING;
        self.finished_starting = settled || self.initialized.elapsed() >= STARTING_CEILING;
        self.finished_starting
    }

    /// Move this server's clocks back, so a test can reach [`SETTLING`] or
    /// [`STARTING_CEILING`] without spending it.
    #[cfg(test)]
    pub(crate) fn rewind(&mut self, by: Duration) {
        let back = |instant: Instant| instant.checked_sub(by).unwrap_or(instant);
        self.initialized = back(self.initialized);
        self.last_spoke = back(self.last_spoke);
    }
}

/// Whether a piece of announced work is the server keeping a loaded project up to date
/// rather than starting up - see [`BACKGROUND_WORK`].
fn is_background_work(token: &str, title: Option<&str>) -> bool {
    let token = token.to_lowercase();
    let title = title.unwrap_or_default().to_lowercase();
    BACKGROUND_WORK
        .iter()
        .any(|marker| token.contains(marker) || title.contains(marker))
}

/// What one server is doing right now, as it last said in a `$/progress` notification.
///
/// One per server rather than a list: a server that begins a second piece of work before it
/// has ended the first has moved on to it, and a bar that named both would be a bar nobody
/// reads. The title is carried forward from the `begin`, because a `report` does not repeat
/// it - see [`crate::protocol::ProgressNote`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Working {
    /// What the server called this piece of work: "Indexing", "Fetching metadata".
    pub title: String,
    /// The line it is writing under that title, where it writes one.
    pub detail: Option<String>,
    /// How far through, where the server says. Most work does not.
    pub percentage: Option<u8>,
}

/// Fold one `$/progress` notification into what the server is doing.
///
/// A `begin` is a new piece of work, a `report` moves the one already there along, and an
/// `end` leaves the server doing nothing. A `report` with no work behind it is a server
/// whose `begin` this client never saw, and it is left alone rather than made into work
/// with no name.
pub fn follow_progress(working: &mut Option<Working>, kind: Option<&str>, note: ProgressNote) {
    match kind {
        Some("begin") => {
            *working = Some(Working {
                // The protocol makes the title of a `begin` mandatory; a server that leaves
                // it out is still doing something, and "working" is what the bar says then.
                title: note.title.unwrap_or_else(|| "working".to_string()),
                detail: note.message,
                percentage: note.percentage,
            });
        }
        Some("report") => {
            if let Some(working) = working.as_mut() {
                // Only what the report actually carried is taken: a report that says nothing
                // but a percentage leaves the line under the title as it was.
                if note.message.is_some() {
                    working.detail = note.message;
                }
                if note.percentage.is_some() {
                    working.percentage = note.percentage;
                }
            }
        }
        Some("end") => *working = None,
        _ => {}
    }
}

/// Why a server would not answer: what to tell the caller, and the JSON-RPC code where
/// there was one.
///
/// The code is kept apart from the sentence because one of them is worth acting on:
/// [`CONTENT_MODIFIED`] is a request the document moved under, and asking again is the
/// answer. A timeout or a server that exited has no code, and nothing to retry.
pub(crate) struct Refusal {
    /// What the caller is told, already written as the sentence it will read as.
    said: String,
    /// The JSON-RPC error code, where the refusal came from the server rather than from
    /// this side of the pipe.
    code: Option<i64>,
}

impl Refusal {
    /// Whether asking the same thing again is worth it.
    ///
    /// Only [`CONTENT_MODIFIED`] is: the protocol defines it as the document having changed
    /// under the request, so the question was never really answered. Every other refusal is
    /// the server's considered answer - a method it does not have, a position it will not
    /// read - and asking twice would only be slower.
    pub(crate) fn worth_asking_again(&self) -> bool {
        self.code == Some(CONTENT_MODIFIED)
    }
}

/// What the error object of an answer means, kept whole - see [`Refusal`].
pub(crate) fn refusal_from(error: &Value) -> Refusal {
    Refusal {
        said: error.to_string(),
        code: error.get("code").and_then(Value::as_i64),
    }
}

/// The requests waiting for their answers, by the id they went out with.
type Pending = Arc<Mutex<HashMap<i64, mpsc::Sender<Result<Value, Refusal>>>>>;

/// One running language server, and everything asked of it.
///
/// Held behind an `Arc` and spoken to from several threads: the reader thread carries its
/// answers back, and any number of callers may have a request outstanding at once.
pub struct LanguageServer {
    /// The name the language table knows this server by - see [`ServerSpec::name`].
    pub name: &'static str,
    /// What the server agreed to count columns in, out of its `initialize` reply. Written
    /// once, by [`LanguageServer::start`], before anybody else has the server at all.
    encoding: OnceLock<PositionEncoding>,
    stdin: Arc<Mutex<ChildStdin>>,
    child: Mutex<Child>,
    next_id: AtomicI64,
    pending: Pending,
    readiness: Arc<Mutex<Readiness>>,
    /// What the server last said it is doing. Kept beside readiness rather than folded into
    /// it: readiness is a rule about quiet that the window must not second-guess, and this
    /// is the server's own account of the wait, which is what the status bar reads out.
    working: Arc<Mutex<Option<Working>>>,
    /// The text of every document open in this server, by its path relative to the repo.
    ///
    /// Kept because a position has to be turned into the server's units against the line it
    /// falls on, and because full-text sync means the server is told the whole text anyway.
    documents: Mutex<HashMap<String, String>>,
}

impl LanguageServer {
    /// Start a server on a repo and walk it through `initialize` / `initialized`. Blocks
    /// until it has replied, which is the only point at which what it agreed to is known.
    ///
    /// `search_path` is a `PATH` the command is looked for on and started with - see
    /// [`crate::languages::installed_at`]. It is the caller's, because where a user installs
    /// their servers is the host application's business. `client` is who the server is told
    /// it is talking to, which is the caller's for the same reason - see
    /// [`ClientIdentity`](crate::protocol::ClientIdentity).
    pub fn start(
        spec: &'static ServerSpec,
        repo_root: &std::path::Path,
        search_path: &str,
        client: &crate::protocol::ClientIdentity,
    ) -> Result<Self> {
        let command_path = crate::languages::installed_at(spec.command, search_path)
            .ok_or_else(|| anyhow!("{} is not installed", spec.command))?;
        let mut child = Command::new(command_path)
            .args(spec.args)
            .current_dir(repo_root)
            .env("PATH", search_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to start {}", spec.command))?;

        let stdin = Arc::new(Mutex::new(
            child.stdin.take().context("the server has no stdin")?,
        ));
        let stdout = child.stdout.take().context("the server has no stdout")?;
        let stderr = child.stderr.take().context("the server has no stderr")?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let readiness = Arc::new(Mutex::new(Readiness::new()));
        let working: Arc<Mutex<Option<Working>>> = Arc::new(Mutex::new(None));

        read_messages(
            stdout,
            Arc::clone(&pending),
            Arc::clone(&readiness),
            Arc::clone(&working),
            Arc::clone(&stdin),
        );
        // Nothing reads a server's diagnostics prose, but a full stderr pipe would stop the
        // server dead, so it is drained and dropped.
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let mut stderr = stderr;
            let _ = stderr.read_to_end(&mut sink);
        });

        let server = Self {
            name: spec.name,
            encoding: OnceLock::new(),
            stdin,
            child: Mutex::new(child),
            next_id: AtomicI64::new(1),
            pending,
            readiness,
            working,
            documents: Mutex::new(HashMap::new()),
        };

        let reply = server.request_with_timeout(
            "initialize",
            crate::protocol::initialize_params(repo_root, client),
            INITIALIZE_TIMEOUT,
        )?;
        let encoding = crate::protocol::agreed_encoding(&reply)?;
        let _ = server.encoding.set(encoding);
        server.notify("initialized", json!({}))?;
        // Readiness is counted from here rather than from the spawn: what came before is
        // the server reading the project, and what comes after is what it announces.
        *server.readiness.lock().unwrap() = Readiness::new();
        *server.working.lock().unwrap() = None;

        Ok(server)
    }

    /// What the server agreed to count columns in, out of its `initialize` reply.
    pub fn encoding(&self) -> PositionEncoding {
        *self
            .encoding
            .get()
            .expect("a started server has agreed an encoding")
    }

    /// Whether the server has finished starting - see [`Readiness`] for what that means and
    /// why background work does not unmake it.
    pub fn is_ready(&self) -> bool {
        self.readiness.lock().unwrap().is_ready()
    }

    /// What the server is doing right now, if it has said. `None` for one that is doing
    /// nothing it has announced, which is every server that has finished starting.
    pub fn working(&self) -> Option<Working> {
        self.working.lock().unwrap().clone()
    }

    /// The text of one open document, as this side last told the server it stands. `None`
    /// for a document that is not open here, which is what makes a request on a file nobody
    /// opened an error rather than a question the server cannot answer.
    pub fn document_text(&self, file_path: &str) -> Option<String> {
        self.documents.lock().unwrap().get(file_path).cloned()
    }

    /// Keep the text this side last told the server a document holds.
    pub fn remember_document(&self, file_path: &str, text: &str) {
        self.documents
            .lock()
            .unwrap()
            .insert(file_path.to_string(), text.to_string());
    }

    /// Drop a document this side has told the server is closed.
    pub fn forget_document(&self, file_path: &str) {
        self.documents.lock().unwrap().remove(file_path);
    }

    /// Whether this file has already been announced to the server, which is the difference
    /// between `didOpen` and `didChange`.
    pub fn has_document(&self, file_path: &str) -> bool {
        self.documents.lock().unwrap().contains_key(file_path)
    }

    /// Ask the server something and wait for its answer. A server that has stopped
    /// answering gives an error rather than the calling thread.
    ///
    /// One answer is not taken at face value: [`CONTENT_MODIFIED`] means the document moved
    /// while the question was in flight, so it is asked once more after [`RETRY_PAUSE`]
    /// rather than handed back as "no answer". That is what makes readiness a hint - a
    /// question let through a moment too early costs a retry, not a wrong answer in a pane.
    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        match self.ask(method, params.clone(), REQUEST_TIMEOUT) {
            Err(refusal) if refusal.worth_asking_again() => {
                std::thread::sleep(RETRY_PAUSE);
                self.ask(method, params, REQUEST_TIMEOUT)
                    .map_err(|refusal| anyhow!("{}", refusal.said))
            }
            answer => answer.map_err(|refusal| anyhow!("{}", refusal.said)),
        }
    }

    fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        self.ask(method, params, timeout)
            .map_err(|refusal| anyhow!("{}", refusal.said))
    }

    /// One round trip, with the server's refusal kept whole so the caller can tell a
    /// transient one from a real one.
    fn ask(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> std::result::Result<Value, Refusal> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, sender);

        let sent = write_message(
            &self.stdin,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );
        if let Err(error) = sent {
            self.pending.lock().unwrap().remove(&id);
            return Err(Refusal {
                said: error.to_string(),
                code: None,
            });
        }

        match receiver.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(refusal)) => Err(Refusal {
                said: format!("{} refused {method}: {}", self.name, refusal.said),
                code: refusal.code,
            }),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(Refusal {
                    said: format!(
                        "{} did not answer {method} within {} seconds",
                        self.name,
                        timeout.as_secs()
                    ),
                    code: None,
                })
            }
        }
    }

    /// Tell the server something. Notifications have no answer to wait for.
    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        write_message(
            &self.stdin,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
    }
}

impl Drop for LanguageServer {
    /// Ask the server to stop, and take it down if it will not. The grace is short on
    /// purpose: this runs on whoever dropped the registry, and a window closing must not
    /// wait on a server that has stopped listening.
    fn drop(&mut self) {
        let _ = self.request_with_timeout("shutdown", json!(null), SHUTDOWN_GRACE);
        let _ = self.notify("exit", json!(null));
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn write_message(stdin: &Mutex<ChildStdin>, message: &Value) -> Result<()> {
    let mut stdin = stdin
        .lock()
        .map_err(|_| anyhow!("the server's stdin lock is poisoned"))?;
    stdin
        .write_all(&crate::framing::frame(&message.to_string()))
        .context("failed to write to the language server")?;
    stdin
        .flush()
        .context("failed to flush to the language server")?;
    Ok(())
}

/// Read the server's stdout for as long as it has one: answers go to whoever is waiting for
/// them, progress moves readiness along, and the server's own requests are answered so it
/// does not sit waiting on us.
fn read_messages(
    stdout: std::process::ChildStdout,
    pending: Pending,
    readiness: Arc<Mutex<Readiness>>,
    working: Arc<Mutex<Option<Working>>>,
    stdin: Arc<Mutex<ChildStdin>>,
) {
    std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut frames = Frames::default();
        let mut buffer = vec![0u8; READ_CHUNK];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => frames.push(&buffer[..count]),
            }
            while let Some(message) = frames.next_message() {
                let Ok(message) = serde_json::from_str::<Value>(&message) else {
                    continue;
                };
                handle_message(&message, &pending, &readiness, &working, &stdin);
            }
        }

        // The server is gone. Everyone waiting on it is told now rather than sitting out
        // their whole timeout.
        for (_, waiting) in pending.lock().unwrap().drain() {
            let _ = waiting.send(Err(Refusal {
                said: "the language server exited".to_string(),
                code: None,
            }));
        }
    });
}

fn handle_message(
    message: &Value,
    pending: &Pending,
    readiness: &Mutex<Readiness>,
    working: &Mutex<Option<Working>>,
    stdin: &Mutex<ChildStdin>,
) {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id");

    match (method, id) {
        // The server asking us something. Every one of these is answered, because a server
        // waiting on a reply that never comes stops making progress.
        (Some(method), Some(id)) => {
            let result = crate::protocol::reply_to_server_request(method, message);
            let _ = write_message(
                stdin,
                &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            );
        }
        (Some("$/progress"), None) => {
            readiness.lock().unwrap().follow(message);
            // What the status bar reads out takes every piece of work, background or not: a
            // person watching a `cargo check` run wants to see it, even though it says
            // nothing about whether the server has finished starting.
            follow_progress(
                &mut working.lock().unwrap(),
                crate::protocol::progress_kind(message),
                crate::protocol::progress_note(message),
            );
        }
        // Any other notification - diagnostics, log messages - is not this client's business.
        (Some(_), None) => {}
        // An answer to something we asked.
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(waiting) = pending.lock().unwrap().remove(&id) else {
                return;
            };
            let answer = match message.get("error") {
                Some(error) => Err(refusal_from(error)),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = waiting.send(answer);
        }
        (None, None) => {}
    }
}
