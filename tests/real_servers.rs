//! The proof that the whole path works: a real server, a real project, and the place a
//! symbol is actually defined.
//!
//! Both are `#[ignore]`d, because each starts a language server and waits for it to index -
//! seconds to minutes, and only where that server is installed:
//!
//! ```sh
//! cargo test -- --ignored --test-threads=1
//! ```

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use moon_lsp::{
    LspPosition, LspRegistry, LspStatus, Workspace, languages, process::PositionEncoding,
};

/// What the servers are held under. Only one workspace is open at a time here, so the key
/// says what it is rather than distinguishing anything.
const WORKSPACE: &str = "the fixture";

/// The only proof the whole path works: a real server, a real project, and the place a
/// symbol is actually defined.
///
/// `#[ignore]`d because it starts `typescript-language-server` and waits for it to be
/// ready, which is seconds rather than milliseconds and depends on what is installed:
/// `cargo test --test real_servers -- --ignored a_real_language_server_says_where_a_symbol_is_defined`.
#[test]
#[ignore]
fn a_real_language_server_says_where_a_symbol_is_defined() {
    let root = fixture_root("typescript");
    let source = "export function greet(name: string): string {\n    return `hi ${name}`;\n}\n\nconst message = greet(\"world\");\nconsole.log(message);\n";
    std::fs::write(root.join("main.ts"), source).expect("failed to write the fixture file");

    let servers = registry();
    let repo = Workspace {
        key: WORKSPACE,
        root: &root,
    };
    servers
        .did_open(&repo, "main.ts", source)
        .expect("failed to open the document");
    wait_until_ready(&servers, "main.ts", Duration::from_secs(60));

    // The `greet` of `const message = greet("world")`, on the fifth line.
    let at = LspPosition {
        line: 4,
        column: 18,
    };
    let locations = servers
        .definition(&repo, "main.ts", at)
        .expect("failed to ask where greet is defined");
    println!(
        "definition: {:?}",
        locations
            .iter()
            .map(|location| (&location.file_path, location.line_number))
            .collect::<Vec<_>>()
    );
    assert_eq!(locations.len(), 1, "expected one definition");
    assert_eq!(locations[0].file_path, "main.ts");
    assert_eq!(
        locations[0].line_number, 1,
        "greet is defined on the first line"
    );

    // What the server itself says should open a completion list, kept out of its
    // `initialize` reply rather than guessed at here. This one names `.`, `"`, `'`, `/`, `@`
    // and `<`; the assertion is on the one character every language agrees on, so a version
    // that adds or drops one of the others does not fail a test about something else.
    let triggers = servers.trigger_characters(WORKSPACE, "main.ts");
    println!("triggers: {triggers:?}");
    assert!(
        triggers.contains(&'.'),
        "expected a server that completes members after a dot to say so"
    );

    let completions = servers
        .completion(&repo, "main.ts", at)
        .expect("failed to ask what could be typed");
    println!("completions: {}", completions.len());
    assert!(
        completions
            .iter()
            .any(|completion| completion.label == "greet"),
        "expected greet among what can be typed here"
    );

    servers
        .did_close(&repo, "main.ts")
        .expect("failed to close the document");
    let _ = std::fs::remove_dir_all(&root);
}

