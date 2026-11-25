//! Actions: what a key, a menu item or the palette does.

use super::App;
use super::state::*;
use crate::input::Editor;
use crate::keys::{self, Act, BINDINGS, Place};
use crate::model::Row;
use crate::msg::{Effect, Fault};
use crate::query;
use crate::tables::{self, SECTIONS};
use crate::text;

impl App {
    pub(super) fn act(&mut self, act: Act) {
        match act {
            Act::Up => self.step(-1),
            Act::Down => self.step(1),
            Act::PageUp => self.step(-self.page()),
            Act::PageDown => self.step(self.page()),
            Act::Top => self.step(i32::MIN / 2),
            Act::Bottom => self.step(i32::MAX / 2),
            Act::Open => match self.focus {
                Pane::Rail => self.open_rail(),
                Pane::List => match &self.list.phase {
                    ListPhase::Failed(_) => self.retry_list(),
                    _ => self.open_row(),
                },
                Pane::Record => match self.record.as_ref().map(|r| &r.full) {
                    Some(Load::Failed(_)) => self.reload_record(),
                    _ => self.open_in_record(),
                },
            },
            Act::Back => self.step_back(),
            Act::NextPane => self.cycle(1),
            Act::PrevPane => self.cycle(-1),
            Act::Goto => self.overlays.push(Overlay::Goto),
            Act::Palette | Act::OpenTable => self.overlays.push(Overlay::Palette(Palette {
                tables_only: act == Act::OpenTable,
                ..Palette::default()
            })),
            Act::Help => self.overlays.push(Overlay::Help(0)),
            Act::Quit => self.quit(),
            Act::Refresh => {
                self.metas.retain(|_, m| !matches!(m, Load::Failed(_)));
                match self.focus {
                    Pane::Record => self.reload_record(),
                    _ => self.retry_list(),
                }
                self.load_counts();
            }
            Act::AutoRefresh => {
                self.auto_refresh = !self.auto_refresh;
                self.refresh_at = self.ms;
                let text = match self.auto_refresh {
                    true => "Auto-refresh is on: every 30 seconds.",
                    false => "Auto-refresh is off.",
                };
                self.toast(ToastKind::Info, text);
            }
            Act::Profiles => {
                if self.profiles.is_empty() {
                    self.toast(
                        ToastKind::Info,
                        "There is only this connection. `sn profile add` makes another.",
                    );
                    return;
                }
                let at = self
                    .profiles
                    .iter()
                    .position(|p| p.name == self.connection.profile);
                self.overlays.push(Overlay::Profiles(at.unwrap_or(0)));
            }
            Act::SignIn => {
                if self.is_sending() {
                    self.toast(
                        ToastKind::Warning,
                        "A save is on its way. Wait until it ends.",
                    );
                } else {
                    self.effects.push(Effect::SignIn);
                }
            }
            Act::Search => {
                self.focus = Pane::List;
                self.list.searching = true;
            }
            Act::Filter => self.open_filter(),
            Act::Query => {
                let text = self.list_query();
                self.overlays.push(Overlay::Query(Editor::new(text)));
            }
            Act::ResetView => self.reset_view(),
            Act::Sort => {
                let at = self
                    .list
                    .order
                    .first()
                    .and_then(|(f, _)| self.list.columns.iter().position(|c| c.field == *f));
                self.overlays.push(Overlay::Sort(at.unwrap_or(0)));
            }
            Act::Select => self.toggle_select(),
            Act::SelectAll => self.select_all(),
            Act::Pin => self.toggle_pin(),
            Act::New => {
                if self.connection.read_only {
                    self.toast(ToastKind::Warning, self.read_only_text());
                } else {
                    self.new_record();
                }
            }
            Act::Menu => {
                let items = self.menu();
                if items.is_empty() {
                    self.toast(ToastKind::Info, "Select a record first.");
                } else {
                    self.overlays.push(Overlay::Menu { items, cursor: 0 });
                }
            }
            Act::Comment => self.open_form(FormKind::Comment),
            Act::WorkNote => self.open_form(FormKind::WorkNote),
            Act::AssignMe => self.assign_me(),
            Act::Assign => self.open_form(FormKind::Assign),
            Act::Resolve => self.open_form(FormKind::Resolve),
            Act::Approve => self.open_form(FormKind::Approve),
            Act::Reject => self.open_form(FormKind::Reject),
            Act::CopyName | Act::CopyLink => self.copy(act),
            Act::Browser => {
                let url = self.link();
                self.effects.push(Effect::Open(url));
            }
            Act::Preview => {
                if !self.side_by_side() {
                    self.toast(
                        ToastKind::Info,
                        "The terminal is too narrow for both. Press Enter to open the record.",
                    );
                    return;
                }
                let on = !self.previewing();
                self.preview = Some(on);
                if on {
                    self.follow();
                } else if !self.open_has_draft() {
                    self.close_record();
                }
            }
            Act::NextTab => self.turn_tab(1),
            Act::PrevTab => self.turn_tab(-1),
            Act::Details => self.show_tab(Tab::Details),
            Act::Activity => self.show_tab(Tab::Activity),
            Act::Related => self.show_tab(Tab::Related),
            Act::Attachments => self.show_tab(Tab::Attachments),
            Act::Slas => self.show_tab(Tab::Slas),
            Act::History => self.show_tab(Tab::History),
            Act::Empty => {
                if let Some(record) = &mut self.record {
                    record.show_empty = !record.show_empty;
                    record.cursor = 0;
                }
            }
            Act::Follow => self.follow_reference(),
            Act::Review => self.review_edits(),
            Act::Undo => self.undo_field(),
            Act::Discard => {
                if self.record.as_ref().is_some_and(|r| !r.edits.is_empty()) {
                    self.overlays.push(Overlay::Confirm {
                        text: "Drop every change to this record that is not sent?".into(),
                        yes: "drop",
                        then: Then::DropEdits,
                    });
                } else {
                    self.toast(ToastKind::Info, "There is no change to drop.");
                }
            }
        }
    }

