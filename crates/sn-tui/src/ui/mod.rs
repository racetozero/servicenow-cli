//! Drawing. Reads the state, changes nothing, and reports where things are,
//! so that the mouse and paging work on what is really on screen.

mod list;
mod overlay;
mod record;
#[cfg(test)]
mod tests;

use crate::app::App;
use crate::app::state::*;
use crate::input::Editor;
use crate::keys::{self, Act, BINDINGS, Place};
use crate::text;
use crate::theme::{Theme, hints_that_fit};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

/// Something that can be clicked.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Pane(Pane),
    Rail(usize),
    Row(usize),
    /// The selection mark of a row.
    Mark(usize),
    /// A column heading.
    Column(String),
    Tab(Tab),
    Detail(usize),
    Related(usize),
    File(usize),
    Act(Act),
    /// Everything around an overlay.
    Outside,
    /// An overlay itself.
    Overlay,
    /// An item of the top overlay.
    Item(usize),
    /// Stands for a key.
    Button(KeyEvent),
}

/// What drawing found out.
#[derive(Clone, Debug, Default)]
pub struct Places {
    pub hits: Vec<(Rect, Hit)>,
    pub list_rows: usize,
    pub list_offset: usize,
    pub rail_offset: usize,
    pub record_rows: usize,
    pub record_offset: usize,
    pub record_max_scroll: usize,
    pub overlay_offset: usize,
    /// The field a list of values belongs to.
    pub anchor: Option<Rect>,
    pub overlay_max_scroll: usize,
}

const RAIL_WIDTH: u16 = 24;

/// One frame in the making.
pub(crate) struct Pen<'a, 'f> {
    pub frame: &'a mut Frame<'f>,
    pub app: &'a App,
    pub t: Theme,
    pub places: Places,
}

pub fn draw(frame: &mut Frame, app: &App) -> Places {
    let area = frame.area();
    let mut pen = Pen {
        frame,
        app,
        t: app.theme,
        places: Places::default(),
    };
    pen.screen(area);
    let places = pen.places;
    if !app.theme.caps.unicode {
        // Record text can hold anything; this terminal cannot.
        for cell in &mut frame.buffer_mut().content {
            if !cell.symbol().is_ascii() {
                cell.set_symbol("?");
            }
        }
    }
    places
}

/// The first visible line so that `cursor` stays visible.
pub(crate) fn scrolled(before: usize, cursor: usize, rows: usize, total: usize) -> usize {
    let rows = rows.max(1);
    let max = total.saturating_sub(rows);
    let mut offset = before.min(max);
    if cursor < offset {
        offset = cursor;
    } else if cursor >= offset + rows {
        offset = cursor + 1 - rows;
    }
    offset.min(max)
}

/// A key and what it does, for the footer.
pub(crate) type Hint = (String, String, Option<Hit>);

pub(crate) fn hint(key: &str, label: &str) -> Hint {
    (key.to_owned(), label.to_owned(), None)
}

