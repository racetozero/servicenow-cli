//! Sections and the list.

use super::state::*;
use super::{App, PAUSE};
use crate::keys::Act;
use crate::msg::{Call, Fault, Outcome, Page, Req};
use crate::query;
use crate::tables::{self, Section};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Rows loaded at once when a long list is refreshed.
const REFRESH_LIMIT: u64 = 500;
/// The next page loads when the cursor is this close to the last row.
const NEAR_END: usize = 8;
const BACK_LIMIT: usize = 30;

impl App {
    /// A fresh list of a section or a pinned table.
    pub(super) fn section_list(&self, id: &str) -> ListState {
        let mut list = match Section::find(id) {
            Some(section) => {
                // Personal sections have no query until the user is known;
                // `load_list` does not run before.
                let query = section.query(self.me()).unwrap_or_default();
                ListState::new(section.table, section.label, &query, section.order())
            }
            None => {
                let table = id.strip_prefix("pin:").unwrap_or(id);
                ListState::new(
                    table,
                    &tables::many(table),
                    "",
                    vec![("sys_updated_on".into(), true)],
                )
            }
        };
        list.section = Some(id.to_owned());
        list
    }

    /// Shows another list and remembers where the person is, to come back.
    fn open_list(&mut self, list: ListState) {
        let record = self.record.take();
        let mut list = std::mem::replace(&mut self.list, list);
        list.extra = Extra::None;
        list.searching = false;
        if matches!(list.phase, ListPhase::Loading(_)) {
            list.phase = ListPhase::Waiting;
        }
        if let Some(name) = record
            .as_ref()
            .filter(|r| r.has_draft())
            .map(RecordState::name)
        {
            self.toast(
                ToastKind::Info,
                format!("Your changes to {name} wait. Esc goes back to them."),
            );
        }
        let snapshot = Snapshot {
            list,
            record,
            records: std::mem::take(&mut self.records),
            focus: self.focus,
        };
        self.back.push(snapshot);
        if self.back.len() > BACK_LIMIT
            && let Some(at) = self.back.iter().position(|s| !s.has_draft())
        {
            self.back.remove(at);
        }
        self.focus = Pane::List;
        self.follow_at = None;
        self.rail_to_list();
        self.load_list();
    }

    pub(super) fn go_back(&mut self) -> bool {
        let Some(snapshot) = self.back.pop() else {
            return false;
        };
        self.list = snapshot.list;
        self.record = snapshot.record;
        self.records = snapshot.records;
        self.focus = snapshot.focus;
        if self.focus == Pane::Record && self.record.is_none() {
            self.focus = Pane::List;
        }
        if let Some(record) = &mut self.record {
            record.forget();
        }
        for record in &mut self.records {
            record.forget();
        }
        self.rail_to_list();
        if self.list.phase == ListPhase::Waiting {
            self.load_list();
        }
        self.load_record();
        true
    }

    pub(super) fn open_section(&mut self, id: &str) {
        let list = self.section_list(id);
        self.open_list(list);
    }

    pub(super) fn open_table(&mut self, table: &str, label: &str) {
        let mut list = ListState::new(
            table,
            if label.is_empty() { table } else { label },
            "",
            vec![("sys_updated_on".into(), true)],
        );
        let pin = format!("pin:{table}");
        if self.settings.pins.iter().any(|p| p == table) {
            list.section = Some(pin);
        }
        self.open_list(list);
    }

    pub(super) fn open_rail(&mut self) {
        let rail = self.rail();
        if let Some(RailItem::Section(id, _)) = rail.get(self.rail_cursor) {
            let id = id.clone();
            if self.list.section.as_ref() == Some(&id) && !self.list.is_changed() {
                self.focus = Pane::List;
            } else {
                self.open_section(&id);
            }
        }
    }

    pub(super) fn move_rail(&mut self, by: i32) {
        let rail = self.rail();
        let mut at = self.rail_cursor as i32;
        loop {
            at += by.signum();
            match rail.get(usize::try_from(at).unwrap_or(usize::MAX)) {
                None => return,
                Some(RailItem::Group(_)) => {}
                Some(RailItem::Section(..)) => break,
            }
        }
        self.rail_cursor = at as usize;
    }

