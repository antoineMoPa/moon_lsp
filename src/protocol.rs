//! The messages themselves: what is sent to a server, what is made of its answers, and the
//! one place a position is turned from the editor's units into the server's.
//!
//! [`lsp_types`] is used for the answers, where the protocol has three shapes for a
//! definition and two for a completion list, and plain JSON for what goes out, which is
//! short enough to read as the message it is.

use anyhow::{Context, Result, bail};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, GotoDefinitionResponse,
    InitializeResult, Location, PositionEncodingKind, ServerCapabilities,
};
use serde_json::{Value, json};

use crate::{
    payload::{LspCompletion, LspCompletionKind, LspLocation, LspPosition},
    process::PositionEncoding,
};

/// Who a server is told it is talking to: the `clientInfo` of `initialize`.
///
/// This is the caller's for the same reason the search path handed to
/// [`LspRegistry::new`](crate::LspRegistry::new) is - it is a thing the crate cannot know. A
/// library has no host application's name to give, and a server writes what it is told into
/// its log and in places changes what it offers by it, so what goes out has to be true of
/// whoever is really speaking. Said nothing about, that is this crate: [`Default`] is
/// `moon_lsp` and the version of the crate itself, which is the only honest answer about a
/// program it knows nothing about. A host application says its own name with
/// [`LspRegistry::identifying_as`](crate::LspRegistry::identifying_as).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ClientIdentity {
    /// The name of the program talking to the server, as it should read in a server's log.
    pub name: String,
    /// Its version, which is worth sending: a server's log is where a bug in a client is
    /// eventually found, and a version is what tells one report from another. `None` for a
    /// caller with no version to give, and then nothing is sent rather than something made
    /// up.
    pub version: Option<String>,
}

impl Default for ClientIdentity {
    fn default() -> Self {
        Self {
            name: env!("CARGO_PKG_NAME").to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }
}

impl ClientIdentity {
    /// A program of this name and version. A caller with no version to give writes the two
    /// fields out itself with `version: None`.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: Some(version.into()),
        }
    }

    /// The `clientInfo` of `initialize`, with the version left out where there is none - the
    /// field is optional in the protocol, and a server reading it back should see an absence
    /// rather than an empty string.
    fn as_client_info(&self) -> Value {
        match &self.version {
            Some(version) => json!({ "name": self.name, "version": version }),
            None => json!({ "name": self.name }),
        }
    }
}

/// What is asked of a server as it starts: the repo as the workspace, who is asking, and -
/// the point of this - a request to count columns in bytes.
///
/// Bytes are what the editor has. The protocol's default is UTF-16 code units, so without
/// this every position on a line holding anything outside ASCII is wrong by however many
/// bytes the multi-byte characters before it take. 3.17 lets a client ask for another
/// encoding, and what the server answers with is read back by [`agreed_encoding`] rather
/// than assumed - a server free to ignore the request is a server that will.
pub fn initialize_params(repo_root: &std::path::Path, client: &ClientIdentity) -> Value {
    let root_uri = file_uri(repo_root);
    json!({
        "processId": std::process::id(),
        "clientInfo": client.as_client_info(),
        "rootUri": root_uri,
        "workspaceFolders": [{
            "uri": root_uri,
            "name": repo_root.file_name().and_then(|name| name.to_str()).unwrap_or("workspace"),
        }],
        "capabilities": {
            "general": { "positionEncodings": ["utf-8", "utf-16"] },
            "window": { "workDoneProgress": true },
            "textDocument": {
                "synchronization": { "dynamicRegistration": false },
                "definition": { "linkSupport": true },
                "completion": {
                    "completionItem": { "snippetSupport": false },
                    "contextSupport": false,
                },
            },
        },
    })
}

/// What the server actually agreed to count columns in. A server that says nothing has
/// agreed to nothing, which the protocol says means UTF-16 - and then every position is
/// converted rather than passed through.
pub fn agreed_encoding(initialize_reply: &Value) -> Result<PositionEncoding> {
    let result: InitializeResult = serde_json::from_value(initialize_reply.clone())
        .context("the language server's initialize reply could not be read")?;
    Ok(encoding_of(&result.capabilities))
}