impl Pen<'_, '_> {
    fn screen(&mut self, area: Rect) {
        if area.width < 30 || area.height < 8 {
            self.frame.render_widget(
                Paragraph::new(
                    "Make the terminal larger: 30 columns and 8 rows at least. Ctrl-C leaves.",
                )
                .wrap(ratatui::widgets::Wrap { trim: true }),
                area,
            );
            return;
        }
        let air = u16::from(area.height >= 18);
        let [header, crumbs, body, _, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(air),
            Constraint::Min(3),
            Constraint::Length(u16::from(area.height >= 26)),
            Constraint::Length(1),
        ])
        .areas(area);
        self.header(header);
        self.crumbs(crumbs);
        self.body(body);
        self.footer(footer);
        self.overlays(area);
        self.toasts(area);
    }

    pub fn put(&mut self, area: Rect, line: impl Into<Line<'static>>) {
        if !area.is_empty() {
            self.frame.render_widget(line.into(), area);
        }
    }

    pub fn hit(&mut self, area: Rect, hit: Hit) {
        if !area.is_empty() {
            self.places.hits.push((area, hit));
        }
    }

    fn header(&mut self, area: Rect) {
        let app = self.app;
        let t = self.t;
        let connection = &app.connection;
        let mut right: Vec<Span> = Vec::new();
        if let Some(activity) = app.activity() {
            right.push(Span::styled(
                format!(
                    "{} {activity}{}  ",
                    t.spinner(app.ms as usize / 100),
                    t.g.ellipsis
                ),
                t.accent(),
            ));
        }
        if app.auto_refresh {
            right.push(Span::styled(format!("{} auto  ", t.g.refresh), t.good()));
        }
        if connection.read_only {
            right.push(Span::styled(" READ-ONLY ", t.badge_warning()));
            right.push(Span::raw(" "));
        }
        if connection.dry_run {
            right.push(Span::styled(" DRY RUN ", t.badge_warning()));
            right.push(Span::raw(" "));
        }
        let mut left = vec![Span::styled(" sn ", t.badge()), Span::raw("  ")];
        if !connection.host.is_empty() {
            left.push(Span::styled(text::line(&connection.host), t.bold()));
        }
        // The profile says something the host does not, unless it is default.
        if connection.profile != "default" {
            left.push(Span::styled(
                format!("  {}", text::line(&connection.profile)),
                t.accent(),
            ));
        }
        if let Some(me) = app.me() {
            left.push(Span::styled(
                format!("  {} {}", t.g.sep, text::line(&me.name)),
                t.dim(),
            ));
        }
        let right_width = right.iter().map(Span::width).sum::<usize>() as u16;
        let [left_area, right_area] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(right_width.min(area.width)),
        ])
        .areas(area);
        self.put(left_area, Line::from(left));
        self.put(right_area, Line::from(right));
    }

    /// The way back: the lists this one was reached from.
    fn crumbs(&mut self, area: Rect) {
        let app = self.app;
        if app.back.is_empty() || app.connection.signed_out.is_some() {
            return;
        }
        let t = self.t;
        let from = app.back.len().saturating_sub(3);
        let mut words: Vec<String> = app.back[from..]
            .iter()
            .map(|s| match (&s.record, s.focus) {
                (Some(record), Pane::Record) => record.name(),
                _ => text::line(&s.list.title),
            })
            .collect();
        words.push(text::line(&app.list.title));
        let line = format!(" {}", words.join(&format!(" {} ", t.g.crumb)));
        self.put(
            area,
            Line::styled(text::cut(&line, area.width as usize, t.g.ellipsis), t.dim()),
        );
    }

    fn body(&mut self, area: Rect) {
        let app = self.app;
        if app.connection.signed_out.is_some() {
            self.sign_in(area);
            return;
        }
        let mut rest = area;
        if app.rail_shown() {
            let [rail, gap, main] = Layout::horizontal([
                Constraint::Length(RAIL_WIDTH),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .areas(area);
            self.rail(rail);
            self.vline(gap);
            rest = main;
        }
        let open = app.record.is_some();
        if open && app.side_by_side() {
            // The record gets more room as the terminal grows.
            let list = match rest.width {
                0..100 => rest.width * 38 / 100,
                100..140 => rest.width * 44 / 100,
                _ => rest.width * 54 / 100,
            };
            let [list, gap, record] = Layout::horizontal([
                Constraint::Length(list),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .areas(rest);
            self.list(list);
            self.vline(gap);
            self.record(record);
        } else if open && app.stacked() {
            let [list, rule, record] = Layout::vertical([
                Constraint::Length(rest.height * 38 / 100),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .areas(rest);
            self.list(list);
            self.hline(rule);
            self.record(record);
        } else if open && app.focus == Pane::Record {
            self.record(rest);
        } else {
            self.list(rest);
        }
        if app.focus == Pane::Rail && !app.rail_shown() {
            let drawer = Rect {
                width: (RAIL_WIDTH + 3).min(area.width),
                ..area
            };
            self.step_back(self.frame.area());
            self.frame.render_widget(Clear, drawer);
            let [rail, gap] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(3)]).areas(drawer);
            self.rail(rail);
            self.vline(gap);
        }
    }

    fn vline(&mut self, area: Rect) {
        let line = Line::styled(format!(" {}", self.t.g.vline), self.t.dim());
        self.frame
            .render_widget(Paragraph::new(vec![line; area.height as usize]), area);
    }

    pub fn hline(&mut self, area: Rect) {
        let line = self.t.g.hline.repeat(area.width as usize);
        let style = self.t.dim();
        self.put(area, Line::styled(line, style));
    }

    /// The keys that matter now. On the right, the way to everything else.
    fn footer(&mut self, area: Rect) {
        let mut hints: Vec<Hint> = match self.app.top() {
            Some(overlay) => self.overlay_hints(overlay),
            None => self.pane_hints(),
        };
        hints_that_fit(&mut hints, area.width.saturating_sub(1) as usize);
        self.hints(area, hints);
    }

    pub fn hints(&mut self, area: Rect, hints: Vec<Hint>) {
        let mut x = area.x + 1;
        for (key, label, hit) in hints {
            let key_width = text::width(&key) as u16;
            let width = key_width + 1 + text::width(&label) as u16;
            let spot = Rect { x, width, ..area }.intersection(area);
            if spot.is_empty() {
                break;
            }
            let (accent, dim) = (self.t.accent_bold(), self.t.dim());
            self.put(
                spot,
                Line::from(vec![
                    Span::styled(key, accent),
                    Span::styled(format!(" {label}"), dim),
                ]),
            );
            if let Some(hit) = hit {
                self.hit(spot, hit);
            }
            x += width + 2;
        }
    }

    fn pane_hints(&self) -> Vec<Hint> {
        let app = self.app;
        if app.connection.signed_out.is_some() {
            return vec![
                hint("Enter", "sign in"),
                hint("P", "profiles"),
                hint("q", "quit"),
            ];
        }
        if app.list.searching && app.focus == Pane::List {
            return vec![
                hint("Enter", "keep"),
                hint("Esc", "clear"),
                hint("Ctrl-K", "search the whole instance"),
            ];
        }
        if let Some(inline) = app
            .record
            .as_ref()
            .and_then(|r| r.inline.as_ref())
            .filter(|_| app.focus == Pane::Record)
        {
            use crate::model::Kind;
            let mut out = vec![hint("Enter", "keep"), hint("Esc", "cancel")];
            if matches!(inline.field.kind, Kind::Date | Kind::DateTime) {
                out.push(hint(
                    if self.t.caps.unicode {
                        "↑ ↓"
                    } else {
                        "Up Down"
                    },
                    "one day",
                ));
                out.push(hint("UTC", "for example +2h, tomorrow"));
            }
            return out;
        }
        let place = app.place();
        let targets = app.target_rows();
        let tab = app.record.as_ref().map(|r| r.tab);
        let mut bindings: Vec<&keys::Binding> = BINDINGS
            .iter()
            .filter(|b| b.footer.is_some() && (b.place == place || b.place == Place::Anywhere))
            .filter(|b| {
                // A key of this pane hides the general one of the same name.
                b.place == place || !BINDINGS.iter().any(|o| o.place == place && o.act == b.act)
            })
            .filter(|b| app.possible(b.act, &targets))
            .filter(|b| match b.act {
                Act::Empty => tab == Some(Tab::Details),
                Act::Comment | Act::WorkNote | Act::AssignMe | Act::Resolve | Act::Browser => {
                    place != Place::Rail
                }
                _ => true,
            })
            .collect();
        bindings.sort_by_key(|b| b.footer);
        // Without a rail on screen, the way to it comes early.
        let drawer: Vec<Hint> = match place == Place::List && !app.rail_shown() {
            true => vec![(
                "Tab".into(),
                "sections".into(),
                Some(Hit::Act(Act::PrevPane)),
            )],
            false => Vec::new(),
        };
        let help = bindings.iter().position(|b| b.act == Act::Help);
        let mut out: Vec<Hint> = bindings
            .into_iter()
            .map(|b| {
                let label = match (b.act, place, tab) {
                    (Act::Open, Place::Record, Some(Tab::Details | Tab::Activity))
                        if app.connection.read_only =>
                    {
                        ""
                    }
                    (Act::Open, Place::Record, Some(Tab::Activity)) => "write a note",
                    (Act::Open, Place::Record, Some(Tab::Related)) => "open",
                    (Act::Open, Place::Record, Some(Tab::Attachments)) => "download",
                    (Act::Open, Place::Record, Some(Tab::Slas | Tab::History)) => "",
                    (Act::Open, Place::List, _)
                        if matches!(app.list.phase, ListPhase::Failed(_)) =>
                    {
                        "try again"
                    }
                    _ => b.short,
                };
                (
                    keys::label(b, self.t.caps.unicode),
                    label.to_owned(),
                    Some(Hit::Act(b.act)),
                )
            })
            .filter(|(_, label, _)| !label.is_empty())
            .collect();
        let at = help.map_or(0, |i| i + 1).min(out.len());
        out.splice(at..at, drawer);
        out
    }

    fn sign_in(&mut self, area: Rect) {
        let app = self.app;
        let t = self.t;
        let reason = app.connection.signed_out.clone().unwrap_or_default();
        let width = area.width.saturating_sub(4).min(72);
        let mut lines: Vec<Line> = Vec::new();
        let title = match app.connection.host.is_empty() {
            true => "Connect to ServiceNow".to_owned(),
            false => format!("Sign in to {}", text::line(&app.connection.host)),
        };
        lines.push(Line::styled(title, t.bold()));
        lines.push(Line::default());
        for line in text::wrap(&text::line(&reason), width.saturating_sub(6) as usize) {
            lines.push(Line::styled(line, t.dim()));
        }
        if app.has_unsent() {
            lines.push(Line::default());
            lines.push(Line::styled(
                "What you wrote is kept and waits for you.",
                t.good(),
            ));
        }
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled(" Enter ", t.badge()),
            Span::raw(" sign in    "),
            Span::styled("P", t.accent_bold()),
            Span::styled(" other profile    ", t.dim()),
            Span::styled("q", t.accent_bold()),
            Span::styled(" quit", t.dim()),
        ]));
        let height = (lines.len() as u16 + 2).min(area.height);
        let rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        let border = t.accent();
        let inner = self.card(rect, "Signed out", border);
        let buttons = Rect {
            y: (inner.y + inner.height).saturating_sub(1),
            height: 1,
            width: 16.min(inner.width),
            ..inner
        };
        self.frame.render_widget(Paragraph::new(lines), inner);
        self.hit(
            buttons,
            Hit::Button(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
    }

    /// A bordered card. Gives the area inside it.
    pub fn card(&mut self, rect: Rect, title: &str, border: Style) -> Rect {
        let rect = rect.intersection(self.frame.area());
        self.frame.render_widget(Clear, rect);
        let block = Block::bordered()
            .border_set(self.t.g.border)
            .border_style(border)
            .title(Span::styled(
                format!(
                    " {} ",
                    text::cut(
                        title,
                        rect.width.saturating_sub(6) as usize,
                        self.t.g.ellipsis
                    )
                ),
                border.add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(rect);
        self.frame.render_widget(block, rect);
        inner.inner(Margin::new(1, 0))
    }

    fn toasts(&mut self, area: Rect) {
        let app = self.app;
        let t = self.t;
        let mut bottom = area.bottom().saturating_sub(2);
        for toast in app.toasts.iter().rev() {
            let (sign, style) = match toast.kind {
                ToastKind::Info => (t.g.dot, t.accent()),
                ToastKind::Good => (t.g.check, t.good()),
                ToastKind::Warning => (t.g.warn, t.warning()),
                ToastKind::Error => (t.g.cross, t.danger()),
            };
            let room = area.width.saturating_sub(8).min(88) as usize;
            let mut lines = text::wrap(&text::line(&toast.text), room);
            let rest = lines.len().saturating_sub(4);
            lines.truncate(4);
            if rest > 0
                && let Some(last) = lines.last_mut()
            {
                last.push_str(t.g.ellipsis);
            }
            let inner = lines.iter().map(|l| text::width(l)).max().unwrap_or(0) as u16;
            let width = (inner + 6).min(area.width);
            let height = lines.len() as u16 + 2;
            let Some(y) = bottom.checked_sub(height).filter(|y| *y > area.y) else {
                break;
            };
            let rect = Rect {
                x: area.right().saturating_sub(width + 1).max(area.x),
                y,
                width,
                height,
            };
            let rect = rect.intersection(area);
            self.frame.render_widget(Clear, rect);
            let block = Block::bordered().border_set(t.g.border).border_style(style);
            let body = block.inner(rect);
            self.frame.render_widget(block, rect);
            let shown: Vec<Line> = lines
                .into_iter()
                .enumerate()
                .map(|(i, l)| {
                    Line::from(vec![
                        Span::styled(
                            format!(" {} ", if i == 0 { sign } else { " " }),
                            style.add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(l),
                    ])
                })
                .collect();
            self.frame.render_widget(Paragraph::new(shown), body);
            bottom = y;
        }
    }

    /// A title of a pane: bold, and marked when it has the focus.
    pub fn title(&self, title: &str, focused: bool) -> Vec<Span<'static>> {
        let t = self.t;
        vec![
            Span::styled(if focused { t.g.bar } else { " " }, t.accent()),
            Span::styled(
                title.to_owned(),
                match focused {
                    true => t.accent_bold(),
                    false => t.bold(),
                },
            ),
        ]
    }

    /// An editor in one line, with the terminal's cursor in it.
    pub fn input(&mut self, area: Rect, editor: &Editor, placeholder: &str, focused: bool) {
        if area.is_empty() {
            return;
        }
        let (shown, column) = editor.window(area.width as usize);
        let line = match editor.text.is_empty() {
            true => Line::styled(
                text::cut(placeholder, area.width as usize, self.t.g.ellipsis),
                self.t.dim(),
            ),
            false => Line::raw(shown),
        };
        self.put(area, line);
        if focused {
            self.frame.set_cursor_position((
                (area.x + column as u16).min(area.right().saturating_sub(1)),
                area.y,
            ));
        }
    }

    /// An editor of several lines. Scrolls to the cursor.
    pub fn text_area(&mut self, area: Rect, editor: &Editor, placeholder: &str, focused: bool) {
        if area.is_empty() {
            return;
        }
        let visual = editor.visual(area.width as usize);
        let rows = area.height as usize;
        let offset = (visual.row + 1).saturating_sub(rows);
        let lines: Vec<Line> = match editor.text.is_empty() {
            true => vec![Line::styled(placeholder.to_owned(), self.t.dim())],
            false => visual
                .lines
                .iter()
                .skip(offset)
                .take(rows)
                .map(|l| Line::raw(l.clone()))
                .collect(),
        };
        self.frame.render_widget(Paragraph::new(lines), area);
        if focused {
            self.frame.set_cursor_position((
                (area.x + visual.column as u16).min(area.right().saturating_sub(1)),
                area.y + (visual.row - offset) as u16,
            ));
        }
    }

    /// A centered sign and sentence for a pane without rows.
    pub fn message(&mut self, area: Rect, sign: &str, style: Style, text_: &str, action: &str) {
        let width = area.width.saturating_sub(4).min(64) as usize;
        if width == 0 {
            return;
        }
        let mut lines: Vec<Line> = Vec::new();
        if !sign.is_empty() {
            lines.push(Line::styled(sign.to_owned(), style).centered());
        }
        for l in text::wrap(&text::line(text_), width) {
            lines.push(Line::raw(l).centered());
        }
        if !action.is_empty() {
            lines.push(Line::default());
            lines.push(Line::styled(action.to_owned(), self.t.dim()).centered());
        }
        let height = (lines.len() as u16).min(area.height);
        let rect = Rect {
            y: area.y + (area.height - height) / 3,
            height,
            ..area
        };
        self.frame.render_widget(Paragraph::new(lines), rect);
    }
}