    /// Asks for the first rows, when what it needs is known.
    pub(super) fn load_list(&mut self) {
        self.list.phase = ListPhase::Waiting;
        self.list.rows.clear();
        self.list.total = None;
        self.list.next_offset = 0;
        self.list.exhausted = false;
        self.list.extra = Extra::None;
        self.list.cursor = 0;
        self.list.selected.clear();
        if self.waits_for_me() {
            match &self.me {
                Load::Ready(_) if self.list.query.is_empty() => {
                    if let Some(id) = self.list.section.clone() {
                        self.list = self.section_list(&id);
                    }
                }
                Load::Ready(_) => {}
                Load::Failed(fault) => {
                    self.list.phase = ListPhase::Failed(fault.clone());
                    return;
                }
                _ => return,
            }
        }
        let table = self.list.table.clone();
        if let Err(err) = sn_core::query::check_name("table", &table) {
            self.list.phase = ListPhase::Failed(err.into());
            return;
        }
        self.want_meta(&table);
        if self.list.columns.is_empty() && self.list.view.is_none() {
            match self.metas.get(&table) {
                Some(Load::Ready(meta)) => {
                    self.list.columns = tables::guess_columns(
                        meta.fields.iter().map(|f| f.name.as_str()),
                        Some(meta),
                    );
                }
                Some(Load::Failed(fault)) if fault.code == "auth" => return,
                // Without the fields, the rows themselves say which exist.
                Some(Load::Failed(_)) => {}
                _ => return,
            }
        }
        let req = self.req();
        self.list.phase = ListPhase::Loading(req);
        let limit = self.settings.page_size;
        self.ask_list(req, 0, limit);
    }

    fn ask_list(&mut self, req: Req, offset: u64, limit: u64) {
        let list = &self.list;
        let fields = match list.view.is_some() || list.columns.is_empty() {
            true => Vec::new(),
            false => tables::list_fields(&list.table, &list.columns),
        };
        let call = Call::List {
            req,
            table: list.table.clone(),
            query: self.list_query(),
            fields,
            view: list.view.clone(),
            offset,
            limit,
        };
        self.call(call);
    }

    pub(super) fn list_query(&self) -> String {
        let order = query::order_text(&self.list.order);
        sn_core::query::all([self.list.query.as_str(), order.as_str()])
    }

    /// Loads again and keeps what is on screen until the answer comes.
    pub(super) fn refresh_list(&mut self) {
        match self.list.phase {
            ListPhase::Ready => {
                let req = self.req();
                let limit = self.list.next_offset.clamp(
                    self.settings.page_size,
                    REFRESH_LIMIT.max(self.settings.page_size),
                );
                self.list.extra = Extra::Refresh { req, limit };
                self.ask_list(req, 0, limit);
            }
            ListPhase::Loading(_) => {}
            _ => self.load_list(),
        }
    }

    pub(super) fn list_reply(&mut self, req: Req, outcome: Outcome<Page>) {
        if self.list.phase == ListPhase::Loading(req) {
            match outcome {
                Ok(page) => {
                    let limit = self.settings.page_size;
                    self.take_page(page, limit);
                    self.list.phase = ListPhase::Ready;
                    self.infer_columns();
                    self.follow_soon();
                }
                Err(fault) => self.list.phase = ListPhase::Failed(fault),
            }
        } else if let Extra::Refresh { req: wanted, limit } = self.list.extra
            && wanted == req
        {
            self.list.extra = Extra::None;
            match outcome {
                Ok(page) => {
                    let current = self.list.current().map(|r| r.sys_id.clone());
                    self.list.rows.clear();
                    self.take_page(page, limit);
                    let rows = &self.list.rows;
                    self.list
                        .selected
                        .retain(|id| rows.iter().any(|r| r.sys_id == *id));
                    let visible = self.list.visible();
                    let at = visible
                        .iter()
                        .position(|i| Some(&self.list.rows[*i].sys_id) == current.as_ref());
                    self.list.cursor =
                        at.unwrap_or(self.list.cursor.min(visible.len().saturating_sub(1)));
                    if at.is_none() {
                        self.follow_soon();
                    }
                }
                Err(fault) => self.fail(&fault),
            }
        } else if self.list.extra == Extra::More(req) {
            self.list.extra = Extra::None;
            match outcome {
                Ok(page) => {
                    let limit = self.settings.page_size;
                    self.take_page(page, limit);
                }
                Err(fault) => self.fail(&fault),
            }
        }
    }

    fn take_page(&mut self, page: Page, limit: u64) {
        let got = page.rows.len() as u64;
        let first = self.list.rows.is_empty();
        let list = &mut self.list;
        list.next_offset = match first {
            true => limit,
            false => list.next_offset + limit,
        };
        list.total = page.total.or(if first { None } else { list.total });
        list.exhausted = match list.total {
            Some(total) => list.next_offset >= total,
            // Without a total, a short page is the last one.
            None => got < limit,
        };
        for row in page.rows {
            // Rows move between pages while people work.
            if row.sys_id.is_empty() || !list.rows.iter().any(|r| r.sys_id == row.sys_id) {
                list.rows.push(row);
            }
        }
        if let (Some(total), Some(id)) = (self.list.total, &self.list.section)
            && !self.list.is_filtered()
        {
            self.counts.insert(id.clone(), Load::Ready(total));
        }
    }

