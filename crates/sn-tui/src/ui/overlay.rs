//! What lies over the workspace: palette, menus, forms, review, help.

use super::{Hint, Hit, Pen, hint, scrolled};
use crate::app::state::*;
use crate::keys::{self, Act, BINDINGS, Place};
use crate::query::Join;
use crate::tables::SECTIONS;
use crate::text;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// Under the field it belongs to, or over it when there is no room.
fn under(area: Rect, anchor: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    let below = area.bottom().saturating_sub(anchor.bottom() + 1);
    let y = match below >= height || below >= anchor.y.saturating_sub(area.y) {
        true => anchor
            .bottom()
            .min(area.bottom().saturating_sub(height + 1)),
        false => anchor.y.saturating_sub(height),
    };
    Rect {
        x: anchor
            .x
            .saturating_sub(2)
            .min(area.right().saturating_sub(width + 1)),
        y: y.max(area.y),
        width,
        height,
    }
}

impl Pen<'_, '_> {
    pub(super) fn overlays(&mut self, area: Rect) {
        let app = self.app;
        let Some(top) = app.top() else {
            return;
        };
        // Only the top one is drawn, except that a picker stands on its form.
        if let (Overlay::Picker(_), Some(under)) = (
            top,
            app.overlays.len().checked_sub(2).map(|i| &app.overlays[i]),
        ) {
            self.step_back(area);
            self.overlay(area, under, false);
        }
        // A list of values stands on its field, which stays readable.
        if !matches!(top, Overlay::Picker(_)) {
            self.step_back(area);
        }
        self.hit(area, Hit::Outside);
        self.overlay(area, top, true);
    }

    /// What lies under an overlay turns dim.
    pub(super) fn step_back(&mut self, area: Rect) {
        let back = Style::new()
            .add_modifier(Modifier::DIM)
            .remove_modifier(Modifier::BOLD);
        let above_footer = Rect {
            height: area.height.saturating_sub(1),
            ..area
        };
        self.frame.buffer_mut().set_style(above_footer, back);
    }

    fn overlay(&mut self, area: Rect, overlay: &Overlay, top: bool) {
        match overlay {
            Overlay::Help(scroll) => self.help(area, *scroll),
            Overlay::Goto => self.goto(area),
            Overlay::Palette(palette) => self.palette(area, palette),
            Overlay::Menu { items, cursor } => self.menu(area, items, *cursor),
            Overlay::Profiles(cursor) => self.profiles(area, *cursor),
            Overlay::Sort(cursor) => self.sort(area, *cursor),
            Overlay::Query(editor) => self.query(area, editor),
            Overlay::Filter(filter) => self.filter(area, filter, top),
            Overlay::Form(form) => self.form(area, form, top),
            Overlay::Picker(picker) => self.picker(area, picker),
            Overlay::Text { field, editor } => {
                let height = (area.height * 2 / 3).max(8);
                let inner = self.open(area, area.width.min(96), height, &field.label);
                self.text_area(inner, editor, "Type the text", true);
            }
            Overlay::Read {
                title,
                text,
                scroll,
            } => self.read(area, title, text, *scroll),
            Overlay::Review(review) => self.review(area, review),
            Overlay::Confirm { text, yes, .. } => self.confirm(area, text, yes),
        }
    }

    /// A centered card that takes clicks. Gives the area inside it.
    fn open(&mut self, area: Rect, width: u16, height: u16, title: &str) -> Rect {
        let rect = centered(area, width, height);
        let border = self.t.accent();
        self.hit(rect, Hit::Overlay);
        self.card(rect, title, border)
    }