    fn page(&self) -> i32 {
        let rows = match self.focus {
            Pane::Record => self.places.record_rows,
            _ => self.places.list_rows,
        };
        (rows.saturating_sub(1)).max(1) as i32
    }

    fn step(&mut self, by: i32) {
        match self.focus {
            Pane::Rail => {
                if by.abs() > 1 {
                    let rail = self.rail();
                    let mut places = rail
                        .iter()
                        .enumerate()
                        .filter(|(_, i)| matches!(i, RailItem::Section(..)))
                        .map(|(at, _)| at);
                    self.rail_cursor = match by < 0 {
                        true => places.next(),
                        false => places.next_back(),
                    }
                    .unwrap_or(0);
                } else {
                    self.move_rail(by);
                }
            }
            Pane::List => self.move_list(by),
            Pane::Record => self.move_record(by),
        }
    }

    /// The next pane that is on screen.
    fn cycle(&mut self, by: i32) {
        let mut panes = vec![Pane::Rail, Pane::List];
        if self.record.is_some() {
            panes.push(Pane::Record);
        }
        let at = panes.iter().position(|p| *p == self.focus).unwrap_or(1) as i32;
        self.focus = panes[(at + by).rem_euclid(panes.len() as i32) as usize];
    }

    pub(super) fn close_record(&mut self) {
        self.record = None;
        self.records.clear();
        self.focus = Pane::List;
    }

    fn step_back(&mut self) {
        match self.focus {
            Pane::Rail => self.focus = Pane::List,
            Pane::Record => {
                if self.record.as_ref().is_some_and(RecordState::has_draft) {
                    self.ask_to_close();
                } else if let Some(mut under) = self.records.pop() {
                    under.forget();
                    self.record = Some(under);
                    self.load_record();
                } else if self.previewing() {
                    self.focus = Pane::List;
                    self.follow_soon();
                } else {
                    self.close_record();
                }
            }
            Pane::List => {
                if !self.list.search.text.is_empty() {
                    self.list.search.clear();
                    self.list.cursor = 0;
                } else if !self.list.selected.is_empty() {
                    self.list.selected.clear();
                } else if self.record.is_some() && !self.previewing() && self.split() {
                    if self.open_has_draft() {
                        // One record at a time, so none is lost unseen.
                        self.focus = Pane::Record;
                        self.step_back();
                    } else {
                        self.close_record();
                    }
                } else if !self.go_back() {
                    self.toast(ToastKind::Info, "You are at the start. Press q to leave.");
                }
            }
        }
    }

