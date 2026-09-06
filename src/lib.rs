//! A language server client: the process, the framing, the position arithmetic, and a
//! registry of the servers running for each workspace.
//!
//! An editor that wants go-to-definition and completions needs a client, and a client is
//! more work than it sounds: a server has to be found on the user's own `PATH`, started in
//! the repo, walked through `initialize`, kept fed with the text of every open document,
//! and - the part that is quietly wrong in most clients - asked about a position counted in
//! the units *that* server agreed to rather than the ones the editor has.
//!
//! ```no_run
//! use moon_lsp::{LspPosition, LspRegistry, Workspace};
//!
//! let servers = LspRegistry::new(std::env::var("PATH").unwrap_or_default());
//! let root = std::path::Path::new("/home/dev/repo");
//! let repo = Workspace { key: "one window", root };
//! let text = std::fs::read_to_string(root.join("src/main.rs"))?;
//!
//! servers.did_open(&repo, "src/main.rs", &text)?;
//! // A server answers with nothing until it has finished indexing, so wait for it.
//! while servers.status(repo.key, "src/main.rs") == moon_lsp::LspStatus::Starting {
//!     std::thread::sleep(std::time::Duration::from_millis(500));
//! }
//! let at = LspPosition { line: 4, column: 18 };
//! for place in servers.definition(&repo, "src/main.rs", at)? {
//!     println!("{}:{}", place.file_path, place.line_number);
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # The seam
//!
//! The crate owns the servers and what is said to them. What is drawn, where the text came
//! from, and how much a set of servers is shared stay with the caller: a [`Workspace`] is a
//! repo root and an opaque key, and the caller decides whether that key is the repo, one
//! window's work, or anything else.
//!
//! Three other things are the caller's on purpose. The `PATH` servers are looked for and
//! started on is handed to [`LspRegistry::new`], because a window started from a desktop
//! launcher has a `PATH` with nothing on it and only the host application knows where its
//! user really installs things. The name a server is told it is talking to is handed to
//! [`LspRegistry::identifying_as`] for the same reason - a library cannot know whose program
//! it is inside, and what it says about that goes into every server's log, so said nothing
//! about it says `moon_lsp` and this crate's version rather than guessing. And the debouncing
//! of [`LspRegistry::did_change`] is the caller's, since a round trip per keystroke is a flood
//! wherever this is reached over a network.
//!
//! # The parts
//!
//! [`languages`] says which server serves which file, [`process`] runs one and carries the
//! JSON-RPC, [`framing`] is the envelope that goes over its stdio, [`protocol`] is the
//! messages - and the one place a position is converted between what an editor counts and
//! what a server counts - and [`registry`] is what a caller talks to.

#![forbid(unsafe_code)]
#![warn(clippy::doc_markdown)]

pub mod framing;
pub mod languages;
pub mod payload;
pub mod process;
pub mod protocol;
pub mod registry;

pub use payload::{
    LspCompletion, LspCompletionKind, LspLocation, LspPosition, LspStatus, LspWork,
};
pub use protocol::ClientIdentity;
pub use registry::{LspRegistry, Workspace};

#[cfg(test)]
mod tests;