    pub(super) fn overlay_hints(&self, overlay: &Overlay) -> Vec<Hint> {
        let arrows = match self.t.caps.unicode {
            true => "↑ ↓",
            false => "Up Down",
        };
        let key = |code, mods| Some(Hit::Button(KeyEvent::new(code, mods)));
        let save: Hint = (
            "Ctrl-S".into(),
            "review".into(),
            key(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        let close: Hint = (
            "Esc".into(),
            "close".into(),
            key(KeyCode::Esc, KeyModifiers::NONE),
        );
        match overlay {
            Overlay::Help(_) | Overlay::Read { .. } => vec![hint(arrows, "scroll"), close],
            Overlay::Goto => vec![hint("letter", "go there"), close],
            Overlay::Palette(_) => vec![hint("Enter", "open"), hint(arrows, "choose"), close],
            Overlay::Menu { .. } => vec![
                hint("Enter", "do it"),
                hint("letter", "do it at once"),
                hint(arrows, "choose"),
                close,
            ],
            Overlay::Profiles(_) => vec![hint("Enter", "switch"), hint(arrows, "choose"), close],
            Overlay::Sort(_) => vec![
                hint("Enter", "sort, or turn the order"),
                hint(arrows, "choose"),
                close,
            ],
            Overlay::Query(_) => vec![hint("Enter", "apply"), close],
            Overlay::Filter(f) if f.editing.is_some() => {
                vec![hint("Enter", "keep"), hint("Esc", "cancel")]
            }
            Overlay::Filter(_) => vec![
                (
                    "Ctrl-S".into(),
                    "apply".into(),
                    key(KeyCode::Char('s'), KeyModifiers::CONTROL),
                ),
                hint("Enter", "edit"),
                (
                    "n".into(),
                    "add".into(),
                    key(KeyCode::Char('n'), KeyModifiers::NONE),
                ),
                (
                    "d".into(),
                    "delete".into(),
                    key(KeyCode::Char('d'), KeyModifiers::NONE),
                ),
                (
                    "o".into(),
                    "and / or".into(),
                    key(KeyCode::Char('o'), KeyModifiers::NONE),
                ),
                close,
            ],
            Overlay::Form(form) => {
                let mut out = vec![save];
                if form.fields.len() > 1 {
                    out.push(hint("Tab", "next field"));
                }
                if form.fields.get(form.cursor).is_some_and(|f| !f.is_typed()) {
                    out.push(hint("Enter", "choose"));
                }
                out.push(close);
                out
            }
            Overlay::Picker(_) => vec![
                hint("Enter", "choose"),
                hint(arrows, "move"),
                hint("type", "to search"),
                close,
            ],
            Overlay::Text { .. } => vec![
                (
                    "Ctrl-S".into(),
                    "keep".into(),
                    key(KeyCode::Char('s'), KeyModifiers::CONTROL),
                ),
                hint("Esc", "keep"),
                hint("u", "in the record undoes it"),
            ],
            Overlay::Review(review) => match review.state {
                ReviewState::Sending(..) => vec![hint("", "waiting for the instance")],
                _ => vec![
                    (
                        "Enter".into(),
                        review.verb.to_lowercase(),
                        key(KeyCode::Enter, KeyModifiers::NONE),
                    ),
                    (
                        "Esc".into(),
                        "back".into(),
                        key(KeyCode::Esc, KeyModifiers::NONE),
                    ),
                    hint(arrows, "scroll"),
                ],
            },
            Overlay::Confirm { yes, .. } => vec![hint("y", yes), hint("any key", "stay")],
        }
    }

    /// One line of a menu-like list, with its cursor and its place.
    fn item(&mut self, area: Rect, n: usize, here: bool, left: Vec<Span<'static>>, right: &str) {
        let t = self.t;
        let mut spans = vec![Span::styled(
            format!("{} ", if here { t.g.bar } else { " " }),
            t.accent(),
        )];
        spans.extend(left);
        let mut line = Line::from(spans);
        if here {
            line = line.patch_style(t.bold());
        }
        self.put(area, line);
        let width = text::width(right) as u16;
        if width > 0 && width + 12 < area.width {
            let spot = Rect {
                x: area.right() - width,
                width,
                ..area
            };
            self.frame.render_widget(ratatui::widgets::Clear, spot);
            let style = t.dim();
            self.put(spot, Line::styled(right.to_owned(), style));
        }
        self.hit(area, Hit::Item(n));
    }

    fn help(&mut self, area: Rect, scroll: usize) {
        let app = self.app;
        let t = self.t;
        let place = app.place();
        // The keys of the pane in focus come first.
        let mut order = vec![place];
        for p in [Place::List, Place::Record, Place::Anywhere, Place::Rail] {
            if !order.contains(&p) {
                order.push(p);
            }
        }
        let width = area.width.min(if area.width >= 130 { 124 } else { 72 });
        let columns = if width >= 120 { 2 } else { 1 };
        let column_width = (width.saturating_sub(4) / columns) as usize;
        let mut blocks: Vec<Vec<Line<'static>>> = Vec::new();
        for p in order {
            let mut block = vec![Line::styled(
                p.title().to_uppercase(),
                match p == place {
                    true => t.accent_bold(),
                    false => t.heading(),
                },
            )];
            for b in BINDINGS
                .iter()
                .filter(|b| b.place == p && !b.label.is_empty())
            {
                block.push(Line::from(vec![
                    Span::styled(
                        format!(" {} ", text::fit(&keys::label(b, t.caps.unicode), 10, "")),
                        t.accent_bold(),
                    ),
                    Span::raw(text::cut(
                        b.long,
                        column_width.saturating_sub(14),
                        t.g.ellipsis,
                    )),
                ]));
            }
            block.push(Line::default());
            blocks.push(block);
        }
        blocks.push(vec![
            Line::styled("TEXT FIELDS", t.heading()),
            Line::from(vec![
                Span::styled(
                    format!(" {} ", text::fit("Ctrl-W", 10, "")),
                    t.accent_bold(),
                ),
                Span::raw("Delete a word"),
            ]),
            Line::from(vec![
                Span::styled(
                    format!(" {} ", text::fit("Ctrl-U", 10, "")),
                    t.accent_bold(),
                ),
                Span::raw("Delete to the start of the line"),
            ]),
            Line::from(vec![
                Span::styled(
                    format!(" {} ", text::fit("Ctrl-S", 10, "")),
                    t.accent_bold(),
                ),
                Span::raw("Done: go on to the review"),
            ]),
            Line::default(),
            Line::styled("MOUSE", t.heading()),
            Line::raw(" Click to focus, click again to open, scroll to move."),
        ]);
        // Fill the first column to about half.
        let total: usize = blocks.iter().map(Vec::len).sum();
        let mut parts: Vec<Vec<Line<'static>>> = vec![Vec::new(); columns as usize];
        for block in blocks {
            let at = match columns {
                2 if parts[0].len() + block.len() / 2 > total / 2 && !parts[0].is_empty() => 1,
                _ => 0,
            };
            parts[at].extend(block);
        }
        let tall = parts.iter().map(Vec::len).max().unwrap_or(0);
        let inner = self.open(area, width, tall as u16 + 2, "Keys");
        let max = tall.saturating_sub(inner.height as usize);
        self.places.overlay_max_scroll = max;
        let scroll = scroll.min(max);
        let areas = Layout::horizontal(vec![Constraint::Fill(1); columns as usize]).split(inner);
        for (part, spot) in parts.into_iter().zip(areas.iter()) {
            let shown: Vec<Line> = part
                .into_iter()
                .skip(scroll)
                .take(inner.height as usize)
                .collect();
            self.frame.render_widget(Paragraph::new(shown), *spot);
        }
        self.more_below(inner, scroll < max);
    }

    fn read(&mut self, area: Rect, title: &str, body: &str, scroll: usize) {
        let width = area.width.saturating_sub(4).min(104);
        let lines = text::wrap(body, width.saturating_sub(4) as usize);
        let height = (lines.len() + 2).min(area.height as usize) as u16;
        let inner = self.open(area, width, height, title);
        let max = lines.len().saturating_sub(inner.height as usize);
        self.places.overlay_max_scroll = max;
        let scroll = scroll.min(max);
        let shown: Vec<Line> = lines
            .into_iter()
            .skip(scroll)
            .take(inner.height as usize)
            .map(Line::raw)
            .collect();
        self.frame.render_widget(Paragraph::new(shown), inner);
        self.more_below(inner, scroll < max);
    }

    /// Says that there is more under the edge.
    fn more_below(&mut self, inner: Rect, more: bool) {
        if !more || inner.height == 0 {
            return;
        }
        let label = format!(" {} more ", self.t.g.down);
        let width = text::width(&label) as u16;
        let spot = Rect {
            x: inner.right().saturating_sub(width),
            y: inner.bottom(),
            width: width.min(inner.width),
            height: 1,
        }
        .intersection(self.frame.area());
        let style = self.t.accent();
        self.put(spot, Line::styled(label, style));
    }

    fn goto(&mut self, area: Rect) {
        let t = self.t;
        let mut lines: Vec<(Option<usize>, Line<'static>)> = Vec::new();
        let mut group = "";
        let tall = SECTIONS.len() + 8 <= area.height as usize;
        for (i, s) in SECTIONS.iter().enumerate() {
            if s.group != group && tall {
                group = s.group;
                lines.push((None, Line::styled(group.to_uppercase(), t.heading())));
            }
            lines.push((
                Some(i),
                Line::from(vec![
                    Span::styled(format!(" {}  ", s.key), t.accent_bold()),
                    Span::raw(s.label),
                ]),
            ));
        }
        lines.push((
            None,
            Line::from(vec![
                Span::styled(" g  ", t.accent_bold()),
                Span::styled("first row", t.dim()),
            ]),
        ));
        // Two columns when the terminal is too low for one.
        let rows = (area.height as usize).saturating_sub(4).max(1);
        let columns = lines.len().div_ceil(rows).max(1);
        let per = lines.len().div_ceil(columns);
        let inner = self.open(area, columns as u16 * 26 + 4, per as u16 + 2, "Go to");
        for (n, (item, line)) in lines.into_iter().enumerate() {
            let spot = Rect {
                x: inner.x + (n / per) as u16 * 26,
                y: inner.y + (n % per) as u16,
                width: 26,
                height: 1,
            }
            .intersection(inner);
            self.put(spot, line);
            if let Some(i) = item {
                self.hit(spot, Hit::Item(i));
            }
        }
    }

    fn palette(&mut self, area: Rect, palette: &Palette) {
        let t = self.t;
        let items = self.app.palette_items(palette);
        let width = area.width.saturating_sub(4).min(88);
        let height = (area.height * 7 / 10).clamp(8, 24).min(area.height);
        let rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 4,
            width,
            height,
        };
        self.hit(rect, Hit::Overlay);
        let border = t.accent();
        let inner = self.card(
            rect,
            match palette.tables_only {
                true => "Open a table",
                false => "Find anything",
            },
            border,
        );
        let [input, rule, body] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(inner);
        self.put(input, Line::styled(t.g.crumb, t.accent_bold()));
        let field = Rect {
            x: input.x + 2,
            width: input.width.saturating_sub(2),
            ..input
        };
        self.input(
            field,
            &palette.editor,
            match palette.tables_only {
                true => "label or name of any table, custom ones too",
                false => "a number, words of a record, a section, a table, or an action",
            },
            true,
        );
        self.hline(rule);
        let searching = palette.tables.is_loading()
            || palette.records.is_loading()
            || palette.search_at.is_some();
        if items.is_empty() {
            let text_ = match searching {
                true => "Searching the instance",
                false => "Nothing has these words. Try fewer letters, or a record number.",
            };
            let sign = match searching {
                true => t.spinner(self.app.ms as usize / 100),
                false => t.g.ring,
            };
            self.message(body, sign, t.dim(), text_, "");
            return;
        }
        let failed = [
            match &palette.records {
                Load::Failed(fault) => Some(format!("Records: {}", fault.message)),
                _ => None,
            },
            match &palette.tables {
                Load::Failed(fault) if palette.tables_only => {
                    Some(format!("Tables: {}", fault.message))
                }
                _ => None,
            },
        ];
        let note = match failed.into_iter().flatten().next() {
            Some(text_) => Some((text_, t.danger())),
            None if searching && !palette.editor.text.trim().is_empty() => Some((
                format!(
                    "{} searching the instance",
                    t.spinner(self.app.ms as usize / 100)
                ),
                t.dim(),
            )),
            None => None,
        };
        let [list, status] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(u16::from(note.is_some())),
        ])
        .areas(body);
        if let Some((text_, style)) = note {
            let line = text::cut(&text::line(&text_), status.width as usize, t.g.ellipsis);
            self.put(status, Line::styled(line, style));
        }
        let cursor = palette.cursor.min(items.len() - 1);
        let rows = list.height as usize;
        let offset = scrolled(self.app.places.overlay_offset, cursor, rows, items.len());
        self.places.overlay_offset = offset;
        for (spot, (n, item)) in list.rows().zip(items.iter().enumerate().skip(offset)) {
            let left = vec![
                Span::styled(text::fit(item.group, 8, ""), t.dim()),
                Span::raw(text::cut(
                    &text::line(&item.title),
                    (list.width as usize).saturating_sub(12 + text::width(&item.detail)),
                    t.g.ellipsis,
                )),
            ];
            self.item(spot, n, n == cursor, left, &item.detail);
        }
    }

