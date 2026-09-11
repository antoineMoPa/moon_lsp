# Changelog

## Unreleased

- Code actions and signature help. `LspRegistry::code_actions` asks what the server offers at a
  place, with the diagnostics published there, and hands back only what can be carried out -
  literal actions whose edits fit, in the editor's bytes; commands and disabled actions are left
  out. `LspRegistry::signature_help` is the signature of the call around a place, with the
  parameter being typed as bytes of its label.

- Hover and diagnostics. `LspRegistry::hover` is what the server says about a name, as markdown
  whichever shape it answered in; `LspRegistry::diagnostics` is what it last published about a
  file, kept as it arrives and turned into the editor's bytes when asked for. `did_save` tells a
  server a file was written, which is what rust-analyzer runs `cargo check` on.

- `LspRegistry::places` asks for where a name is defined, where its type is, where it is
  implemented, or everywhere it is used - `LspPlaces` says which - and every `LspLocation` now
  carries what its line reads. `LspRegistry::format` formats a whole file, and refuses one whose
  server never said it formats rather than answering "already formatted".

- Rename. `LspRegistry::prepare_rename` says what the name at a place is called, as the server
  would rename it, and `LspRegistry::rename` hands back everything calling it something else
  changes, one `LspFileEdit` per file with every place already in the editor's bytes - counted
  against the copy the server was sent for an open file and against the file on disk for one
  that is not. Nothing is written: which files are open in a buffer is the caller's to know. An
  answer that would edit a file outside the repo, or create, rename or delete one, is refused
  whole. `edits` puts the edits into a text, and refuses any that do not fit it.

## 0.1.0

First release. A language server client: `LanguageServer` runs one and carries the JSON-RPC over
its stdio, `LspRegistry` keeps one per workspace and language and answers the questions —
whether a file has a server behind it, what those servers are doing, the document
notifications, where a name is defined and what could be typed next — and `protocol` holds the
one place a position is converted between the bytes an editor counts and the units a server
agreed to.
