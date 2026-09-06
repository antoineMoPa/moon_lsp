//! The servers this machine is running, and the questions asked of them.
//!
//! A server is started per workspace and per language, and lives as long as the caller
//! keeps the registry. Everything a caller needs is here: whether a file has a server
//! behind it, what those servers are doing, the document notifications, and the two
//! questions - where a name is defined, and what could be typed next.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow, bail};

use crate::{
    languages::{self, ExtensionSpec, ServerSpec},
    payload::{LspCompletion, LspLocation, LspPosition, LspStatus, LspWork},
    process::{LanguageServer, PositionEncoding},
    protocol::{self, ClientIdentity},
};

/// The repo a question is about: where its files are, and what its servers are held under.
///
/// The two are separate because the caller decides how much its servers are shared. A key
/// of the repo path is one set of servers per repo; a key of whatever the caller calls one
/// window's work is a set each, so closing that window takes its servers with it rather
/// than leaving another one's indexing half done. This crate never looks inside the key.
pub struct Workspace<'a> {
    /// What the servers for these files are held under.
    pub key: &'a str,
    /// The repo itself, which is what a server is started in and what its answers are read
    /// against.
    pub root: &'a Path,
}

/// One server, per workspace key and per server in the language table.
type ServerKey = (String, &'static str);

/// The language servers running for one host application.
///
/// Built once and held for the life of the process - dropping it shuts every server down.
pub struct LspRegistry {
    /// The `PATH` servers are looked for and started on. The caller's, because where a user
    /// installs their servers is the host application's business - see
    /// [`languages::installed_at`].
    search_path: String,
    /// Who every server started here is told it is talking to. The caller's for the same
    /// reason the search path is: a library has no host application's name to give, so
    /// unless one is said with [`LspRegistry::identifying_as`] this is the crate itself -
    /// see [`ClientIdentity`].
    client: ClientIdentity,
    servers: Mutex<HashMap<ServerKey, Arc<LanguageServer>>>,
    /// The servers that are on PATH and would not start, which is not the same as not being
    /// installed at all: a `rust-analyzer` on PATH that is really a rustup shim for a
    /// component nobody added exits the moment it is spoken to, which is how this was found.
    /// The first attempt says why; after it, the file reads as having no server rather than
    /// as one that is forever starting.
    would_not_start: Mutex<HashSet<ServerKey>>,
}

impl LspRegistry {
    /// A registry that looks for its servers on `search_path`, which is a `PATH`.
    ///
    /// Every server it starts is told it is talking to `moon_lsp`, of this crate's version,
    /// until a caller says otherwise with [`LspRegistry::identifying_as`].
    pub fn new(search_path: String) -> Self {
        Self {
            search_path,
            client: ClientIdentity::default(),
            servers: Mutex::new(HashMap::new()),
            would_not_start: Mutex::new(HashSet::new()),
        }
    }

    /// Say who the host application is, so that is what its servers are told rather than the
    /// name of the library they are being spoken to through.
    ///
    /// Worth saying: a server writes `clientInfo` into its log, which is where a report about
    /// one of these servers is eventually read, and a few of them offer different things to
    /// different editors. It is a separate call rather than an argument to
    /// [`LspRegistry::new`] because a caller that has nothing to say about itself is already
    /// answered truthfully - see [`ClientIdentity`].
    ///
    /// ```
    /// use moon_lsp::{ClientIdentity, LspRegistry};
    ///
    /// let servers = LspRegistry::new(std::env::var("PATH").unwrap_or_default())
    ///     .identifying_as(ClientIdentity::new("my-editor", env!("CARGO_PKG_VERSION")));
    /// ```
    pub fn identifying_as(mut self, client: ClientIdentity) -> Self {
        self.client = client;
        self
    }

    fn running(&self, key: &ServerKey) -> Option<Arc<LanguageServer>> {
        self.servers.lock().unwrap().get(key).cloned()
    }

    /// What one running server agreed to count columns in. `None` when no server of that
    /// name is running for this key.
    pub fn agreed_encoding(
        &self,
        workspace_key: &str,
        server_name: &'static str,
    ) -> Option<PositionEncoding> {
        self.running(&(workspace_key.to_string(), server_name))
            .map(|server| server.encoding())
    }

    /// Every server running for one workspace, with the name it is known by. What a caller
    /// showing the wait is answered out of: the question is about the workspace rather than
    /// about one file, because a window with a Rust file and a TypeScript file open is
    /// waiting on both.
    fn running_for(&self, workspace_key: &str) -> Vec<(&'static str, Arc<LanguageServer>)> {
        self.servers
            .lock()
            .unwrap()
            .iter()
            .filter(|((key, _), _)| key == workspace_key)
            .map(|((_, name), server)| (*name, Arc::clone(server)))
            .collect()
    }

    /// Whether this server has already been tried and would not start.
    pub(crate) fn gave_up_on(&self, key: &ServerKey) -> bool {
        self.would_not_start.lock().unwrap().contains(key)
    }

    pub(crate) fn give_up_on(&self, key: &ServerKey) {
        self.would_not_start.lock().unwrap().insert(key.clone());
    }

    /// The row for a file whose server is installed here. `None` covers both of the
    /// [`LspStatus::Unavailable`] cases.
    fn served_language(&self, file_path: &str) -> Option<&'static ExtensionSpec> {
        let language = languages::for_file(file_path)?;
        languages::installed_at(language.server.command, &self.search_path)?;
        Some(language)
    }

    /// The server for one workspace and one language, started if it is not running yet.
    ///
    /// Starting blocks until the server has answered `initialize`, so the lock on the map
    /// is given up for the wait: a caller asking for a file's status while a server comes up
    /// must not wait on it. Two callers starting the same server at once is therefore
    /// possible, and the loser's server is shut down again as it is dropped.
    fn ensure(
        &self,
        key: &ServerKey,
        spec: &'static ServerSpec,
        repo_root: &Path,
    ) -> Result<Arc<LanguageServer>> {
        if let Some(running) = self.running(key) {
            return Ok(running);
        }

        let started = Arc::new(LanguageServer::start(
            spec,
            repo_root,
            &self.search_path,
            &self.client,
        )?);
        let mut servers = self.servers.lock().unwrap();
        Ok(Arc::clone(servers.entry(key.clone()).or_insert(started)))
    }

    /// Whether a language server is behind this file, and whether it has finished starting.
    ///
    /// Answered without the repo root: nothing has to be read off disk to say whether a
    /// server exists, is installed, and has settled.
    pub fn status(&self, workspace_key: &str, file_path: &str) -> LspStatus {
        let language = languages::for_file(file_path);
        let to_start = language.filter(|language| {
            let key = (workspace_key.to_string(), language.server.name);
            languages::installed_at(language.server.command, &self.search_path).is_some()
                && !self.gave_up_on(&key)
        });
        let Some(language) = to_start else {
            return status_without_a_server(language, false);
        };

        let key = (workspace_key.to_string(), language.server.name);
        match self.running(&key) {
            // Nothing started yet, and something to start: the file has a server behind it
            // and it is not answering questions about this file yet, which is what starting
            // is.
            None => LspStatus::Starting,
            Some(server) if server.is_ready() => LspStatus::Ready,
            Some(_) => LspStatus::Starting,
        }
    }

    /// What every language server running for this workspace is doing right now.
    ///
    /// Empty means nothing is working: either no server has started for this workspace, or
    /// every one of them has finished what it announced. That is the ordinary state, and it
    /// reads as "nothing to wait for" rather than as an answer that could not be got.
    ///
    /// A server that is starting but has announced nothing yet is not in here either. It has
    /// said nothing about itself, and inventing a line for it would be a guess at what a
    /// server is doing - which is the whole thing this exists not to do.
    pub fn working(&self, workspace_key: &str) -> Vec<LspWork> {
        let mut working: Vec<LspWork> = self
            .running_for(workspace_key)
            .into_iter()
            .filter_map(|(name, server)| {
                let doing = server.working()?;
                Some(LspWork {
                    server: name.to_string(),
                    title: doing.title,
                    detail: doing.detail,
                    percentage: doing.percentage,
                })
            })
            .collect();
        // By server name, so a caller with two of them running does not have its line
        // swapping between them as a hash map is walked in whatever order it feels like.
        working.sort_by(|left, right| left.server.cmp(&right.server));
        working
    }

    /// Tell the server a file is open and what is in it, starting the server if this is the
    /// first file of its language. A file no server serves is quietly nothing to do.
    pub fn did_open(&self, workspace: &Workspace<'_>, file_path: &str, text: &str) -> Result<()> {
        let Some(language) = self.served_language(file_path) else {
            return Ok(());
        };
        let key = (workspace.key.to_string(), language.server.name);
        if self.gave_up_on(&key) {
            return Ok(());
        }
        let server = self
            .ensure(&key, language.server, workspace.root)
            .inspect_err(|_| {
                // Said once, and then the file is one with no server behind it - see
                // [`LspRegistry::would_not_start`].
                self.give_up_on(&key);
            })?;

        let uri = protocol::file_uri(&workspace.root.join(file_path));
        if server.has_document(file_path) {
            // Opening the same file twice is a second pane on it, not a new document.
            server.notify(
                "textDocument/didChange",
                protocol::did_change_params(&uri, text),
            )?;
        } else {
            server.notify(
                "textDocument/didOpen",
                protocol::did_open_params(&uri, language.language_id, text),
            )?;
        }
        server.remember_document(file_path, text);
        Ok(())
    }

    /// The whole text again, as it stands.
    ///
    /// **The caller debounces.** Where this is reached over a network it is a round trip,
    /// and one per keystroke would flood it - send it after the typing has paused, not while
    /// it is going on.
    pub fn did_change(&self, workspace: &Workspace<'_>, file_path: &str, text: &str) -> Result<()> {
        let Some(language) = self.served_language(file_path) else {
            return Ok(());
        };
        let key = (workspace.key.to_string(), language.server.name);
        let Some(server) = self.running(&key) else {
            // Nothing has this file open, so there is nothing to tell about the change.
            return Ok(());
        };
        if !server.has_document(file_path) {
            return Ok(());
        }

        let uri = protocol::file_uri(&workspace.root.join(file_path));
        server.notify(
            "textDocument/didChange",
            protocol::did_change_params(&uri, text),
        )?;
        server.remember_document(file_path, text);
        Ok(())
    }

    /// Tell the server this side is done with a file.
    pub fn did_close(&self, workspace: &Workspace<'_>, file_path: &str) -> Result<()> {
        let Some(language) = self.served_language(file_path) else {
            return Ok(());
        };
        let key = (workspace.key.to_string(), language.server.name);
        let Some(server) = self.running(&key) else {
            return Ok(());
        };
        if !server.has_document(file_path) {
            return Ok(());
        }

        let uri = protocol::file_uri(&workspace.root.join(file_path));
        server.notify("textDocument/didClose", protocol::did_close_params(&uri))?;
        server.forget_document(file_path);
        Ok(())
    }

    /// Where the name at this place is defined. Empty when the server has no answer.
    pub fn definition(
        &self,
        workspace: &Workspace<'_>,
        file_path: &str,
        at: LspPosition,
    ) -> Result<Vec<LspLocation>> {
        let Some(question) = self.ask(workspace, file_path, &at)? else {
            return Ok(Vec::new());
        };
        let answer = question.server.request(
            "textDocument/definition",
            protocol::position_params(&question.uri, at.line, question.character),
        )?;
        protocol::locations_from(answer, workspace.root)
    }

    /// What could be typed at this place. Empty when the server offers nothing.
    pub fn completion(
        &self,
        workspace: &Workspace<'_>,
        file_path: &str,
        at: LspPosition,
    ) -> Result<Vec<LspCompletion>> {
        let Some(question) = self.ask(workspace, file_path, &at)? else {
            return Ok(Vec::new());
        };
        let answer = question.server.request(
            "textDocument/completion",
            protocol::position_params(&question.uri, at.line, question.character),
        )?;
        protocol::completions_from(answer)
    }

    /// Work out what a question about one place in one file needs.
    ///
    /// `None` for a file no server serves. A file that is not open is an error rather than
    /// an empty answer: a question about a document the server has never been told about is
    /// a caller that skipped [`LspRegistry::did_open`], not a question with no answer.
    fn ask(
        &self,
        workspace: &Workspace<'_>,
        file_path: &str,
        at: &LspPosition,
    ) -> Result<Option<Question>> {
        let Some(language) = self.served_language(file_path) else {
            return Ok(None);
        };
        let key = (workspace.key.to_string(), language.server.name);
        let server = self.running(&key).ok_or_else(|| {
            anyhow!(
                "no {} server is running for this workspace",
                language.server.name
            )
        })?;
        let Some(text) = server.document_text(file_path) else {
            bail!(
                "{file_path} is not open in the {} server",
                language.server.name
            );
        };

        let uri = protocol::file_uri(&workspace.root.join(file_path));
        let character = protocol::position_in(&text, at, server.encoding())?;
        Ok(Some(Question {
            server,
            uri,
            character,
        }))
    }
}

/// What a question about a place in a file needs: the server to ask, the document's URI,
/// and the position in the units that server agreed to.
struct Question {
    server: Arc<LanguageServer>,
    uri: String,
    /// The column, already converted - see [`protocol::lsp_character`].
    character: u32,
}

/// What to say about a file before any running server is looked at.
///
/// Every way of having no server reads the same to the person: an extension nothing in the
/// table serves, a server that is in the table but is not installed on this machine, and
/// one that is installed and would not start. Anything else has a server behind it, which
/// has at the least started.
pub(crate) fn status_without_a_server(
    language: Option<&ExtensionSpec>,
    has_a_server_to_start: bool,
) -> LspStatus {
    match (language, has_a_server_to_start) {
        (Some(_), true) => LspStatus::Starting,
        _ => LspStatus::Unavailable,
    }
}