    fn menu(&mut self, area: Rect, items: &[Act], cursor: usize) {
        let app = self.app;
        let t = self.t;
        let targets = app.targets();
        let title = match targets.as_slice() {
            [one] => one.name.clone(),
            many => format!("{} records", many.len()),
        };
        let inner = self.open(area, 52, items.len() as u16 + 2, &title);
        let rows = inner.height as usize;
        let offset = scrolled(0, cursor, rows, items.len());
        for (spot, (n, act)) in inner.rows().zip(items.iter().enumerate().skip(offset)) {
            let Some(binding) = keys::binding(*act, app.place()) else {
                continue;
            };
            let key = keys::label(binding, t.caps.unicode);
            self.item(spot, n, n == cursor, vec![Span::raw(binding.long)], &key);
        }
    }

    fn profiles(&mut self, area: Rect, cursor: usize) {
        let app = self.app;
        let t = self.t;
        let inner = self.open(area, 64, app.profiles.len() as u16 + 2, "Profiles");
        let rows = inner.height as usize;
        let offset = scrolled(0, cursor, rows, app.profiles.len());
        let names = app
            .profiles
            .iter()
            .map(|p| text::width(&p.name))
            .max()
            .unwrap_or(0)
            .min(22);
        for (spot, (n, profile)) in inner
            .rows()
            .zip(app.profiles.iter().enumerate().skip(offset))
        {
            let current = profile.name == app.connection.profile;
            let mut left = vec![
                Span::styled(
                    format!("{} ", if current { t.g.dot } else { " " }),
                    t.good(),
                ),
                Span::raw(text::fit(&text::line(&profile.name), names, t.g.ellipsis)),
                Span::styled(format!("  {}", text::line(&profile.host)), t.dim()),
            ];
            if profile.read_only {
                left.push(Span::styled("  read-only", t.warning()));
            }
            self.item(
                spot,
                n,
                n == cursor,
                left,
                if current { "in use" } else { "" },
            );
        }
    }