    /// Columns for rows whose fields were not chosen.
    fn infer_columns(&mut self) {
        if !self.list.columns.is_empty() && self.list.view.is_none() {
            return;
        }
        let Some(row) = self.list.rows.first() else {
            return;
        };
        let names: Vec<&str> = row.cells.iter().map(|(n, _)| n.as_str()).collect();
        let columns = match self.list.view {
            Some(_) => names
                .iter()
                .filter(|n| !n.starts_with("sys_") || **n == "sys_updated_on")
                .take(8)
                .copied()
                .collect::<Vec<_>>(),
            None => names,
        };
        let known = tables::columns(&self.list.table).unwrap_or_default();
        let meta = self.meta(&self.list.table);
        self.list.columns = match self.list.view {
            None => tables::guess_columns(columns.into_iter(), meta),
            // A view is in the order its author chose.
            Some(_) => columns
                .iter()
                .enumerate()
                .map(|(i, field)| {
                    let mut column = known.iter().find(|c| c.field == *field).cloned().unwrap_or(
                        tables::Column {
                            field: (*field).to_owned(),
                            label: self.field(&self.list.table, field).label.clone(),
                            look: tables::Look::Plain,
                            width: 16,
                            rank: 0,
                        },
                    );
                    column.rank = i as u8;
                    column
                })
                .collect(),
        };
        if !self
            .list
            .columns
            .iter()
            .any(|c| c.look == tables::Look::Main)
            && let Some(first) = self.list.columns.first_mut()
        {
            first.look = tables::Look::Main;
        }
    }

    pub(super) fn move_list(&mut self, by: i32) {
        let last = self.list.visible().len().saturating_sub(1);
        let at = (self.list.cursor as i64 + i64::from(by)).clamp(0, last as i64);
        self.move_list_to(at as usize);
    }

    pub(super) fn move_list_to(&mut self, at: usize) {
        let visible = self.list.visible().len();
        self.list.cursor = at.min(visible.saturating_sub(1));
        if self.list.cursor + NEAR_END >= visible {
            self.load_more();
        }
        self.follow_soon();
    }

    pub(super) fn load_more(&mut self) {
        let list = &self.list;
        if list.phase != ListPhase::Ready || list.exhausted || list.extra != Extra::None {
            return;
        }
        let req = self.req();
        self.list.extra = Extra::More(req);
        let (offset, limit) = (self.list.next_offset, self.settings.page_size);
        self.ask_list(req, offset, limit);
    }

    /// With the record beside the list, it follows the cursor once the
    /// cursor rests.
    pub(super) fn follow_soon(&mut self) {
        if self.previewing() || (self.record.is_some() && self.split()) {
            self.follow_at = Some(self.ms + 150);
        }
    }

    pub(super) fn follow(&mut self) {
        // While the person works in the record, the list does not take it away.
        if self.focus == Pane::Record && self.record.is_some() {
            return;
        }
        if !self.previewing() && (self.record.is_none() || !self.split()) {
            return;
        }
        let Some(row) = self.list.current().cloned() else {
            if !self.open_has_draft() {
                self.record = None;
                self.records.clear();
            }
            return;
        };
        if let Some(record) = &self.record {
            if record.sys_id == row.sys_id {
                return;
            }
            if self.open_has_draft() {
                let name = record.name();
                self.toast(
                    ToastKind::Info,
                    format!("{name} stays open: it has changes that are not sent."),
                );
                return;
            }
        }
        let tab = self.record.as_ref().map_or(Tab::Details, |r| r.tab);
        self.records.clear();
        self.set_record(row, tab);
    }