/// The reported bug: go-to-definition on a real Rust project answered "rust is still
/// indexing this project - try again in a moment" and went on saying it for the whole
/// session.
///
/// What no fixture catches is that rust-analyzer never falls silent on a project somebody is
/// working in: it checks the workspace with `cargo check` when the project loads and again
/// after every edit, and the rule this test guards used to read that checking as the server
/// still starting up. So the shape of the test is the shape of the use: open a real file in a
/// real cargo workspace, wait for `Ready`, then keep editing and keep watching, and the
/// status must not drop back to `Starting` - and the server must still answer.
///
/// The workspace is the outermost cargo project this checkout sits in - the whole repo where
/// this crate is vendored into one, and this crate alone where it is checked out on its own.
/// The bigger the workspace, the more checking there is to mistake for starting up.
///
/// `#[ignore]`d like the others: it starts rust-analyzer, waits out the indexing of a real
/// project, and then spends half a minute watching it.
#[test]
#[ignore]
fn a_real_rust_server_stays_ready_while_it_goes_on_checking_the_project() {
    let root = outermost_cargo_project();
    println!("workspace: {}", root.display());
    let file_path = "crates/moon_lsp/src/framing.rs";
    let file_path = if root.join(file_path).is_file() {
        file_path
    } else {
        "src/framing.rs"
    };
    let text = std::fs::read_to_string(root.join(file_path)).expect("failed to read the file");

    let servers = registry();
    let repo = Workspace {
        key: WORKSPACE,
        root: &root,
    };
    servers
        .did_open(&repo, file_path, &text)
        .expect("failed to open the document");
    let started_at = Instant::now();
    wait_until_ready(&servers, file_path, Duration::from_secs(300));
    println!("ready after {:?}", started_at.elapsed());

    // Now the part that was broken. Each edit sets rust-analyzer checking the project again,
    // and every one of those checks used to read as the server starting over.
    for edit in 0..4 {
        let edited = format!("{text}\n// an edit, the {edit}th\n");
        servers
            .did_change(&repo, file_path, &edited)
            .expect("failed to send the edit");
        let watching_until = Instant::now() + Duration::from_secs(6);
        while Instant::now() < watching_until {
            let status = servers.status(WORKSPACE, file_path);
            assert_eq!(
                status,
                LspStatus::Ready,
                "edit {edit}: a server that has started must not go back to starting because \
                 it is checking the project - {:?}",
                working_titles(&servers)
            );
            std::thread::sleep(Duration::from_millis(250));
        }
        println!(
            "edit {edit}: still ready, working: {:?}",
            working_titles(&servers)
        );
    }

    // And it is not only saying it is ready: the question it was refusing to be asked is
    // asked here, on the text as it now stands.
    let edited = format!("{text}\n// an edit, the last\n");
    servers
        .did_change(&repo, file_path, &edited)
        .expect("failed to send the last edit");
    let call = "content_length(&header)";
    let line = edited
        .lines()
        .position(|line| line.contains(&format!("= {call}")))
        .expect("expected the call to content_length");
    let column = edited
        .lines()
        .nth(line)
        .expect("expected that line")
        .find(call)
        .expect("expected the call on that line")
        + 1;
    let locations = servers
        .definition(&repo, file_path, LspPosition { line, column })
        .expect("failed to ask where content_length is defined");
    println!(
        "definition: {:?}",
        locations
            .iter()
            .map(|location| (&location.file_path, location.line_number))
            .collect::<Vec<_>>()
    );
    let defined_at = edited
        .lines()
        .position(|line| line.starts_with("fn content_length("))
        .expect("expected content_length to be defined in this file")
        + 1;
    assert_eq!(locations.len(), 1, "expected one definition");
    assert_eq!(locations[0].line_number, defined_at);

    servers
        .did_close(&repo, file_path)
        .expect("failed to close the document");
}

/// What every server running for this workspace says it is doing, as titles - which is what
/// a status bar would be showing while the test watches.
fn working_titles(servers: &LspRegistry) -> Vec<String> {
    servers
        .working(WORKSPACE)
        .iter()
        .map(|work| format!("{}: {}", work.server, work.title))
        .collect()
}

/// The outermost cargo project this crate sits in: the repo where it is vendored into one,
/// and the crate itself where it is checked out on its own. What the test above wants is the
/// largest real workspace at hand, since the mistake it guards against grows with the amount
/// of work the server does.
fn outermost_cargo_project() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.ancestors()
        .filter(|directory| directory.join("Cargo.toml").is_file())
        .last()
        .expect("this crate has a Cargo.toml")
        .to_path_buf()
}

/// A registry that looks for its servers on this process's own `PATH`, which is the one
/// `cargo test` was started with and so the one the servers were installed on.
fn registry() -> LspRegistry {
    LspRegistry::new(std::env::var("PATH").expect("expected a PATH to look for servers on"))
}

/// Somewhere throwaway to put a fixture repo, one directory per test so two of these can be
/// run in the same process without treading on each other.
fn fixture_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("moon-lsp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("failed to create the fixture directory");
    root
}

