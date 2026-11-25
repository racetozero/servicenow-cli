use super::*;
use crate::app::tests::{PLAIN, app, keys, started, wait};
use crate::msg::Fault;
use crate::theme::Caps;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

fn buffer(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    app.update(crate::msg::Msg::Resize(width, height));
    let mut places = None;
    terminal
        .draw(|f| places = Some(draw(f, app)))
        .expect("draw");
    let places = places.expect("places");
    let screen = Rect::new(0, 0, width, height);
    for (area, hit) in &places.hits {
        assert_eq!(
            area.intersection(screen),
            *area,
            "{hit:?} reaches outside {width}x{height}"
        );
    }
    app.update(crate::msg::Msg::Drawn(places));
    terminal.backend().buffer().clone()
}

fn screen(app: &mut App, width: u16, height: u16) -> String {
    let buffer = buffer(app, width, height);
    buffer
        .content
        .chunks(width as usize)
        .map(|row| {
            // A wide character leaves an empty cell behind it.
            let mut line = String::new();
            let mut skip = 0;
            for cell in row {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                skip = text::width(cell.symbol()).saturating_sub(1);
                line.push_str(cell.symbol());
            }
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_shows(screen: &str, words: &[&str]) {
    for word in words {
        assert!(screen.contains(word), "`{word}` is missing in\n{screen}");
    }
}

#[test]
fn the_first_screen_at_every_size() {
    let shown = screen(&mut started(80, 24), 80, 24);
    assert_shows(
        &shown,
        &[
            "sn",
            "acme.service-now.com",
            "Assigned to me",
            "INC0000000",
            "▲ 1 Critical",
            "● In Progress",
            "Email down in 東京",
            "? keys",
            "Tab sections",
        ],
    );
    assert!(!shown.contains("SERVICE DESK"), "no rail in 80 columns");
    let shown = screen(&mut started(120, 40), 120, 40);
    assert_shows(
        &shown,
        &[
            "MY WORK",
            "SERVICE DESK",
            "BUILD",
            "PINNED",
            "Catalog tasks",
            "1.2k",
            "INC0000000",
            "1h",
            "Ctrl-K find anything",
        ],
    );
    let shown = screen(&mut started(200, 50), 200, 50);
    assert_shows(
        &shown,
        &[
            "MY WORK",
            "Assigned to me",
            "Details",
            "History",
            "Short description",
            "PEOPLE",
            "Assignment group",
            "[REDACTED]",
        ],
    );
    assert!(!shown.contains("do-not-show"));
}

#[test]
fn a_record_stands_beside_under_or_over_the_list() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    let shown = screen(&mut app, 120, 40);
    assert_shows(
        &shown,
        &[
            "MY WORK",
            "INC0000007",
            "Details   Activity",
            "Ada Lovelace",
        ],
    );
    let shown = screen(&mut app, 100, 40);
    assert_shows(&shown, &["INC0000007", "Details", "Network"]);
    let shown = screen(&mut app, 80, 24);
    assert_shows(
        &shown,
        &["INC0000000", "Details", "Activity", "History", "SUMMARY"],
    );
    assert!(!shown.contains("INC0000007"), "one pane at 80x24");
}

#[test]
fn every_tab_shows_its_content() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    for (key, words) in [
        (
            "1",
            &["first line", "second line with[31m an escape", "late"][..2],
        ),
        ("2", &["work note", "It is the DPD timer.", "1h ago"]),
        (
            "3",
            &[
                "APPROVALS",
                "CHILD INCIDENTS",
                "INC0000003",
                "None: child tasks, affected cis",
            ],
        ),
        ("4", &["gateway.log", "47.1 KB"]),
        ("5", &["Priority 1 resolution", "BREACHED", "141%"]),
        ("6", &["State", "New → In Progress", "bob"]),
    ] {
        keys(&mut app, &[key]);
        let shown = screen(&mut app, 120, 40);
        assert_shows(&shown, words);
    }
}

#[test]
fn edits_and_the_review_show_old_and_new() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter", "Down", "Enter", "!", "Enter", "C-s"]);
    let shown = screen(&mut app, 120, 40);
    assert_shows(
        &shown,
        &[
            "Change INC0000000",
            "- Email down in 東京",
            "+ Email down in 東京!",
            "Nothing is sent before you press Enter.",
            " Save ",
        ],
    );
    keys(&mut app, &["Esc"]);
    let shown = screen(&mut app, 160, 40);
    assert_shows(&shown, &["✎ 1 change", "was Email down in 東京"]);
}

