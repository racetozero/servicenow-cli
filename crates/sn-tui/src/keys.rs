//! The one table of keys. Dispatch, the footer, the help and the palette all
//! read it, so a key cannot work without being shown, or be shown without
//! working.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Act {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    Open,
    Back,
    NextPane,
    PrevPane,
    Goto,
    Palette,
    OpenTable,
    Help,
    Quit,
    Refresh,
    AutoRefresh,
    Profiles,
    SignIn,
    Search,
    Filter,
    Query,
    ResetView,
    Sort,
    Select,
    SelectAll,
    Pin,
    New,
    Menu,
    Comment,
    WorkNote,
    AssignMe,
    Assign,
    Resolve,
    Approve,
    Reject,
    CopyName,
    CopyLink,
    Browser,
    Preview,
    NextTab,
    PrevTab,
    Details,
    Activity,
    Related,
    Attachments,
    Slas,
    History,
    Empty,
    Follow,
    Review,
    Discard,
    Undo,
}

/// Where a key works.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    Anywhere,
    Rail,
    List,
    Record,
}

impl Place {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Anywhere => "Anywhere",
            Self::Rail => "Sections",
            Self::List => "List",
            Self::Record => "Record",
        }
    }
}

pub struct Binding {
    pub act: Act,
    pub place: Place,
    pub keys: &'static [(KeyCode, KeyModifiers)],
    /// The keys as people read them.
    pub label: &'static str,
    /// For the footer.
    pub short: &'static str,
    /// For the help and the palette.
    pub long: &'static str,
    /// Lower shows first in the footer; `None` keeps it out.
    pub footer: Option<u8>,
}

const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;

const fn k(c: char) -> (KeyCode, KeyModifiers) {
    (KeyCode::Char(c), NONE)
}

const fn ctrl(c: char) -> (KeyCode, KeyModifiers) {
    (KeyCode::Char(c), CTRL)
}

const fn b(
    act: Act,
    place: Place,
    keys: &'static [(KeyCode, KeyModifiers)],
    label: &'static str,
    short: &'static str,
    long: &'static str,
    footer: Option<u8>,
) -> Binding {
    Binding {
        act,
        place,
        keys,
        label,
        short,
        long,
        footer,
    }
}

use Place::{Anywhere, List, Rail, Record};

