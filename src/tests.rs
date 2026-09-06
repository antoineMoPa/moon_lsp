//! What can be checked without a language server: the framing, the position arithmetic and
//! the language tables.
//!
//! All of it is arithmetic and tables, so it runs in the normal suite. The proof that the
//! whole path works needs a real server and lives in `tests/real_servers.rs`, `#[ignore]`d.

use std::time::Duration;

use serde_json::json;

use crate::{
    framing::{self, Frames},
    languages,
    payload::{LspCompletionKind, LspPosition, LspStatus},
    process::{
        PositionEncoding, Readiness, SETTLING, STARTING_CEILING, Working, follow_progress,
        refusal_from,
    },
    protocol,
    registry::{LspRegistry, status_without_a_server},
};

/// One `$/progress` notification, as a server puts it on the wire.
fn progress(token: &str, value: serde_json::Value) -> serde_json::Value {
    json!({ "jsonrpc": "2.0", "method": "$/progress", "params": { "token": token, "value": value } })
}

/// The reported bug, in the notifications rust-analyzer really sends on a large workspace:
/// it primes its cache, starts checking the project with `cargo check` before that has
/// finished, and then goes on checking - on every edit, for as long as the workspace is open.
///
/// The rule this replaced measured its quiet on every notification alike, so the checking
/// kept the server `Starting` and every go-to-definition came back "rust is still indexing
/// this project - try again in a moment" for the rest of the session.
#[test]
fn a_server_that_keeps_checking_the_project_after_it_has_started_becomes_and_stays_ready() {
    let mut readiness = Readiness::new();

    readiness.follow(&progress(
        "rustAnalyzer/cachePriming",
        json!({ "kind": "begin", "title": "Indexing" }),
    ));
    readiness.follow(&progress(
        "rust-analyzer/flycheck/0",
        json!({ "kind": "begin", "title": "cargo check" }),
    ));
    readiness.rewind(SETTLING);
    assert!(
        !readiness.is_ready(),
        "the cache is still being primed, which is the server starting up"
    );

    readiness.follow(&progress("rustAnalyzer/cachePriming", json!({ "kind": "end" })));
    readiness.rewind(SETTLING);
    assert!(
        readiness.is_ready(),
        "starting up has finished, and a check running in the background is not starting up"
    );

    // And now the checking goes on, the way it does while a person edits: a run every few
    // seconds, each one reporting as it goes.
    for run in 0..5 {
        let token = format!("rust-analyzer/flycheck/{run}");
        readiness.follow(&progress(
            &token,
            json!({ "kind": "begin", "title": "cargo check" }),
        ));
        readiness.follow(&progress(
            &token,
            json!({ "kind": "report", "message": "checking moon_lsp" }),
        ));
        readiness.follow(&progress(&token, json!({ "kind": "end" })));
        assert!(
            readiness.is_ready(),
            "run {run} of a background check must not put a started server back to starting"
        );
    }
}

/// The other way round, and the bug the quiet was added for: rust-analyzer announces the
/// pieces of starting up one after another - fetch, scan the roots, load the proc-macros,
/// prime the cache - and the outstanding work passes through empty in the gaps between them.
///
/// A request let through in one of those gaps comes back `content modified` or empty, which
/// reads as "no definition found" for a symbol that has one.
#[test]
fn a_server_announcing_its_startup_work_in_a_row_is_not_ready_in_the_gap_between_two_pieces() {
    let mut readiness = Readiness::new();
    let pieces = [
        ("rustAnalyzer/Fetching", "Fetching"),
        ("rustAnalyzer/Building CrateGraph", "Building CrateGraph"),
        ("rustAnalyzer/Roots Scanned", "Roots Scanned"),
        ("rustAnalyzer/cachePriming", "Indexing"),
    ];

    for (token, title) in pieces {
        readiness.follow(&progress(token, json!({ "kind": "begin", "title": title })));
        assert!(!readiness.is_ready(), "{title} has not finished");
        readiness.follow(&progress(token, json!({ "kind": "end" })));
        assert!(
            !readiness.is_ready(),
            "nothing is outstanding after {title}, but the next piece of starting up is a \
             moment away and a request sent now lands mid-startup"
        );
    }

    readiness.rewind(SETTLING);
    assert!(
        readiness.is_ready(),
        "the quiet after the last piece is what says starting up is over"
    );
}