    fn sort(&mut self, area: Rect, cursor: usize) {
        let app = self.app;
        let t = self.t;
        let columns = &app.list.columns;
        let inner = self.open(area, 40, columns.len() as u16 + 2, "Sort by");
        let rows = inner.height as usize;
        let offset = scrolled(0, cursor, rows, columns.len());
        for (spot, (n, column)) in inner.rows().zip(columns.iter().enumerate().skip(offset)) {
            let order = app
                .list
                .order
                .iter()
                .find(|(f, _)| *f == column.field)
                .map(|(_, down)| if *down { t.g.down } else { t.g.up })
                .unwrap_or_default();
            self.item(
                spot,
                n,
                n == cursor,
                vec![Span::raw(column.label.clone())],
                order,
            );
        }
    }

    fn query(&mut self, area: Rect, editor: &crate::input::Editor) {
        let t = self.t;
        let inner = self.open(area, 100, 6, "Encoded query");
        let [input, _, note] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(inner);
        self.input(
            input,
            editor,
            "active=true^priority<=2^ORDERBYDESCsys_updated_on",
            true,
        );
        self.put(
            note,
            Line::styled(
                text::cut(
                    "The instance ignores a field it does not know. f builds a filter that cannot be wrong.",
                    note.width as usize,
                    t.g.ellipsis,
                ),
                t.dim(),
            ),
        );
    }