pub const BINDINGS: &[Binding] = &[
    // Moving.
    b(
        Act::Up,
        Anywhere,
        &[k('k'), (KeyCode::Up, NONE)],
        "↑ k",
        "up",
        "Move up",
        None,
    ),
    b(
        Act::Down,
        Anywhere,
        &[k('j'), (KeyCode::Down, NONE)],
        "↓ j",
        "down",
        "Move down",
        None,
    ),
    b(
        Act::PageUp,
        Anywhere,
        &[(KeyCode::PageUp, NONE), ctrl('u')],
        "PgUp",
        "page up",
        "Move up by a page",
        None,
    ),
    b(
        Act::PageDown,
        Anywhere,
        &[(KeyCode::PageDown, NONE), ctrl('d')],
        "PgDn",
        "page down",
        "Move down by a page",
        None,
    ),
    b(
        Act::Top,
        Anywhere,
        &[(KeyCode::Home, NONE)],
        "Home",
        "top",
        "Go to the first row (also g g)",
        None,
    ),
    b(
        Act::Bottom,
        Anywhere,
        &[(KeyCode::End, NONE), k('G')],
        "End G",
        "bottom",
        "Go to the last row",
        None,
    ),
    b(
        Act::NextPane,
        Anywhere,
        &[(KeyCode::Tab, NONE)],
        "Tab",
        "next pane",
        "Go to the next pane",
        Some(40),
    ),
    b(
        Act::PrevPane,
        Anywhere,
        &[(KeyCode::BackTab, NONE)],
        "Shift-Tab",
        "pane",
        "Go to the previous pane",
        None,
    ),
    b(
        Act::Back,
        Anywhere,
        &[(KeyCode::Esc, NONE), (KeyCode::Backspace, NONE)],
        "Esc",
        "back",
        "Go back one step",
        Some(41),
    ),
    // Reaching things.
    b(
        Act::Palette,
        Anywhere,
        &[ctrl('k'), k(':')],
        "Ctrl-K",
        "find anything",
        "Find a record, table, section or action",
        Some(7),
    ),
    b(
        Act::Goto,
        Anywhere,
        &[k('g')],
        "g",
        "go to",
        "Jump to a section by its letter",
        Some(8),
    ),
    b(
        Act::OpenTable,
        Anywhere,
        &[k('t')],
        "t",
        "table",
        "Open any table",
        None,
    ),
    b(
        Act::Help,
        Anywhere,
        &[k('?')],
        "?",
        "keys",
        "Show every key",
        Some(0),
    ),
    b(
        Act::Refresh,
        Anywhere,
        &[k('r'), (KeyCode::F(5), NONE)],
        "r",
        "refresh",
        "Load again what you look at",
        Some(38),
    ),
    b(
        Act::AutoRefresh,
        Anywhere,
        &[k('R')],
        "R",
        "auto-refresh",
        "Turn auto-refresh on or off (every 30 seconds)",
        None,
    ),
    b(
        Act::Profiles,
        Anywhere,
        &[k('P')],
        "P",
        "profiles",
        "Switch to another profile",
        None,
    ),
    b(
        Act::SignIn,
        Anywhere,
        &[],
        "",
        "sign in",
        "Sign in again to this profile",
        None,
    ),
    b(
        Act::Quit,
        Anywhere,
        &[k('q')],
        "q",
        "quit",
        "Leave the workspace",
        Some(45),
    ),
    // Sections.
    b(
        Act::Open,
        Rail,
        &[(KeyCode::Enter, NONE), (KeyCode::Right, NONE), k('l')],
        "Enter",
        "open",
        "Open the section",
        Some(1),
    ),
    b(
        Act::Pin,
        Rail,
        &[k('p')],
        "p",
        "unpin",
        "Remove a pinned table",
        Some(3),
    ),
    // List.
    b(
        Act::Open,
        List,
        &[(KeyCode::Enter, NONE), (KeyCode::Right, NONE), k('l')],
        "Enter",
        "open",
        "Open the record",
        Some(1),
    ),
    b(
        Act::Menu,
        List,
        &[k('a')],
        "a",
        "actions",
        "Show what you can do with the record",
        Some(2),
    ),
    b(
        Act::Search,
        List,
        &[k('/')],
        "/",
        "search",
        "Search the rows on screen as you type",
        Some(3),
    ),
    b(
        Act::Filter,
        List,
        &[k('f')],
        "f",
        "filter",
        "Build a filter from fields and values",
        Some(4),
    ),
    b(
        Act::Query,
        List,
        &[k('F')],
        "F",
        "query",
        "Edit the encoded query",
        None,
    ),
    b(
        Act::ResetView,
        List,
        &[k('0')],
        "0",
        "reset",
        "Put the filter and the order of the section back",
        None,
    ),
    b(
        Act::Sort,
        List,
        &[k('s')],
        "s",
        "sort",
        "Sort by a column",
        Some(6),
    ),
    b(
        Act::Select,
        List,
        &[k(' ')],
        "Space",
        "select",
        "Select the row, to act on several at once",
        Some(5),
    ),
    b(
        Act::SelectAll,
        List,
        &[ctrl('a')],
        "Ctrl-A",
        "select all",
        "Select every loaded row, or none",
        None,
    ),
    b(
        Act::Preview,
        List,
        &[k('v')],
        "v",
        "preview",
        "Show or hide the record beside the list",
        None,
    ),
    b(
        Act::Pin,
        List,
        &[k('p')],
        "p",
        "pin",
        "Keep this table in the sections",
        None,
    ),
    // Record.
    b(
        Act::Open,
        Record,
        &[(KeyCode::Enter, NONE), k('e')],
        "Enter",
        "edit",
        "Edit the field, or open the row",
        Some(1),
    ),
    b(
        Act::Review,
        Record,
        &[ctrl('s')],
        "Ctrl-S",
        "review changes",
        "Review your changes and send them",
        Some(2),
    ),
    b(
        Act::Undo,
        Record,
        &[k('u')],
        "u",
        "undo field",
        "Put the old value of the field back",
        None,
    ),
    b(
        Act::Discard,
        Record,
        &[k('D')],
        "D",
        "discard",
        "Drop every change that is not sent",
        None,
    ),
    b(
        Act::NextTab,
        Record,
        &[k(']'), (KeyCode::Right, NONE), k('l')],
        "← →",
        "tab",
        "Next or previous tab",
        Some(4),
    ),
    b(
        Act::PrevTab,
        Record,
        &[k('['), (KeyCode::Left, NONE), k('h')],
        "[",
        "tab",
        "Previous tab",
        None,
    ),
    b(
        Act::Details,
        Record,
        &[k('1')],
        "1",
        "details",
        "Show the fields",
        None,
    ),
    b(
        Act::Activity,
        Record,
        &[k('2')],
        "2",
        "activity",
        "Show comments and work notes",
        None,
    ),
    b(
        Act::Related,
        Record,
        &[k('3')],
        "3",
        "related",
        "Show approvals, tasks and affected items",
        None,
    ),
    b(
        Act::Attachments,
        Record,
        &[k('4')],
        "4",
        "files",
        "Show attachments",
        None,
    ),
    b(
        Act::Slas,
        Record,
        &[k('5')],
        "5",
        "SLAs",
        "Show service levels",
        None,
    ),
    b(
        Act::History,
        Record,
        &[k('6')],
        "6",
        "history",
        "Show who changed what",
        None,
    ),
    b(
        Act::Follow,
        Record,
        &[k('f')],
        "f",
        "follow",
        "Open the record that the field points to",
        Some(5),
    ),
    b(
        Act::Empty,
        Record,
        &[k('z')],
        "z",
        "empty fields",
        "Show or hide empty fields",
        Some(7),
    ),
    b(
        Act::Menu,
        Record,
        &[k('a')],
        "a",
        "actions",
        "Show what you can do with the record",
        Some(3),
    ),
    // Acting on a record.
    b(
        Act::New,
        Anywhere,
        &[k('n')],
        "n",
        "new",
        "Create a record in this table",
        None,
    ),
    b(
        Act::Comment,
        Anywhere,
        &[k('c')],
        "c",
        "comment",
        "Write a comment that the caller sees",
        Some(18),
    ),
    b(
        Act::WorkNote,
        Anywhere,
        &[k('w')],
        "w",
        "work note",
        "Write a work note for your colleagues",
        Some(19),
    ),
    b(
        Act::AssignMe,
        Anywhere,
        &[k('m')],
        "m",
        "assign to me",
        "Assign to me",
        Some(20),
    ),
    b(
        Act::Assign,
        Anywhere,
        &[k('i')],
        "i",
        "assign",
        "Assign to a person or a group",
        None,
    ),
    b(
        Act::Resolve,
        Anywhere,
        &[k('x')],
        "x",
        "resolve",
        "Resolve or close",
        Some(21),
    ),
    b(
        Act::Approve,
        Anywhere,
        &[k('A')],
        "A",
        "approve",
        "Approve",
        Some(9),
    ),
    b(
        Act::Reject,
        Anywhere,
        &[k('X')],
        "X",
        "reject",
        "Reject",
        Some(10),
    ),
    b(
        Act::Browser,
        Anywhere,
        &[k('o')],
        "o",
        "browser",
        "Open in the browser",
        Some(24),
    ),
    b(
        Act::CopyName,
        Anywhere,
        &[k('y')],
        "y",
        "copy number",
        "Copy the number",
        None,
    ),
    b(
        Act::CopyLink,
        Anywhere,
        &[k('Y')],
        "Y",
        "copy link",
        "Copy the link",
        None,
    ),
];