/// Nothing the server says afterwards can take a pane that was answering questions and stop
/// it: readiness latches. A server that begins a piece of work this client cannot place - a
/// server not in [`crate::process`]'s table of background work, or one whose own re-indexing
/// looks like starting up - is a server that has already started.
#[test]
fn work_a_server_begins_after_it_has_started_never_puts_it_back_to_starting() {
    let mut readiness = Readiness::new();
    readiness.rewind(SETTLING);
    assert!(readiness.is_ready(), "a server that announced nothing at all");

    readiness.follow(&progress(
        "some/unfamiliar/work",
        json!({ "kind": "begin", "title": "Re-indexing" }),
    ));
    readiness.follow(&progress(
        "some/unfamiliar/work",
        json!({ "kind": "report", "message": "half way" }),
    ));

    assert!(
        readiness.is_ready(),
        "a started server that picked up more work is still a started server"
    );
}

/// Whatever else is true, a misread of readiness has to cost a request that comes back empty
/// rather than a session that never asks. A server whose starting-up work never ends is let
/// through once the ceiling has passed.
#[test]
fn a_server_whose_startup_work_never_ends_is_let_through_once_the_ceiling_has_passed() {
    let mut readiness = Readiness::new();
    readiness.follow(&progress(
        "rustAnalyzer/Roots Scanned",
        json!({ "kind": "begin", "title": "Roots Scanned" }),
    ));

    readiness.rewind(STARTING_CEILING - Duration::from_secs(1));
    assert!(
        !readiness.is_ready(),
        "the scan is still outstanding and there is still time for it"
    );

    readiness.rewind(Duration::from_secs(2));
    assert!(
        readiness.is_ready(),
        "past the ceiling the question is asked anyway rather than waited on forever"
    );
}

/// `content modified` is the one refusal worth asking twice about: the protocol defines it as
/// the document having changed under the request, so the question was never answered. Every
/// other refusal is the server's answer.
#[test]
fn a_request_the_document_moved_under_is_worth_asking_again_and_no_other_refusal_is() {
    assert!(
        refusal_from(&json!({ "code": -32801, "message": "content modified" }))
            .worth_asking_again()
    );
    assert!(
        !refusal_from(&json!({ "code": -32601, "message": "method not found" }))
            .worth_asking_again(),
        "a method the server does not have will not appear on a second ask"
    );
    assert!(
        !refusal_from(&json!({ "message": "no code at all" })).worth_asking_again(),
        "a refusal with no code is nothing this client can act on"
    );
}