fn encoding_of(capabilities: &ServerCapabilities) -> PositionEncoding {
    match capabilities.position_encoding.as_ref() {
        Some(kind) if *kind == PositionEncodingKind::UTF8 => PositionEncoding::Utf8,
        _ => PositionEncoding::Utf16,
    }
}

/// Answer one of the server's own requests. Only the shape matters: a server that asked for
/// its configuration and is handed one entry per item it asked about carries on, and one
/// left waiting stops.
pub fn reply_to_server_request(method: &str, message: &Value) -> Value {
    match method {
        "workspace/configuration" => {
            let asked_for = message
                .pointer("/params/items")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Value::Array(vec![Value::Null; asked_for])
        }
        // `window/workDoneProgress/create`, `client/registerCapability` and the rest are
        // all served by "yes, fine".
        _ => Value::Null,
    }
}

/// Which end of a piece of work a `$/progress` notification is - `begin`, `report` or
/// `end`. This is what readiness is counted from; see [`crate::process`].
pub fn progress_kind(message: &Value) -> Option<&str> {
    message.pointer("/params/value/kind")?.as_str()
}

/// Which piece of work the notification is about.
///
/// Readiness is tracked per token rather than as one count of outstanding work, because a
/// server announces several pieces at once and interleaves them: rust-analyzer scans its
/// roots while it loads proc-macros, and it checks the project with `cargo check` while it
/// is still priming its cache. A count cannot tell those apart, and a token can - see
/// [`crate::process`].
///
/// The protocol lets a token be a string or an integer, and both are read as the text that
/// names the work. `None` is a notification with no token at all, which names no work and
/// so says nothing about whether the server has finished starting.
pub fn progress_token(message: &Value) -> Option<String> {
    match message.pointer("/params/token")? {
        Value::String(token) => Some(token.clone()),
        Value::Number(token) => Some(token.to_string()),
        _ => None,
    }
}

/// What the same notification says the server is actually doing, which is what a caller
/// showing the wait reads out.
///
/// Every field is optional because the protocol makes them so, and a caller has to be written
/// to read well without any of them: only a `begin` carries a `title`, a `report` may carry
/// nothing but a `message`, and plenty of work reports no percentage at all - rust-analyzer
/// fetches a project's metadata without ever saying how far through it is. Carrying the
/// title forward from the `begin` is [`crate::process::Working`]'s business, since it is the
/// only thing that sees the notifications of one piece of work in a row.
#[derive(Default, PartialEq, Eq, Debug)]
pub struct ProgressNote {
    /// What the work is called: "Indexing", "Fetching metadata".
    pub title: Option<String>,
    /// The line under the title: which crate is being read, how many files are left.
    pub message: Option<String>,
    /// How far through, 0 to 100.
    pub percentage: Option<u8>,
}

/// Read one `$/progress` notification for what it says about the work.
pub fn progress_note(message: &Value) -> ProgressNote {
    let Some(value) = message.pointer("/params/value") else {
        return ProgressNote::default();
    };
    ProgressNote {
        title: text_at(value, "title"),
        message: text_at(value, "message"),
        // The protocol calls this an unsigned integer, but it is a JSON number on the wire
        // and servers have sent it as a float, so it is read as one and rounded. Out of
        // range is held to the ends: a bar drawn past its own width is worse than a bar that
        // sits full while the last of the work finishes.
        percentage: value
            .get("percentage")
            .and_then(Value::as_f64)
            .map(|percentage| percentage.round().clamp(0.0, 100.0) as u8),
    }
}