/// The binding of a key. The keys of the focused pane win over general ones.
pub fn find(key: KeyEvent, place: Place) -> Option<Act> {
    let code = match key.code {
        // Terminals differ in whether they report Shift with a capital.
        KeyCode::Char(_) => key.modifiers & !KeyModifiers::SHIFT,
        _ => key.modifiers - KeyModifiers::SHIFT,
    };
    let hit = |p: Place| {
        BINDINGS
            .iter()
            .find(|b| b.place == p && b.keys.contains(&(key.code, code)))
            .map(|b| b.act)
    };
    hit(place).or_else(|| hit(Anywhere))
}

/// The binding that stands for an action in a place, for help and menus.
pub fn binding(act: Act, place: Place) -> Option<&'static Binding> {
    BINDINGS
        .iter()
        .find(|b| b.act == act && b.place == place)
        .or_else(|| {
            BINDINGS
                .iter()
                .find(|b| b.act == act && b.place == Anywhere)
        })
        .or_else(|| BINDINGS.iter().find(|b| b.act == act))
}

/// Key labels have arrows; terminals without Unicode get words.
pub fn label(binding: &Binding, unicode: bool) -> String {
    match unicode {
        true => binding.label.to_owned(),
        false => binding
            .label
            .replace('↑', "Up")
            .replace('↓', "Down")
            .replace('←', "Left")
            .replace('→', "Right"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_means_one_thing_in_each_place() {
        for (i, a) in BINDINGS.iter().enumerate() {
            for other in &BINDINGS[i + 1..] {
                if a.place == other.place {
                    for key in a.keys {
                        assert!(
                            !other.keys.contains(key),
                            "{key:?} is {:?} and {:?}",
                            a.act,
                            other.act
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn capitals_work_with_and_without_a_reported_shift() {
        for mods in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            assert_eq!(
                find(KeyEvent::new(KeyCode::Char('R'), mods), List),
                Some(Act::AutoRefresh)
            );
        }
        assert_eq!(
            find(KeyEvent::new(KeyCode::Right, NONE), Record),
            Some(Act::NextTab)
        );
        assert_eq!(
            find(KeyEvent::new(KeyCode::Right, NONE), List),
            Some(Act::Open)
        );
    }
}
