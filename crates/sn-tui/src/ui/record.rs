//! The open record.

use super::{Hit, Pen, scrolled};
use crate::app::state::*;
use crate::model::{Detail, Kind, Row};
use crate::msg::{EntryKind, TAB_LIMIT};
use crate::tables;
use crate::text;
use crate::theme::{self, Tone};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Lines of a long value on the Details tab.
const LONG_LINES: usize = 6;

/// A row of the Related tab.
struct Parts {
    id: String,
    about: String,
    end: String,
    tone: Tone,
}

impl Pen<'_, '_> {
    pub(super) fn record(&mut self, area: Rect) {
        let app = self.app;
        let Some(record) = &app.record else { return };
        self.hit(area, Hit::Pane(Pane::Record));
        let width = area.width as usize;
        let summary = match record.is_new() {
            true => "Fill in the fields, then press Ctrl-S to review and create it.".to_owned(),
            false => record.row.summary(),
        };
        let mut sentence = text::wrap(&summary, width.saturating_sub(2));
        let max = if area.height >= 20 { 2 } else { 1 };
        if sentence.len() > max {
            sentence.truncate(max);
            if let Some(last) = sentence.last_mut() {
                *last = text::cut(
                    &format!("{last}{}", self.t.g.ellipsis),
                    width.saturating_sub(2),
                    self.t.g.ellipsis,
                );
            }
        }
        if summary.is_empty() {
            sentence.clear();
        }
        let chips = !record.is_new();
        let status = match chips {
            true => self.chips(&record.row, area.width),
            false => Line::default(),
        };
        let air = u16::from(area.height >= 20);
        let [title, about, chip_area, _, tabs, rule, body] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(sentence.len() as u16),
            Constraint::Length(u16::from(status.width() > 0)),
            Constraint::Length(air),
            Constraint::Length(u16::from(chips)),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);
        self.record_title(title, record);
        let lines: Vec<Line> = sentence
            .into_iter()
            .map(|l| {
                Line::styled(
                    format!(" {l}"),
                    match record.is_new() {
                        true => self.t.dim(),
                        false => self.t.bold(),
                    },
                )
            })
            .collect();
        self.frame.render_widget(Paragraph::new(lines), about);
        self.put(chip_area, status);
        self.tabs(tabs, record);
        self.hline(rule);
        if let Load::Failed(fault) = &record.full {
            let (danger, sign) = (self.t.danger(), self.t.g.cross);
            self.message(body, sign, danger, &fault.message, "Enter tries again");
            return;
        }
        match record.tab {
            Tab::Details => self.details(body, record),
            Tab::Activity => self.activity(body, record),
            Tab::Related => self.related(body, record),
            Tab::Attachments => self.files(body, record),
            Tab::Slas => self.slas(body, record),
            Tab::History => self.history(body, record),
        }
    }

    fn record_title(&mut self, area: Rect, record: &RecordState) {
        let app = self.app;
        let t = self.t;
        let focused = app.focus == Pane::Record && app.overlays.is_empty();
        let mut spans = vec![Span::styled(
            if focused { t.g.bar } else { " " },
            t.accent(),
        )];
        for under in &app.records {
            spans.push(Span::styled(
                format!(
                    "{} {} ",
                    text::cut(&under.name(), 16, t.g.ellipsis),
                    t.g.crumb
                ),
                t.dim(),
            ));
        }
        spans.push(Span::styled(record.name(), t.accent_bold()));
        if !record.is_new() {
            let class = match record.row.display("sys_class_name") {
                "" => {
                    let one = tables::one(&record.table);
                    let mut chars = one.chars();
                    chars
                        .next()
                        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
                }
                class => text::line(class),
            };
            spans.push(Span::styled(format!("  {class}"), t.dim()));
        }
        self.put(area, Line::from(spans));
        let right = match record.edits.len() {
            0 => match text::parse_utc(record.row.value("sys_updated_on")) {
                Some(at) => {
                    let by = text::line(record.row.display("sys_updated_by"));
                    let ago = text::since(at, app.now);
                    let by = match by.is_empty() {
                        true => by,
                        false => format!(" by {by}"),
                    };
                    Span::styled(format!("updated {ago}{by}"), t.dim())
                }
                None => return,
            },
            n => Span::styled(
                format!(" {} {} ", t.g.pencil, text::plural(n, "change", "changes")),
                t.badge_warning(),
            ),
        };
        let width = right.width() as u16;
        if width + 24 <= area.width {
            let spot = Rect {
                x: area.right() - width,
                width,
                ..area
            };
            self.put(spot, Line::from(right));
            if !record.edits.is_empty() {
                self.hit(spot, Hit::Act(crate::keys::Act::Review));
            }
        }
    }

    /// State, priority and owner.
    fn chips(&self, row: &Row, width: u16) -> Line<'static> {
        let t = self.t;
        let mut spans: Vec<Span> = vec![Span::raw(" ")];
        let mut add = |parts: Vec<Span<'static>>| {
            if spans.len() > 1 {
                spans.push(Span::raw("   "));
            }
            spans.extend(parts);
        };
        for field in [
            "state",
            "request_state",
            "workflow_state",
            "operational_status",
        ] {
            let label = text::line(row.display(field));
            // Codes without a label say nothing.
            if !label.is_empty() && !label.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
                let tone = theme::state(&label);
                let sign = match theme::sign(&t, tone) {
                    " " => t.g.dot,
                    sign => sign,
                };
                add(vec![Span::styled(
                    format!("{sign} {label}"),
                    t.tone(tone).add_modifier(Modifier::BOLD),
                )]);
                break;
            }
        }
        let priority = row.cell("priority");
        if !priority.value.is_empty() {
            let tone = theme::priority(&priority.value);
            let sign = theme::priority_sign(&t, &priority.value).trim();
            let label = theme::priority_label(&text::line(&priority.display));
            add(vec![Span::styled(
                format!("{sign} {label}").trim().to_owned(),
                t.tone(tone),
            )]);
        }
        let owner = text::line(row.display("assigned_to"));
        let group = text::line(row.display("assignment_group"));
        match (owner.is_empty(), group.is_empty()) {
            (false, _) => {
                let mut parts = vec![Span::raw(owner)];
                if !group.is_empty() && width >= 70 {
                    parts.push(Span::styled(format!(" {} {group}", t.g.sep), t.dim()));
                }
                add(parts);
            }
            (true, false) => add(vec![
                Span::styled("unassigned", t.warning()),
                Span::styled(format!(" {} {group}", t.g.sep), t.dim()),
            ]),
            (true, true) => {
                if row.has("assigned_to") {
                    add(vec![Span::styled("unassigned", t.dim())]);
                }
                let approver = text::line(row.display("approver"));
                if !approver.is_empty() {
                    add(vec![Span::styled(format!("approver {approver}"), t.dim())]);
                }
                let author = text::line(row.display("author"));
                if !author.is_empty() {
                    add(vec![Span::styled(format!("by {author}"), t.dim())]);
                }
            }
        }
        match spans.len() {
            1 => Line::default(),
            _ => Line::from(spans),
        }
    }

    fn tabs(&mut self, area: Rect, record: &RecordState) {
        if area.is_empty() {
            return;
        }
        let t = self.t;
        let counted = |tab: Tab| match tab {
            Tab::Activity => record.activity.ready().map(Vec::len),
            Tab::Related => (!record.related.is_empty()
                && !record.related.iter().any(|(_, l)| l.is_loading()))
            .then(|| record.related_rows().len()),
            Tab::Attachments => record.attachments.ready().map(Vec::len),
            Tab::Slas => record.slas.ready().map(Vec::len),
            _ => None,
        };
        let name = |tab: Tab, count: bool| match counted(tab).filter(|_| count) {
            Some(n) => format!("{} {n}", tab.label()),
            None => tab.label().to_owned(),
        };
        // Full labels, then without counts, then closer together, then only
        // the open one.
        let total = |count: bool, pad: usize| {
            Tab::ALL
                .iter()
                .map(|tab| text::width(&name(*tab, count)) + pad)
                .sum::<usize>()
                .saturating_sub(1)
        };
        let width = area.width as usize;
        let count = total(true, 3) <= width;
        let airy = count || total(false, 3) <= width;
        let all = airy || total(false, 2) + 2 <= width;
        let mut x = area.x;
        for tab in Tab::ALL {
            let active = tab == record.tab;
            let label = match all || active {
                true if airy || !all || active => format!(" {} ", name(tab, count)),
                true => format!("{} ", name(tab, count)),
                false => format!(" {} ", tab.index() + 1),
            };
            let spot = Rect {
                x,
                width: text::width(&label) as u16,
                ..area
            }
            .intersection(area);
            let style = match (active, self.app.focus == Pane::Record) {
                (true, true) => t.badge(),
                (true, false) => t.bold().add_modifier(Modifier::UNDERLINED),
                _ => t.dim(),
            };
            let gap = u16::from(airy || !all);
            x = x.saturating_add(text::width(&label) as u16 + gap);
            self.put(spot, Line::from(Vec::from(Pen::underlined(&label, style))));
            self.hit(spot, Hit::Tab(tab));
        }
    }

    fn waiting(&mut self, area: Rect, what: &str) {
        let t = self.t;
        let sign = t.spinner(self.app.ms as usize / 100);
        self.message(area, sign, t.accent(), what, "");
    }

    /// Draws lines from the scroll position and reports how far it goes.
    fn scroll(&mut self, area: Rect, lines: Vec<Line<'static>>, at: usize) {
        let rows = area.height as usize;
        let max = lines.len().saturating_sub(rows);
        self.places.record_rows = rows;
        self.places.record_max_scroll = max;
        let offset = at.min(max);
        let shown: Vec<Line> = lines.into_iter().skip(offset).take(rows).collect();
        self.frame.render_widget(Paragraph::new(shown), area);
    }

    fn details(&mut self, area: Rect, record: &RecordState) {
        let app = self.app;
        let t = self.t;
        let width = area.width as usize;
        let label_width = (width * 36 / 100).clamp(8, 24);
        let value_width = width.saturating_sub(label_width + 4).max(1);
        let fields = app.detail_fields(record);
        if fields.is_empty() {
            match record.full.is_loading() {
                true => self.waiting(area, "Loading the record"),
                false => self.waiting(area, "Loading the fields"),
            }
            return;
        }
        let focused = app.focus == Pane::Record && app.overlays.is_empty();
        let mut lines: Vec<(Option<usize>, Line<'static>)> = Vec::new();
        let (mut cursor_top, mut cursor_bottom) = (0, 0);
        let mut inline_line = None;
        let mut index = 0;
        for detail in &fields {
            let name = match detail {
                Detail::Group(title) => {
                    if !lines.is_empty() {
                        lines.push((None, Line::default()));
                    }
                    lines.push((
                        None,
                        Line::styled(format!(" {}", title.to_uppercase()), t.heading()),
                    ));
                    continue;
                }
                Detail::Field(name) => name,
            };
            let field = app.field(&record.table, name);
            let here = index == record.cursor;
            let edited = record.edit(name).is_some();
            let editing = here
                .then_some(record.inline.as_ref())
                .flatten()
                .filter(|i| i.field.name == *name);
            let cell = record.cell(name);
            let mut shown = text::block(&cell.display);
            if field.html {
                shown = text::block(&text::html(&shown));
            }
            if field.kind == Kind::Bool {
                shown = match cell.value.as_str() {
                    "true" => format!("{} yes", t.g.check),
                    "false" => format!("{} no", t.g.dash),
                    _ => shown,
                };
            }
            let mut value: Vec<String> = text::wrap(&shown, value_width);
            let more = value.len().saturating_sub(LONG_LINES);
            if more > 0 {
                value.truncate(LONG_LINES);
                value.push(format!(
                    "{} {}, Enter shows all",
                    t.g.ellipsis,
                    text::plural(more, "more line", "more lines")
                ));
            }
            // A date says how far away it is, too.
            if matches!(field.kind, Kind::Date | Kind::DateTime)
                && let Some(at) = text::parse_utc(&cell.value)
                && let [only] = value.as_mut_slice()
            {
                only.push_str(&format!("  ({})", text::since(at, app.now)));
            }
            let style = match () {
                () if edited => t.warning().add_modifier(Modifier::BOLD),
                () if name == "state" || name.ends_with("_state") => t.tone(theme::state(&shown)),
                () if name == "priority" => t.tone(theme::priority(&cell.value)),
                () if field.kind == Kind::Reference => t.accent(),
                () if field.locked().is_some() && name.starts_with("sys_") => t.dim(),
                () => Style::new(),
            };
            if here {
                cursor_top = lines.len();
            }
            let required = field.mandatory && cell.value.is_empty();
            for (n, part) in value.into_iter().enumerate() {
                let mut label = match n {
                    0 => field.label.clone(),
                    _ => String::new(),
                };
                if n == 0 && field.mandatory {
                    label.push_str(" *");
                }
                let marker = match (here, edited) {
                    (true, _) => Span::styled(
                        t.g.bar,
                        match focused {
                            true => t.accent(),
                            false => t.dim(),
                        },
                    ),
                    (false, true) => Span::styled(t.g.vline, t.warning()),
                    _ => Span::raw(" "),
                };
                let label_style = match () {
                    () if here && focused => t.bold(),
                    () if required && record.is_new() => t.warning(),
                    () => t.dim(),
                };
                let text = match (n, part.is_empty(), required) {
                    (0, true, true) if record.is_new() => {
                        Span::styled("required", t.warning().add_modifier(Modifier::DIM))
                    }
                    (0, true, _) => Span::styled(t.g.dash, t.dim()),
                    _ => Span::styled(part, style),
                };
                lines.push((
                    Some(index),
                    Line::from(vec![
                        marker,
                        Span::styled(
                            format!("{}  ", text::fit(&label, label_width, t.g.ellipsis)),
                            label_style,
                        ),
                        text,
                    ]),
                ));
                // The old value stands beside the new one.
                if n == 0
                    && let Some(edit) = record.edit(name)
                    && !record.is_new()
                    && editing.is_none()
                    && let Some((_, line)) = lines.last_mut()
                {
                    let old = match edit.old.display.is_empty() {
                        true => "empty".to_owned(),
                        false => text::cut(&text::line(&edit.old.display), 24, t.g.ellipsis),
                    };
                    let room = width.saturating_sub(line.width());
                    let note = format!("  was {old}");
                    if text::width(&note) <= room {
                        line.push_span(Span::styled(note, t.dim()));
                    }
                }
            }
            if editing.is_some() {
                inline_line = Some(cursor_top);
                if let Some(error) = editing.and_then(|i| i.error.clone()) {
                    for part in text::wrap(&error, value_width) {
                        lines.push((
                            None,
                            Line::from(vec![
                                Span::raw(" ".repeat(label_width + 3)),
                                Span::styled(part, t.danger()),
                            ]),
                        ));
                    }
                }
            }
            if here {
                cursor_bottom = lines.len().saturating_sub(1);
            }
            index += 1;
        }
        lines.push((None, Line::default()));
        let hidden = match (record.show_empty, record.is_new()) {
            (false, false) => "Empty fields are hidden. z shows them.",
            (false, true) => "z shows every field of the table.",
            (true, _) => "Every field is shown. z hides the empty ones.",
        };
        lines.push((None, Line::styled(format!("  {hidden}"), t.dim())));
        let rows = area.height as usize;
        self.places.record_rows = rows;
        let mut offset = scrolled(app.places.record_offset, cursor_bottom, rows, lines.len());
        if cursor_top < offset {
            offset = cursor_top;
        }
        // The first field brings its heading along.
        if record.cursor == 0 {
            offset = 0;
        }
        self.places.record_offset = offset;
        for (row, (n, (item, line))) in area.rows().zip(lines.into_iter().enumerate().skip(offset))
        {
            self.put(row, line);
            if let Some(i) = item {
                self.hit(row, Hit::Detail(i));
            }
            if n == cursor_top {
                self.places.anchor = Some(Rect {
                    x: row.x + label_width as u16 + 1,
                    width: row.width.saturating_sub(label_width as u16 + 1),
                    ..row
                });
            }
            if inline_line == Some(n)
                && let Some(inline) = &record.inline
            {
                let spot = Rect {
                    x: row.x + label_width as u16 + 3,
                    width: row.width.saturating_sub(label_width as u16 + 4),
                    ..row
                }
                .intersection(area);
                self.frame.render_widget(ratatui::widgets::Clear, spot);
                let style = self.t.accent().add_modifier(Modifier::UNDERLINED);
                let (shown, _) = inline.editor.window(spot.width as usize);
                self.put(
                    spot,
                    Line::from(Vec::from(Pen::underlined(
                        &text::fit(&shown, spot.width as usize, ""),
                        style,
                    ))),
                );
                let (_, column) = inline.editor.window(spot.width as usize);
                if !spot.is_empty() && app.overlays.is_empty() {
                    self.frame.set_cursor_position((
                        (spot.x + column as u16).min(spot.right().saturating_sub(1)),
                        spot.y,
                    ));
                }
            }
        }
        if record.full.is_loading() && (index as u16) < area.height / 2 {
            let spot = Rect {
                y: area.bottom().saturating_sub(1),
                height: 1.min(area.height),
                ..area
            };
            let dim = t.dim();
            self.put(
                spot,
                Line::styled(
                    format!(
                        "  {} Loading the other fields",
                        t.spinner(app.ms as usize / 100)
                    ),
                    dim,
                ),
            );
        }
    }

    fn activity(&mut self, area: Rect, record: &RecordState) {
        let t = self.t;
        let entries = match &record.activity {
            Load::Ready(entries) => entries,
            Load::Failed(fault) => {
                self.message(area, t.g.cross, t.danger(), &fault.message, "r tries again");
                return;
            }
            _ => {
                self.waiting(area, "Loading the conversation");
                return;
            }
        };
        if entries.is_empty() {
            self.message(
                area,
                t.g.ring,
                t.dim(),
                "Nobody has written anything yet.",
                "w writes a work note, c a comment",
            );
            return;
        }
        let width = (area.width as usize).saturating_sub(4).clamp(1, 110);
        let mut lines: Vec<Line> = Vec::new();
        for entry in entries {
            let (label, style) = match entry.kind {
                EntryKind::Comment => ("comment", t.accent()),
                EntryKind::WorkNote => ("work note", t.warning()),
            };
            let when = entry
                .time
                .map(|at| text::since(at, self.app.now))
                .unwrap_or_default();
            let mut head = vec![
                Span::styled(format!(" {} ", t.g.dot), style),
                Span::styled(entry.by.clone(), t.bold()),
                Span::styled(format!("  {label}"), style),
                Span::styled(format!("  {when}"), t.dim()),
            ];
            let used: usize = head.iter().map(Span::width).sum();
            if used + text::width(&entry.at) + 3 <= area.width as usize {
                head.push(Span::styled(format!("  {}", entry.at), t.dim()));
            }
            lines.push(Line::from(head));
            for part in text::wrap(&entry.text, width) {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {} ", t.g.vline),
                        style.add_modifier(Modifier::DIM),
                    ),
                    Span::raw(part),
                ]));
            }
            lines.push(Line::default());
        }
        if entries.len() as u64 >= TAB_LIMIT {
            lines.push(Line::styled(
                format!(" These are the latest {TAB_LIMIT}. Press o for all of them."),
                t.dim(),
            ));
        }
        self.scroll(area, lines, record.scroll);
    }

    fn related(&mut self, area: Rect, record: &RecordState) {
        let t = self.t;
        if record.related.is_empty() {
            if record.full.is_loading() {
                self.waiting(area, "Loading the record");
            } else {
                self.message(
                    area,
                    t.g.ring,
                    t.dim(),
                    "This kind of record has no related lists here.",
                    "o opens it in the browser",
                );
            }
            return;
        }
        let focused = self.app.focus == Pane::Record && self.app.overlays.is_empty();
        let rows_index = record.related_rows();
        let mut lines: Vec<(Option<usize>, Line<'static>)> = Vec::new();
        let mut cursor_line = 0;
        let mut seen = 0;
        // Empty lists share one line at the end.
        let empty: Vec<String> = record
            .related
            .iter()
            .filter(|(_, l)| l.ready().is_some_and(Vec::is_empty))
            .map(|(list, _)| list.title.to_lowercase())
            .collect();
        for (list, load) in &record.related {
            if load.ready().is_some_and(Vec::is_empty) {
                continue;
            }
            if !lines.is_empty() {
                lines.push((None, Line::default()));
            }
            let count = load.ready().map_or(String::new(), |r| r.len().to_string());
            lines.push((
                None,
                Line::from(vec![
                    Span::styled(format!(" {}", list.title.to_uppercase()), t.heading()),
                    Span::styled(format!("  {count}"), t.dim()),
                ]),
            ));
            match load {
                Load::Ready(rows) => {
                    // The columns of one list line up.
                    let parts: Vec<Parts> = rows
                        .iter()
                        .map(|r| self.related_parts(list.table, r))
                        .collect();
                    let longest = |part: fn(&Parts) -> &str| {
                        parts
                            .iter()
                            .map(|p| text::width(part(p)))
                            .max()
                            .unwrap_or(0)
                    };
                    let third = area.width as usize / 3;
                    let ids = longest(|p| &p.id).min(third.max(11));
                    let ends = longest(|p| &p.end).min(third);
                    for part in parts {
                        let here = seen == record.related_cursor;
                        if here {
                            cursor_line = lines.len();
                        }
                        let line = self.related_row(part, (ids, ends), here, focused, area.width);
                        lines.push((Some(seen), line));
                        seen += 1;
                    }
                    if rows.len() as u64 >= TAB_LIMIT {
                        lines.push((
                            None,
                            Line::styled(format!("   These are the first {TAB_LIMIT}."), t.dim()),
                        ));
                    }
                }
                Load::Failed(fault) => {
                    for part in text::wrap(
                        &text::line(&fault.message),
                        (area.width as usize).saturating_sub(5),
                    ) {
                        lines.push((None, Line::styled(format!("   {part}"), t.danger())));
                    }
                    lines.push((None, Line::styled("   r tries again", t.dim())));
                }
                _ => lines.push((
                    None,
                    Line::styled(
                        format!("   {} loading", t.spinner(self.app.ms as usize / 100)),
                        t.dim(),
                    ),
                )),
            }
        }
        if !empty.is_empty() {
            if !lines.is_empty() {
                lines.push((None, Line::default()));
            }
            for part in text::wrap(
                &format!("None: {}", empty.join(", ")),
                (area.width as usize).saturating_sub(2),
            ) {
                lines.push((None, Line::styled(format!(" {part}"), t.dim())));
            }
        }
        let rows = area.height as usize;
        self.places.record_rows = rows;
        let mut offset = scrolled(
            self.app.places.record_offset,
            cursor_line,
            rows,
            lines.len(),
        );
        if record.related_cursor == 0 {
            offset = 0;
        }
        // Lists below the last row stay reachable.
        if record.related_cursor + 1 == rows_index.len() {
            let max = lines.len().saturating_sub(rows);
            offset = max.min(cursor_line);
        }
        self.places.record_offset = offset;
        for (row, (item, line)) in area.rows().zip(lines.into_iter().skip(offset)) {
            self.put(row, line);
            if let Some(i) = item {
                self.hit(row, Hit::Related(i));
            }
        }
    }

    /// What names a related row, what it is about, and its status.
    fn related_parts(&self, table: &str, row: &Row) -> Parts {
        let t = self.t;
        let show = |field: &str| text::line(row.display(field));
        let plain = |id: String, about: String, end: String| Parts {
            id,
            about,
            end,
            tone: Tone::Done,
        };
        let stated = |id: String, about: String, state: String| {
            let tone = theme::state(&state);
            Parts {
                id,
                about,
                end: match state.is_empty() {
                    true => state,
                    false => format!("{} {state}", theme::sign(&t, tone)),
                },
                tone,
            }
        };
        match table {
            "sysapproval_approver" => stated(show("approver"), show("comments"), show("state")),
            "task_ci" => stated(
                show("ci_item"),
                show("ci_item.sys_class_name"),
                show("ci_item.operational_status"),
            ),
            "cmdb_rel_ci" => {
                let side = match row.has("child.sys_class_name") {
                    true => "child",
                    false => "parent",
                };
                plain(
                    show(side),
                    show(&format!("{side}.sys_class_name")),
                    show("type"),
                )
            }
            "sys_user_grmember" => plain(show("user"), show("user.title"), show("user.email")),
            "sys_update_xml" => plain(
                show("type"),
                match show("target_name") {
                    name if name.is_empty() => show("name"),
                    name => name,
                },
                show("action"),
            ),
            _ => stated(row.name(), row.summary(), show("state")),
        }
    }

    fn related_row(
        &self,
        Parts {
            id,
            about,
            end,
            tone,
        }: Parts,
        (id_width, end_width): (usize, usize),
        here: bool,
        focused: bool,
        width: u16,
    ) -> Line<'static> {
        let t = self.t;
        let about_width = (width as usize).saturating_sub(id_width + end_width + 7);
        let mut line = Line::from(vec![
            Span::styled(
                format!("{}  ", if here { t.g.bar } else { " " }),
                match focused {
                    true => t.accent(),
                    false => t.dim(),
                },
            ),
            Span::styled(text::fit(&id, id_width, t.g.ellipsis), t.accent()),
            Span::raw("  "),
            Span::raw(text::fit(&about, about_width, t.g.ellipsis)),
            Span::raw("  "),
            Span::styled(text::cut(&end, end_width, t.g.ellipsis), t.tone(tone)),
        ]);
        if here {
            line = line.patch_style(t.cursor(focused));
        }
        line
    }

    fn files(&mut self, area: Rect, record: &RecordState) {
        let t = self.t;
        let files = match &record.attachments {
            Load::Ready(files) => files,
            Load::Failed(fault) => {
                self.message(area, t.g.cross, t.danger(), &fault.message, "r tries again");
                return;
            }
            _ => {
                self.waiting(area, "Loading the files");
                return;
            }
        };
        if files.is_empty() {
            self.message(area, t.g.ring, t.dim(), "No file is attached.", "");
            return;
        }
        let focused = self.app.focus == Pane::Record && self.app.overlays.is_empty();
        let rows = area.height as usize;
        self.places.record_rows = rows;
        let offset = scrolled(
            self.app.places.record_offset,
            record.file_cursor,
            rows,
            files.len(),
        );
        self.places.record_offset = offset;
        for (row, (n, file)) in area.rows().zip(files.iter().enumerate().skip(offset)) {
            let here = n == record.file_cursor;
            let when = file
                .time
                .map(|at| text::ago(at, self.app.now))
                .unwrap_or_default();
            let end = format!("{:>9}  {:<5}  {}", text::bytes(file.size), when, file.by);
            let end = text::cut(&end, (area.width / 2) as usize, t.g.ellipsis);
            let room = (area.width as usize).saturating_sub(text::width(&end) + 5);
            let mut line = Line::from(vec![
                Span::styled(
                    format!("{}  ", if here { t.g.bar } else { " " }),
                    match focused {
                        true => t.accent(),
                        false => t.dim(),
                    },
                ),
                Span::raw(text::fit(&file.name, room, t.g.ellipsis)),
                Span::styled(format!("  {end}"), t.dim()),
            ]);
            if here {
                line = line.patch_style(t.cursor(focused));
            }
            self.put(row, line);
            self.hit(row, Hit::File(n));
        }
    }

    fn slas(&mut self, area: Rect, record: &RecordState) {
        let t = self.t;
        let slas = match &record.slas {
            Load::Ready(slas) => slas,
            Load::Failed(fault) => {
                self.message(area, t.g.cross, t.danger(), &fault.message, "r tries again");
                return;
            }
            _ => {
                self.waiting(area, "Loading the SLAs");
                return;
            }
        };
        if slas.is_empty() {
            self.message(area, t.g.ring, t.dim(), "No SLA runs on this record.", "");
            return;
        }
        let width = area.width as usize;
        let mut lines: Vec<Line> = Vec::new();
        for sla in slas {
            let running = sla.stage.to_lowercase().contains("progress");
            let percent = sla.percent.unwrap_or(0);
            let (word, tone) = match () {
                () if sla.breached || sla.stage.eq_ignore_ascii_case("breached") => {
                    ("BREACHED", Tone::Bad)
                }
                () if running && percent >= 75 => ("at risk", Tone::Warn),
                () if running => ("on track", Tone::Good),
                () => ("", Tone::Done),
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", theme::sign(&t, tone)), t.tone(tone)),
                Span::styled(sla.name.clone(), t.bold()),
                Span::styled(format!("  {}", sla.stage), t.dim()),
                Span::styled(
                    format!("  {word}"),
                    t.tone(tone).add_modifier(Modifier::BOLD),
                ),
            ]));
            let bar = width.saturating_sub(14).clamp(4, 60);
            let filled = (bar * percent.min(100) as usize / 100).min(bar);
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(t.g.full.repeat(filled), t.tone(tone)),
                Span::styled(t.g.empty.repeat(bar - filled), t.dim()),
                Span::raw(match sla.percent {
                    Some(p) => format!("  {p}%"),
                    None => String::new(),
                }),
            ]));
            let mut facts = Vec::new();
            if !sla.left.is_empty() {
                facts.push(format!("time left {}", sla.left));
            }
            if !sla.due.is_empty() {
                facts.push(match sla.due_time {
                    Some(at) => format!("due {} ({})", sla.due, text::since(at, self.app.now)),
                    None => format!("due {}", sla.due),
                });
            }
            for part in text::wrap(
                &facts.join(&format!("  {}  ", t.g.sep)),
                width.saturating_sub(4),
            ) {
                lines.push(Line::styled(format!("   {part}"), t.dim()));
            }
            lines.push(Line::default());
        }
        self.scroll(area, lines, record.scroll);
    }

    fn history(&mut self, area: Rect, record: &RecordState) {
        let app = self.app;
        let t = self.t;
        let changes = match &record.history {
            Load::Ready(changes) => changes,
            Load::Failed(fault) if fault.code == "permission" => {
                self.message(
                    area,
                    t.g.lock,
                    t.dim(),
                    "Your role cannot read the change history (table sys_audit).",
                    "o opens the record in the browser",
                );
                return;
            }
            Load::Failed(fault) => {
                self.message(area, t.g.cross, t.danger(), &fault.message, "r tries again");
                return;
            }
            _ => {
                self.waiting(area, "Loading the history");
                return;
            }
        };
        if changes.is_empty() {
            self.message(
                area,
                t.g.ring,
                t.dim(),
                "No change is recorded, or your role cannot read it.",
                "",
            );
            return;
        }
        let width = area.width as usize;
        // The audit stores raw values; a choice shows its label.
        let label = |field: &crate::model::Field, value: &str| match value {
            "" => "empty".to_owned(),
            value => match field.choices.iter().find(|(v, _)| v == value) {
                Some((_, label)) => text::line(label),
                None if record.row.value(&field.name) == value => {
                    text::line(record.row.display(&field.name))
                }
                None if app.me().is_some_and(|m| m.sys_id == value) => {
                    app.me().map(|m| m.name.clone()).unwrap_or_default()
                }
                None if sn_core::is_sys_id(value) => "another record".to_owned(),
                None => value.to_owned(),
            },
        };
        let mut lines: Vec<Line> = Vec::new();
        let mut before: Option<(Option<i64>, &str)> = None;
        for change in changes {
            // One save changes several fields at one moment.
            if before != Some((change.time, change.by.as_str())) {
                if before.is_some() {
                    lines.push(Line::default());
                }
                let when = change.time.map_or_else(String::new, |at| {
                    format!("{}  {}", text::ago(at, app.now), text::format_utc(at))
                });
                lines.push(Line::from(vec![
                    Span::styled(format!(" {} ", t.g.dot), t.accent()),
                    Span::styled(change.by.clone(), t.bold()),
                    Span::styled(format!("  {when} UTC"), t.dim()),
                ]));
                before = Some((change.time, change.by.as_str()));
            }
            let field = app.field(&record.table, &change.field);
            let name = text::fit(&field.label, (width / 3).clamp(8, 22), t.g.ellipsis);
            let room = width.saturating_sub(text::width(&name) + 8) / 2;
            let old = text::cut(&label(&field, &change.old), room, t.g.ellipsis);
            let new = text::cut(&label(&field, &change.new), room, t.g.ellipsis);
            lines.push(Line::from(vec![
                Span::styled(format!("   {name}  "), t.dim()),
                Span::styled(old, t.dim()),
                Span::styled(format!(" {} ", t.g.arrow), t.dim()),
                Span::raw(new),
            ]));
        }
        if changes.len() as u64 >= TAB_LIMIT {
            lines.push(Line::default());
            lines.push(Line::styled(
                format!(" These are the latest {TAB_LIMIT}."),
                t.dim(),
            ));
        }
        self.scroll(area, lines, record.scroll);
    }
}
