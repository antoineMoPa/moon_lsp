# moon_lsp

A language server client: the process, the framing, the position arithmetic, and a registry of
the servers running for each workspace.

An editor that wants go-to-definition and completions needs a client, and a client is more work
than it sounds:

- find the server on the user's own `PATH` — not the process's, which from a desktop launcher
  holds nothing the user installed — and start it in the repo
- walk it through `initialize` / `initialized`, and read back what it *agreed* to rather than
  what it was asked for
- keep it fed with the whole text of every open document
- know when it is actually ready, which is not when it answered `initialize`: rust-analyzer
  replies in milliseconds and then indexes for a minute, and a question asked in between comes
  back empty, which reads as "no definition" rather than "not yet"
- ask about a position counted in the units that server agreed to. `héllo` is five characters,
  six bytes and five UTF-16 units, so a client that hands a UTF-16 server its byte columns points
  at the wrong place on every line holding anything but ASCII. That conversion lives in one
  function, on purpose.

```rust
use moon_lsp::{LspPosition, LspRegistry, Workspace};

let servers = LspRegistry::new(std::env::var("PATH")?);
let root = std::path::Path::new("/home/dev/repo");
let repo = Workspace { key: "one window", root };

servers.did_open(&repo, "src/main.rs", &text)?;
for place in servers.definition(&repo, "src/main.rs", LspPosition { line: 4, column: 18 })? {
    println!("{}:{}", place.file_path, place.line_number);
}
```

Rust, TypeScript, JavaScript and Python are in the table today. Adding a language is a row in
`languages::EXTENSIONS`, and a `ServerSpec` beside the others if its server is a new one.

## The seam

The crate owns the servers and what is said to them. What is drawn, where the text came from,
and how much a set of servers is shared stay with the caller: a `Workspace` is a repo root and
an opaque key, and the caller decides whether that key is the repo, one window's work, or
anything else. The `PATH` is the caller's for the same reason, and so is debouncing
`did_change`, since a round trip per keystroke is a flood wherever this is reached over a
network.

The answers are plain `Serialize`/`Deserialize` types, so a client asking about a repo on
another machine can send the question and read the answer back over a wire of its own.

## License

MIT