    fn filter(&mut self, area: Rect, filter: &FilterBuilder, top: bool) {
        let app = self.app;
        let t = self.t;
        let height = (filter.conditions.len() as u16 + 5).max(9);
        let inner = self.open(area, 92, height, &format!("Filter {}", app.list.title));
        let [list, foot] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
        let rows = list.height as usize;
        let total = filter.conditions.len() + 1;
        let offset = scrolled(0, filter.row, rows, total);
        let width = list.width as usize;
        let field_width = (width / 3).clamp(4, 28);
        let op_width = 18.min(width / 4);
        for (spot, n) in list.rows().zip(offset..total) {
            let here = n == filter.row;
            let bar = Span::styled(format!("{} ", if here { t.g.bar } else { " " }), t.accent());
            let Some(condition) = filter.conditions.get(n) else {
                let mut line = Line::from(vec![bar, Span::styled("+ Add a condition", t.accent())]);
                if here {
                    line = line.patch_style(t.bold());
                }
                self.put(spot, line);
                self.hit(spot, Hit::Item(n * 3));
                if here {
                    self.places.anchor = Some(Rect {
                        x: spot.x + 2,
                        ..spot
                    });
                }
                continue;
            };
            let join = match (n, condition.join) {
                (0, _) => "    ",
                (_, Join::And) => "and ",
                (_, Join::Or) => "or  ",
                (_, Join::New) => "OR  ",
            };
            let cell = |column: usize| match here && filter.column == column {
                true => t.accent_bold().add_modifier(Modifier::UNDERLINED),
                false => Style::new(),
            };
            let x = spot.x + 6;
            let value_at = x + (field_width + op_width) as u16 + 2;
            let value_spot = Rect {
                x: value_at.min(spot.right()),
                width: spot.right().saturating_sub(value_at),
                ..spot
            };
            let mut spans = vec![bar, Span::styled(join, t.dim())];
            match &condition.raw {
                Some(raw) => {
                    spans.push(Span::styled(
                        text::cut(raw, width.saturating_sub(7), t.g.ellipsis),
                        cell(filter.column),
                    ));
                    self.put(spot, Line::from(spans));
                }
                None => {
                    let label = &app.field(&filter.table, &condition.field).label;
                    spans.push(Span::styled(
                        text::fit(label, field_width, t.g.ellipsis),
                        cell(0),
                    ));
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        text::fit(condition.op.label(), op_width, t.g.ellipsis),
                        match here && filter.column == 1 {
                            true => cell(1),
                            false => t.dim(),
                        },
                    ));
                    spans.push(Span::raw(" "));
                    if condition.op.needs_value() {
                        spans.push(match condition.label.is_empty() {
                            true => Span::styled("choose a value", t.warning()),
                            false => Span::styled(
                                text::cut(
                                    &text::line(&condition.label),
                                    value_spot.width as usize,
                                    t.g.ellipsis,
                                ),
                                cell(2),
                            ),
                        });
                    }
                    self.put(spot, Line::from(spans));
                }
            }
            self.hit(
                Rect {
                    x,
                    width: field_width as u16,
                    ..spot
                }
                .intersection(spot),
                Hit::Item(n * 3),
            );
            self.hit(
                Rect {
                    x: x + field_width as u16 + 1,
                    width: op_width as u16,
                    ..spot
                }
                .intersection(spot),
                Hit::Item(n * 3 + 1),
            );
            self.hit(value_spot, Hit::Item(n * 3 + 2));
            if here {
                self.places.anchor = Some(match filter.column {
                    0 => Rect { x, ..spot },
                    1 => Rect {
                        x: x + field_width as u16 + 1,
                        ..spot
                    },
                    _ => value_spot,
                });
            }
            if let Some(editor) = filter.editing.as_ref().filter(|_| here) {
                let place = match condition.raw {
                    Some(_) => Rect {
                        x,
                        width: spot.right().saturating_sub(x),
                        ..spot
                    },
                    None => value_spot,
                }
                .intersection(spot);
                self.frame.render_widget(ratatui::widgets::Clear, place);
                self.input(place, editor, "type a value", top);
            }
        }
        let words = crate::query::build(&filter.conditions);
        self.put(
            foot,
            Line::styled(
                text::cut(
                    match words.is_empty() {
                        true => "No condition: every record",
                        false => &words,
                    },
                    foot.width as usize,
                    t.g.ellipsis,
                ),
                t.dim(),
            ),
        );
    }

    fn form(&mut self, area: Rect, form: &Form, top: bool) {
        let t = self.t;
        let long = form.fields.iter().filter(|f| f.editor.multiline).count() as u16;
        // Long texts share the room that is left.
        let count = form.fields.len() as u16;
        let short = count - long;
        let fixed = short * 2 + long + 3;
        // A blank line between fields, when there is room for it.
        let gap = u16::from(area.height >= fixed + count + long * 3 + 4);
        let each = match long {
            0 => 0,
            n => {
                let room = area.height.saturating_sub(6 + fixed + count * gap);
                (room / n).clamp(2, if n == 1 { 10 } else { 6 })
            }
        };
        let height = fixed + count * gap + long * each + 1;
        let inner = self.open(area, 84, height, &form.title);
        let mut y = inner.y;
        for (i, field) in form.fields.iter().enumerate() {
            let here = i == form.cursor;
            let focused = here && top;
            let mut head = vec![
                Span::styled(if here { t.g.bar } else { " " }, t.accent()),
                Span::styled(
                    field.field.label.clone(),
                    match here {
                        true => t.accent_bold(),
                        false => t.bold(),
                    },
                ),
            ];
            if field.required {
                head.push(Span::styled(" *", t.warning()));
            }
            if !field.hint.is_empty() {
                head.push(Span::styled(format!("  {}", field.hint), t.dim()));
            }
            let line = |y: u16, height: u16| Rect {
                x: inner.x,
                y,
                width: inner.width,
                height,
            };
            if field.editor.multiline {
                let spot = line(y, 1).intersection(inner);
                self.put(spot, Line::from(head));
                self.hit(spot, Hit::Item(i));
                let body = Rect {
                    x: inner.x + 1,
                    width: inner.width.saturating_sub(2),
                    ..line(y + 1, each)
                }
                .intersection(inner);
                self.hit(body, Hit::Item(i));
                self.text_area(
                    body,
                    &field.editor,
                    match field.required {
                        true => "Write here",
                        false => "Optional",
                    },
                    focused,
                );
                y = y.saturating_add(each + 1 + gap);
            } else {
                let spot = line(y, 1).intersection(inner);
                self.put(spot, Line::from(head));
                let body = line(y + 1, 1).intersection(inner);
                let value = Rect {
                    x: body.x + 1,
                    width: body.width.saturating_sub(1),
                    ..body
                };
                if field.is_typed() {
                    self.input(value, &field.editor, "type here", focused);
                } else {
                    let shown = text::line(&field.cell.display);
                    self.put(
                        value,
                        match shown.is_empty() {
                            true => Line::styled("press Enter to choose, or type a name", t.dim()),
                            false => Line::from(vec![
                                Span::raw(shown),
                                Span::styled(format!("  {}", t.g.down), t.dim()),
                            ]),
                        },
                    );
                }
                self.hit(spot.union(body).intersection(inner), Hit::Item(i));
                if here {
                    self.places.anchor = Some(value);
                }
                y = y.saturating_add(2 + gap);
            }
        }
        if let Some(error) = &form.error {
            let spot = Rect {
                x: inner.x,
                y: inner.bottom().saturating_sub(1),
                width: inner.width,
                height: 1.min(inner.height),
            };
            self.frame.render_widget(ratatui::widgets::Clear, spot);
            let line = format!("{} {error}", t.g.warn);
            self.put(spot, Line::styled(line, t.danger()));
        }
    }

    fn picker(&mut self, area: Rect, picker: &Picker) {
        let t = self.t;
        let items = picker.items();
        let wanted = match &picker.source {
            PickSource::Fixed(all) => all.len() + 1,
            PickSource::Table(_) => 10,
        };
        let height = (wanted as u16 + 4)
            .min(area.height.saturating_sub(4))
            .clamp(6, 18);
        let rect = match self.places.anchor {
            Some(anchor) => under(area, anchor, 58, height),
            None => centered(area, 64, height),
        };
        self.hit(rect, Hit::Overlay);
        let border = t.accent();
        let inner = self.card(rect, &picker.title, border);
        let [input, rule, list] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(inner);
        self.put(input, Line::styled(t.g.crumb, t.accent_bold()));
        let field = Rect {
            x: input.x + 2,
            width: input.width.saturating_sub(2),
            ..input
        };
        self.input(field, &picker.editor, "type to search", true);
        self.hline(rule);
        if items.is_empty() {
            match &picker.found {
                Load::Failed(fault) => {
                    self.message(list, t.g.cross, t.danger(), &fault.message, "")
                }
                Load::Ready(_) => {
                    self.message(list, t.g.ring, t.dim(), "Nothing has this name.", "");
                }
                _ if matches!(picker.source, PickSource::Fixed(_)) => {
                    self.message(list, t.g.ring, t.dim(), "Nothing has this name.", "");
                }
                _ => {
                    let sign = t.spinner(self.app.ms as usize / 100);
                    self.message(list, sign, t.accent(), "Searching", "");
                }
            }
            return;
        }
        let cursor = picker.cursor.min(items.len() - 1);
        let rows = list.height as usize;
        let offset = scrolled(self.app.places.overlay_offset, cursor, rows, items.len());
        self.places.overlay_offset = offset;
        for (spot, (n, item)) in list.rows().zip(items.iter().enumerate().skip(offset)) {
            let room = (list.width as usize).saturating_sub(2);
            let label = text::cut(&text::line(&item.label), room, t.g.ellipsis);
            let rest = room.saturating_sub(text::width(&label) + 2);
            let detail = text::cut(&text::line(&item.detail), rest, t.g.ellipsis);
            self.item(spot, n, n == cursor, vec![Span::raw(label)], &detail);
        }
    }

    fn review(&mut self, area: Rect, review: &Review) {
        let t = self.t;
        let width = area.width.saturating_sub(4).min(92);
        let room = width.saturating_sub(6) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();
        let count = review.names.len();
        for name in review.names.iter().take(8) {
            lines.push(Line::styled(
                text::cut(name, room + 2, t.g.ellipsis),
                t.bold(),
            ));
        }
        if count > 8 {
            lines.push(Line::styled(format!("and {} more", count - 8), t.dim()));
        }
        if count > 0 {
            lines.push(Line::default());
        }
        for change in &review.lines {
            let mut head = vec![Span::styled(change.label.clone(), t.accent_bold())];
            if change.moved {
                head.push(Span::styled("  changed by someone else", t.warning()));
            }
            lines.push(Line::from(head));
            let old = text::block(&change.old);
            let new = match change.new.trim().is_empty() {
                true => "(empty)".to_owned(),
                false => text::block(&change.new),
            };
            if !change.adds {
                let old = match old.trim().is_empty() {
                    true => "(empty)".to_owned(),
                    false => old,
                };
                let old_lines = text::wrap(&old, room);
                let more = old_lines.len().saturating_sub(4);
                for part in old_lines.into_iter().take(4) {
                    lines.push(Line::from(vec![
                        Span::styled("  - ", t.danger()),
                        Span::styled(part, t.dim()),
                    ]));
                }
                if more > 0 {
                    lines.push(Line::styled(format!("    {more} more lines"), t.dim()));
                }
            }
            for part in text::wrap(&new, room) {
                lines.push(Line::from(vec![
                    Span::styled("  + ", t.good()),
                    Span::raw(part),
                ]));
            }
            lines.push(Line::default());
        }
        let mut status: Vec<Line<'static>> = Vec::new();
        match &review.state {
            ReviewState::Ready => status.push(Line::styled(
                match &review.work {
                    ReviewWork::Write(w) if w.len() > 1 => {
                        format!("Each of the {} records gets this change.", w.len())
                    }
                    ReviewWork::Write(_) => "Nothing is sent before you press Enter.".into(),
                    ReviewWork::Create(table, _) => format!("This creates one record in {table}."),
                },
                t.dim(),
            )),
            ReviewState::Sending(_, done) => {
                let total = match &review.work {
                    ReviewWork::Write(w) => w.len(),
                    ReviewWork::Create(..) => 1,
                };
                status.push(Line::styled(
                    match total {
                        1 => format!("{} Sending", t.spinner(self.app.ms as usize / 100)),
                        n => format!(
                            "{} Sending {done} of {n}",
                            t.spinner(self.app.ms as usize / 100)
                        ),
                    },
                    t.accent_bold(),
                ));
            }
            ReviewState::Failed(problems) => {
                for problem in problems.iter().take(5) {
                    for part in text::wrap(&text::line(problem), room + 2) {
                        status.push(Line::styled(part, t.danger()));
                    }
                }
                status.push(Line::styled("Enter sends it again.", t.dim()));
            }
            ReviewState::Conflict(who, when) => {
                let sentence = format!(
                    "{} {who} changed this record {when}. Nothing was sent. The old values above are the ones it has now; Enter puts yours over them.",
                    t.g.warn,
                );
                for part in text::wrap(&sentence, room + 2) {
                    status.push(Line::styled(part, t.warning()));
                }
            }
        }
        let button = match review.state {
            ReviewState::Sending(..) => None,
            _ => Some(format!(" {} ", review.verb)),
        };
        let height = lines.len() + status.len() + 3;
        let inner = self.open(area, width, height as u16, &review.title);
        let [body, _, status_area, buttons] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(0),
            Constraint::Length((status.len() as u16).min(inner.height / 2)),
            Constraint::Length(1),
        ])
        .areas(inner);
        let max = lines.len().saturating_sub(body.height as usize);
        self.places.overlay_max_scroll = max;
        let scroll = review.scroll.min(max);
        let shown: Vec<Line> = lines
            .into_iter()
            .skip(scroll)
            .take(body.height as usize)
            .collect();
        self.frame.render_widget(Paragraph::new(shown), body);
        self.frame
            .render_widget(Paragraph::new(status), status_area);
        if let Some(button) = button {
            let width = text::width(&button) as u16;
            let spot = Rect {
                x: buttons.right().saturating_sub(width),
                width: width.min(buttons.width),
                ..buttons
            };
            let badge = t.badge();
            self.put(spot, Line::styled(button, badge));
            self.hit(
                spot,
                Hit::Button(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            );
            let back = Rect {
                x: spot.x.saturating_sub(8).max(buttons.x),
                width: 6.min(spot.x.saturating_sub(buttons.x)),
                ..buttons
            };
            let dim = t.dim();
            self.put(back, Line::styled(" Back ", dim));
            self.hit(
                back,
                Hit::Button(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            );
        }
        if scroll < max {
            let spot = Rect {
                y: body.bottom().saturating_sub(1),
                height: 1.min(body.height),
                ..body
            };
            let label = format!("{} more", t.g.down);
            let width = text::width(&label) as u16;
            let spot = Rect {
                x: spot.right().saturating_sub(width),
                width: width.min(spot.width),
                ..spot
            };
            let accent = t.accent();
            self.put(spot, Line::styled(label, accent));
        }
    }

    fn confirm(&mut self, area: Rect, sentence: &str, yes: &str) {
        let t = self.t;
        let width = area.width.saturating_sub(4).min(60);
        let mut lines: Vec<Line> = text::wrap(sentence, width.saturating_sub(4) as usize)
            .into_iter()
            .map(Line::raw)
            .collect();
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled("y", t.accent_bold()),
            Span::styled(format!(" {yes}      "), t.dim()),
            Span::styled("any other key", t.accent_bold()),
            Span::styled(" stay", t.dim()),
        ]));
        let rect = centered(area, width, lines.len() as u16 + 2);
        self.hit(rect, Hit::Overlay);
        let border = t.warning();
        let inner = self.card(rect, "Changes not sent", border);
        let foot = Rect {
            y: inner.bottom().saturating_sub(1),
            height: 1.min(inner.height),
            width: inner.width.min(text::width(yes) as u16 + 2),
            ..inner
        };
        self.frame.render_widget(Paragraph::new(lines), inner);
        self.hit(
            foot,
            Hit::Button(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        );
    }
}