/// Wait for a server to finish starting, printing what it says on the way so the
/// Starting -> Ready transition is visible in the test's output.
fn wait_until_ready(servers: &LspRegistry, file_path: &str, deadline: Duration) {
    let giving_up_at = Instant::now() + deadline;
    while Instant::now() < giving_up_at {
        let status = servers.status(WORKSPACE, file_path);
        println!("status: {status:?}");
        assert_ne!(
            status,
            LspStatus::Unavailable,
            "the server for {file_path} has to be installed to run this test"
        );
        if status == LspStatus::Ready {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("the server was still starting after {deadline:?}");
}

/// The other half of the proof: a server that agreed to count columns in bytes, and a
/// position with several non-ASCII characters to the left of the name being asked about.
///
/// rust-analyzer answers `initialize` with `positionEncoding: "utf-8"`, which is the branch
/// the typescript test cannot reach - that server ignores the request and is converted for.
/// And the five `ñ` before the call put its byte column five ahead of its UTF-16 column, so
/// a conversion applied the wrong way round lands on a different token and the definition
/// comes back as something else or as nothing, rather than passing by luck.
///
/// `#[ignore]`d, and slower than the typescript one: rust-analyzer reads the manifest and
/// indexes the crate before it is ready. Run it with
/// `cargo test --test real_servers -- --ignored --nocapture a_utf8_server`.
#[test]
#[ignore]
fn a_utf8_server_resolves_a_position_with_accents_to_the_left_of_it() {
    let root = fixture_root("rust");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"lsp-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("failed to write the fixture manifest");
    std::fs::create_dir_all(root.join("src")).expect("failed to create the fixture source folder");

    let source = "pub fn café_size() -> u32 { 3 }\npub fn total() -> u32 { let ñññññ = 0; café_size() + ñññññ }\n";
    std::fs::write(root.join("src/lib.rs"), source).expect("failed to write the fixture file");

    let servers = registry();
    let repo = Workspace {
        key: WORKSPACE,
        root: &root,
    };
    servers
        .did_open(&repo, "src/lib.rs", source)
        .expect("failed to open the document");

    // A minute is not enough on a cold cargo registry; this waits for the indexing rather
    // than for the initialize reply, which comes back long before the answers are any good.
    wait_until_ready(&servers, "src/lib.rs", Duration::from_secs(300));

    let encoding = servers
        .agreed_encoding(WORKSPACE, languages::RUST_ANALYZER.name)
        .expect("expected a running rust server");
    println!("agreed encoding: {encoding:?}");
    assert_eq!(
        encoding,
        PositionEncoding::Utf8,
        "rust-analyzer agrees to count columns in bytes, which is the branch this test is for"
    );

    // The call on the second line. The column is worked out rather than written down: the
    // point of the test is that the two counts differ, so a number in the source would say
    // nothing about which of them it is.
    let call_site = source.lines().nth(1).expect("expected a second line");
    let byte_column = call_site
        .find("café_size() + ")
        .expect("expected the call on the second line")
        + 1;
    let utf16_column = call_site[..byte_column].encode_utf16().count();
    assert_eq!(
        byte_column - utf16_column,
        5,
        "the five ñ to the left take two bytes and one UTF-16 unit each, so the two counts \
         are five apart at the call - which is what makes getting this backwards visible"
    );
    println!("byte column {byte_column}, utf-16 column {utf16_column}");

    let locations = servers
        .definition(
            &repo,
            "src/lib.rs",
            LspPosition {
                line: 1,
                column: byte_column,
            },
        )
        .expect("failed to ask where café_size is defined");
    println!(
        "definition: {:?}",
        locations
            .iter()
            .map(|location| (&location.file_path, location.line_number))
            .collect::<Vec<_>>()
    );

    assert_eq!(locations.len(), 1, "expected one definition");
    assert_eq!(locations[0].file_path, "src/lib.rs");
    assert_eq!(
        locations[0].line_number, 1,
        "café_size is defined on the first line"
    );

    servers
        .did_close(&repo, "src/lib.rs")
        .expect("failed to close the document");
    let _ = std::fs::remove_dir_all(&root);
}

/// A rename across two files of a real crate: the name asked about first, then everywhere it
/// is used, with every place already in bytes so the edits go straight into the texts.
///
/// `#[ignore]`d like the rest of this file. Run it with
/// `cargo test --test real_servers -- --ignored --nocapture a_real_rust_server_renames`.
#[test]
#[ignore]
fn a_real_rust_server_renames_a_name_across_the_files_that_use_it() {
    let root = fixture_root("rust-rename");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"lsp-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("failed to write the fixture manifest");
    std::fs::create_dir_all(root.join("src")).expect("failed to create the fixture source folder");
    let lib = "pub fn café() -> u32 {\n    1\n}\n\nmod other;\n";
    let other = "pub fn twice() -> u32 {\n    let ñ = 2; crate::café() * ñ\n}\n";
    std::fs::write(root.join("src/lib.rs"), lib).expect("failed to write lib.rs");
    std::fs::write(root.join("src/other.rs"), other).expect("failed to write other.rs");

    let servers = registry();
    let repo = Workspace {
        key: WORKSPACE,
        root: &root,
    };
    servers
        .did_open(&repo, "src/lib.rs", lib)
        .expect("failed to open the document");
    wait_until_ready(&servers, "src/lib.rs", Duration::from_secs(300));

    let at = LspPosition { line: 0, column: 8 };
    assert_eq!(
        servers
            .prepare_rename(&repo, "src/lib.rs", at)
            .expect("failed to ask what is at the caret"),
        Some("café".to_string())
    );

    let files = servers
        .rename(&repo, "src/lib.rs", at, "tea")
        .expect("failed to rename");
    println!(
        "rename: {:?}",
        files
            .iter()
            .map(|file| (&file.file_path, file.edits.len()))
            .collect::<Vec<_>>()
    );
    let edited = |file_path: &str, text: &str| {
        let file = files
            .iter()
            .find(|file| file.file_path == file_path)
            .unwrap_or_else(|| panic!("expected {file_path} to be edited"));
        moon_lsp::edits::apply(text, &file.edits).expect("expected the edits to fit")
    };
    assert_eq!(edited("src/lib.rs", lib), lib.replace("café", "tea"));
    // Not open in the server, so its places were counted against the file on disk - with an
    // `ñ` ahead of the call to prove the columns came back as bytes.
    assert_eq!(edited("src/other.rs", other), other.replace("café", "tea"));

    servers
        .did_close(&repo, "src/lib.rs")
        .expect("failed to close the document");
    let _ = std::fs::remove_dir_all(&root);
}

/// rustfmt, through rust-analyzer: a badly laid-out file comes back as rustfmt lays it out.
///
/// `#[ignore]`d like the rest of this file. Run it with
/// `cargo test --test real_servers -- --ignored --nocapture a_real_rust_server_formats`.
#[test]
#[ignore]
fn a_real_rust_server_formats_a_file() {
    let root = fixture_root("rust-format");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"lsp-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("failed to write the fixture manifest");
    std::fs::create_dir_all(root.join("src")).expect("failed to create the fixture source folder");
    let lib = "pub fn   café( )->u32{1}\n";
    std::fs::write(root.join("src/lib.rs"), lib).expect("failed to write lib.rs");

    let servers = registry();
    let repo = Workspace {
        key: WORKSPACE,
        root: &root,
    };
    servers
        .did_open(&repo, "src/lib.rs", lib)
        .expect("failed to open the document");
    wait_until_ready(&servers, "src/lib.rs", Duration::from_secs(300));

    let edits = servers
        .format(
            &repo,
            "src/lib.rs",
            moon_lsp::LspFormatting {
                tab_size: 4,
                insert_spaces: true,
            },
        )
        .expect("failed to format");
    assert_eq!(
        moon_lsp::edits::apply(lib, &edits).expect("expected the edits to fit"),
        "pub fn café() -> u32 {\n    1\n}\n"
    );

    servers
        .did_close(&repo, "src/lib.rs")
        .expect("failed to close the document");
    let _ = std::fs::remove_dir_all(&root);
}
