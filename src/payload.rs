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
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
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
    /// What that line reads, so a list of places can be read without opening each one. `None`
    /// for a file that could not be read - one deleted since the server last looked at it.
    pub line_text: Option<String>,
}

/// Which places a server is asked for, about the name at one place in a file.
///
/// One question in the protocol's four spellings: each answers with a list of places, and a
/// caller shows and opens those the same way whichever was asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspPlaces {
    /// Where the name is defined.
    Definition,
    /// Where the type of the name is defined - the struct a variable holds, rather than the
    /// line that declared the variable.
    TypeDefinition,
    /// Where a trait, an interface or an abstract method is implemented.
    Implementation,
    /// Everywhere the name is used, its declaration included.
    References,
}

/// How a file is indented, as a server formatting it is told: the protocol's
/// `FormattingOptions`, less the parts nobody here has an opinion on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LspFormatting {
    /// How many columns wide one level of indentation is.
    pub tab_size: u32,
    /// Whether a level is spaces rather than a tab.
    pub insert_spaces: bool,
}

/// Something a server found wrong with a file: where, how bad, and what it says.
///
/// Both ends are counted the way [`LspPosition`] counts, against the text the server was last
/// sent - see [`crate::registry::LspRegistry::diagnostics`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LspDiagnostic {
    /// Where the stretch it is about starts.
    pub start: LspPosition,
    /// Where it ends - the same place for a diagnostic about a point rather than a stretch.
    pub end: LspPosition,
    /// How bad it is.
    pub severity: LspSeverity,
    /// What the server says about it, which can run to several lines.
    pub message: String,
    /// What found it, where the server says: `rustc`, `clippy`, `rust-analyzer`, `ts`.
    pub source: Option<String>,
}

/// How bad a diagnostic is, in the protocol's four grades.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspSeverity {
    /// Something that stops the code building.
    Error,
    /// Something that builds and is probably wrong.
    Warning,
    /// Something worth knowing.
    Information,
    /// A suggestion, often a quiet one.
    Hint,
}

/// Something a server offers to do to the code at a place - a fix for what it found wrong
/// there, or a rewrite - with everything it changes already worked out.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LspCodeAction {
    /// What the server calls it: "Insert explicit type `i32`", "Import `HashMap`".
    pub title: String,
    /// The protocol's kind, where the server said: `quickfix`, `refactor.extract`.
    pub kind: Option<String>,
    /// Whether the server says this is the fix to take, of several.
    pub preferred: bool,
    /// Everything it changes, one entry per file - see [`LspFileEdit`].
    pub files: Vec<LspFileEdit>,
}

/// The signature of the call being typed, and which of its parameters the caret is at.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LspSignature {
    /// The whole signature, as the server writes it: `fn add(a: u32, b: u32) -> u32`.
    pub label: String,
    /// Where in `label` the parameter being typed is, in bytes. `None` for a signature with no
    /// parameters, or one the server did not say the caret is at.
    pub active_parameter: Option<std::ops::Range<usize>>,
    /// What the server says about the function, where it says anything.
    pub documentation: Option<String>,
}

/// One stretch of a file to replace with other text: part of what a rename or a format
/// changes.
///
/// Both ends are counted the way [`LspPosition`] counts, in bytes into their line, and are
/// places in the text as the server had it when it answered - see [`crate::edits`], which is
/// what turns them into ranges of that text and refuses them against any other.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LspTextEdit {
    /// Where the text to replace starts.
    pub start: LspPosition,
    /// Where it ends, which is where it starts for text that is only put in.
    pub end: LspPosition,
    /// What goes in its place.
    pub new_text: String,
}

/// Everything one answer changes in one file.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LspFileEdit {
    /// The file, relative to the repo. Always inside it: an answer that would change a file
    /// outside the repo is refused whole rather than handed back - see
    /// [`crate::protocol::file_edits_from`].
    pub file_path: String,
    /// The stretches to replace, in the order they sit in the file and never overlapping.
    pub edits: Vec<LspTextEdit>,
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
