//! Keys, clicks and pasted text while something lies over the workspace.

use super::App;
use super::record::check_value;
use super::state::*;
use crate::input::Editor;
use crate::keys::Act;
use crate::model::{Cell, Field, Kind};
use crate::msg::Candidate;
use crate::query::{Condition, Join, Op};
use crate::tables::SECTIONS;
use crate::text;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Moves a cursor in a list of `count` items.
fn step(at: usize, by: i32, count: usize) -> usize {
    (at as i64 + i64::from(by)).clamp(0, count.saturating_sub(1) as i64) as usize
}

/// How far a key moves in a list, when it does.
fn movement(key: KeyEvent, letters: bool) -> Option<i32> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Up => Some(-1),
        KeyCode::Down => Some(1),
        KeyCode::Char('p') if ctrl => Some(-1),
        KeyCode::Char('n') if ctrl => Some(1),
        KeyCode::PageUp => Some(-10),
        KeyCode::PageDown => Some(10),
        KeyCode::Char('k') if letters => Some(-1),
        KeyCode::Char('j') if letters => Some(1),
        KeyCode::Home if letters => Some(i32::MIN / 2),
        KeyCode::End if letters => Some(i32::MAX / 2),
        _ => None,
    }
}

impl App {
    pub(super) fn overlay_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // The same key everywhere means "done with this".
        let submit = (ctrl && key.code == KeyCode::Char('s'))
            || (key.code == KeyCode::Enter && (alt || ctrl));
        let last_column = self.list.columns.len().saturating_sub(1);
        let max_scroll = self.places.overlay_max_scroll;
        let Some(top) = self.overlays.last_mut() else {
            return;
        };
        match top {
            Overlay::Help(scroll) => match movement(key, true) {
                Some(by) => *scroll = step(*scroll, by, max_scroll + 1),
                None => {
                    self.overlays.pop();
                }
            },
            Overlay::Read { scroll, .. } => match movement(key, true) {
                Some(by) => *scroll = step(*scroll, by, max_scroll + 1),
                None => {
                    self.overlays.pop();
                }
            },
            Overlay::Goto => {
                self.overlays.pop();
                if let KeyCode::Char(c) = key.code {
                    match SECTIONS.iter().find(|s| s.key == c) {
                        Some(section) => self.open_section(section.id),
                        None if c == 'g' => self.act(Act::Top),
                        None => self.toast(
                            ToastKind::Info,
                            format!("No section has the letter {}.", text::line(&c.to_string())),
                        ),
                    }
                }
            }
            Overlay::Palette(palette) => match (key.code, movement(key, false)) {
                (KeyCode::Esc, _) => {
                    self.overlays.pop();
                }
                (KeyCode::Enter, _) => self.palette_enter(),
                (_, Some(by)) => {
                    let count = match self.overlays.last() {
                        Some(Overlay::Palette(p)) => self.palette_items(p).len(),
                        _ => 0,
                    };
                    if let Some(Overlay::Palette(p)) = self.overlays.last_mut() {
                        p.cursor = step(p.cursor, by, count);
                    }
                }
                _ => {
                    if palette.editor.key(key) {
                        self.typed_in_search();
                    }
                }
            },
            Overlay::Menu { items, cursor } => {
                if let Some(by) = movement(key, false) {
                    *cursor = step(*cursor, by, items.len());
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.overlays.pop();
                    }
                    KeyCode::Enter => self.menu_enter(),
                    _ => {
                        // The letters that work outside work here too.
                        let here = items.clone();
                        if let Some(act) =
                            crate::keys::find(key, self.place()).filter(|a| here.contains(a))
                        {
                            self.overlays.pop();
                            self.act(act);
                        } else if let Some(by) = movement(key, true)
                            && let Some(Overlay::Menu { items, cursor }) = self.overlays.last_mut()
                        {
                            *cursor = step(*cursor, by, items.len());
                        }
                    }
                }
            }
            Overlay::Profiles(cursor) => {
                let count = self.profiles.len();
                match (key.code, movement(key, true)) {
                    (_, Some(by)) => *cursor = step(*cursor, by, count),
                    (KeyCode::Enter, _) => self.profile_enter(),
                    (KeyCode::Esc | KeyCode::Char('q' | 'P'), _) => {
                        self.overlays.pop();
                    }
                    _ => {}
                }
            }
            Overlay::Sort(cursor) => match (key.code, movement(key, true)) {
                (_, Some(by)) => *cursor = step(*cursor, by, last_column + 1),
                (KeyCode::Enter | KeyCode::Char('s' | ' '), _) => self.sort_enter(),
                (KeyCode::Esc | KeyCode::Char('q'), _) => {
                    self.overlays.pop();
                }
                _ => {}
            },
            Overlay::Query(editor) => match key.code {
                KeyCode::Esc => {
                    self.overlays.pop();
                }
                KeyCode::Enter => {
                    let text = editor.text.trim().to_owned();
                    self.overlays.pop();
                    self.set_query(&text);
                }
                _ => {
                    editor.key(key);
                }
            },
            Overlay::Filter(_) => self.filter_key(key, submit),
            Overlay::Form(_) => self.form_key(key, submit),
            Overlay::Picker(picker) => match (key.code, movement(key, false)) {
                (KeyCode::Esc, _) => {
                    self.overlays.pop();
                }
                (KeyCode::Enter | KeyCode::Tab, _) => self.picker_enter(),
                (_, Some(by)) => {
                    let count = picker.items().len();
                    picker.cursor = step(picker.cursor, by, count);
                }
                _ => {
                    if picker.editor.key(key) {
                        self.typed_in_search();
                    }
                }
            },
            Overlay::Text { .. } if submit => self.text_enter(),
            Overlay::Text { editor, field } => match key.code {
                KeyCode::Esc => {
                    let before = self
                        .record
                        .as_ref()
                        .map(|r| r.cell(&field.name).value.clone())
                        .unwrap_or_default();
                    // A long text is not lost by one key.
                    match editor.text == before {
                        true => {
                            self.overlays.pop();
                        }
                        false => self.text_enter(),
                    }
                }
                _ => {
                    editor.key(key);
                }
            },
            Overlay::Review(review) => {
                if matches!(review.state, ReviewState::Sending(..)) {
                    return;
                }
                match (key.code, movement(key, true)) {
                    (_, Some(by)) => review.scroll = step(review.scroll, by, max_scroll + 1),
                    (KeyCode::Esc | KeyCode::Char('q'), _) => {
                        self.overlays.pop();
                    }
                    (KeyCode::Enter, _) => self.send_review(),
                    _ => {}
                }
            }
            Overlay::Confirm { then, .. } => {
                let then = then.clone();
                self.overlays.pop();
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    self.confirmed(then);
                }
            }
        }
    }

    pub(super) fn overlay_paste(&mut self, pasted: &str) {
        match self.overlays.last_mut() {
            Some(Overlay::Palette(p)) => {
                p.editor.insert(pasted);
                self.typed_in_search();
            }
            Some(Overlay::Picker(p)) => {
                p.editor.insert(pasted);
                self.typed_in_search();
            }
            Some(Overlay::Query(editor) | Overlay::Text { editor, .. }) => editor.insert(pasted),
            Some(Overlay::Filter(f)) => {
                if let Some(editor) = &mut f.editing {
                    editor.insert(pasted);
                }
            }
            Some(Overlay::Form(form)) => {
                if let Some(field) = form.fields.get_mut(form.cursor)
                    && field.is_typed()
                {
                    field.editor.insert(pasted);
                }
            }
            _ => {}
        }
    }

    /// A click on an item of the top overlay.
    pub(super) fn overlay_click(&mut self, at: usize) {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        match self.overlays.last_mut() {
            Some(Overlay::Palette(p)) => {
                p.cursor = at;
                self.palette_enter();
            }
            Some(Overlay::Menu { cursor, .. }) => {
                *cursor = at;
                self.menu_enter();
            }
            Some(Overlay::Profiles(cursor)) => {
                *cursor = at;
                self.profile_enter();
            }
            Some(Overlay::Sort(cursor)) => {
                *cursor = at;
                self.sort_enter();
            }
            Some(Overlay::Picker(p)) => {
                p.cursor = at;
                self.picker_enter();
            }
            Some(Overlay::Goto) => {
                self.overlays.pop();
                if let Some(section) = SECTIONS.get(at) {
                    self.open_section(section.id);
                }
            }
            Some(Overlay::Form(form)) => {
                let again = form.cursor == at;
                form.cursor = at.min(form.fields.len().saturating_sub(1));
                let picks = form.fields.get(form.cursor).is_some_and(|f| !f.is_typed());
                if again && picks {
                    self.form_key(enter, false);
                }
            }
            Some(Overlay::Filter(f)) => {
                // Three cells in each row.
                f.row = at / 3;
                f.column = at % 3;
                f.editing = None;
                self.filter_key(enter, false);
            }
            _ => {}
        }
    }

    fn palette_enter(&mut self) {
        let Some(Overlay::Palette(palette)) = self.overlays.last() else {
            return;
        };
        let items = self.palette_items(palette);
        let Some(item) = items.get(palette.cursor) else {
            return;
        };
        let action = item.action.clone();
        self.overlays.pop();
        self.palette_do(action);
    }

    fn menu_enter(&mut self) {
        if let Some(Overlay::Menu { items, cursor }) = self.overlays.pop()
            && let Some(act) = items.get(cursor)
        {
            self.act(*act);
        }
    }

    fn profile_enter(&mut self) {
        let Some(Overlay::Profiles(cursor)) = self.overlays.pop() else {
            return;
        };
        let Some(profile) = self.profiles.get(cursor) else {
            return;
        };
        let name = profile.name.clone();
        if name == self.connection.profile && self.connection.signed_out.is_none() {
            return;
        }
        if self.is_sending() {
            self.toast(
                ToastKind::Warning,
                "A save is on its way. Wait until it ends.",
            );
        } else if self.has_unsent() {
            self.overlays.push(Overlay::Confirm {
                text: format!(
                    "You have changes that are not sent. Switch to {name} and lose them?"
                ),
                yes: "switch",
                then: Then::SwitchProfile(name),
            });
        } else {
            self.effects.push(crate::msg::Effect::SwitchProfile(name));
        }
    }

    fn sort_enter(&mut self) {
        if let Some(Overlay::Sort(cursor)) = self.overlays.pop()
            && let Some(column) = self.list.columns.get(cursor)
        {
            let field = column.field.clone();
            self.sort_by(&field);
        }
    }

    fn text_enter(&mut self) {
        let Some(Overlay::Text { field, editor }) = self.overlays.pop() else {
            return;
        };
        let value = editor.text;
        if value.trim().is_empty() && field.mandatory {
            self.toast(
                ToastKind::Warning,
                format!("{} cannot be empty, so it stays as it was.", field.label),
            );
            return;
        }
        if let Some(record) = &mut self.record {
            let old = record.row.cell(&field.name).clone();
            let cell = match value == old.value {
                true => old,
                false => Cell {
                    display: value.clone(),
                    value,
                },
            };
            record.set_edit(&field, cell);
        }
    }

    fn picker_enter(&mut self) {
        let Some(Overlay::Picker(picker)) = self.overlays.last() else {
            return;
        };
        let Some(chosen) = picker.items().get(picker.cursor).cloned() else {
            return;
        };
        let Some(Overlay::Picker(picker)) = self.overlays.pop() else {
            return;
        };
        let cell = match chosen.value.is_empty() {
            true => Cell::default(),
            false => Cell {
                value: chosen.value,
                display: chosen.label,
            },
        };
        match picker.purpose {
            PickFor::Record => {
                if let Some(record) = &mut self.record {
                    record.set_edit(&picker.field, cell);
                }
            }
            PickFor::Form(i) => {
                if let Some(Overlay::Form(form)) = self.overlays.last_mut()
                    && let Some(field) = form.fields.get_mut(i)
                {
                    field.cell = cell;
                    form.error = None;
                    form.cursor = (i + 1).min(form.fields.len() - 1);
                }
            }
            PickFor::FilterField(i) => {
                let field = match self.overlays.last() {
                    Some(Overlay::Filter(f)) => self.field(&f.table, &cell.value).into_owned(),
                    _ => return,
                };
                if let Some(Overlay::Filter(f)) = self.overlays.last_mut() {
                    let mut condition = Condition::new(&field);
                    match f.conditions.get_mut(i) {
                        Some(old) => {
                            condition.join = old.join;
                            *old = condition;
                        }
                        None => f.conditions.push(condition),
                    }
                    f.row = i;
                    f.column = 2;
                    // A new condition goes straight on to its value.
                    self.filter_edit();
                }
            }
            PickFor::FilterOp(i) => {
                if let Some(Overlay::Filter(f)) = self.overlays.last_mut()
                    && let Some(condition) = f.conditions.get_mut(i)
                {
                    let op = Op::of(picker.field.kind)
                        .iter()
                        .find(|o| o.label() == cell.value);
                    if let Some(op) = op {
                        condition.op = *op;
                    }
                    f.column = 2;
                    if condition.op.needs_value() && condition.value.is_empty() {
                        self.filter_edit();
                    }
                }
            }
            PickFor::FilterValue(i) => {
                if let Some(Overlay::Filter(f)) = self.overlays.last_mut()
                    && let Some(condition) = f.conditions.get_mut(i)
                {
                    condition.value = cell.value;
                    condition.label = cell.display;
                    f.row = i;
                }
            }
        }
    }

    fn form_key(&mut self, key: KeyEvent, submit: bool) {
        if submit {
            self.submit_form();
            return;
        }
        let Some(Overlay::Form(form)) = self.overlays.last_mut() else {
            return;
        };
        let last = form.fields.len().saturating_sub(1);
        let at = form.cursor.min(last);
        let typed = form.fields[at].is_typed();
        match key.code {
            KeyCode::Esc => {
                let written = form
                    .fields
                    .iter()
                    .any(|f| f.is_typed() && f.editor.text.trim().chars().count() > 20);
                // A long note is not lost by one key.
                if written && form.error.as_deref() != Some(LOSE) {
                    form.error = Some(LOSE.into());
                } else {
                    self.overlays.pop();
                }
            }
            KeyCode::Tab => form.cursor = (at + 1) % (last + 1),
            KeyCode::BackTab => form.cursor = (at + last) % (last + 1),
            _ if typed => {
                let field = &mut form.fields[at];
                let single = !field.editor.multiline;
                if key.code == KeyCode::Enter && single {
                    if at == last {
                        self.submit_form();
                    } else {
                        form.cursor = at + 1;
                    }
                    return;
                }
                let taken = field.editor.key(key);
                if taken && form.error.as_deref() == Some(LOSE) {
                    form.error = None;
                }
                if !taken {
                    match key.code {
                        KeyCode::Up => form.cursor = at.saturating_sub(1),
                        KeyCode::Down => form.cursor = (at + 1).min(last),
                        _ => {}
                    }
                }
            }
            KeyCode::Up => form.cursor = at.saturating_sub(1),
            KeyCode::Down => form.cursor = (at + 1).min(last),
            KeyCode::Enter | KeyCode::Char(_) => {
                let (field, cell) = (form.fields[at].field.clone(), form.fields[at].cell.clone());
                let mut picker = self.picker(field, PickFor::Form(at), &cell);
                picker.can_clear = !cell.value.is_empty();
                // Typing starts the search.
                if let KeyCode::Char(c) = key.code
                    && c != ' '
                {
                    picker.editor.insert(&c.to_string());
                }
                self.open_picker(picker);
            }
            KeyCode::Backspace | KeyCode::Delete => form.fields[at].cell = Cell::default(),
            _ => {}
        }
    }

    fn filter_key(&mut self, key: KeyEvent, submit: bool) {
        let now = self.now;
        let Some(Overlay::Filter(f)) = self.overlays.last_mut() else {
            return;
        };
        if let Some(editor) = &mut f.editing {
            match key.code {
                KeyCode::Esc => f.editing = None,
                KeyCode::Enter | KeyCode::Tab => {
                    let typed = editor.text.trim().to_owned();
                    let (table, row) = (f.table.clone(), f.row);
                    f.editing = None;
                    self.filter_typed(&table, row, &typed, now);
                }
                _ if submit => {
                    let typed = editor.text.trim().to_owned();
                    let (table, row) = (f.table.clone(), f.row);
                    f.editing = None;
                    if self.filter_typed(&table, row, &typed, now) {
                        self.filter_apply();
                    }
                }
                _ => {
                    editor.key(key);
                }
            }
            return;
        }
        let count = f.conditions.len();
        if submit {
            self.filter_apply();
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlays.pop();
            }
            KeyCode::Up | KeyCode::Char('k') => f.row = f.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => f.row = (f.row + 1).min(count),
            KeyCode::Left | KeyCode::Char('h') => f.column = f.column.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') => f.column = (f.column + 1).min(2),
            KeyCode::Tab => {
                if f.column == 2 {
                    f.column = 0;
                    f.row = (f.row + 1).min(count);
                } else {
                    f.column += 1;
                }
            }
            KeyCode::Char('+' | 'n') => {
                f.row = count;
                f.column = 0;
                self.filter_edit();
            }
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => {
                if f.row < count {
                    f.conditions.remove(f.row);
                    if let Some(first) = f.conditions.first_mut() {
                        first.join = Join::And;
                    }
                }
            }
            KeyCode::Char('o') => {
                if f.row > 0
                    && let Some(condition) = f.conditions.get_mut(f.row)
                {
                    condition.join = match condition.join {
                        Join::And => Join::Or,
                        Join::Or => Join::New,
                        Join::New => Join::And,
                    };
                }
            }
            KeyCode::Enter => {
                if f.row >= count {
                    f.column = 0;
                }
                self.filter_edit();
            }
            KeyCode::Char('a') => self.filter_apply(),
            _ => {}
        }
    }

    /// Opens the editor of the cell under the cursor of the filter builder.
    fn filter_edit(&mut self) {
        let Some(Overlay::Filter(f)) = self.overlays.last_mut() else {
            return;
        };
        let (row, table) = (f.row, f.table.clone());
        let condition = f.conditions.get(row).cloned();
        if let Some(condition) = condition.as_ref().filter(|c| c.raw.is_some()) {
            f.editing = Some(Editor::new(condition.raw.clone().unwrap_or_default()));
            return;
        }
        let column = if condition.is_none() { 0 } else { f.column };
        let Some(meta) = self.meta(&table) else {
            return;
        };
        let pick = |title: &str, purpose, field: Field, items: Vec<Candidate>, at: usize| Picker {
            title: title.to_owned(),
            purpose,
            field,
            source: PickSource::Fixed(items),
            editor: Editor::default(),
            cursor: at,
            found: Load::Idle,
            search_at: None,
            can_clear: false,
            me: None,
        };
        match (column, condition) {
            (0, condition) | (_, condition @ None) => {
                let mut items: Vec<Candidate> = meta
                    .fields
                    .iter()
                    .filter(|f| f.kind != Kind::Journal && !crate::model::is_sensitive(&f.name))
                    .map(|f| Candidate {
                        value: f.name.clone(),
                        label: f.label.clone(),
                        detail: f.name.clone(),
                    })
                    .collect();
                // The columns on screen are the likely choice.
                let rank = |c: &Candidate| {
                    self.list
                        .columns
                        .iter()
                        .position(|column| column.field == c.value)
                        .unwrap_or(usize::MAX)
                };
                items.sort_by(|a, b| (rank(a), &a.label).cmp(&(rank(b), &b.label)));
                let at = condition
                    .and_then(|c| items.iter().position(|i| i.value == c.field))
                    .unwrap_or(0);
                let picker = pick(
                    "Field",
                    PickFor::FilterField(row),
                    Field::guess(""),
                    items,
                    at,
                );
                self.overlays.push(Overlay::Picker(Box::new(picker)));
            }
            (1, Some(condition)) => {
                let field = self.field(&table, &condition.field).into_owned();
                let items: Vec<Candidate> = Op::of(field.kind)
                    .iter()
                    .map(|o| Candidate {
                        value: o.label().into(),
                        label: o.label().into(),
                        detail: String::new(),
                    })
                    .collect();
                let at = items
                    .iter()
                    .position(|i| i.value == condition.op.label())
                    .unwrap_or(0);
                let picker = pick("Condition", PickFor::FilterOp(row), field, items, at);
                self.overlays.push(Overlay::Picker(Box::new(picker)));
            }
            (_, Some(condition)) => {
                if !condition.op.needs_value() {
                    return;
                }
                let mut field = self.field(&table, &condition.field).into_owned();
                let exact = matches!(condition.op, Op::Is | Op::IsNot);
                let cell = Cell {
                    value: condition.value.clone(),
                    display: condition.label.clone(),
                };
                match field.kind {
                    Kind::Bool => {
                        let items = [("true", "Yes"), ("false", "No")]
                            .map(|(value, label)| Candidate {
                                value: value.into(),
                                label: label.into(),
                                detail: String::new(),
                            })
                            .to_vec();
                        let at = usize::from(condition.value == "false");
                        let picker = pick(
                            &field.label.clone(),
                            PickFor::FilterValue(row),
                            field,
                            items,
                            at,
                        );
                        self.overlays.push(Overlay::Picker(Box::new(picker)));
                    }
                    Kind::Choice if exact && !field.choices.is_empty() => {
                        field.mandatory = true;
                        let picker = self.picker(field, PickFor::FilterValue(row), &cell);
                        self.open_picker(picker);
                    }
                    Kind::Reference if exact && field.reference.is_some() => {
                        field.mandatory = true;
                        let picker = self.picker(field, PickFor::FilterValue(row), &cell);
                        self.open_picker(picker);
                    }
                    _ => {
                        if let Some(Overlay::Filter(f)) = self.overlays.last_mut() {
                            f.column = 2;
                            f.editing = Some(Editor::new(condition.value));
                        }
                    }
                }
            }
        }
    }

    /// Takes a typed value of the filter builder. False when it is wrong.
    fn filter_typed(&mut self, table: &str, row: usize, typed: &str, now: i64) -> bool {
        let condition = match self.overlays.last() {
            Some(Overlay::Filter(f)) => f.conditions.get(row).cloned(),
            _ => None,
        };
        let Some(condition) = condition else {
            return false;
        };
        let mut field = self.field(table, &condition.field).into_owned();
        field.mandatory = false;
        if matches!(condition.op, Op::LastDays | Op::OlderThanDays) {
            field.kind = Kind::Number;
        }
        let checked = match condition.raw {
            Some(_) => Ok(typed.to_owned()),
            None => check_value(&field, typed, now),
        };
        match checked {
            Ok(value) => {
                if let Some(Overlay::Filter(f)) = self.overlays.last_mut()
                    && let Some(condition) = f.conditions.get_mut(row)
                {
                    if condition.raw.is_some() {
                        condition.raw = Some(value);
                    } else {
                        condition.label.clone_from(&value);
                        condition.value = value;
                    }
                }
                true
            }
            Err(why) => {
                if let Some(Overlay::Filter(f)) = self.overlays.last_mut() {
                    f.editing = Some(Editor::new(typed));
                }
                self.toast(ToastKind::Warning, format!("{}: {why}.", field.label));
                false
            }
        }
    }

    fn filter_apply(&mut self) {
        let Some(Overlay::Filter(f)) = self.overlays.pop() else {
            return;
        };
        let query = crate::query::build(&f.conditions);
        if query != self.list.query {
            self.set_query(&query);
        }
    }
}

const LOSE: &str = "Press Esc again to throw this text away";
