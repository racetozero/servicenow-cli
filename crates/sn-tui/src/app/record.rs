//! The open record: its tabs and its edits.

use super::state::*;
use super::{App, PAUSE};
use crate::input::Editor;
use crate::keys::Act;
use crate::model::{self, Cell, Detail, Field, Kind, Row};
use crate::msg::{Call, Candidate, Outcome, Req};
use crate::tables;
use crate::text;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl App {
    /// Opens a record by its key, over the one that is open.
    pub(super) fn open_record(&mut self, table: &str, sys_id: &str) {
        if let Some(record) = self.record.take() {
            if record.sys_id == sys_id {
                self.record = Some(record);
                self.focus = Pane::Record;
                return;
            }
            self.records.push(record);
        }
        let row = Row {
            table: table.to_owned(),
            sys_id: sys_id.to_owned(),
            cells: Vec::new(),
        };
        self.record = Some(RecordState::new(table, sys_id, row));
        self.focus = Pane::Record;
        self.load_record();
    }

    /// Opens the row under the cursor of the list.
    pub(super) fn open_row(&mut self) {
        let Some(row) = self.list.current().cloned() else {
            return;
        };
        if let Some(record) = &self.record {
            if record.sys_id == row.sys_id {
                self.focus = Pane::Record;
                return;
            }
            if self.open_has_draft() {
                // The changes wait on the way back.
                let (list, focus) = (self.list.clone(), Pane::Record);
                let record = self.record.take();
                self.back.push(Snapshot {
                    list,
                    record,
                    records: std::mem::take(&mut self.records),
                    focus,
                });
            }
        }
        self.records.clear();
        self.set_record(row, Tab::Details);
        self.focus = Pane::Record;
    }

    pub(super) fn set_record(&mut self, row: Row, tab: Tab) {
        let mut record = RecordState::new(&row.table.clone(), &row.sys_id.clone(), row);
        record.tab = tab;
        self.record = Some(record);
        self.load_record();
    }

    /// Loads what the open record still lacks.
    pub(super) fn load_record(&mut self) {
        let Some(record) = &self.record else { return };
        if record.is_new() {
            return;
        }
        let (table, sys_id) = (record.table.clone(), record.sys_id.clone());
        if matches!(record.full, Load::Idle) {
            let req = self.req();
            if let Some(record) = &mut self.record {
                record.full = Load::Loading(req);
            }
            self.want_meta(&table);
            self.call(Call::Record { req, table, sys_id });
        }
        self.load_tab();
    }

    pub(super) fn reload_record(&mut self) {
        let Some(record) = &mut self.record else {
            return;
        };
        if record.is_new() || record.full.is_loading() {
            return;
        }
        record.full = Load::Idle;
        match record.tab {
            Tab::Details => {}
            Tab::Activity => record.activity = Load::Idle,
            Tab::Related => record.related.clear(),
            Tab::Attachments => record.attachments = Load::Idle,
            Tab::Slas => record.slas = Load::Idle,
            Tab::History => record.history = Load::Idle,
        }
        self.load_record();
    }

    pub(super) fn record_reply(&mut self, req: Req, outcome: Outcome<Row>) {
        let Some(record) = self.record.as_mut().filter(|r| r.full.waits_for(req)) else {
            return;
        };
        match outcome {
            Ok(row) => {
                // With changes waiting, the version they were made on stays,
                // so that the write finds out about other people's changes.
                if !record.has_draft() {
                    record.baseline = row.baseline();
                }
                let class = row.table.clone();
                record.row = row;
                if class != record.table {
                    // The table that was read lacks the fields of the class.
                    record.table = class;
                    record.full = Load::Idle;
                    self.load_record();
                    return;
                }
                record.full = Load::Ready(());
                self.load_tab();
            }
            Err(fault) => {
                record.full = Load::Failed(fault);
            }
        }
    }

    pub(super) fn show_tab(&mut self, tab: Tab) {
        let Some(record) = &mut self.record else {
            return;
        };
        if record.is_new() {
            return;
        }
        if record.tab != tab {
            record.tab = tab;
            record.scroll = 0;
            record.inline = None;
        }
        self.load_tab();
    }

    /// Asks for the open tab, once.
    fn load_tab(&mut self) {
        let req = self.next_req + 1;
        let Some(record) = &mut self.record else {
            return;
        };
        // The related lists depend on what the record is.
        if record.is_new() || record.full.ready().is_none() {
            return;
        }
        let (table, sys_id) = (record.table.clone(), record.sys_id.clone());
        let call = match record.tab {
            Tab::Activity if matches!(record.activity, Load::Idle) => {
                record.activity = Load::Loading(req);
                Call::Activity { req, table, sys_id }
            }
            Tab::Related if record.related.is_empty() => {
                let lists = tables::related(&record.row);
                if lists.is_empty() {
                    return;
                }
                record.related = lists
                    .iter()
                    .map(|l| (l.clone(), Load::Loading(req)))
                    .collect();
                Call::Related { req, lists }
            }
            Tab::Attachments if matches!(record.attachments, Load::Idle) => {
                record.attachments = Load::Loading(req);
                Call::Attachments { req, table, sys_id }
            }
            Tab::Slas if matches!(record.slas, Load::Idle) => {
                record.slas = Load::Loading(req);
                Call::Slas { req, sys_id }
            }
            Tab::History if matches!(record.history, Load::Idle) => {
                record.history = Load::Loading(req);
                Call::History { req, sys_id }
            }
            _ => return,
        };
        self.next_req = req;
        self.call(call);
    }

    /// The fields on the Details tab, in order.
    pub fn detail_fields(&self, record: &RecordState) -> Vec<Detail> {
        let meta = self.meta(&record.table);
        let edited: Vec<String> = record.edits.iter().map(|e| e.field.clone()).collect();
        if record.is_new() {
            let Some(meta) = meta else { return Vec::new() };
            let mut names = model::new_record_fields(meta);
            if record.show_empty {
                let mut more: Vec<String> = meta
                    .fields
                    .iter()
                    .filter(|f| f.locked().is_none() && f.kind != Kind::Journal)
                    .map(|f| f.name.clone())
                    .filter(|n| !names.contains(n))
                    .collect();
                more.sort_unstable();
                names.extend(more);
            }
            return std::iter::once(Detail::Group("New record"))
                .chain(names.into_iter().map(Detail::Field))
                .collect();
        }
        model::details(&record.row, meta, record.show_empty, &edited)
    }

    fn detail_names(&self) -> Vec<String> {
        let Some(record) = &self.record else {
            return Vec::new();
        };
        self.detail_fields(record)
            .into_iter()
            .filter_map(|d| match d {
                Detail::Field(name) => Some(name),
                Detail::Group(_) => None,
            })
            .collect()
    }

    pub(super) fn move_record(&mut self, by: i32) {
        let fields = self.detail_names().len();
        let max_scroll = self.places.record_max_scroll;
        let Some(record) = &mut self.record else {
            return;
        };
        let step = |at: usize, count: usize| {
            (at as i64 + i64::from(by)).clamp(0, count.saturating_sub(1) as i64) as usize
        };
        match record.tab {
            Tab::Details => record.cursor = step(record.cursor, fields),
            Tab::Related => {
                record.related_cursor = step(record.related_cursor, record.related_rows().len());
            }
            Tab::Attachments => {
                let count = record.attachments.ready().map_or(0, Vec::len);
                record.file_cursor = step(record.file_cursor, count);
            }
            _ => record.scroll = step(record.scroll.min(max_scroll), max_scroll + 1),
        }
    }

    /// Enter in the record pane.
    pub(super) fn open_in_record(&mut self) {
        let Some(record) = &self.record else { return };
        match record.tab {
            Tab::Details => {
                if let Some(name) = self.detail_names().get(record.cursor).cloned() {
                    self.edit_field(&name);
                }
            }
            Tab::Activity => self.act(Act::WorkNote),
            Tab::Related => {
                let rows = record.related_rows();
                let Some((i, j)) = rows.get(record.related_cursor).copied() else {
                    return;
                };
                let target = record.related.get(i).and_then(|(list, load)| {
                    let row = load.ready()?.get(j)?;
                    list.target(row)
                });
                match target {
                    Some((table, sys_id)) => self.open_record(&table, &sys_id),
                    None => self.toast(ToastKind::Info, "This row points to no record."),
                }
            }
            Tab::Attachments => {
                let Some(file) = record
                    .attachments
                    .ready()
                    .and_then(|f| f.get(record.file_cursor))
                    .cloned()
                else {
                    return;
                };
                if self.downloading.is_some() {
                    self.toast(
                        ToastKind::Info,
                        "One download at a time. Wait for it to end.",
                    );
                    return;
                }
                let req = self.req();
                self.downloading = Some(req);
                let dir = self.settings.download_dir.clone();
                self.call(Call::Download {
                    req,
                    sys_id: file.sys_id,
                    name: file.name,
                    dir,
                });
            }
            Tab::Slas | Tab::History => {}
        }
    }

    /// The record that the field under the cursor points to.
    pub(super) fn reference_here(&self) -> Option<(String, String)> {
        let record = self.record.as_ref().filter(|r| r.tab == Tab::Details)?;
        let name = self.detail_names().get(record.cursor).cloned()?;
        let field = self.field(&record.table, &name);
        let id = &record.cell(&name).value;
        let table = field
            .reference
            .as_ref()
            .filter(|_| field.kind == Kind::Reference)?;
        sn_core::is_sys_id(id).then(|| (table.clone(), id.clone()))
    }

    pub(super) fn follow_reference(&mut self) {
        match self.reference_here() {
            Some((table, sys_id)) => self.open_record(&table, &sys_id),
            None => self.toast(ToastKind::Info, "This field does not point to a record."),
        }
    }

    /// Opens the editor that fits the field.
    pub(super) fn edit_field(&mut self, name: &str) {
        let Some(record) = &self.record else { return };
        let mut field = self.field(&record.table, name).into_owned();
        let why = match self.connection.read_only {
            true => Some("read-only"),
            false => field.locked(),
        };
        // What cannot be edited can still be read in full.
        if why.is_some() {
            let stored = record.row.display(name);
            let shown = match field.html {
                true => text::block(&text::html(stored)),
                false => text::block(stored),
            };
            if shown.chars().count() > 100 || shown.contains('\n') {
                self.overlays.push(Overlay::Read {
                    title: field.label,
                    text: shown,
                    scroll: 0,
                });
                return;
            }
        }
        if self.connection.read_only {
            self.toast(ToastKind::Warning, self.read_only_text());
            return;
        }
        if record.full.ready().is_none() && !record.is_new() {
            self.toast(ToastKind::Info, "The record is still loading.");
            return;
        }
        if let Some(why) = field.locked() {
            self.toast(ToastKind::Info, format!("{}: {why}.", field.label));
            return;
        }
        let cell = record.cell(name).clone();
        if field.kind == Kind::Journal {
            self.act(if name == "comments" {
                Act::Comment
            } else {
                Act::WorkNote
            });
            return;
        }
        // A reference without a known table cannot be searched.
        if field.kind == Kind::Reference && field.reference.is_none() {
            field.kind = Kind::Text;
        }
        match field.kind {
            Kind::Bool => {
                let on = cell.value != "true";
                let new = Cell {
                    value: on.to_string(),
                    display: if on { "Yes" } else { "No" }.into(),
                };
                if let Some(record) = &mut self.record {
                    record.set_edit(&field, new);
                }
            }
            Kind::Choice | Kind::Reference => {
                let picker = self.picker(field, PickFor::Record, &cell);
                self.open_picker(picker);
            }
            Kind::Long => self.overlays.push(Overlay::Text {
                editor: Editor::multiline(cell.value),
                field,
            }),
            _ => {
                if let Some(record) = &mut self.record {
                    record.inline = Some(Inline {
                        field,
                        editor: Editor::new(cell.value),
                        error: None,
                    });
                }
            }
        }
    }

    /// The list of values for a choice or a reference, at its current value.
    pub(super) fn picker(&self, field: Field, purpose: PickFor, cell: &Cell) -> Picker {
        let source = match field.kind {
            Kind::Reference => PickSource::Table(field.reference.clone().unwrap_or_default()),
            _ => PickSource::Fixed(
                field
                    .choices
                    .iter()
                    .map(|(value, label)| Candidate {
                        value: value.clone(),
                        label: text::line(label),
                        detail: String::new(),
                    })
                    .collect(),
            ),
        };
        let me = match &source {
            PickSource::Table(table) if table == "sys_user" => self.me().map(|me| Candidate {
                value: me.sys_id.clone(),
                label: me.name.clone(),
                detail: "you".into(),
            }),
            _ => None,
        };
        let cursor = match &source {
            PickSource::Fixed(all) => all.iter().position(|c| c.value == cell.value).unwrap_or(0),
            PickSource::Table(_) => 0,
        };
        Picker {
            title: field.label.clone(),
            can_clear: !field.mandatory && !cell.value.is_empty(),
            purpose,
            field,
            source,
            editor: Editor::default(),
            cursor,
            found: Load::Idle,
            search_at: None,
            me,
        }
    }

    pub(super) fn open_picker(&mut self, mut picker: Picker) {
        if matches!(picker.source, PickSource::Table(_)) {
            picker.search_at = Some(self.ms);
        }
        self.overlays.push(Overlay::Picker(Box::new(picker)));
        self.run_searches();
    }

    pub(super) fn inline_key(&mut self, key: KeyEvent) {
        let now = self.now;
        let Some(inline) = self.record.as_mut().and_then(|r| r.inline.as_mut()) else {
            return;
        };
        let dated = matches!(inline.field.kind, Kind::Date | Kind::DateTime);
        match key.code {
            KeyCode::Esc => {
                if let Some(record) = &mut self.record {
                    record.inline = None;
                }
            }
            KeyCode::Enter => {
                self.commit_inline();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if self.commit_inline() {
                    self.move_record(if key.code == KeyCode::Tab { 1 } else { -1 });
                }
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.commit_inline() {
                    self.act(Act::Review);
                }
            }
            // A date moves by a day.
            KeyCode::Up | KeyCode::Down if dated => {
                let by = if key.code == KeyCode::Up {
                    86_400
                } else {
                    -86_400
                };
                let at = text::parse_utc(&inline.editor.text).unwrap_or(now);
                let moved = text::format_utc(at + by);
                inline.editor = Editor::new(match inline.field.kind {
                    Kind::Date => &moved[..10],
                    _ => &moved,
                });
                inline.error = None;
            }
            _ => {
                inline.editor.key(key);
                inline.error = None;
            }
        }
    }

    /// Takes what was typed. False when it does not fit the field.
    pub(super) fn commit_inline(&mut self) -> bool {
        let now = self.now;
        let Some(record) = &mut self.record else {
            return true;
        };
        let Some(inline) = &mut record.inline else {
            return true;
        };
        match check_value(&inline.field, &inline.editor.text, now) {
            Ok(value) => {
                let (field, old) = (inline.field.clone(), record.row.cell(&inline.field.name));
                let cell = match value == old.value {
                    true => old.clone(),
                    false => Cell {
                        display: value.clone(),
                        value,
                    },
                };
                record.set_edit(&field, cell);
                record.inline = None;
                true
            }
            Err(why) => {
                inline.error = Some(why);
                false
            }
        }
    }

    pub(super) fn undo_field(&mut self) {
        let name = self
            .record
            .as_ref()
            .filter(|r| r.tab == Tab::Details)
            .and_then(|r| self.detail_names().get(r.cursor).cloned());
        if let (Some(name), Some(record)) = (name, &mut self.record) {
            let before = record.edits.len();
            record.edits.retain(|e| e.field != name);
            if record.edits.len() == before {
                self.toast(ToastKind::Info, "This field has no change to undo.");
            }
        }
    }

    pub(super) fn new_record(&mut self) {
        let table = self.list.table.clone();
        if matches!(table.as_str(), "task" | "sysapproval_approver" | "cmdb_ci") {
            self.toast(
                ToastKind::Info,
                "Open one kind of record first, for example g i for incidents, then press n.",
            );
            return;
        }
        match self.metas.get(&table) {
            Some(Load::Ready(_)) => {}
            Some(Load::Failed(fault)) => {
                let text = format!(
                    "The fields of {table} are not known ({}). Press o to create the record in the browser.",
                    fault.message
                );
                self.toast(ToastKind::Error, text);
                return;
            }
            _ => {
                self.want_meta(&table);
                self.pending = Some(Pending::Fields(table, Act::New));
                return;
            }
        }
        if let Some(record) = self.record.take() {
            match record.is_new() {
                true => {
                    self.record = Some(record);
                    self.focus = Pane::Record;
                    return;
                }
                false => self.records.push(record),
            }
        }
        let row = Row {
            table: table.clone(),
            ..Row::default()
        };
        let mut record = RecordState::new(&table, "", row);
        record.full = Load::Ready(());
        self.record = Some(record);
        self.focus = Pane::Record;
    }

    /// Asks the instance for what was typed, once the typing rests.
    pub(super) fn run_searches(&mut self) {
        let ms = self.ms;
        let Some(top) = self.overlays.last_mut() else {
            return;
        };
        let req = self.next_req + 1;
        let call = match top {
            Overlay::Picker(picker) if picker.search_at.is_some_and(|at| at <= ms) => {
                picker.search_at = None;
                let PickSource::Table(table) = &picker.source else {
                    return;
                };
                picker.found = Load::Loading(req);
                Call::References {
                    req,
                    table: table.clone(),
                    text: picker.editor.text.clone(),
                }
            }
            Overlay::Palette(palette) if palette.search_at.is_some_and(|at| at <= ms) => {
                palette.search_at = None;
                let text = palette.editor.text.trim().to_owned();
                if text.chars().count() < 2 {
                    palette.tables = Load::Idle;
                    palette.records = Load::Idle;
                    return;
                }
                palette.tables = Load::Loading(req);
                let records = !palette.tables_only;
                palette.records = match records {
                    true => Load::Loading(req),
                    false => Load::Idle,
                };
                Call::Search { req, text, records }
            }
            _ => return,
        };
        self.next_req = req;
        self.dirty = true;
        self.call(call);
    }

    pub(super) fn typed_in_search(&mut self) {
        let at = self.ms + PAUSE;
        match self.overlays.last_mut() {
            Some(Overlay::Picker(p)) => {
                p.cursor = 0;
                if matches!(p.source, PickSource::Table(_)) {
                    p.search_at = Some(at);
                }
            }
            Some(Overlay::Palette(p)) => {
                p.cursor = 0;
                p.search_at = Some(at);
            }
            _ => {}
        }
    }
}