    pub(super) fn search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.list.searching = false;
                self.list.search.clear();
                self.list.cursor = 0;
                self.follow_soon();
            }
            KeyCode::Enter | KeyCode::Tab => {
                self.list.searching = false;
                self.follow_soon();
            }
            KeyCode::Down => self.move_list(1),
            KeyCode::Up => self.move_list(-1),
            KeyCode::PageDown => self.move_list(10),
            KeyCode::PageUp => self.move_list(-10),
            // The search covers loaded rows; the palette asks the instance.
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let text = self.list.search.text.clone();
                self.list.searching = false;
                self.list.search.clear();
                self.act(Act::Palette);
                if let Some(Overlay::Palette(p)) = self.overlays.last_mut() {
                    p.editor.insert(&text);
                    p.search_at = Some(self.ms + PAUSE);
                }
            }
            _ => {
                if self.list.search.key(key) {
                    self.list.cursor = 0;
                    self.follow_soon();
                }
            }
        }
    }

    /// Sorts by a column: first the natural way, then the other way.
    pub(super) fn sort_by(&mut self, field: &str) {
        let natural_down = self
            .list
            .columns
            .iter()
            .any(|c| c.field == field && c.look == tables::Look::Time);
        let down = match self.list.order.first() {
            Some((f, down)) if f == field => !down,
            _ => natural_down,
        };
        self.list.order = vec![(field.to_owned(), down)];
        self.load_list();
    }

    pub(super) fn set_query(&mut self, text: &str) {
        let (query, order) = query::split_order(text);
        self.list.query = query;
        if !order.is_empty() {
            self.list.order = order;
        }
        // A personal section with an empty query would mean everyone's work,
        // which is what the person just asked for; it stops being the section.
        if self.list.query.is_empty() && self.waits_for_me() {
            self.list.section = None;
            self.list.title = tables::many(&self.list.table);
        }
        self.load_list();
    }

    pub(super) fn reset_view(&mut self) {
        if !self.list.is_changed() {
            self.toast(ToastKind::Info, "This list has its own filter and order.");
            return;
        }
        (self.list.query, self.list.order) = self.list.base.clone();
        self.load_list();
    }

    pub(super) fn toggle_select(&mut self) {
        let Some(id) = self.list.current().map(|r| r.sys_id.clone()) else {
            return;
        };
        if !self.list.selected.remove(&id) {
            self.list.selected.insert(id);
        }
        self.move_list(1);
    }

    pub(super) fn select_all(&mut self) {
        let visible = self.list.visible();
        let all = visible
            .iter()
            .all(|i| self.list.selected.contains(&self.list.rows[*i].sys_id));
        for i in visible {
            let id = self.list.rows[i].sys_id.clone();
            match all {
                true => self.list.selected.remove(&id),
                false => self.list.selected.insert(id),
            };
        }
    }

    /// Keeps the table in the rail, or takes it out.
    pub(super) fn toggle_pin(&mut self) {
        let table = match self.focus {
            Pane::Rail => {
                let rail = self.rail();
                match rail.get(self.rail_cursor) {
                    Some(RailItem::Section(id, _)) if id.starts_with("pin:") => {
                        id.trim_start_matches("pin:").to_owned()
                    }
                    _ => {
                        self.toast(
                            ToastKind::Info,
                            "This section is always here. Open a table with t and pin it with p.",
                        );
                        return;
                    }
                }
            }
            _ => self.list.table.clone(),
        };
        let id = format!("pin:{table}");
        if let Some(at) = self.settings.pins.iter().position(|p| *p == table) {
            self.settings.pins.remove(at);
            self.counts.remove(&id);
            if self.list.section.as_deref() == Some(&id) {
                self.list.section = None;
            }
            self.toast(
                ToastKind::Good,
                format!("Unpinned {}", tables::many(&table)),
            );
        } else {
            self.settings.pins.push(table.clone());
            if self.list.section.is_none() && self.list.table == table {
                self.list.section = Some(id);
            }
            self.toast(
                ToastKind::Good,
                format!(
                    "Pinned {}. It is in the sections now.",
                    tables::many(&table)
                ),
            );
            self.load_counts();
        }
        self.rail_cursor = self.rail_cursor.min(self.rail().len().saturating_sub(1));
        if matches!(self.rail().get(self.rail_cursor), Some(RailItem::Group(_))) {
            self.rail_cursor = self.rail_cursor.saturating_sub(1);
        }
        let pins = self.settings.pins.clone();
        self.effects.push(crate::msg::Effect::SavePins(pins));
    }

    /// What an empty list says.
    pub fn empty_text(&self) -> String {
        if !self.list.search.text.trim().is_empty() {
            return match self.list.exhausted {
                true => "No row has these words. Esc clears the search; Ctrl-K searches the whole instance.".into(),
                false => "No loaded row has these words. Esc clears the search; Ctrl-K searches the whole instance.".into(),
            };
        }
        if self.list.is_filtered() {
            return "Nothing matches this filter. Press f to change it, or 0 to reset it.".into();
        }
        match self.list.section.as_deref().and_then(Section::find) {
            Some(section) => section.empty.to_owned(),
            None => format!(
                "{} has no record that you can read.",
                tables::many(&self.list.table)
            ),
        }
    }

    pub(super) fn retry_list(&mut self) {
        if let Load::Failed(_) = &self.me
            && self.waits_for_me()
        {
            let req = self.req();
            self.me = Load::Loading(req);
            self.list.phase = ListPhase::Waiting;
            self.call(Call::Me { req });
            return;
        }
        if let ListPhase::Failed(Fault { .. }) = &self.list.phase {
            let table = self.list.table.clone();
            if matches!(self.metas.get(&table), Some(Load::Failed(_))) {
                self.metas.remove(&table);
            }
            self.load_list();
        } else {
            self.refresh_list();
        }
    }
}