#[test]
fn waiting_empty_failed_and_signed_out_say_what_to_do() {
    let mut waiting = app(120, 40);
    waiting.start();
    assert_shows(
        &screen(&mut waiting, 120, 40),
        &["signing in", "Short description"],
    );
    let mut empty = started(120, 40);
    keys(&mut empty, &["gr"]);
    assert_shows(
        &screen(&mut empty, 120, 40),
        &["You have no open request. Press g o"],
    );
    let mut failed = started(120, 40);
    keys(&mut failed, &["gf"]);
    let shown = screen(&mut failed, 120, 40);
    assert_shows(
        &shown,
        &["no access", "Enter tries again", "Enter try again"],
    );
    let mut out = app(80, 24);
    out.connection.signed_out = Some("The session ended.".into());
    assert_shows(
        &screen(&mut out, 80, 24),
        &[
            "Sign in to acme.service-now.com",
            "The session ended.",
            "Enter  sign in",
            "q quit",
        ],
    );
    let mut read_only = started(120, 40);
    read_only.connection.read_only = true;
    assert_shows(&screen(&mut read_only, 120, 40), &["READ-ONLY"]);
}

/// Every state of the workspace that looks different.
fn states() -> Vec<(&'static str, App)> {
    let mut out: Vec<(&'static str, App)> = Vec::new();
    let mut with = |name, keys_: &[&str]| {
        let mut app = started(120, 40);
        keys(&mut app, keys_);
        out.push((name, app));
    };
    with("list", &[]);
    with("search", &["/", "printer"]);
    with("no match", &["/", "zzzz"]);
    with("selected", &["Space", "Space"]);
    with("filtered", &["F", "Enter", "f"]);
    with("rail", &["BackTab"]);
    with("details", &["Enter"]);
    with("inline", &["Enter", "Down", "Enter", "a long value"]);
    with("long text", &["Enter", "Down", "Down", "Enter"]);
    with("choice", &["Enter", "Down", "Down", "Down", "Enter"]);
    with("activity", &["Enter", "2"]);
    with("related", &["Enter", "3"]);
    with("files", &["Enter", "4"]);
    with("slas", &["Enter", "5"]);
    with("history", &["Enter", "6"]);
    with("new", &["gi", "n"]);
    with("help", &["?"]);
    with("goto", &["g"]);
    with("palette", &["C-k"]);
    with("palette typed", &["C-k", "incident"]);
    with("tables", &["t", "u_x"]);
    with("menu", &["a"]);
    with("profiles", &["P"]);
    with("sort", &["s"]);
    with("query", &["F"]);
    with("filter", &["gi", "f"]);
    with("filter field", &["gi", "f", "n"]);
    with("filter value", &["gi", "f", "n", "short", "Enter", "vpn"]);
    with("comment", &["c", "hello"]);
    with("assign", &["i"]);
    with("assign picker", &["i", "Enter"]);
    with("resolve", &["x"]);
    with("review", &["w", "some words", "C-s"]);
    with("approvals", &["ga", "Space", "Space", "A", "C-s"]);
    with("confirm", &["Enter", "Down", "Enter", "x", "Enter", "q"]);
    with("failed", &["gf"]);
    with("empty", &["gr"]);
    let mut toasts = started(120, 40);
    for kind in [ToastKind::Info, ToastKind::Good, ToastKind::Error] {
        toasts.toast(kind, format!("{kind:?} {}", "words ".repeat(60)));
    }
    out.push(("toasts", toasts));
    let mut read = started(120, 40);
    read.overlays.push(Overlay::Read {
        title: "Text".into(),
        text: "words ".repeat(900),
        scroll: 99_999,
    });
    out.push(("reader", read));
    let mut conflict = started(120, 40);
    keys(&mut conflict, &["m"]);
    if conflict.overlays.is_empty() {
        keys(&mut conflict, &["Down", "m"]);
    }
    if let Some(Overlay::Review(review)) = conflict.overlays.last_mut() {
        review.state = ReviewState::Conflict("grace".into(), "2m ago".into());
        review.lines[0].moved = true;
    }
    out.push(("conflict", conflict));
    let mut broken = started(120, 40);
    keys(&mut broken, &["Enter"]);
    if let Some(record) = &mut broken.record {
        record.full = Load::Failed(Fault {
            code: "api",
            message: "x".repeat(400),
        });
    }
    out.push(("failed record", broken));
    let mut waiting = app(120, 40);
    waiting.start();
    out.push(("waiting", waiting));
    let mut signed_out = app(120, 40);
    signed_out.connection.signed_out = Some("The session ended. ".repeat(20));
    out.push(("signed out", signed_out));
    out
}

#[test]
fn nothing_breaks_at_any_size() {
    const WIDTHS: &[u16] = &[
        1, 2, 3, 7, 12, 20, 29, 30, 31, 38, 47, 60, 79, 80, 89, 90, 109, 110, 150, 260,
    ];
    const HEIGHTS: &[u16] = &[1, 2, 4, 7, 8, 9, 11, 14, 17, 18, 24, 32, 60];
    std::thread::scope(|scope| {
        for (name, mut app) in states() {
            scope.spawn(move || {
                for (width, height) in WIDTHS
                    .iter()
                    .flat_map(|w| HEIGHTS.iter().map(move |h| (*w, *h)))
                {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        buffer(&mut app, width, height);
                    }));
                    assert!(result.is_ok(), "{name} at {width}x{height}");
                }
            });
        }
    });
}

