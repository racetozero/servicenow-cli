//! Colors and signs. Colors are ANSI names, so they follow the palette of
//! the terminal and read on light and dark; only the accent is a fixed color,
//! and only when the terminal says it can show it. Every color also has a
//! sign or a word.

use crate::text;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;

/// What the terminal can show. Read from the environment once, at the start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub color: bool,
    pub truecolor: bool,
    pub unicode: bool,
}

impl Caps {
    pub fn detect(no_color: bool) -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let term = var("TERM").unwrap_or_default();
        let color = !no_color && var("NO_COLOR").is_none() && term != "dumb";
        let truecolor =
            color && var("COLORTERM").is_some_and(|v| matches!(v.as_str(), "truecolor" | "24bit"));
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"].into_iter().find_map(var);
        let unicode = match locale {
            Some(locale) => {
                let locale = locale.to_ascii_lowercase();
                locale.contains("utf-8") || locale.contains("utf8")
            }
            // Windows consoles take UTF-16, and macOS terminals are UTF-8.
            None => cfg!(any(windows, target_os = "macos")),
        };
        Self {
            color,
            truecolor,
            unicode: unicode && term != "linux",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Glyphs {
    pub bar: &'static str,
    pub dot: &'static str,
    pub ring: &'static str,
    pub check: &'static str,
    pub cross: &'static str,
    pub warn: &'static str,
    pub high: &'static str,
    pub low: &'static str,
    pub arrow: &'static str,
    pub crumb: &'static str,
    pub sep: &'static str,
    pub up: &'static str,
    pub down: &'static str,
    pub ellipsis: &'static str,
    pub dash: &'static str,
    pub vline: &'static str,
    pub hline: &'static str,
    pub full: &'static str,
    pub empty: &'static str,
    pub pencil: &'static str,
    pub lock: &'static str,
    pub refresh: &'static str,
    pub spinner: &'static [&'static str],
    pub border: border::Set<'static>,
}

const UNICODE: Glyphs = Glyphs {
    bar: "▌",
    dot: "●",
    ring: "○",
    check: "✓",
    cross: "✗",
    warn: "▲",
    high: "△",
    low: "▽",
    arrow: "→",
    crumb: "›",
    sep: "·",
    up: "↑",
    down: "↓",
    ellipsis: "…",
    dash: "–",
    vline: "│",
    hline: "─",
    full: "━",
    empty: "─",
    pencil: "✎",
    lock: "⊘",
    refresh: "⟳",
    spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"],
    border: border::ROUNDED,
};

const ASCII: Glyphs = Glyphs {
    bar: ">",
    dot: "*",
    ring: "o",
    check: "+",
    cross: "x",
    warn: "!",
    high: "^",
    low: "v",
    arrow: "->",
    crumb: ">",
    sep: "-",
    up: "^",
    down: "v",
    ellipsis: "...",
    dash: "-",
    vline: "|",
    hline: "-",
    full: "=",
    empty: "-",
    pencil: "*",
    lock: "!",
    refresh: "@",
    spinner: &["|", "/", "-", "\\"],
    border: border::Set {
        top_left: "+",
        top_right: "+",
        bottom_left: "+",
        bottom_right: "+",
        vertical_left: "|",
        vertical_right: "|",
        horizontal_top: "-",
        horizontal_bottom: "-",
    },
};

/// What a status means, whatever its table calls it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// Waiting for a first action.
    New,
    Active,
    /// Waiting for someone else.
    Hold,
    Good,
    Bad,
    Warn,
    Done,
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub caps: Caps,
    pub g: Glyphs,
}

impl Theme {
    pub fn new(caps: Caps) -> Self {
        Self {
            caps,
            g: if caps.unicode { UNICODE } else { ASCII },
        }
    }

    fn fg(&self, color: Color) -> Style {
        match self.caps.color {
            true => Style::new().fg(color),
            false => Style::new(),
        }
    }

    /// The one color that marks focus and what can be pressed.
    pub fn accent(&self) -> Style {
        // Readable on white (3.7:1) and on black (5.6:1).
        self.fg(match self.caps.truecolor {
            true => Color::Rgb(14, 148, 136),
            false => Color::Cyan,
        })
    }

    pub fn accent_bold(&self) -> Style {
        self.accent().add_modifier(Modifier::BOLD)
    }

