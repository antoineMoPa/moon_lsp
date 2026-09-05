# Changelog

## 0.1.0

First release. A language server client: `LanguageServer` runs one and carries the JSON-RPC over
its stdio, `LspRegistry` keeps one per workspace and language and answers the questions —
whether a file has a server behind it, what those servers are doing, the document
notifications, where a name is defined and what could be typed next — and `protocol` holds the
one place a position is converted between the bytes an editor counts and the units a server
agreed to.