#[test]
fn every_state_is_usable_at_80_by_24() {
    for (name, mut app) in states() {
        let shown = screen(&mut app, 80, 24);
        let footer = shown.lines().last().unwrap_or_default();
        assert!(
            footer.trim().len() > 3,
            "{name} has no hint in the footer:\n{shown}"
        );
        // The thing to look at is on screen.
        let word = match name {
            "review" => "+ some words",
            "approvals" => "+ Approved",
            "resolve" => "Resolution notes",
            "assign" => "Person",
            "palette typed" => "Incidents",
            "help" => "Open the record",
            "goto" => "Flows",
            "confirm" => "lose them",
            "conflict" => "Nothing was sent",
            "filter value" => "Short description",
            _ => continue,
        };
        assert_shows(&shown, &[word]);
    }
}

#[test]
fn without_unicode_only_ascii_is_drawn() {
    for (name, mut app) in states() {
        app.theme = Theme::new(Caps {
            unicode: false,
            ..PLAIN
        });
        for (width, height) in [(80, 24), (160, 50)] {
            for cell in &buffer(&mut app, width, height).content {
                assert!(cell.symbol().is_ascii(), "{name}: {:?}", cell.symbol());
            }
        }
    }
}

#[test]
fn without_color_nothing_is_colored() {
    for (name, mut app) in states() {
        app.theme = Theme::new(Caps {
            color: false,
            ..PLAIN
        });
        for cell in &buffer(&mut app, 120, 40).content {
            assert!(
                cell.fg == Color::Reset && cell.bg == Color::Reset,
                "{name}: {cell:?}"
            );
        }
    }
}

#[test]
fn no_text_of_a_record_reaches_the_terminal_raw() {
    let mut app = started(160, 50);
    for key in ["1", "2", "3"] {
        keys(&mut app, &["Enter", key]);
        wait(&mut app);
        for cell in &buffer(&mut app, 160, 50).content {
            assert!(
                !cell.symbol().chars().any(char::is_control),
                "{:?}",
                cell.symbol()
            );
        }
    }
}

#[test]
fn the_cursor_stays_in_view() {
    let mut app = started(100, 20);
    for _ in 0..70 {
        keys(&mut app, &["Down"]);
        let name = app.list.current().expect("row").name();
        assert_shows(&screen(&mut app, 100, 20), &[&name]);
    }
    keys(&mut app, &["Enter", "z"]);
    let record = app.record.clone().expect("record");
    let fields = app.detail_fields(&record).len();
    for _ in 0..fields {
        keys(&mut app, &["Down"]);
        screen(&mut app, 100, 20);
        let cursor = app.record.as_ref().expect("record").cursor;
        assert!(
            app.places
                .hits
                .iter()
                .any(|(_, h)| *h == Hit::Detail(cursor)),
            "field {cursor} is out of view"
        );
    }
}

#[test]
fn a_moment_ago_reads_as_now() {
    use crate::app::tests::NOW;
    let mut app = started(160, 50);
    keys(&mut app, &["Enter", "5"]);
    let moment = text::format_utc(NOW - 5);
    if let Some(Load::Ready(slas)) = app.record.as_mut().map(|r| &mut r.slas) {
        slas[0].due_time = Some(NOW - 5);
        slas[0].due.clone_from(&moment);
    }
    let mut column = app.list.columns[0].clone();
    column.look = crate::tables::Look::Due;
    column.field = "start_date".into();
    app.list.columns.insert(0, column);
    app.list.rows[0].merge(crate::model::Row::new(
        "task",
        &serde_json::json!({ "start_date": moment }),
    ));
    let shown = screen(&mut app, 160, 50);
    assert_shows(&shown, &[&format!("due {moment} (now)")]);
    assert!(!shown.contains("now ago"), "{shown}");
}