/// One string field of a progress value, if it is there and is not empty. An empty title is
/// a title the bar would draw a blank space for.
fn text_at(value: &Value, field: &str) -> Option<String> {
    let text = value.get(field)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// A document the server is being told about for the first time, with the whole of its text.
pub fn did_open_params(uri: &str, language_id: &str, text: &str) -> Value {
    json!({
        "textDocument": {
            "uri": uri,
            "languageId": language_id,
            "version": 1,
            "text": text,
        }
    })
}

/// The whole text again. Full-text sync: the alternative is keeping a version number and a
/// list of edits in step with an editor that does not report them, and every server here
/// accepts the whole document.
pub fn did_change_params(uri: &str, text: &str) -> Value {
    json!({
        "textDocument": { "uri": uri, "version": 2 },
        "contentChanges": [{ "text": text }],
    })
}

/// A document the server is being told is no longer open here.
pub fn did_close_params(uri: &str) -> Value {
    json!({ "textDocument": { "uri": uri } })
}

/// A question about one place in one document, in the units the server agreed to.
pub fn position_params(uri: &str, line: usize, character: u32) -> Value {
    json!({
        "textDocument": { "uri": uri },
        "position": { "line": line, "character": character },
    })
}

/// The places a definition answer names, as the file pane opens them.
///
/// A definition outside the repo - a dependency's source, the standard library - keeps its
/// absolute path, because that is the only thing that names it. The pane decides whether it
/// can open one; making it a repo-relative path it is not would be a lie.
pub fn locations_from(
    answer: Value,
    repo_root: &std::path::Path,
) -> Result<Vec<LspLocation>> {
    if answer.is_null() {
        return Ok(Vec::new());
    }
    let response: GotoDefinitionResponse =
        serde_json::from_value(answer).context("the definition answer could not be read")?;
    let places: Vec<(String, u32)> = match response {
        GotoDefinitionResponse::Scalar(location) => vec![place_of(&location)],
        GotoDefinitionResponse::Array(locations) => locations.iter().map(place_of).collect(),
        GotoDefinitionResponse::Link(links) => links
            .iter()
            .map(|link| {
                (
                    link.target_uri.as_str().to_string(),
                    link.target_selection_range.start.line,
                )
            })
            .collect(),
    };

    Ok(places
        .into_iter()
        .filter_map(|(uri, line)| {
            Some(LspLocation {
                file_path: path_in_repo(&path_from_file_uri(&uri)?, repo_root),
                // The protocol counts lines from zero and the panes count from one.
                line_number: line as usize + 1,
            })
        })
        .collect())
}

fn place_of(location: &Location) -> (String, u32) {
    (location.uri.as_str().to_string(), location.range.start.line)
}

/// What a completion answer offers. `insertText` is what a server means to be typed and the
/// label is what it means to be read; where there is no insert text the two are the same.
pub fn completions_from(answer: Value) -> Result<Vec<LspCompletion>> {
    if answer.is_null() {
        return Ok(Vec::new());
    }
    let response: CompletionResponse =
        serde_json::from_value(answer).context("the completion answer could not be read")?;
    let items = match response {
        CompletionResponse::Array(items) => items,
        CompletionResponse::List(list) => list.items,
    };
    Ok(items.iter().map(completion_of).collect())
}

fn completion_of(item: &CompletionItem) -> LspCompletion {
    LspCompletion {
        insert: item
            .insert_text
            .clone()
            .unwrap_or_else(|| item.label.clone()),
        label: item.label.clone(),
        detail: item.detail.clone(),
        kind: kind_of(item.kind),
    }
}

/// The protocol's completion kinds beside this crate's own, one for one.
///
/// A table because that is what it is - the protocol's numbering on one side and the name it
/// stands for on the other - and because it is where a kind the protocol adds after this was
/// written shows up as a row that is missing rather than as a branch that quietly took the
/// wrong one.
const KINDS: &[(CompletionItemKind, LspCompletionKind)] = &[
    (CompletionItemKind::TEXT, LspCompletionKind::Text),
    (CompletionItemKind::METHOD, LspCompletionKind::Method),
    (CompletionItemKind::FUNCTION, LspCompletionKind::Function),
    (
        CompletionItemKind::CONSTRUCTOR,
        LspCompletionKind::Constructor,
    ),
    (CompletionItemKind::FIELD, LspCompletionKind::Field),
    (CompletionItemKind::VARIABLE, LspCompletionKind::Variable),
    (CompletionItemKind::CLASS, LspCompletionKind::Class),
    (CompletionItemKind::INTERFACE, LspCompletionKind::Interface),
    (CompletionItemKind::MODULE, LspCompletionKind::Module),
    (CompletionItemKind::PROPERTY, LspCompletionKind::Property),
    (CompletionItemKind::UNIT, LspCompletionKind::Unit),
    (CompletionItemKind::VALUE, LspCompletionKind::Value),
    (CompletionItemKind::ENUM, LspCompletionKind::Enum),
    (CompletionItemKind::KEYWORD, LspCompletionKind::Keyword),
    (CompletionItemKind::SNIPPET, LspCompletionKind::Snippet),
    (CompletionItemKind::COLOR, LspCompletionKind::Color),
    (CompletionItemKind::FILE, LspCompletionKind::File),
    (CompletionItemKind::REFERENCE, LspCompletionKind::Reference),
    (CompletionItemKind::FOLDER, LspCompletionKind::Folder),
    (
        CompletionItemKind::ENUM_MEMBER,
        LspCompletionKind::EnumMember,
    ),
    (CompletionItemKind::CONSTANT, LspCompletionKind::Constant),
    (CompletionItemKind::STRUCT, LspCompletionKind::Struct),
    (CompletionItemKind::EVENT, LspCompletionKind::Event),
    (CompletionItemKind::OPERATOR, LspCompletionKind::Operator),
    (
        CompletionItemKind::TYPE_PARAMETER,
        LspCompletionKind::TypeParameter,
    ),
];

/// What sort of thing the server said an item is, as this crate's own name for it.
///
/// `None` twice over, and both mean the same thing to whoever is asked to act on it: an item
/// the server sent no kind for, and one it sent a number this table has no row for - the
/// protocol's kinds are a bare integer on the wire, so a server is free to send a kind newer
/// than this list. Neither is guessed at.
fn kind_of(kind: Option<CompletionItemKind>) -> Option<LspCompletionKind> {
    let kind = kind?;
    KINDS
        .iter()
        .find(|(protocol, _)| *protocol == kind)
        .map(|(_, ours)| *ours)
}

/// The column the server wants, from the byte column the editor gives, against the line it
/// falls on.
///
/// This is the whole of the encoding question, in one function, on purpose: it is the bug
/// every LSP client has, it is invisible in a file of ASCII, and the only defence is that
/// there is one place to get it right. A column past the end of the line is clamped to the
/// end, which is what the protocol says to do with one.
pub fn lsp_character(line: &str, byte_column: usize, encoding: PositionEncoding) -> u32 {
    let byte_column = byte_column.min(line.len());
    // A column landing inside a character belongs to that character's start.
    let cut = (0..=byte_column)
        .rev()
        .find(|index| line.is_char_boundary(*index))
        .unwrap_or(0);
    match encoding {
        PositionEncoding::Utf8 => cut as u32,
        PositionEncoding::Utf16 => line[..cut].encode_utf16().count() as u32,
    }
}

/// One line of a document, by its number counted from zero. `None` says the position is
/// past the end of the file, which is a caller working from a stale copy of it.
pub fn line_of(text: &str, line: usize) -> Option<&str> {
    text.split('\n')
        .nth(line)
        .map(|line| line.trim_end_matches('\r'))
}

/// The position a request goes out with, in the server's own units.
pub fn position_in(text: &str, at: &LspPosition, encoding: PositionEncoding) -> Result<u32> {
    let Some(line) = line_of(text, at.line) else {
        bail!("line {} is past the end of the file", at.line + 1);
    };
    Ok(lsp_character(line, at.column, encoding))
}

/// A path as a `file://` URI. Everything outside the unreserved set is escaped, so a repo
/// under a folder with a space or an accent in its name is named correctly.
pub fn file_uri(path: &std::path::Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                uri.push(*byte as char);
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// The path a `file://` URI names. `None` for anything else - a server may answer with a
/// `jdt:` or `untitled:` URI, which names nothing on disk.
pub fn path_from_file_uri(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///path` has an empty authority; anything else is a host we cannot open.
    let path = rest.strip_prefix('/').map(|path| format!("/{path}"))?;
    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    Some(std::path::PathBuf::from(String::from_utf8(decoded).ok()?))
}

/// A path as the panes name files: relative to the repo when it is inside it, and as it
/// stands when it is not.
fn path_in_repo(path: &std::path::Path, repo_root: &std::path::Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .display()
        .to_string()
}
