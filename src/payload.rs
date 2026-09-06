//! What a caller asks about and what it is answered with.
//!
//! These are the shapes that cross the boundary out of this crate, and they are the shapes
//! that go over a wire: a client asking about a repo on another machine sends the question
//! and reads the answer back as these, so they derive [`serde`]'s traits and nothing here
//! carries anything a JSON document cannot.

use serde::{Deserialize, Serialize};

/// A place in a file, counted the way an editor counts: the line from zero, and how far
/// into that line in **bytes**.
///
/// Bytes because that is what a text buffer reports and what a `String` is indexed by. The
/// protocol counts UTF-16 code units, and converting between the two happens in one place -
/// [`crate::protocol::lsp_character`] - rather than at every call site.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct LspPosition {
    /// The line, counted from zero.
    pub line: usize,
    /// How far into that line, in bytes.
    pub column: usize,
}

/// Somewhere a definition was found: the file, and the line to open it at.
///
/// `line_number` counts from one, the way a caller with somewhere to open counts, so it can
/// be opened without an off-by-one at the boundary.
#[derive(Clone, Serialize, Deserialize)]
pub struct LspLocation {
    /// The file, relative to the repo when it is inside it, and absolute when it is not - a
    /// dependency's source or the standard library.
    pub file_path: String,
    /// The line to open it at, counted from one.
    pub line_number: usize,
}

/// One thing a server offered to complete with.
#[derive(Clone, Serialize, Deserialize)]
pub struct LspCompletion {
    /// What the server means to be read.
    pub label: String,
    /// The line beside it, where the server writes one: a type, a signature, a module.
    pub detail: Option<String>,
    /// What the server means to be typed, which is the label where it did not say.
    pub insert: String,
    /// What sort of thing it is, where the server said. `None` is a server that did not say,
    /// which is a thing of no known sort rather than a thing of some default sort - a caller
    /// acting on the kind has to be able to tell those apart.
    pub kind: Option<LspCompletionKind>,
}

/// What sort of thing a server offered: a function, a field, a keyword.
///
/// The protocol's own list, written out here rather than passed on as
/// [`lsp_types::CompletionItemKind`], because that type is this crate's business and the
/// answer crosses out of it - to a widget that never heard of the protocol, and over a wire
/// as JSON. Nothing here reads anything into a kind: what a caller does about a function is a
/// caller's decision, and this crate only carries which one it was told.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspCompletionKind {
    /// Plain text with no meaning behind it.
    Text,
    /// A method on something.
    Method,
    /// A free function.
    Function,
    /// A constructor, however the language spells one.
    Constructor,
    /// A field of a struct, a record, an object.
    Field,
    /// A local, a parameter, a global.
    Variable,
    /// A class.
    Class,
    /// An interface, a trait, a protocol.
    Interface,
    /// A module, a namespace, a package.
    Module,
    /// A property, where a language has those apart from fields.
    Property,
    /// A unit - a measure a language has literals for.
    Unit,
    /// A value, where the server means the value rather than the name of one.
    Value,
    /// An enum.
    Enum,
    /// A keyword of the language.
    Keyword,
    /// A snippet the server means to be expanded. Never asked for here - see
    /// [`crate::protocol::initialize_params`], which says this client does not do snippets -
    /// but a server is free to send one anyway.
    Snippet,
    /// A colour.
    Color,
    /// A file.
    File,
    /// A reference.
    Reference,
    /// A folder.
    Folder,
    /// One member of an enum.
    EnumMember,
    /// A constant.
    Constant,
    /// A struct.
    Struct,
    /// An event.
    Event,
    /// An operator.
    Operator,
    /// A type parameter - a generic's `T`.
    TypeParameter,
}

/// Whether there is a language server behind a file, and whether it can answer yet.
///
/// The three are told apart because they read completely differently to a person: a server
/// that is still indexing answers with nothing, which is indistinguishable from "there is
/// no definition" unless the caller is told which it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LspStatus {
    /// No server serves this file, or the one that would is not installed here, or it is
    /// installed and would not start. All three are the same to whoever is reading.
    Unavailable,
    /// There is a server behind this file and it cannot answer about it yet.
    Starting,
    /// A server is behind this file and has finished starting.
    Ready,
}

/// One piece of work a language server has begun and not finished, as it described it.
///
/// This is the server's own account of the wait, not a guess at it: the title and the line
/// under it are the server's words, and `percentage` is there only when it said how far
/// through it is - most work never does.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspWork {
    /// The server doing it, by the name the language table knows it by - `rust`.
    pub server: String,
    /// What the server called this piece of work: "Indexing", "Fetching metadata".
    pub title: String,
    /// The line it is writing under that title, where it writes one.
    pub detail: Option<String>,
    /// How far through, 0 to 100, where the server says.
    pub percentage: Option<u8>,
}