    fn ask_to_close(&mut self) {
        self.overlays.push(Overlay::Confirm {
            text: "This record has changes that are not sent. Close it and lose them?".into(),
            yes: "close",
            then: Then::CloseRecord,
        });
    }

    pub(super) fn confirmed(&mut self, then: Then) {
        match then {
            Then::Quit => self.effects.push(Effect::Quit),
            Then::DropEdits => {
                if let Some(record) = &mut self.record {
                    record.edits.clear();
                    if record.is_new() {
                        self.confirmed(Then::CloseRecord);
                    }
                }
            }
            Then::CloseRecord => {
                if let Some(record) = &mut self.record {
                    record.edits.clear();
                    record.inline = None;
                }
                self.focus = Pane::Record;
                self.step_back();
            }
            Then::SwitchProfile(name) => self.effects.push(Effect::SwitchProfile(name)),
        }
    }

    fn turn_tab(&mut self, by: i32) {
        if let Some(record) = &self.record {
            let at = record.tab.index() as i32 + by;
            let tab = Tab::ALL[at.rem_euclid(Tab::ALL.len() as i32) as usize];
            self.show_tab(tab);
        }
    }

    fn copy(&mut self, act: Act) {
        let targets = self.targets();
        let text = match act {
            Act::CopyLink => self.link(),
            _ if targets.is_empty() => {
                self.toast(ToastKind::Info, "Select a record first.");
                return;
            }
            _ => targets
                .iter()
                .map(|t| t.name.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let shown = match targets.len() {
            0 | 1 => text.clone(),
            n if act == Act::CopyName => format!("{n} numbers"),
            _ => text.clone(),
        };
        self.toast(ToastKind::Good, format!("Copied {}", text::line(&shown)));
        self.effects.push(Effect::Copy(text));
    }

    /// The address of the record, or of the list, in the web interface.
    pub fn link(&self) -> String {
        let base = &self.connection.base;
        let new =
            self.focus == Pane::Record && self.record.as_ref().is_some_and(RecordState::is_new);
        match self.targets().first() {
            Some(t) => format!(
                "{base}/nav_to.do?uri={}.do%3Fsys_id%3D{}",
                t.table, t.sys_id
            ),
            None if new => format!("{base}/nav_to.do?uri={}.do%3Fsys_id%3D-1", self.list.table),
            None => {
                // Encoded twice: once for the list, once for the frame.
                let query = sn_core::percent::encode(&sn_core::percent::encode(&self.list_query()));
                format!(
                    "{base}/nav_to.do?uri={}_list.do%3Fsysparm_query%3D{query}",
                    self.list.table
                )
            }
        }
    }

    /// What can be done with the target now, for the action menu.
    pub fn menu(&self) -> Vec<Act> {
        let mut out = Vec::new();
        let targets = self.target_rows();
        if targets.is_empty() {
            return out;
        }
        if self
            .record
            .as_ref()
            .is_some_and(|r| self.focus == Pane::Record && !r.edits.is_empty())
        {
            out.extend([Act::Review, Act::Discard]);
        }
        for act in [
            Act::Approve,
            Act::Reject,
            Act::AssignMe,
            Act::Assign,
            Act::WorkNote,
            Act::Comment,
            Act::Resolve,
        ] {
            if self.possible(act, &targets) {
                out.push(act);
            }
        }
        out.extend([Act::Browser, Act::CopyName, Act::CopyLink]);
        out
    }

    /// True when the action would do something for what is in focus.
    pub fn possible(&self, act: Act, targets: &[(&str, &Row)]) -> bool {
        let form = |kind| self.form_problem(kind, targets).is_none();
        match act {
            Act::Comment => form(FormKind::Comment),
            Act::WorkNote => form(FormKind::WorkNote),
            Act::Assign => form(FormKind::Assign),
            Act::AssignMe => {
                let mine = self.me().map(|m| m.sys_id.as_str());
                form(FormKind::Assign)
                    && targets
                        .iter()
                        .any(|(_, row)| Some(row.value("assigned_to")) != mine)
            }
            Act::Resolve => form(FormKind::Resolve),
            Act::Approve | Act::Reject => form(FormKind::Approve),
            Act::New => {
                !self.connection.read_only
                    && !matches!(
                        self.list.table.as_str(),
                        "task" | "sysapproval_approver" | "cmdb_ci"
                    )
            }
            Act::Follow => self.reference_here().is_some(),
            Act::Review | Act::Discard | Act::Undo => {
                self.record.as_ref().is_some_and(|r| !r.edits.is_empty())
            }
            Act::CopyName | Act::Menu => !targets.is_empty(),
            Act::ResetView => self.list.is_changed(),
            Act::Profiles => self.profiles.len() > 1,
            Act::Preview => self.side_by_side(),
            Act::Pin if self.focus == Pane::Rail => matches!(
                self.rail().get(self.rail_cursor),
                Some(RailItem::Section(id, _)) if id.starts_with("pin:")
            ),
            _ => true,
        }
    }

    /// What the palette offers for what was typed.
    pub fn palette_items(&self, palette: &Palette) -> Vec<PaletteItem> {
        let typed = palette.editor.text.trim();
        let mut out: Vec<PaletteItem> = Vec::new();
        let is_link = typed.starts_with("http://") || typed.starts_with("https://");
        let is_number = sn_core::refs::prefix(typed).is_some() || typed.contains(':');
        if !palette.tables_only && (is_link || is_number) {
            out.push(PaletteItem {
                group: "Open",
                title: match is_link {
                    true => "Open this link".into(),
                    false => format!("Open {}", typed.to_uppercase()),
                },
                detail: "record".into(),
                action: PaletteDo::Locate(typed.to_owned()),
            });
        }
        let mut scored: Vec<(i32, PaletteItem)> = Vec::new();
        let mut add = |weight: i32, also: &str, item: PaletteItem| {
            let score = match typed.is_empty() {
                true => Some(0),
                false => {
                    text::fuzzy(typed, &item.title).max(text::fuzzy(typed, also).map(|s| s - 10))
                }
            };
            if let Some(score) = score {
                scored.push((score + weight, item));
            }
        };
        if !palette.tables_only {
            for section in SECTIONS {
                add(
                    6,
                    section.table,
                    PaletteItem {
                        group: "Go to",
                        title: section.label.to_owned(),
                        detail: format!("g {}", section.key),
                        action: PaletteDo::Section(section.id.to_owned()),
                    },
                );
            }
            let place = self.place();
            let targets = self.target_rows();
            let mut seen: Vec<Act> = Vec::new();
            for binding in BINDINGS {
                let moving = matches!(
                    binding.act,
                    Act::Up
                        | Act::Down
                        | Act::PageUp
                        | Act::PageDown
                        | Act::Top
                        | Act::Bottom
                        | Act::Open
                        | Act::Back
                        | Act::NextPane
                        | Act::PrevPane
                        | Act::NextTab
                        | Act::PrevTab
                        | Act::Palette
                        | Act::Select
                );
                let here = binding.place == Place::Anywhere || binding.place == place;
                if moving
                    || !here
                    || seen.contains(&binding.act)
                    || !self.possible(binding.act, &targets)
                {
                    continue;
                }
                seen.push(binding.act);
                add(
                    0,
                    binding.short,
                    PaletteItem {
                        group: "Do",
                        title: binding.long.to_owned(),
                        detail: keys::label(binding, self.theme.caps.unicode),
                        action: PaletteDo::Act(binding.act),
                    },
                );
            }
        }
        if palette.tables_only || !typed.is_empty() {
            for (table, label) in tables::known_tables() {
                add(
                    -4,
                    table,
                    PaletteItem {
                        group: "Table",
                        title: label.to_owned(),
                        detail: table.to_owned(),
                        action: PaletteDo::Table(table.to_owned(), label.to_owned()),
                    },
                );
            }
        }
        for (table, label) in palette.tables.ready().into_iter().flatten() {
            if tables::known_tables().any(|(t, _)| t == table) {
                continue;
            }
            add(
                -6,
                table,
                PaletteItem {
                    group: "Table",
                    title: match label.is_empty() {
                        true => table.clone(),
                        false => label.clone(),
                    },
                    detail: table.clone(),
                    action: PaletteDo::Table(table.clone(), label.clone()),
                },
            );
        }
        scored.sort_by_key(|(score, _)| -score);
        // Records come right after the best matches, so a long list of
        // actions does not bury them.
        let records: Vec<PaletteItem> = palette
            .records
            .ready()
            .into_iter()
            .flatten()
            .map(|row| PaletteItem {
                group: "Record",
                title: match row.summary() {
                    summary if summary.is_empty() => row.name(),
                    summary => format!("{} {} {summary}", row.name(), self.theme.g.sep),
                },
                detail: text::line(row.display("state")),
                action: PaletteDo::Record(row.table.clone(), row.sys_id.clone()),
            })
            .collect();
        let mut best: Vec<PaletteItem> = scored.into_iter().map(|(_, i)| i).collect();
        let rest = best.split_off(
            best.len()
                .min(if records.is_empty() { usize::MAX } else { 4 }),
        );
        out.extend(best);
        out.extend(records);
        out.extend(rest);
        // A table the instance does not list can still be opened by its name.
        let name = typed.to_ascii_lowercase();
        if palette.tables_only
            && sn_core::query::check_name("table", &name).is_ok()
            && !out
                .iter()
                .any(|i| matches!(&i.action, PaletteDo::Table(t, _) if *t == name))
        {
            out.push(PaletteItem {
                group: "Table",
                title: format!("Open the table {name}"),
                detail: name.clone(),
                action: PaletteDo::Table(name, String::new()),
            });
        }
        out
    }

    pub(super) fn palette_do(&mut self, action: PaletteDo) {
        match action {
            PaletteDo::Act(act) => self.act(act),
            PaletteDo::Section(id) => self.open_section(&id),
            PaletteDo::Table(table, label) => self.open_table(&table, &label),
            PaletteDo::Record(table, sys_id) => self.open_record(&table, &sys_id),
            PaletteDo::Locate(text) => self.locate(&text),
        }
    }

    /// What a person reads for a stored value.
    pub fn value_label(&self, table: &str, field: &str, value: &str) -> String {
        let known = self.field(table, field);
        if let Some((_, label)) = known.choices.iter().find(|(v, _)| v == value) {
            return text::line(label);
        }
        if known.kind == crate::model::Kind::Bool {
            return match value {
                "true" => "yes".into(),
                "false" => "no".into(),
                other => other.to_owned(),
            };
        }
        if let Some(me) = self.me().filter(|m| m.sys_id == value) {
            return me.name.clone();
        }
        if value.starts_with("javascript:") {
            return "(script)".into();
        }
        if !sn_core::is_sys_id(value) {
            return value.to_owned();
        }
        let rows = self
            .list
            .rows
            .iter()
            .chain(self.record.iter().map(|r| &r.row));
        for row in rows {
            if row.value(field) == value {
                return text::line(row.display(field));
            }
        }
        "one record".into()
    }

    fn open_filter(&mut self) {
        let table = self.list.table.clone();
        match self.metas.get(&table) {
            Some(Load::Ready(_)) => {}
            Some(Load::Failed(Fault { .. })) => {
                // Without the fields, the encoded query still works.
                self.toast(
                    ToastKind::Info,
                    format!("The fields of {table} are not known, so this is the encoded query."),
                );
                self.act(Act::Query);
                return;
            }
            _ => {
                self.want_meta(&table);
                self.pending = Some(Pending::Fields(table, Act::Filter));
                return;
            }
        }
        let mut conditions = query::parse(&self.list.query);
        for condition in conditions.iter_mut().filter(|c| c.raw.is_none()) {
            condition.label = self.value_label(&table, &condition.field, &condition.value);
        }
        let row = conditions.len();
        self.overlays.push(Overlay::Filter(FilterBuilder {
            table,
            conditions,
            row,
            column: 0,
            editing: None,
        }));
    }
}
