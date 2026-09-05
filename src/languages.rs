//! Which language server serves which file, as two constant tables.
//!
//! Adding a language is a row in [`EXTENSIONS`], and a [`ServerSpec`] beside the others if
//! its server is a new one. Nothing else in this module knows the name of a language, so
//! that row is the whole of the change.
//!
//! The two tables are separate because the mapping is not one to one: `.ts`, `.tsx`, `.js`
//! and `.jsx` are four languages as far as the protocol is concerned - each has its own
//! `languageId` - and one server as far as the machine is concerned, so a project mixing
//! them is indexed once rather than four times.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

/// One language server: what it is called here, and how it is started. The command speaks
/// the protocol on its stdin and stdout, which every server in the table below does.
pub struct ServerSpec {
    /// What the server is held under in the registry, and what a message about it reads as.
    pub name: &'static str,
    /// The binary to run, looked for on the search path the caller supplies.
    pub command: &'static str,
    /// What it is started with. Most servers speak the protocol on stdio only once told to.
    pub args: &'static [&'static str],
}

/// One file extension: what the protocol calls that language, and the server behind it.
pub struct ExtensionSpec {
    /// The extension itself, without the dot, matched without regard for case.
    pub extension: &'static str,
    /// The protocol's `languageId` - `typescriptreact` rather than `typescript` for a
    /// `.tsx`, which is the difference between a server parsing JSX and choking on it.
    pub language_id: &'static str,
    /// The server that serves it. Several extensions share one.
    pub server: &'static ServerSpec,
}

/// rust-analyzer, which serves `.rs`.
pub const RUST_ANALYZER: ServerSpec = ServerSpec {
    name: "rust",
    command: "rust-analyzer",
    args: &[],
};

/// typescript-language-server, which serves TypeScript, JavaScript and their React flavours.
pub const TYPESCRIPT: ServerSpec = ServerSpec {
    name: "typescript",
    command: "typescript-language-server",
    args: &["--stdio"],
};

/// pyright's language server, which serves `.py`.
pub const PYRIGHT: ServerSpec = ServerSpec {
    name: "python",
    command: "pyright-langserver",
    args: &["--stdio"],
};

/// Every extension a server is offered for. An extension that is not here has no server
/// behind it, which is most of a repo - markdown, configuration, images.
pub const EXTENSIONS: &[ExtensionSpec] = &[
    ExtensionSpec {
        extension: "rs",
        language_id: "rust",
        server: &RUST_ANALYZER,
    },
    ExtensionSpec {
        extension: "ts",
        language_id: "typescript",
        server: &TYPESCRIPT,
    },
    ExtensionSpec {
        extension: "tsx",
        language_id: "typescriptreact",
        server: &TYPESCRIPT,
    },
    ExtensionSpec {
        extension: "js",
        language_id: "javascript",
        server: &TYPESCRIPT,
    },
    ExtensionSpec {
        extension: "jsx",
        language_id: "javascriptreact",
        server: &TYPESCRIPT,
    },
    ExtensionSpec {
        extension: "mjs",
        language_id: "javascript",
        server: &TYPESCRIPT,
    },
    ExtensionSpec {
        extension: "py",
        language_id: "python",
        server: &PYRIGHT,
    },
];

/// The row for a file, by its extension. `None` for a file no server in the table serves.
pub fn for_file(file_path: &str) -> Option<&'static ExtensionSpec> {
    let extension = Path::new(file_path).extension()?.to_str()?;
    EXTENSIONS
        .iter()
        .find(|spec| spec.extension.eq_ignore_ascii_case(extension))
}

/// Where a server's command is on `search_path`, if it is installed at all.
///
/// The search path is the caller's to supply, in the form of a `PATH` - the directories a
/// server may be installed in are the host application's business, not this crate's. A
/// window started from a desktop launcher inherits neither homebrew nor `~/.local/bin`, so
/// the process's own `PATH` is usually the wrong answer and the caller passes the one its
/// user's login shell has.
///
/// The answer is remembered, per search path and command: a pane asks for a file's status as
/// it draws, and installing a language server while the window is open is not a thing that
/// happens mid-session.
pub fn installed_at(command: &str, search_path: &str) -> Option<PathBuf> {
    /// Where each command was found, by the search path it was looked for on and its name.
    type Found = Mutex<HashMap<(String, String), Option<PathBuf>>>;

    static FOUND: OnceLock<Found> = OnceLock::new();
    let cache = FOUND.get_or_init(|| Mutex::new(HashMap::new()));
    let asked = (search_path.to_string(), command.to_string());

    if let Some(found) = cache.lock().unwrap().get(&asked) {
        return found.clone();
    }
    let found = look_up_on_path(command, search_path);
    cache.lock().unwrap().insert(asked, found.clone());
    found
}

fn look_up_on_path(command: &str, search_path: &str) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .map(|directory| directory.join(command))
        .find(|candidate| is_runnable(candidate))
}

/// A file on PATH that this user can actually run. The permission bits are the difference
/// between a server that is installed and a same-named file that happens to sit beside one.
#[cfg(unix)]
fn is_runnable(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(candidate)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_runnable(candidate: &Path) -> bool {
    candidate.is_file()
}