#[test]
fn a_message_goes_out_with_its_length_in_bytes_ahead_of_it() {
    assert_eq!(
        String::from_utf8(framing::frame(r#"{"id":1}"#)).expect("expected a text frame"),
        "Content-Length: 8\r\n\r\n{\"id\":1}"
    );
}

#[test]
fn a_frame_length_counts_bytes_rather_than_characters() {
    let framed = String::from_utf8(framing::frame(r#"{"a":"é"}"#)).expect("expected a text frame");
    assert!(
        framed.starts_with("Content-Length: 10\r\n"),
        "expected the two bytes of é to count twice, got {framed}"
    );
}

#[test]
fn a_message_split_across_reads_is_read_out_once_all_of_it_has_arrived() {
    let framed = framing::frame(r#"{"id":7,"result":null}"#);
    let (front, back) = framed.split_at(12);

    let mut frames = Frames::default();
    frames.push(front);
    assert!(
        frames.next_message().is_none(),
        "half a message is not a message"
    );
    frames.push(back);
    assert_eq!(
        frames.next_message().expect("expected the whole message"),
        r#"{"id":7,"result":null}"#
    );
    assert!(
        frames.next_message().is_none(),
        "there was only one message"
    );
}

#[test]
fn two_messages_arriving_together_are_read_out_in_order() {
    let mut arrived = framing::frame(r#"{"id":1}"#);
    arrived.extend_from_slice(&framing::frame(r#"{"id":2}"#));

    let mut frames = Frames::default();
    frames.push(&arrived);
    assert_eq!(frames.next_message().as_deref(), Some(r#"{"id":1}"#));
    assert_eq!(frames.next_message().as_deref(), Some(r#"{"id":2}"#));
    assert_eq!(frames.next_message(), None);
}

#[test]
fn a_header_with_a_content_type_beside_the_length_is_still_read() {
    let mut frames = Frames::default();
    frames.push(b"Content-Type: application/vscode-jsonrpc\r\nContent-Length: 8\r\n\r\n{\"id\":1}");
    assert_eq!(frames.next_message().as_deref(), Some(r#"{"id":1}"#));
}

/// The one that matters. `héllo` is five characters, six bytes and five UTF-16 units, so a
/// column past the accent is a different number depending on what is counting - and a client
/// that hands a UTF-16 server its byte columns points at the wrong place on every line
/// holding anything but ASCII.
#[test]
fn a_column_past_an_accent_is_counted_in_the_units_the_server_agreed_to() {
    let line = "let héllo = 1;";
    // The byte column of the `=`: `let ` is 4, `héllo` is 6 bytes, the space is one.
    let byte_column = 11;
    assert_eq!(&line[byte_column..byte_column + 1], "=");

    assert_eq!(
        protocol::lsp_character(line, byte_column, PositionEncoding::Utf8),
        11,
        "a server counting bytes wants the column unchanged"
    );
    assert_eq!(
        protocol::lsp_character(line, byte_column, PositionEncoding::Utf16),
        10,
        "the two bytes of é are one UTF-16 unit"
    );
}

#[test]
fn a_column_landing_inside_a_character_belongs_to_the_start_of_it() {
    let line = "héllo";
    // Between the two bytes of é, which is not a place a character starts.
    assert_eq!(protocol::lsp_character(line, 2, PositionEncoding::Utf16), 1);
    assert_eq!(protocol::lsp_character(line, 2, PositionEncoding::Utf8), 1);
}

#[test]
fn a_column_past_the_end_of_a_line_is_the_end_of_it() {
    assert_eq!(
        protocol::lsp_character("ab", 99, PositionEncoding::Utf16),
        2
    );
}

#[test]
fn an_emoji_is_two_utf16_units_and_four_bytes() {
    let line = "x = \"🌙\";";
    let after_the_moon = 4 + 1 + "🌙".len();
    assert_eq!(
        protocol::lsp_character(line, after_the_moon, PositionEncoding::Utf16),
        7,
        "five ASCII characters and the two units of the moon"
    );
    assert_eq!(
        protocol::lsp_character(line, after_the_moon, PositionEncoding::Utf8),
        9
    );
}

#[test]
fn a_position_is_taken_against_the_line_it_falls_on() {
    let text = "fn main() {}\nlet héllo = 1;\n";
    let at = LspPosition {
        line: 1,
        column: 11,
    };
    assert_eq!(
        protocol::position_in(text, &at, PositionEncoding::Utf16)
            .expect("expected a position on the second line"),
        10
    );
}

#[test]
fn a_position_past_the_end_of_the_file_is_refused() {
    let at = LspPosition { line: 9, column: 0 };
    let error = protocol::position_in("one line\n", &at, PositionEncoding::Utf8)
        .expect_err("expected a line past the end to be refused");
    assert!(error.to_string().contains("past the end"), "{error}");
}

/// A `$/progress` as rust-analyzer really sends one: the token names the work, and what is
/// worth reading is inside `value`.
#[test]
fn a_progress_notification_says_what_the_server_is_doing_and_how_far_through_it_is() {
    let note = protocol::progress_note(&json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {
            "token": "rustAnalyzer/Indexing",
            "value": {
                "kind": "report",
                "message": "12/57 (serde)",
                "percentage": 21,
            },
        },
    }));

    assert_eq!(note.title, None, "only a begin carries a title");
    assert_eq!(note.message.as_deref(), Some("12/57 (serde)"));
    assert_eq!(note.percentage, Some(21));
}

/// The ordinary case, and the one the status bar has to read well: a server that says what
/// it is doing and nothing at all about how long it will take.
#[test]
fn a_piece_of_work_that_reports_no_percentage_still_says_what_it_is_doing() {
    let note = protocol::progress_note(&json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {
            "token": "rustAnalyzer/Fetching",
            "value": { "kind": "begin", "title": "Fetching", "cancellable": true },
        },
    }));

    assert_eq!(note.title.as_deref(), Some("Fetching"));
    assert_eq!(note.message, None);
    assert_eq!(note.percentage, None, "nothing said is not nought per cent");
}

/// The title only ever comes with the `begin`, so the reports that follow have to be read
/// against the work already in hand rather than on their own.
#[test]
fn a_report_keeps_the_title_its_begin_gave_it_and_an_end_leaves_the_server_doing_nothing() {
    let notification = |value: serde_json::Value| {
        json!({ "jsonrpc": "2.0", "method": "$/progress", "params": { "token": "t", "value": value } })
    };
    let mut working: Option<Working> = None;

    let begun = notification(json!({ "kind": "begin", "title": "Indexing", "percentage": 0 }));
    follow_progress(
        &mut working,
        protocol::progress_kind(&begun),
        protocol::progress_note(&begun),
    );
    assert_eq!(
        working,
        Some(Working {
            title: "Indexing".to_string(),
            detail: None,
            percentage: Some(0),
        })
    );

    let reported = notification(json!({ "kind": "report", "message": "12/57", "percentage": 21 }));
    follow_progress(
        &mut working,
        protocol::progress_kind(&reported),
        protocol::progress_note(&reported),
    );
    assert_eq!(
        working,
        Some(Working {
            title: "Indexing".to_string(),
            detail: Some("12/57".to_string()),
            percentage: Some(21),
        })
    );

    let ended = notification(json!({ "kind": "end", "message": "57/57" }));
    follow_progress(
        &mut working,
        protocol::progress_kind(&ended),
        protocol::progress_note(&ended),
    );
    assert_eq!(working, None, "work that has ended is not work");
}

/// A report with no begin behind it is a piece of work this client never saw start, and
/// making a nameless line out of it would be the window inventing what a server is doing.
#[test]
fn a_report_with_no_work_behind_it_is_left_alone() {
    let reported = json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": { "token": "t", "value": { "kind": "report", "message": "12/57" } },
    });
    let mut working: Option<Working> = None;

    follow_progress(
        &mut working,
        protocol::progress_kind(&reported),
        protocol::progress_note(&reported),
    );

    assert_eq!(working, None);
}

#[test]
fn a_rust_file_is_served_by_rust_analyzer() {
    let language = languages::for_file("src/main.rs").expect("expected rust to be in the table");
    assert_eq!(language.language_id, "rust");
    assert_eq!(language.server.command, "rust-analyzer");
    assert!(language.server.args.is_empty());
}

#[test]
fn typescript_and_its_react_flavour_are_two_languages_on_one_server() {
    let typescript = languages::for_file("app.ts").expect("expected .ts to be in the table");
    let react = languages::for_file("app.tsx").expect("expected .tsx to be in the table");

    assert_eq!(typescript.language_id, "typescript");
    assert_eq!(react.language_id, "typescriptreact");
    assert_eq!(typescript.server.command, "typescript-language-server");
    assert_eq!(typescript.server.args, &["--stdio"]);
    assert_eq!(
        typescript.server.name, react.server.name,
        "one project's .ts and .tsx are indexed by one server"
    );
}

#[test]
fn an_extension_nothing_serves_has_no_language_behind_it() {
    assert!(languages::for_file("README.md").is_none());
    assert!(languages::for_file("Makefile").is_none());
}

#[test]
fn a_language_whose_server_is_not_installed_reads_as_unavailable() {
    let python = languages::for_file("main.py").expect("expected python to be in the table");
    assert_eq!(
        status_without_a_server(Some(python), false),
        LspStatus::Unavailable,
        "a server that is not installed is no server at all"
    );
    assert_eq!(
        status_without_a_server(None, false),
        LspStatus::Unavailable,
        "and neither is an extension nothing serves"
    );
    assert_eq!(
        status_without_a_server(Some(python), true),
        LspStatus::Starting,
        "an installed server has at least started"
    );
}

/// How this case was found: a `rust-analyzer` on PATH that is really a rustup shim for a
/// component nobody added exits the moment it is spoken to. Being on PATH is not the same as
/// running, and a file behind a server like that must not read as forever starting.
#[test]
fn a_server_that_is_installed_and_will_not_start_is_given_up_on() {
    let registry = LspRegistry::new(String::new());
    let key = ("one window".to_string(), languages::RUST_ANALYZER.name);
    assert!(!registry.gave_up_on(&key));

    registry.give_up_on(&key);
    assert!(registry.gave_up_on(&key));
    assert_eq!(
        status_without_a_server(languages::for_file("main.rs"), false),
        LspStatus::Unavailable,
        "a server that would not start reads the same as one that is not there"
    );
}

#[test]
fn a_server_that_asked_for_utf8_and_was_answered_with_nothing_is_taken_to_count_utf16() {
    let silent = json!({ "capabilities": {} });
    assert_eq!(
        protocol::agreed_encoding(&silent).expect("expected a readable reply"),
        PositionEncoding::Utf16,
        "the protocol's default is what a server that agreed to nothing means"
    );

    let agreed = json!({ "capabilities": { "positionEncoding": "utf-8" } });
    assert_eq!(
        protocol::agreed_encoding(&agreed).expect("expected a readable reply"),
        PositionEncoding::Utf8
    );
}

#[test]
fn a_definition_is_read_as_a_repo_path_and_a_line_counted_from_one() {
    let repo_root = std::path::Path::new("/tmp/a repo");
    let answer = json!({
        "uri": "file:///tmp/a%20repo/src/main.rs",
        "range": {
            "start": { "line": 41, "character": 3 },
            "end": { "line": 41, "character": 8 },
        },
    });

    let locations =
        protocol::locations_from(answer, repo_root).expect("expected a readable definition");
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].file_path, "src/main.rs");
    assert_eq!(
        locations[0].line_number, 42,
        "the protocol counts lines from zero and the panes from one"
    );
}

#[test]
fn a_definition_outside_the_repo_keeps_the_path_that_names_it() {
    let answer = json!([{
        "uri": "file:///home/dev/.cargo/registry/serde/lib.rs",
        "range": {
            "start": { "line": 0, "character": 0 },
            "end": { "line": 0, "character": 1 },
        },
    }]);
    let locations = protocol::locations_from(answer, std::path::Path::new("/home/dev/repo"))
        .expect("expected a readable definition");
    assert_eq!(
        locations[0].file_path,
        "/home/dev/.cargo/registry/serde/lib.rs"
    );
}

/// What a caller decides with: whether an item is a function is the one thing that says
/// whether taking it should write the parentheses of a call, and the kind is the only place
/// the answer comes from. An item with no kind stays an item with no kind - a server that did
/// not say is not a server that said "variable".
#[test]
fn a_completion_keeps_the_kind_the_server_gave_it_and_none_where_it_gave_none() {
    let answer = json!([
        { "label": "greet", "kind": 3, "detail": "fn(&str) -> String" },
        { "label": "greeting", "kind": 6 },
        { "label": "Greeter", "kind": 22, "insertText": "Greeter" },
        { "label": "grep" },
        // The kinds are a bare integer on the wire, so a server may send one newer than the
        // list this client was written against. Carried as nothing rather than guessed at.
        { "label": "growl", "kind": 99 },
    ]);

    let completions = protocol::completions_from(answer).expect("expected a readable list");
    let kinds: Vec<Option<LspCompletionKind>> =
        completions.iter().map(|item| item.kind).collect();
    assert_eq!(
        kinds,
        [
            Some(LspCompletionKind::Function),
            Some(LspCompletionKind::Variable),
            Some(LspCompletionKind::Struct),
            None,
            None,
        ]
    );
    // And the three fields that were already carried are still carried beside it.
    assert_eq!(completions[0].label, "greet");
    assert_eq!(completions[0].detail.as_deref(), Some("fn(&str) -> String"));
    assert_eq!(completions[0].insert, "greet");
}

#[test]
fn a_file_uri_escapes_what_a_path_may_hold_and_reads_back_the_same() {
    let path = std::path::Path::new("/tmp/a repo/src/café.rs");
    let uri = protocol::file_uri(path);
    assert_eq!(uri, "file:///tmp/a%20repo/src/caf%C3%A9.rs");
    assert_eq!(
        protocol::path_from_file_uri(&uri).expect("expected the path back"),
        path
    );
}

#[test]
fn a_uri_that_names_nothing_on_disk_is_no_place_to_open() {
    assert!(protocol::path_from_file_uri("untitled:Untitled-1").is_none());
    assert!(protocol::path_from_file_uri("jdt://contents/rt.jar").is_none());
}

#[test]
fn a_server_asking_for_its_configuration_is_answered_one_entry_per_item() {
    let asked = json!({
        "id": 3,
        "method": "workspace/configuration",
        "params": { "items": [{ "section": "typescript" }, { "section": "javascript" }] },
    });
    let reply = protocol::reply_to_server_request("workspace/configuration", &asked);
    assert_eq!(
        reply.as_array().map(Vec::len),
        Some(2),
        "a server handed the wrong number of settings stops asking"
    );
    assert!(protocol::reply_to_server_request("client/registerCapability", &asked).is_null());
}

/// What a server is told it is talking to when nobody said: this crate, not whatever
/// application happens to vendor it. The name goes into every server's log and a few servers
/// change what they offer by it, so a library that announced one host application's name would
/// be lying to every other caller - and to its own example.
#[test]
fn a_server_is_told_it_is_talking_to_this_crate_when_the_caller_says_nothing() {
    let params = protocol::initialize_params(
        std::path::Path::new("/tmp/a repo"),
        &crate::ClientIdentity::default(),
    );
    assert_eq!(
        params.pointer("/clientInfo/name").and_then(|it| it.as_str()),
        Some("moon_lsp")
    );
    assert_eq!(
        params.pointer("/clientInfo/version").and_then(|it| it.as_str()),
        Some(env!("CARGO_PKG_VERSION")),
        "a version is worth sending: it is what tells one bug report from another"
    );
}

/// A caller that says who it is has that go out instead, version and all. A caller with no
/// version to give sends no version rather than an empty one.
#[test]
fn a_caller_that_says_who_it_is_has_its_own_name_and_version_sent() {
    let params = protocol::initialize_params(
        std::path::Path::new("/tmp/a repo"),
        &crate::ClientIdentity::new("moonreview", "0.20.0"),
    );
    assert_eq!(
        params.pointer("/clientInfo").expect("expected a clientInfo"),
        &json!({ "name": "moonreview", "version": "0.20.0" })
    );

    let versionless = protocol::initialize_params(
        std::path::Path::new("/tmp/a repo"),
        &crate::ClientIdentity {
            name: "a script".to_string(),
            version: None,
        },
    );
    assert_eq!(
        versionless.pointer("/clientInfo").expect("expected a clientInfo"),
        &json!({ "name": "a script" })
    );
}