/// The value to store for what was typed, or what is wrong with it.
pub(super) fn check_value(field: &Field, typed: &str, now: i64) -> Result<String, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return match field.mandatory {
            true => Err(format!("{} cannot be empty", field.label)),
            false => Ok(String::new()),
        };
    }
    match field.kind {
        Kind::Number => match typed.replace(',', "").parse::<f64>() {
            Ok(_) => Ok(typed.replace(',', "")),
            Err(_) => Err("type a number, for example 12 or 1.5".into()),
        },
        Kind::Date | Kind::DateTime => {
            let at = date(typed, now).ok_or(
                "type a date such as 2026-11-30 or 2026-11-30 17:00 (UTC), or now, tomorrow, +3d, +2h",
            )?;
            let full = text::format_utc(at);
            Ok(match field.kind {
                Kind::Date => full[..10].to_owned(),
                _ => full,
            })
        }
        _ => Ok(typed.to_owned()),
    }
}

/// A date as people type it.
fn date(typed: &str, now: i64) -> Option<i64> {
    const DAY: i64 = 86_400;
    let today = now - now.rem_euclid(DAY);
    match typed.to_ascii_lowercase().as_str() {
        "now" => return Some(now),
        "today" => return Some(today),
        "tomorrow" => return Some(today + DAY),
        "yesterday" => return Some(today - DAY),
        _ => {}
    }
    if let Some(rest) = typed.strip_prefix(['+', '-']) {
        let unit = rest.chars().last()?;
        let count: i64 = rest[..rest.len() - unit.len_utf8()].trim().parse().ok()?;
        let seconds = match unit {
            'm' => 60,
            'h' => 3_600,
            'd' => DAY,
            'w' => 7 * DAY,
            _ => return None,
        };
        let gap = count.checked_mul(seconds).filter(|g| *g < 10_000 * DAY)?;
        return Some(match typed.starts_with('-') {
            true => now - gap,
            false => now + gap,
        });
    }
    text::parse_utc(typed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_values_are_checked_by_kind() {
        let now = 1_795_595_400; // 2026-11-25 08:30:00
        let field = |kind| Field {
            kind,
            ..Field::guess("x")
        };
        let check = |kind, typed: &str| check_value(&field(kind), typed, now);
        assert_eq!(
            check(Kind::DateTime, "+2h"),
            Ok("2026-11-25 10:30:00".into())
        );
        assert_eq!(check(Kind::Date, "tomorrow"), Ok("2026-11-26".into()));
        assert_eq!(
            check(Kind::DateTime, "2026-12-01 17:00"),
            Ok("2026-12-01 17:00:00".into())
        );
        assert!(check(Kind::DateTime, "next friday").is_err());
        assert!(check(Kind::DateTime, "+é").is_err());
        assert!(check(Kind::DateTime, "+99999999999999999w").is_err());
        assert_eq!(check(Kind::Number, "1,200"), Ok("1200".into()));
        assert!(check(Kind::Number, "many").is_err());
        assert_eq!(check(Kind::Text, " a "), Ok("a".into()));
        let mut required = field(Kind::Text);
        required.mandatory = true;
        assert!(check_value(&required, " ", now).is_err());
    }
}
