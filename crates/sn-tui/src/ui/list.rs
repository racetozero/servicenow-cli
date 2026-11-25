//! The sections and the list.

use super::{Hit, Pen, scrolled};
use crate::app::state::*;
use crate::model::Row;
use crate::query;
use crate::tables::{Column, Look};
use crate::text;
use crate::theme::{self, Tone};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Columns between two columns.
const GAP: u16 = 2;
/// The cursor bar and the selection mark.
const GUTTER: u16 = 3;

impl Pen<'_, '_> {
    pub(super) fn rail(&mut self, area: Rect) {
        let app = self.app;
        let t = self.t;
        self.hit(area, Hit::Pane(Pane::Rail));
        let rail = app.rail();
        let focused = app.focus == Pane::Rail && app.overlays.is_empty();
        // A blank line goes before every group but the first, when there is
        // room for it.
        let groups = rail
            .iter()
            .filter(|i| matches!(i, RailItem::Group(_)))
            .count();
        let airy = rail.len() + groups <= area.height as usize;
        let mut lines: Vec<(Option<usize>, Line<'static>)> = Vec::new();
        let mut cursor_line = 0;
        for (i, item) in rail.iter().enumerate() {
            match item {
                RailItem::Group(name) => {
                    if airy && i > 0 {
                        lines.push((None, Line::default()));
                    }
                    lines.push((
                        None,
                        Line::styled(format!(" {}", name.to_uppercase()), t.heading()),
                    ));
                }
                RailItem::Section(id, label) => {
                    let open = app.list.section.as_ref() == Some(id);
                    let at = focused && app.rail_cursor == i;
                    if app.rail_cursor == i {
                        cursor_line = lines.len();
                    }
                    let (count, count_style) = match app.counts.get(id) {
                        Some(Load::Ready(0)) => ("0".to_owned(), t.dim()),
                        Some(Load::Ready(n)) => (text::short_count(*n), Style::new()),
                        Some(Load::Loading(_)) => (t.g.ellipsis.to_owned(), t.dim()),
                        Some(Load::Failed(_)) => (t.g.dash.to_owned(), t.dim()),
                        _ => (String::new(), t.dim()),
                    };
                    let style = match (at, open) {
                        (true, _) => t.accent_bold(),
                        (false, true) => t.bold(),
                        _ => Style::new(),
                    };
                    let room = (area.width as usize).saturating_sub(3 + text::width(&count));
                    lines.push((
                        Some(i),
                        Line::from(vec![
                            Span::styled(
                                match (at, open) {
                                    (true, _) => t.g.bar,
                                    (false, true) => t.g.vline,
                                    _ => " ",
                                },
                                t.accent(),
                            ),
                            Span::styled(
                                format!(" {} ", text::fit(&text::line(label), room, t.g.ellipsis)),
                                style,
                            ),
                            Span::styled(count, count_style),
                        ]),
                    ));
                }
            }
        }
        let rows = area.height as usize;
        let offset = scrolled(app.places.rail_offset, cursor_line, rows, lines.len());
        self.places.rail_offset = offset;
        for (row, (item, line)) in area.rows().zip(lines.into_iter().skip(offset)) {
            self.put(row, line);
            if let Some(i) = item {
                self.hit(row, Hit::Rail(i));
            }
        }
    }

    pub(super) fn list(&mut self, area: Rect) {
        let app = self.app;
        let list = &app.list;
        let focused = app.focus == Pane::List && app.overlays.is_empty();
        self.hit(area, Hit::Pane(Pane::List));
        let has_info = list.searching
            || !list.search.text.is_empty()
            || list.is_filtered()
            || list.section.is_none();
        let [title, info, head, body] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(u16::from(has_info && area.height > 5)),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);
        let visible = list.visible();
        self.list_title(title, focused, visible.len());
        self.list_info(info);
        match &list.phase {
            ListPhase::Failed(fault) => {
                let (danger, sign) = (self.t.danger(), self.t.g.cross);
                self.message(body, sign, danger, &fault.message, "Enter tries again");
                return;
            }
            ListPhase::Waiting | ListPhase::Loading(_) => {
                self.list_head(head);
                self.skeleton(body);
                return;
            }
            ListPhase::Ready => {}
        }
        if visible.is_empty() {
            let (dim, sign) = (self.t.dim(), self.t.g.ring);
            self.message(body, sign, dim, &app.empty_text(), "");
            return;
        }
        let columns = self.list_head(head);
        let rows = body.height as usize;
        self.places.list_rows = rows;
        let offset = scrolled(app.places.list_offset, list.cursor, rows, visible.len());
        self.places.list_offset = offset;
        for (line, (n, at)) in body.rows().zip(visible.iter().enumerate().skip(offset)) {
            self.row(line, n, &list.rows[*at], &columns, focused);
        }
        let shown = (visible.len() - offset).min(rows);
        if shown < rows && !list.exhausted && list.search.text.is_empty() {
            let line = Rect {
                y: body.y + shown as u16,
                height: 1,
                ..body
            };
            let dim = self.t.dim();
            self.put(
                line,
                Line::styled("   More rows load when you go down.", dim),
            );
        }
    }

