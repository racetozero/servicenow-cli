//! The start of the full-screen workspace: where it refuses to open.

mod support;

use support::Cli;

#[tokio::test]
async fn it_never_opens_in_agent_mode() {
    let run = Cli::empty().run(&["tui", "--agent"]).await;
    assert_eq!(run.code, 3);
    let envelope = run.envelope();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "invalid");
    let message = envelope["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("agent mode"), "{message}");
}

#[tokio::test]
async fn it_needs_a_terminal_and_says_what_to_use_instead() {
    let run = Cli::empty().run(&["tui"]).await;
    assert_eq!(run.code, 3);
    assert!(run.stdout.is_empty(), "{}", run.stdout);
    assert!(run.stderr.contains("without a terminal"), "{}", run.stderr);
    assert!(run.stderr.contains("table list"), "{}", run.stderr);
}

#[tokio::test]
async fn the_page_size_has_limits() {
    for size in ["4", "201", "many"] {
        let run = Cli::empty().run(&["tui", "--page-size", size]).await;
        assert_eq!(run.code, 3, "{size}");
        assert!(run.stderr.contains("page-size"), "{}", run.stderr);
    }
}
