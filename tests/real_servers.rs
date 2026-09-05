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

use moon_lsp::{LspPosition, LspRegistry, LspStatus, Workspace, languages, process::PositionEncoding};

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