    fn list_title(&mut self, area: Rect, focused: bool, visible: usize) {
        let app = self.app;
        let list = &app.list;
        let t = self.t;
        let mut spans = self.title(&text::line(&list.title), focused);
        if list.phase == ListPhase::Ready {
            let loaded = list.rows.len() as u64;
            let count = match (list.search.text.trim().is_empty(), list.total) {
                (false, _) => format!("{visible} of {} loaded", text::grouped(loaded)),
                (true, Some(total)) => text::grouped(total),
                (true, None) if list.exhausted => text::grouped(loaded),
                (true, None) => format!("{}+", text::grouped(loaded)),
            };
            spans.push(Span::styled(format!("  {count}"), t.dim()));
        }
        if !list.selected.is_empty() {
            spans.push(Span::styled(
                format!("  {} {} selected", t.g.check, list.selected.len()),
                t.accent_bold(),
            ));
        }
        // The order goes to the right end, when there is room.
        let order = list
            .order
            .iter()
            .take(2)
            .map(|(field, down)| {
                let label = list.columns.iter().find(|c| c.field == *field).map_or_else(
                    || app.field(&list.table, field).label.clone(),
                    |c| c.label.clone(),
                );
                format!(
                    "{} {}",
                    label.to_lowercase(),
                    if *down { t.g.down } else { t.g.up }
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let used: usize = spans.iter().map(Span::width).sum();
        self.put(area, Line::from(spans));
        let width = text::width(&order) as u16;
        if used as u16 + width + 3 <= area.width && width > 0 {
            let spot = Rect {
                x: area.right() - width,
                width,
                ..area
            };
            self.put(spot, Line::styled(order, t.dim()));
            self.hit(spot, Hit::Act(crate::keys::Act::Sort));
        }
    }

    /// The search or the filter, in one line under the title.
    fn list_info(&mut self, area: Rect) {
        if area.is_empty() {
            return;
        }
        let app = self.app;
        let list = &app.list;
        let t = self.t;
        if list.searching || !list.search.text.is_empty() {
            let accent = t.accent_bold();
            self.put(area, Line::styled(" /", accent));
            let field = Rect {
                x: area.x + 3,
                width: area.width.saturating_sub(3),
                ..area
            };
            let focused = list.searching && app.focus == Pane::List && app.overlays.is_empty();
            self.input(
                field,
                &list.search,
                "type to search the loaded rows",
                focused,
            );
            return;
        }
        let words = query::describe(
            &list.query,
            |f| app.field(&list.table, f).label.clone(),
            |f, v| app.value_label(&list.table, f, v),
        );
        let line = match words.is_empty() {
            true => Line::styled(format!(" {}", list.table), t.dim()),
            false => Line::from(vec![
                Span::styled(
                    match list.is_filtered() {
                        true => " Filter ",
                        false => " ",
                    },
                    t.accent(),
                ),
                Span::styled(
                    text::cut(
                        &text::line(&words),
                        area.width.saturating_sub(9) as usize,
                        t.g.ellipsis,
                    ),
                    t.dim(),
                ),
            ]),
        };
        self.put(area, line);
        self.hit(area, Hit::Act(crate::keys::Act::Filter));
    }

    /// Draws the headings and says which columns fit, with their widths.
    fn list_head(&mut self, area: Rect) -> Vec<(Column, u16)> {
        let list = &self.app.list;
        let columns = fit_columns(&list.columns, area.width);
        let t = self.t;
        let mut x = area.x + GUTTER;
        for (column, width) in &columns {
            let spot = Rect {
                x,
                width: *width,
                ..area
            }
            .intersection(area);
            let order = list
                .order
                .first()
                .filter(|(f, _)| *f == column.field)
                .map(|(_, down)| if *down { t.g.down } else { t.g.up });
            let label = match order {
                Some(arrow) => format!("{} {arrow}", column.label),
                None => column.label.clone(),
            };
            let style = match order {
                Some(_) => t.heading().remove_modifier(Modifier::DIM),
                None => t.heading(),
            };
            self.put(
                spot,
                Line::styled(text::cut(&label, *width as usize, t.g.ellipsis), style),
            );
            self.hit(spot, Hit::Column(column.field.clone()));
            x += width + GAP;
        }
        columns
    }

    fn row(&mut self, area: Rect, n: usize, row: &Row, columns: &[(Column, u16)], focused: bool) {
        let list = &self.app.list;
        let t = self.t;
        let here = n == list.cursor;
        let selected = list.selected.contains(&row.sys_id);
        let mut spans = vec![
            Span::styled(
                if here { t.g.bar } else { " " },
                match focused {
                    true => t.accent(),
                    false => t.dim(),
                },
            ),
            // Without its column, a high priority still shows.
            match row.value("priority") {
                _ if selected => Span::styled(t.g.check, t.accent_bold()),
                value @ ("1" | "2") if !columns.iter().any(|(c, _)| c.look == Look::Priority) => {
                    Span::styled(
                        theme::priority_sign(&t, value),
                        t.tone(theme::priority(value)),
                    )
                }
                _ => Span::raw(" "),
            },
            Span::raw(" "),
        ];
        for (column, width) in columns {
            let (shown, mut style) = self.cell(row, column, *width as usize);
            if here {
                style = style
                    .remove_modifier(Modifier::DIM)
                    .add_modifier(Modifier::BOLD);
                if column.look == Look::Id && focused {
                    style = t.accent_bold();
                }
            }
            spans.push(Span::styled(
                text::fit(&shown, *width as usize, t.g.ellipsis),
                style,
            ));
            spans.push(Span::raw(" ".repeat(GAP as usize)));
        }
        self.put(area, Line::from(spans));
        self.hit(area, Hit::Row(n));
        let mark = Rect {
            width: GUTTER.min(area.width),
            ..area
        };
        self.hit(mark, Hit::Mark(n));
    }

    /// The text of a cell and how it looks.
    fn cell(&self, row: &Row, column: &Column, width: usize) -> (String, Style) {
        let t = self.t;
        let cell = row.cell(&column.field);
        let shown = text::line(&cell.display);
        match column.look {
            Look::Priority => {
                let tone = theme::priority(&cell.value);
                let label = theme::priority_label(&shown);
                if label.is_empty() {
                    return (String::new(), Style::new());
                }
                (
                    match width < 10 {
                        true => format!("{} {}", theme::priority_sign(&t, &cell.value), cell.value),
                        false => format!("{} {label}", theme::priority_sign(&t, &cell.value)),
                    },
                    t.tone(tone),
                )
            }
            Look::State => {
                if shown.is_empty() {
                    return (shown, Style::new());
                }
                let tone = theme::state(&shown);
                (format!("{} {shown}", theme::sign(&t, tone)), t.tone(tone))
            }
            Look::Time => (
                match text::parse_utc(&cell.value) {
                    Some(at) => text::ago(at, self.app.now),
                    None => shown,
                },
                t.dim(),
            ),
            Look::Due => match text::parse_utc(&cell.value) {
                Some(at) if at < self.app.now && column.field == "due_date" => (
                    format!("{} {} late", t.g.warn, text::ago(at, self.app.now)),
                    t.tone(Tone::Bad),
                ),
                Some(at) if at < self.app.now => (text::since(at, self.app.now), t.dim()),
                Some(at) => (text::ago(at, self.app.now), Style::new()),
                None => (shown, Style::new()),
            },
            Look::Flag => match cell.value.as_str() {
                "true" => (format!("{} yes", t.g.check), t.good()),
                "false" => (format!("{} no", t.g.dash), t.dim()),
                _ => (shown, Style::new()),
            },
            Look::Class => (shown, t.dim()),
            Look::Id => (shown, t.accent()),
            Look::Main | Look::Plain => (shown, Style::new()),
        }
    }

    /// Gray rows while the first page is on its way.
    fn skeleton(&mut self, area: Rect) {
        let t = self.t;
        let columns = fit_columns(&self.app.list.columns, area.width);
        for n in 0..area.height.min(8) {
            let line = Rect {
                y: area.y + n,
                height: 1,
                ..area
            };
            let mut spans = vec![Span::raw(" ".repeat(GUTTER as usize))];
            for (i, (_, width)) in columns.iter().enumerate() {
                let length = (*width as usize * (5 + (n as usize * 7 + i * 3) % 5) / 10).max(1);
                spans.push(Span::styled(
                    text::fit(
                        &t.g.empty.repeat(length.min(*width as usize)),
                        *width as usize,
                        "",
                    ),
                    t.dim(),
                ));
                spans.push(Span::raw(" ".repeat(GAP as usize)));
            }
            self.put(line, Line::from(spans));
        }
    }
}

/// The columns that fit, with their widths: the least important go first,
/// and the main column takes the room that is left.
pub(super) fn fit_columns(columns: &[Column], width: u16) -> Vec<(Column, u16)> {
    let room = width.saturating_sub(GUTTER);
    let mut kept: Vec<&Column> = columns.iter().collect();
    let need = |kept: &[&Column]| {
        kept.iter()
            .map(|c| c.width + GAP)
            .sum::<u16>()
            .saturating_sub(GAP)
    };
    // The sentence is worth more than one more short column.
    let comfort = |kept: &[&Column]| {
        let main = kept.iter().any(|c| c.look == Look::Main);
        need(kept) + if main { 8 } else { 0 }
    };
    while kept.len() > 1 && comfort(&kept) > room {
        // Of equal ranks, the one on the right goes.
        let Some(at) = kept
            .iter()
            .enumerate()
            .filter(|(_, c)| c.rank > 0)
            .max_by_key(|(i, c)| (c.rank, *i))
            .map(|(i, _)| i)
        else {
            break;
        };
        kept.remove(at);
    }
    let mut left = room.saturating_sub(need(&kept));
    let mains = kept.iter().filter(|c| c.look == Look::Main).count() as u16;
    let mut out: Vec<(Column, u16)> = kept
        .iter()
        .map(|c| {
            let extra = match (c.look, mains) {
                (Look::Main, 1..) => left / mains,
                _ => 0,
            };
            ((*c).clone(), c.width + extra)
        })
        .collect();
    if mains == 0
        && let Some(first) = out.first_mut()
    {
        first.1 += left;
        left = 0;
    }
    let _ = left;
    // A very narrow list: cut the widths until they fit.
    let total = |out: &[(Column, u16)]| {
        out.iter()
            .map(|(_, w)| w + GAP)
            .sum::<u16>()
            .saturating_sub(GAP)
    };
    while total(&out) > room {
        let Some(widest) = out.iter_mut().max_by_key(|(_, w)| *w) else {
            break;
        };
        if widest.1 <= 1 {
            break;
        }
        widest.1 -= 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_fit_at_any_width() {
        for table in ["incident", "task", "cmdb_ci", "sys_user"] {
            let columns = crate::tables::columns(table).expect(table);
            for width in 0..260 {
                let fitted = fit_columns(&columns, width);
                let total: u16 = fitted.iter().map(|(_, w)| w + GAP).sum::<u16>();
                assert!(
                    total.saturating_sub(GAP) <= width.saturating_sub(GUTTER) || width < 30,
                    "{table} at {width}: {total}"
                );
                assert!(!fitted.is_empty());
            }
        }
        // At 80 columns an incident still shows what matters.
        let columns = crate::tables::columns("incident").expect("incident");
        let fields: Vec<String> = fit_columns(&columns, 80)
            .into_iter()
            .map(|(c, _)| c.field)
            .collect();
        for field in ["number", "priority", "short_description", "state"] {
            assert!(fields.iter().any(|f| f == field), "{field} in {fields:?}");
        }
    }
}