    pub fn bold(&self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    pub fn dim(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    pub fn heading(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM | Modifier::BOLD)
    }

    pub fn good(&self) -> Style {
        self.fg(Color::Green)
    }

    pub fn warning(&self) -> Style {
        self.fg(Color::Yellow)
    }

    pub fn danger(&self) -> Style {
        self.fg(Color::Red)
    }

    /// A filled label, such as the product name or an active tab.
    pub fn badge(&self) -> Style {
        match self.caps.truecolor {
            true => Style::new()
                .bg(Color::Rgb(14, 148, 136))
                .fg(Color::Rgb(255, 255, 255))
                .add_modifier(Modifier::BOLD),
            false => self.accent_bold().add_modifier(Modifier::REVERSED),
        }
    }

    pub fn badge_warning(&self) -> Style {
        self.warning()
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    }

    /// The cursor row of a pane.
    pub fn cursor(&self, focused: bool) -> Style {
        match focused {
            true => self.bold(),
            false => Style::new(),
        }
    }

    pub fn tone(&self, tone: Tone) -> Style {
        match tone {
            Tone::Plain => Style::new(),
            Tone::New => self.fg(Color::Blue),
            Tone::Active => self.accent(),
            Tone::Hold => self.fg(Color::Magenta),
            Tone::Good => self.good(),
            Tone::Bad => self.danger(),
            Tone::Warn => self.warning(),
            Tone::Done => self.dim(),
        }
    }

    pub fn spinner(&self, frame: usize) -> &'static str {
        self.g.spinner[frame % self.g.spinner.len()]
    }
}

/// The tone of a priority by its stored value, 1 (critical) to 5 (planning).
pub fn priority(value: &str) -> Tone {
    match value {
        "1" => Tone::Bad,
        "2" => Tone::Warn,
        "3" => Tone::Plain,
        "" => Tone::Plain,
        _ => Tone::Done,
    }
}

/// The sign of a priority, so that color is never alone.
pub fn priority_sign(theme: &Theme, value: &str) -> &'static str {
    match value {
        "1" => theme.g.warn,
        "2" => theme.g.high,
        "4" | "5" => theme.g.low,
        _ => " ",
    }
}

/// `1 - Critical` as `1 Critical`.
pub fn priority_label(display: &str) -> String {
    display.replacen(" - ", " ", 1)
}

/// The tone of a state by its label, because every table numbers its states
/// differently and any administrator can rename them.
pub fn state(label: &str) -> Tone {
    let label = label.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| label.contains(w));
    if has(&[
        "reject", "fail", "error", "breach", "unauthor", "denied", "overdue",
    ]) {
        Tone::Bad
    } else if has(&[
        "cancel",
        "closed",
        "retired",
        "ignore",
        "skipped",
        "no longer",
    ]) {
        Tone::Done
    } else if has(&[
        "resolved",
        "approved",
        "publish",
        "achiev",
        "operational",
        "installed",
    ]) && !has(&["non-operational", "not "])
    {
        Tone::Good
    } else if has(&["complete", "done", "deployed"]) && !has(&["incomplete"]) {
        Tone::Done
    } else if has(&["hold", "pending", "await", "wait", "paused", "suspend"]) {
        Tone::Hold
    } else if has(&[
        "progress",
        "implement",
        "active",
        "assess",
        "analysis",
        "authorize",
        "review",
        "scheduled",
    ]) && !has(&["inactive"])
    {
        Tone::Active
    } else if has(&["new", "open", "requested", "draft", "registered"]) {
        Tone::New
    } else {
        Tone::Plain
    }
}

/// A value with the sign of its tone, so color is never alone.
pub fn sign(theme: &Theme, tone: Tone) -> &'static str {
    match tone {
        Tone::Bad | Tone::Warn => theme.g.warn,
        Tone::Active => theme.g.dot,
        Tone::Good | Tone::Done => theme.g.check,
        Tone::New | Tone::Hold => theme.g.ring,
        Tone::Plain => " ",
    }
}

/// Keys and what they do, in one line of at most `max` columns. The last
/// pairs go first when there is no room.
pub fn hints_that_fit<T>(pairs: &mut Vec<(String, String, T)>, max: usize) {
    let size = |pairs: &[(String, String, T)]| {
        pairs
            .iter()
            .map(|(k, a, _)| text::width(k) + text::width(a) + 3)
            .sum::<usize>()
    };
    while pairs.len() > 1 && size(pairs) > max {
        pairs.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_have_a_tone_by_their_label() {
        for (label, tone) in [
            ("New", Tone::New),
            ("In Progress", Tone::Active),
            ("On Hold", Tone::Hold),
            ("Resolved", Tone::Good),
            ("Closed Complete", Tone::Done),
            ("Rejected", Tone::Bad),
            ("Non-Operational", Tone::Plain),
            ("Operational", Tone::Good),
        ] {
            assert_eq!(state(label), tone, "{label}");
        }
    }

    #[test]
    fn no_color_leaves_no_color() {
        let theme = Theme::new(Caps {
            color: false,
            truecolor: false,
            unicode: false,
        });
        for style in [
            theme.accent(),
            theme.badge(),
            theme.danger(),
            theme.tone(Tone::Bad),
            theme.badge_warning(),
        ] {
            assert_eq!((style.fg, style.bg), (None, None));
        }
    }
}
